//! The routes that set the server up: the hosts' profiles, what is sent to
//! and installed on a host, and the registries, secrets, settings and plugins
//! the server keeps. All of them need the role `admin`.

use super::{
    Serve,
    api::{Answer, Bytes, Fail, Json, internal},
    auth::Access,
};
use rookery_api::{
    Done, PluginList, PluginOrigin, PluginRegistries, PluginRegistry, PluginReport, PluginSpec,
    PluginUpdateSpec, PluginUpdates, code,
};
use rookery_core::{
    eggs,
    paths::{SETTINGS, layout},
    plugins,
    profiles::{self, Place},
    provision,
    registry::{self, Store},
    server::safe,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::Arc;

fn bad(error: impl std::fmt::Display) -> Fail {
    Fail::new(code::BAD_REQUEST, error.to_string())
}

fn missing(what: impl std::fmt::Display) -> Fail {
    Fail::new(code::NOT_FOUND, what.to_string())
}

/// The server's registries and secrets: its own, even when its settings
/// name another rookery server for its commands.
fn store(state: &Serve) -> Store {
    Store::own(state.store.clone())
}

// ── Profiles ────────────────────────────────────────────────────────────────

/// Where the profile of a host of this name is: a host of the server's own,
/// whatever its settings say, by a name that is never a path.
pub(crate) fn own_place(paths: &rookery_core::paths::Paths, name: &str) -> Answer<Place> {
    if !safe(name) {
        return Err(bad(format!("Not a host name: {name}")));
    }
    Place::own(paths, name).map_err(bad)
}

/// A saved host's profile, or a 404.
pub(crate) fn saved_profile(state: &Serve, name: &str) -> Answer<(Place, Value)> {
    let place = own_place(&state.paths, name)?;
    match place.load(&state.paths).map_err(internal)? {
        Some(profile) => Ok((place, profile)),
        None => Err(missing(format!("No host {name}"))),
    }
}

pub async fn profile(state: Arc<Serve>, name: String) -> Answer<Json<Value>> {
    Ok(Json(saved_profile(&state, &name)?.1))
}

pub async fn save_profile(
    state: Arc<Serve>,
    name: String,
    access: Option<Access>,
    mut profile: Value,
) -> Answer<Json<Done>> {
    // A caller on the socket or in rookery itself is the server's own user.
    let own_user = access.is_none();
    let place = own_place(&state.paths, &name)?;
    if !profile.is_object() {
        return Err(bad("A profile is an object"));
    }
    // A key file a caller names is on another machine, so the server logs in with its own.
    if let Some(builder) = profile.get_mut("builder").and_then(Value::as_object_mut) {
        builder.remove("ssh_key");
    }
    // A workstation's has its set-up's part; a host of another type, only how it is reached.
    let problems = rookery_api::profile::problems(&profile);
    if !problems.is_empty() {
        return Err(bad(problems.join("; ")));
    }
    let access = rookery_api::profile::Access::of(&profile);
    if let Some(key) = access.auth.key() {
        let store = rookery_core::store::shared(&state.paths).map_err(internal)?;
        let known = rookery_core::keys::ring::names(&store).map_err(internal)?;
        if key != rookery_api::profile::ROOKERY_KEY && !known.iter().any(|known| known == key) {
            return Err(bad(format!(
                "The server has no key {key}. See: rookery key list"
            )));
        }
    }
    // How the caller reached this server is not part of the host.
    if let Some(builder) = profile.get_mut("builder").and_then(Value::as_object_mut) {
        builder.remove("served");
        builder.remove("served_only");
    }
    // The link is a command this server runs, and the SSH configuration is
    // its account's: only its own user chooses either.
    let kept = place.load(&state.paths).map_err(internal)?;
    let kept_access = kept.as_ref().map(rookery_api::profile::Access::of);
    let link = kept_access.as_ref().and_then(|kept| kept.link.clone());
    if !own_user && access.link.is_some() && link != access.link {
        return Err(bad(
            "access.link is a command the server runs, and is set on the server itself",
        ));
    }
    let ssh = kept_access.is_some_and(|kept| kept.auth == rookery_api::profile::Auth::Ssh);
    if !own_user && access.auth == rookery_api::profile::Auth::Ssh && !ssh {
        return Err(bad(
            "access.auth ssh is the SSH configuration of the server's own account, and is set on the server itself",
        ));
    }
    place.save(&state.paths, &profile).map_err(internal)?;
    // Where it is and how it is reached may be different now.
    state.forget(&name);
    state.refresh(false);
    Ok(Json(Done { ok: true }))
}

pub async fn remove_host(state: Arc<Serve>, name: String) -> Answer<Json<Done>> {
    let (place, _) = saved_profile(&state, &name)?;
    // Without its record nobody could delete the machine any more.
    if let Some(kept) = rookery_core::resource::of_place(&state.paths, &place)
        .map_err(internal)?
        .filter(|kept| !kept.record.external_id.is_empty())
    {
        return Err(Fail::new(
            code::CONFLICT,
            format!(
                "{name} has a machine rookery created, {} of {}. Remove the host with its action remove: rookery host remove {name} --destroy destroys the machine too, and without --destroy rookery keeps the machine as the resource {}",
                kept.record.external_id, kept.record.provisioner, kept.record.id
            ),
        ));
    }
    if provision::busy(&state.paths, &name) {
        return Err(Fail::new(
            code::CONFLICT,
            format!(
                "Something is being done to {name} now, such as creating its machine. Remove it when that has ended"
            ),
        ));
    }
    profiles::remove(&state.paths, &place).map_err(internal)?;
    state.forget(&name);
    state.notice();
    Ok(Json(Done { ok: true }))
}

/// The passwords rookery generated for a host's accounts.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Credentials {
    /// The password of each account, by the account's name.
    pub credentials: Map<String, Value>,
}

pub async fn credentials(state: Arc<Serve>, name: String) -> Answer<Json<Credentials>> {
    let (place, _) = saved_profile(&state, &name)?;
    let credentials = rookery_core::credentials::load(&state.paths, &place).map_err(internal)?;
    Ok(Json(Credentials { credentials }))
}

// ── Secrets ─────────────────────────────────────────────────────────────────

/// The names of the secrets the server keeps. Their values never leave it
/// except to a host.
#[derive(Serialize, schemars::JsonSchema)]
pub struct SecretNames {
    /// The global secrets' names, and the addresses of those that belong to
    /// something: a registry's password or token is `registry/NAME`, the
    /// passwords rookery logs in to a host with `ssh/HOST` and `sudo/HOST`,
    /// and a provisioner's own `provisioner/NAME/SECRET`.
    pub names: Vec<String>,
    /// The global secrets, by name, with their ids.
    pub secrets: Vec<rookery_api::Named>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SecretValue {
    pub value: String,
}

fn secret_name(name: &str) -> Answer<&str> {
    use rookery_api::ids::{self, Kind};
    // A host's SSH or sudo password belongs to the host, by its name or id.
    if let Some(host) = name
        .strip_prefix("ssh/")
        .or_else(|| name.strip_prefix("sudo/"))
    {
        return match profiles::valid_name(host) || ids::id_in(Kind::Host, host).is_some() {
            true => Ok(name),
            false => Err(bad(format!("Not a host's name: {host}"))),
        };
    }
    if rookery_core::provisioner::is_secret_name(name) {
        return Ok(name);
    }
    if let Some(registry) = name.strip_prefix("registry/")
        && ids::id_in(Kind::Registry, registry).is_some()
    {
        return Ok(name);
    }
    let plain = name.strip_prefix("registry/").unwrap_or(name);
    match plugins::valid_name(plain) && !ids::looks_like_id(plain) {
        true => Ok(name),
        false => Err(bad(format!(
            "Not a secret's name: {name} (lower-case letters, digits, - and _, and not in the shape of an id)"
        ))),
    }
}

pub async fn secrets(state: Arc<Serve>) -> Answer<Json<SecretNames>> {
    let store = store(&state);
    let names = store.all_secret_names().map_err(internal)?;
    let secrets = store
        .secret_ids()
        .map_err(internal)?
        .into_iter()
        .map(named)
        .collect();
    Ok(Json(SecretNames { names, secrets }))
}

pub(crate) fn named(found: rookery_core::store::Found) -> rookery_api::Named {
    rookery_api::Named {
        id: found.id,
        name: found.name,
    }
}

/// A rename's error as an answer: a name that is taken or cannot be one is
/// refused, a thing that is not there not found.
fn renamed(result: rookery_core::Result<()>) -> Answer<Json<Done>> {
    match result {
        Ok(()) => Ok(Json(Done { ok: true })),
        Err(error) if error.to_string().starts_with("No ") => Err(missing(error.to_string())),
        Err(error) if error.to_string().starts_with("There is ") => {
            Err(Fail::new(code::CONFLICT, error.to_string()))
        }
        Err(error) => Err(bad(error.to_string())),
    }
}

pub async fn rename_secret(
    state: Arc<Serve>,
    name: String,
    to: rookery_api::Rename,
) -> Answer<Json<Done>> {
    renamed(store(&state).rename_secret(&name, to.name.trim()))
}

pub async fn rename_registry(
    state: Arc<Serve>,
    name: String,
    to: rookery_api::Rename,
) -> Answer<Json<Done>> {
    let new = to.name.trim();
    if !plugins::valid_name(new) || rookery_api::ids::looks_like_id(new) {
        return Err(bad(format!(
            "A registry has a name of lower-case letters, digits, - and _, and not in the shape of an id: {new}"
        )));
    }
    renamed(store(&state).rename_registry(&name, new).map(|_| ()))
}

pub async fn set_secret(
    state: Arc<Serve>,
    name: String,
    secret: SecretValue,
) -> Answer<Json<Done>> {
    match store(&state).set_secret(secret_name(&name)?, Some(&secret.value)) {
        Ok(()) => Ok(Json(Done { ok: true })),
        // One that would belong to a registry, host or provisioner there is not.
        Err(error) if error.to_string().starts_with("No ") => Err(missing(error.to_string())),
        Err(error) => Err(bad(error.to_string())),
    }
}

pub async fn remove_secret(state: Arc<Serve>, name: String) -> Answer<Json<Done>> {
    let store = store(&state);
    if !store.has_secret(&name).map_err(internal)? {
        return Err(missing(format!("No secret {name}")));
    }
    store.set_secret(&name, None).map_err(internal)?;
    Ok(Json(Done { ok: true }))
}

// ── Registries ──────────────────────────────────────────────────────────────

#[derive(Serialize, schemars::JsonSchema)]
pub struct Registries {
    /// By name: its `id`, `type` (nexus, npm, raw or docker), `url`,
    /// `username` and, for a Nexus server, its `repositories`. The id is
    /// rookery's, and is left out of what is saved.
    pub registries: Map<String, Value>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct Checked {
    /// One line per thing checked, starting with ✓, ! or ✗.
    pub lines: Vec<String>,
}

pub async fn registries(state: Arc<Serve>) -> Answer<Json<Registries>> {
    let store = store(&state);
    let mut registries = store.registries().map_err(internal)?;
    for found in store.registry_ids().map_err(internal)? {
        if let Some(fields) = registries
            .get_mut(&found.name)
            .and_then(Value::as_object_mut)
        {
            fields.insert("id".into(), Value::String(found.id));
        }
    }
    Ok(Json(Registries { registries }))
}

pub async fn save_registry(state: Arc<Serve>, name: String, registry: Value) -> Answer<Json<Done>> {
    let kind = registry["type"].as_str().unwrap_or("");
    let url = registry["url"].as_str().unwrap_or("");
    if !plugins::valid_name(&name)
        || rookery_api::ids::looks_like_id(&name)
        || !registry::TYPES.contains(&kind)
        || url.is_empty()
    {
        return Err(bad(format!(
            "A registry has a name of lower-case letters, digits, - and _, a type ({}) and a url",
            registry::TYPES.join(", ")
        )));
    }
    let store = store(&state);
    let mut registries = store.registries().map_err(internal)?;
    registries.insert(name, registry);
    store.save_registries(registries).map_err(internal)?;
    Ok(Json(Done { ok: true }))
}

pub async fn remove_registry(state: Arc<Serve>, name: String) -> Answer<Json<Done>> {
    let store = store(&state);
    let mut registries = store.registries().map_err(internal)?;
    if registries.remove(&name).is_none() {
        return Err(missing(format!("No registry {name}")));
    }
    // Its login goes with it.
    store.save_registries(registries).map_err(internal)?;
    Ok(Json(Done { ok: true }))
}

pub async fn check_registry(state: Arc<Serve>, name: String) -> Answer<Json<Checked>> {
    let store = store(&state);
    if !store.registries().map_err(internal)?.contains_key(&name) {
        return Err(missing(format!("No registry {name}")));
    }
    // It asks the registry with curl, and waits for it.
    let lines = tokio::task::spawn_blocking(move || {
        registry::check(&store, &name).map_err(|error| error.to_string())
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(Checked { lines }))
}

// ── Settings ────────────────────────────────────────────────────────────────

/// The settings a caller may change. The others say who gets in and where the
/// server's files are, which is set on the server itself.
const OPEN_SETTINGS: &[&str] = &[
    "default_host",
    "retention_days",
    "tailnet_login_server",
    "tailnet_user",
    "tailnet_tags",
];

#[derive(Serialize, schemars::JsonSchema)]
pub struct Setting {
    pub key: String,
    /// Empty when it is not set, and for a token.
    pub value: String,
    pub meaning: String,
    /// Whether `settings.set` takes it.
    pub open: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SettingValue {
    /// Empty to go back to the default.
    pub value: String,
}

pub async fn settings(state: Arc<Serve>, access: Option<Access>) -> Answer<Json<Vec<Setting>>> {
    // The user the server runs as changes any of them, as rookery config set does.
    let local = access.is_none();
    let saved = state.paths.settings().map_err(internal)?;
    let settings = SETTINGS
        .iter()
        .map(|(key, meaning)| Setting {
            key: (*key).into(),
            value: match saved.get(*key) {
                Some(_) if rookery_core::paths::is_secret(key) => String::new(),
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            },
            meaning: (*meaning).into(),
            open: local || OPEN_SETTINGS.contains(key),
        })
        .collect();
    Ok(Json(settings))
}

pub async fn set_setting(
    state: Arc<Serve>,
    access: Option<Access>,
    key: String,
    setting: SettingValue,
) -> Answer<Json<Done>> {
    if access.is_some() && !OPEN_SETTINGS.contains(&key.as_str()) {
        return Err(Fail::new(
            code::FORBIDDEN,
            format!("{key} is set on the server itself, with rookery config set"),
        ));
    }
    state.paths.set(&key, &setting.value).map_err(bad)?;
    Ok(Json(Done { ok: true }))
}

// ── Plugins ─────────────────────────────────────────────────────────────────

/// One change at a time works on the plugins.
static PLUGINS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run work on the plugins, which waits for registries and git, on a
/// thread for blocking work. An error that names no plugin is the asker's.
async fn on_plugins<T: Send + 'static>(
    work: impl FnOnce() -> rookery_core::Result<T> + Send + 'static,
) -> Answer<T> {
    tokio::task::spawn_blocking(move || {
        let _one = PLUGINS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        work().map_err(|error| error.to_string())
    })
    .await
    .map_err(internal)?
    .map_err(|error| match error.starts_with("No plugin") {
        true => missing(error),
        false => bad(error),
    })
}

/// Run a change of the plugins and say what it did.
async fn change_plugins(
    work: impl FnOnce() -> rookery_core::Result<Vec<String>> + Send + 'static,
) -> Answer<Json<PluginReport>> {
    let lines = on_plugins(work).await?;
    Ok(Json(PluginReport {
        lines,
        behind: vec![],
    }))
}

pub async fn plugins(state: Arc<Serve>) -> Answer<Json<PluginList>> {
    Ok(Json(eggs::Manager::new(&state.paths).list()))
}

pub async fn add_plugin(state: Arc<Serve>, asked: PluginSpec) -> Answer<Json<PluginReport>> {
    let paths = state.paths.clone();
    change_plugins(move || eggs::Manager::new(&paths).add(&asked)).await
}

pub async fn upload_plugin(state: Arc<Serve>, body: Bytes) -> Answer<Json<PluginReport>> {
    let paths = state.paths.clone();
    change_plugins(move || eggs::Manager::new(&paths).upload(&body)).await
}

pub async fn remove_plugin(state: Arc<Serve>, id: String) -> Answer<Json<PluginReport>> {
    let manager = eggs::Manager::new(&state.paths);
    let plugin = manager
        .scan()
        .into_iter()
        .find(|plugin| plugin.id == id || plugin.package == id)
        .ok_or_else(|| missing(format!("No plugin {id}")))?;
    if plugin.origin == PluginOrigin::Shipped {
        return Err(Fail::new(
            code::CONFLICT,
            format!(
                "{} comes with rookery and stays while its registry is on: rookery plugin registry disable rookery turns that off",
                plugin.label
            ),
        ));
    }
    change_plugins(move || manager.remove(&id)).await
}

pub async fn check_plugins(state: Arc<Serve>) -> Answer<Json<PluginUpdates>> {
    let paths = state.paths.clone();
    Ok(Json(
        on_plugins(move || eggs::Manager::new(&paths).check()).await?,
    ))
}

pub async fn update_plugins(state: Arc<Serve>, body: Bytes) -> Answer<Json<PluginReport>> {
    let asked: PluginUpdateSpec = match body.is_empty() {
        true => PluginUpdateSpec::default(),
        false => serde_json::from_slice(&body).map_err(bad)?,
    };
    let paths = state.paths.clone();
    let Json(mut report) =
        change_plugins(move || eggs::Manager::new(&paths).update(asked.id.as_deref())).await?;
    report.behind = super::api::behind_hosts(&state).await;
    Ok(Json(report))
}

pub async fn plugin_registries(state: Arc<Serve>) -> Answer<Json<PluginRegistries>> {
    let paths = state.paths.clone();
    Ok(Json(
        on_plugins(move || eggs::Manager::new(&paths).registries()).await?,
    ))
}

pub async fn add_plugin_registry(
    state: Arc<Serve>,
    access: Option<Access>,
    asked: PluginRegistry,
) -> Answer<Json<PluginReport>> {
    // A folder of the server is read with its rights, so only a caller on
    // this machine names one.
    if access.is_some() && eggs::local_registry(&asked.kind, &asked.location) {
        return Err(Fail::new(
            code::FORBIDDEN,
            format!(
                "{} is a folder of the server's machine, and only a caller there adds it as a registry, over the server's socket: run rookery plugin registry add there",
                asked.location.trim()
            ),
        ));
    }
    let paths = state.paths.clone();
    change_plugins(move || eggs::Manager::new(&paths).add_registry(&asked)).await
}

/// A plugin registry, by its location or name.
#[derive(Deserialize, schemars::JsonSchema)]
pub struct RegistryLocation {
    pub location: String,
}

pub async fn remove_plugin_registry(
    state: Arc<Serve>,
    asked: RegistryLocation,
) -> Answer<Json<PluginReport>> {
    let paths = state.paths.clone();
    let lines = tokio::task::spawn_blocking(move || {
        let _one = PLUGINS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        eggs::Manager::new(&paths)
            .remove_registry(&asked.location)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(internal)?
    .map_err(|error| match error.starts_with("No plugin registry") {
        true => missing(error),
        false => bad(error),
    })?;
    Ok(Json(PluginReport {
        lines,
        behind: vec![],
    }))
}

// ── What a set-up screen needs ──────────────────────────────────────────────

/// Something a plugin installs, for the Features page of a host.
#[derive(Serialize, schemars::JsonSchema)]
pub struct FeatureInfo {
    /// `PLUGIN/FEATURE`, as a profile's `rookery_plugin_features` names it.
    pub key: String,
    pub label: String,
    /// The group of the Features page it is listed in.
    pub group: String,
    pub help: String,
    /// The types of host it is for, such as `workstation` or `node`.
    pub types: Vec<rookery_api::HostType>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct Catalogue {
    /// The profile a new host starts from.
    pub template: Value,
    /// What the server's plugins can install, in the order they are applied.
    pub features: Vec<FeatureInfo>,
}

pub async fn catalogue(state: Arc<Serve>) -> Answer<Json<Catalogue>> {
    let source = state.paths.source().map_err(internal)?;
    let template = std::fs::read(source.join(layout::EXAMPLE)).map_err(internal)?;
    let found = plugins::installed(&state.paths.config_dir);
    let features = found
        .iter()
        .flat_map(|plugin| {
            let hosting = plugins::hosting(plugin);
            plugin.features.iter().map(move |feature| FeatureInfo {
                key: format!("{}/{}", plugin.id, feature.id),
                label: feature.label.clone(),
                group: feature.group.clone(),
                help: feature.help.to_owned(),
                types: hosting
                    .iter()
                    .find(|known| known.feature == feature.id)
                    .map(|known| known.types.clone())
                    .unwrap_or_else(|| vec![rookery_api::HostType::Workstation]),
            })
        })
        .collect();
    let mut template: Value = serde_json::from_slice(&template).map_err(internal)?;
    // How a host is reached is part of every profile, also of one not reached yet.
    if !template["builder"].is_object() {
        template["builder"] = json!({});
    }
    Ok(Json(Catalogue { template, features }))
}

/// Something for the server to make up for a profile.
#[derive(Deserialize, schemars::JsonSchema)]
pub struct Generate {
    /// `hostname` or `password`.
    pub what: String,
    /// For `password`: the password to hash. Without it one is made up and
    /// kept in the server's store with `host`, under `user`, for
    /// `hosts.credentials` to give.
    pub password: Option<String>,
    pub host: Option<String>,
    pub user: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct Generated {
    /// The host name or the password's hash.
    pub value: String,
    /// What to tell whoever asked.
    pub note: String,
    /// A password that was made up, for whoever asked to write down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

pub async fn generate(state: Arc<Serve>, asked: Generate) -> Answer<Json<Generated>> {
    use rookery_core::credentials;
    let kept = match (&asked.host, asked.what.as_str()) {
        (Some(host), "password") if asked.password.is_none() => {
            Some(own_place(&state.paths, host)?)
        }
        _ => None,
    };
    let paths = state.paths.clone();
    let made = tokio::task::spawn_blocking(move || -> Result<Generated, String> {
        let fail = |error: Box<dyn std::error::Error>| error.to_string();
        match asked.what.as_str() {
            "hostname" => Ok(Generated {
                value: credentials::hostname().map_err(fail)?,
                note: String::new(),
                password: None,
            }),
            "password" => match (asked.password, kept) {
                (Some(password), _) => Ok(Generated {
                    value: credentials::hash(&password).map_err(fail)?,
                    note: "Password hashed".into(),
                    password: None,
                }),
                (None, Some(place)) => {
                    let password = credentials::password().map_err(fail)?;
                    let value = credentials::hash(&password).map_err(fail)?;
                    let user = asked.user.filter(|user| !user.is_empty());
                    let user = user.as_deref().unwrap_or("account");
                    credentials::save(&paths, &place, user, &password).map_err(fail)?;
                    Ok(Generated {
                        value,
                        note: format!(
                            "Generated password kept in the server's store; see it with: rookery host credentials {}",
                            place.argument()
                        ),
                        password: Some(password),
                    })
                }
                (None, None) => {
                    Err("A made-up password is kept with a host: name the host".into())
                }
            },
            other => Err(format!("Nothing to make for {other}")),
        }
    })
    .await
    .map_err(internal)?
    .map_err(bad)?;
    Ok(Json(made))
}
