//! The routes that set the server up: the hosts' profiles, what is sent to
//! and installed on a host, and the registries, secrets, settings and plugins
//! the server keeps. All of them need the role `admin`.

use super::{
    Serve,
    api::{Answer, App, Fail, internal},
    auth::Access,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Extension, Path, State},
};
use rookery_api::{
    Done, ErrorBody, PluginList, PluginOrigin, PluginRegistries, PluginRegistry, PluginReport,
    PluginSpec, PluginUpdateSpec, PluginUpdates, code,
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
use utoipa::ToSchema;

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
pub(super) fn own_place(paths: &rookery_core::paths::Paths, name: &str) -> Answer<Place> {
    if !safe(name) {
        return Err(bad(format!("Not a host name: {name}")));
    }
    Place::own(paths, name).map_err(bad)
}

/// A saved host's profile, or a 404.
pub(super) fn saved_profile(state: &Serve, name: &str) -> Answer<(Place, Value)> {
    let place = own_place(&state.paths, name)?;
    match place.load(&state.paths).map_err(internal)? {
        Some(profile) => Ok((place, profile)),
        None => Err(missing(format!("No host {name}"))),
    }
}

#[utoipa::path(get, path = "/v1/hosts/{name}/profile", summary = "A host's profile, as it is saved.", tag = "set-up",
    params(("name" = String, Path, description = "A saved host, by its name or its id (bare or as `host_ID`)")),
    responses((status = 200, description = "The profile, with the passwords and keys in it", body = Value),
              (status = 404, description = "No such host", body = ErrorBody)))]
pub async fn profile(State(state): App, Path(name): Path<String>) -> Answer<Json<Value>> {
    Ok(Json(saved_profile(&state, &name)?.1))
}

#[utoipa::path(put, path = "/v1/hosts/{name}/profile", summary = "Save a host's profile: a new host, or a changed one. A host that is not a workstation needs only its type and access.", tag = "set-up",
    params(("name" = String, Path, description = "The name to save it under: letters, digits, `-`, `_` and `.`, and not in the shape of an id; or the id of a saved host")),
    request_body(content = Value, description = "The whole profile. A workstation's has a builder object; a host of the type ssh or managed needs only type and access"),
    responses((status = 200, description = "Saved", body = Done),
              (status = 400, description = "Not a profile, or not a name", body = ErrorBody)))]
pub async fn save_profile(
    State(state): App,
    Path(name): Path<String>,
    access: Option<Extension<Access>>,
    Json(mut profile): Json<Value>,
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

#[utoipa::path(delete, path = "/v1/hosts/{name}", summary = "Remove a host's profile. The host itself is left as it is.", tag = "set-up",
    params(("name" = String, Path, description = "A saved host, by its name or its id (bare or as `host_ID`)")),
    responses((status = 200, description = "Removed", body = Done),
              (status = 404, description = "No such host", body = ErrorBody),
              (status = 409, description = "It has a machine rookery created, which is deleted first, or one is being created", body = ErrorBody)))]
pub async fn remove_host(State(state): App, Path(name): Path<String>) -> Answer<Json<Done>> {
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
#[derive(Serialize, ToSchema)]
pub struct Credentials {
    /// The password of each account, by the account's name.
    pub credentials: Map<String, Value>,
}

#[utoipa::path(get, path = "/v1/hosts/{name}/credentials", summary = "The passwords the server generated for a host's accounts.", tag = "set-up",
    params(("name" = String, Path, description = "A saved host, by its name or its id (bare or as `host_ID`)")),
    responses((status = 200, description = "The passwords by account; none when none were generated", body = Credentials),
              (status = 404, description = "No such host", body = ErrorBody)))]
pub async fn credentials(State(state): App, Path(name): Path<String>) -> Answer<Json<Credentials>> {
    let (place, _) = saved_profile(&state, &name)?;
    let credentials = rookery_core::credentials::load(&state.paths, &place).map_err(internal)?;
    Ok(Json(Credentials { credentials }))
}

// ── Secrets ─────────────────────────────────────────────────────────────────

/// The names of the secrets the server keeps. Their values never leave it
/// except to a host.
#[derive(Serialize, ToSchema)]
pub struct SecretNames {
    /// The global secrets' names, and the addresses of those that belong to
    /// something: a registry's password or token is `registry/NAME`, the
    /// passwords rookery logs in to a host with `ssh/HOST` and `sudo/HOST`,
    /// and a provisioner's own `provisioner/NAME/SECRET`.
    pub names: Vec<String>,
    /// The global secrets, by name, with their ids.
    pub secrets: Vec<rookery_api::Named>,
}

#[derive(Deserialize, ToSchema)]
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

#[utoipa::path(get, path = "/v1/secrets", summary = "The names of the secrets.", tag = "set-up",
    responses((status = 200, description = "The names", body = SecretNames)))]
pub async fn secrets(State(state): App) -> Answer<Json<SecretNames>> {
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

pub(super) fn named(found: rookery_core::store::Found) -> rookery_api::Named {
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

#[utoipa::path(post, path = "/v1/secrets/{name}/rename", summary = "Give a global secret another name.", tag = "set-up",
    params(("name" = String, Path, description = "The secret, by its name or its id (bare or as `sec_ID`)")),
    request_body(content = rookery_api::Rename, description = "The new name"),
    responses((status = 200, description = "Renamed: the profiles that hand it to hosts refer to it still", body = Done),
              (status = 400, description = "Not a name for a secret, or one that belongs to something", body = ErrorBody),
              (status = 404, description = "No such secret", body = ErrorBody),
              (status = 409, description = "The name is taken", body = ErrorBody)))]
pub async fn rename_secret(
    State(state): App,
    Path(name): Path<String>,
    Json(to): Json<rookery_api::Rename>,
) -> Answer<Json<Done>> {
    renamed(store(&state).rename_secret(&name, to.name.trim()))
}

#[utoipa::path(post, path = "/v1/registries/{name}/rename", summary = "Give a registry another name.", tag = "set-up",
    params(("name" = String, Path, description = "The registry, by its name or its id (bare or as `reg_ID`)")),
    request_body(content = rookery_api::Rename, description = "The new name"),
    responses((status = 200, description = "Renamed, with its login: the hosts that use it refer to it still", body = Done),
              (status = 400, description = "Not a name for a registry", body = ErrorBody),
              (status = 404, description = "No such registry", body = ErrorBody),
              (status = 409, description = "The name is taken", body = ErrorBody)))]
pub async fn rename_registry(
    State(state): App,
    Path(name): Path<String>,
    Json(to): Json<rookery_api::Rename>,
) -> Answer<Json<Done>> {
    let new = to.name.trim();
    if !plugins::valid_name(new) || rookery_api::ids::looks_like_id(new) {
        return Err(bad(format!(
            "A registry has a name of lower-case letters, digits, - and _, and not in the shape of an id: {new}"
        )));
    }
    renamed(store(&state).rename_registry(&name, new).map(|_| ()))
}

#[utoipa::path(put, path = "/v1/secrets/{name}", summary = "Set a secret's value.", tag = "set-up",
    params(("name" = String, Path, description = "The secret, by its name or its id (bare or as `sec_ID`); `registry/REGISTRY` is a registry's login, `ssh/HOST` and `sudo/HOST` a host's passwords and `provisioner/PROVISIONER/SECRET` a provisioner's own")),
    request_body(content = SecretValue, description = "The value"),
    responses((status = 200, description = "Kept", body = Done),
              (status = 400, description = "Not a name", body = ErrorBody)))]
pub async fn set_secret(
    State(state): App,
    Path(name): Path<String>,
    Json(secret): Json<SecretValue>,
) -> Answer<Json<Done>> {
    match store(&state).set_secret(secret_name(&name)?, Some(&secret.value)) {
        Ok(()) => Ok(Json(Done { ok: true })),
        // One that would belong to a registry, host or provisioner there is not.
        Err(error) if error.to_string().starts_with("No ") => Err(missing(error.to_string())),
        Err(error) => Err(bad(error.to_string())),
    }
}

#[utoipa::path(delete, path = "/v1/secrets/{name}", summary = "Remove a secret.", tag = "set-up",
    params(("name" = String, Path, description = "The secret, by its name or its id (bare or as `sec_ID`), or the address of one that belongs to something")),
    responses((status = 200, description = "Removed", body = Done),
              (status = 404, description = "No such secret", body = ErrorBody)))]
pub async fn remove_secret(State(state): App, Path(name): Path<String>) -> Answer<Json<Done>> {
    let store = store(&state);
    if !store.has_secret(&name).map_err(internal)? {
        return Err(missing(format!("No secret {name}")));
    }
    store.set_secret(&name, None).map_err(internal)?;
    Ok(Json(Done { ok: true }))
}

// ── Registries ──────────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct Registries {
    /// By name: its `id`, `type` (nexus, npm, raw or docker), `url`,
    /// `username` and, for a Nexus server, its `repositories`. The id is
    /// rookery's, and is left out of what is saved.
    pub registries: Map<String, Value>,
}

#[derive(Serialize, ToSchema)]
pub struct Checked {
    /// One line per thing checked, starting with ✓, ! or ✗.
    pub lines: Vec<String>,
}

#[utoipa::path(get, path = "/v1/registries", summary = "The registries.", tag = "set-up",
    responses((status = 200, description = "The registries by name", body = Registries)))]
pub async fn registries(State(state): App) -> Answer<Json<Registries>> {
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

#[utoipa::path(put, path = "/v1/registries/{name}", summary = "Add a registry, or change one.", tag = "set-up",
    params(("name" = String, Path, description = "The registry's name here, or the id of one there is")),
    request_body(content = Value, description = "`type`, `url`, and optionally `username` and `repositories`"),
    responses((status = 200, description = "Saved", body = Done),
              (status = 400, description = "Not a registry", body = ErrorBody)))]
pub async fn save_registry(
    State(state): App,
    Path(name): Path<String>,
    Json(registry): Json<Value>,
) -> Answer<Json<Done>> {
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

#[utoipa::path(delete, path = "/v1/registries/{name}", summary = "Remove a registry with its stored login.", tag = "set-up",
    params(("name" = String, Path, description = "A registry, by its name or its id (bare or as `reg_ID`)")),
    responses((status = 200, description = "Removed", body = Done),
              (status = 404, description = "No such registry", body = ErrorBody)))]
pub async fn remove_registry(State(state): App, Path(name): Path<String>) -> Answer<Json<Done>> {
    let store = store(&state);
    let mut registries = store.registries().map_err(internal)?;
    if registries.remove(&name).is_none() {
        return Err(missing(format!("No registry {name}")));
    }
    // Its login goes with it.
    store.save_registries(registries).map_err(internal)?;
    Ok(Json(Done { ok: true }))
}

#[utoipa::path(post, path = "/v1/registries/{name}/check", summary = "Whether a registry answers and takes its login.", tag = "set-up",
    params(("name" = String, Path, description = "A registry, by its name or its id (bare or as `reg_ID`)")),
    responses((status = 200, description = "What was found", body = Checked),
              (status = 404, description = "No such registry", body = ErrorBody)))]
pub async fn check_registry(State(state): App, Path(name): Path<String>) -> Answer<Json<Checked>> {
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

#[derive(Serialize, ToSchema)]
pub struct Setting {
    pub key: String,
    /// Empty when it is not set, and for a token.
    pub value: String,
    pub meaning: String,
    /// Whether `settings.set` takes it.
    pub open: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct SettingValue {
    /// Empty to go back to the default.
    pub value: String,
}

#[utoipa::path(get, path = "/v1/settings", summary = "The server's settings.", tag = "set-up",
    responses((status = 200, description = "The settings", body = Vec<Setting>)))]
pub async fn settings(
    State(state): App,
    access: Option<Extension<Access>>,
) -> Answer<Json<Vec<Setting>>> {
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

#[utoipa::path(put, path = "/v1/settings/{key}", summary = "Change one of the settings that are open to callers, or any for the user the server runs as.", tag = "set-up",
    params(("key" = String, Path, description = "The setting")),
    request_body(content = SettingValue, description = "The value"),
    responses((status = 200, description = "Saved", body = Done),
              (status = 403, description = "This one is set on the server itself", body = ErrorBody)))]
pub async fn set_setting(
    State(state): App,
    access: Option<Extension<Access>>,
    Path(key): Path<String>,
    Json(setting): Json<SettingValue>,
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

#[utoipa::path(get, path = "/v1/plugins", summary = "The plugins: those that come with rookery and the server's own.", tag = "set-up",
    responses((status = 200, description = "The plugins with their features, and why Rookery does not use one", body = PluginList)))]
pub async fn plugins(State(state): App) -> Answer<Json<PluginList>> {
    Ok(Json(eggs::Manager::new(&state.paths).list()))
}

#[utoipa::path(post, path = "/v1/plugins", summary = "Install a plugin from the plugin registries, an egg's address or a git repository.", tag = "set-up",
    request_body(content = PluginSpec, description = "What to install"),
    responses((status = 200, description = "Installed; what was installed", body = PluginReport),
              (status = 400, description = "Not a package, address or repository, or it could not be installed; the message says why", body = ErrorBody)))]
pub async fn add_plugin(
    State(state): App,
    Json(asked): Json<PluginSpec>,
) -> Answer<Json<PluginReport>> {
    let paths = state.paths.clone();
    change_plugins(move || eggs::Manager::new(&paths).add(&asked)).await
}

#[utoipa::path(post, path = "/v1/plugins/package", summary = "Install a plugin from an egg, as hatchery pack makes one, which the server keeps.", tag = "set-up",
    request_body(content = Vec<u8>, content_type = "application/gzip", description = "The egg"),
    responses((status = 200, description = "Installed; what was installed", body = PluginReport),
              (status = 400, description = "Not an egg, or it could not be installed; the message says why", body = ErrorBody)))]
pub async fn upload_plugin(State(state): App, body: Bytes) -> Answer<Json<PluginReport>> {
    let paths = state.paths.clone();
    change_plugins(move || eggs::Manager::new(&paths).upload(&body)).await
}

#[utoipa::path(delete, path = "/v1/plugins/{id}", summary = "Remove one of the server's own plugins.", tag = "set-up",
    params(("id" = String, Path, description = "The plugin's id or package")),
    responses((status = 200, description = "Removed", body = PluginReport),
              (status = 404, description = "No such plugin", body = ErrorBody),
              (status = 409, description = "It comes with rookery and stays", body = ErrorBody)))]
pub async fn remove_plugin(
    State(state): App,
    Path(id): Path<String>,
) -> Answer<Json<PluginReport>> {
    let manager = eggs::Manager::new(&state.paths);
    let plugin = manager
        .scan()
        .into_iter()
        .find(|plugin| plugin.id == id || plugin.package == id)
        .ok_or_else(|| missing(format!("No plugin {id}")))?;
    if plugin.origin == PluginOrigin::Shipped {
        return Err(Fail::new(
            code::CONFLICT,
            format!("{} comes with rookery and stays", plugin.label),
        ));
    }
    change_plugins(move || manager.remove(&id)).await
}

#[utoipa::path(get, path = "/v1/plugins/updates", summary = "Ask the registries the server's own plugins came from whether they have newer versions.", tag = "set-up",
    responses((status = 200, description = "Each of the server's own plugins, with the newest version that can be installed where it is newer", body = PluginUpdates),
              (status = 400, description = "The plugin registries could not be read", body = ErrorBody)))]
pub async fn check_plugins(State(state): App) -> Answer<Json<PluginUpdates>> {
    let paths = state.paths.clone();
    Ok(Json(
        on_plugins(move || eggs::Manager::new(&paths).check()).await?,
    ))
}

#[utoipa::path(post, path = "/v1/plugins/update", summary = "Update one of the server's own plugins, or all, to the newest versions that can be installed, and say which hosts are behind then.", tag = "set-up",
    request_body(content = Option<PluginUpdateSpec>, description = "The plugin, by id or package; all without one"),
    responses((status = 200, description = "Updated; what changed, and the hosts that need applying", body = PluginReport),
              (status = 400, description = "It has no registry to ask, comes with rookery, or could not be updated; the message says which", body = ErrorBody),
              (status = 404, description = "No such plugin", body = ErrorBody)))]
pub async fn update_plugins(State(state): App, body: Bytes) -> Answer<Json<PluginReport>> {
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

#[utoipa::path(get, path = "/v1/plugins/registries", summary = "The registries the server installs plugins from, in the order they are searched.", tag = "set-up",
    responses((status = 200, description = "The plugin registries", body = PluginRegistries),
              (status = 400, description = "hatchery.kdl does not read", body = ErrorBody)))]
pub async fn plugin_registries(State(state): App) -> Answer<Json<PluginRegistries>> {
    let paths = state.paths.clone();
    Ok(Json(
        on_plugins(move || eggs::Manager::new(&paths).registries()).await?,
    ))
}

#[utoipa::path(post, path = "/v1/plugins/registries", summary = "Add a plugin registry after the others, or change the one at its location.", tag = "set-up",
    request_body(content = PluginRegistry, description = "The registry"),
    responses((status = 200, description = "Added or changed", body = PluginReport),
              (status = 400, description = "Not a registry the server can read; the message says why", body = ErrorBody)))]
pub async fn add_plugin_registry(
    State(state): App,
    Json(asked): Json<PluginRegistry>,
) -> Answer<Json<PluginReport>> {
    let paths = state.paths.clone();
    change_plugins(move || eggs::Manager::new(&paths).add_registry(&asked)).await
}

/// A plugin registry, by its location.
#[derive(Deserialize, ToSchema)]
pub struct RegistryLocation {
    pub location: String,
}

#[utoipa::path(post, path = "/v1/plugins/registries/remove", summary = "Remove a plugin registry. What was installed from it stays.", tag = "set-up",
    request_body(content = RegistryLocation, description = "The registry's location"),
    responses((status = 200, description = "Removed", body = PluginReport),
              (status = 404, description = "No such registry", body = ErrorBody)))]
pub async fn remove_plugin_registry(
    State(state): App,
    Json(asked): Json<RegistryLocation>,
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
#[derive(Serialize, ToSchema)]
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

#[derive(Serialize, ToSchema)]
pub struct Catalogue {
    /// The profile a new host starts from.
    pub template: Value,
    /// What the server's plugins can install, in the order they are applied.
    pub features: Vec<FeatureInfo>,
}

#[utoipa::path(get, path = "/v1/setup", summary = "What a new host starts from, and what plugins can install on one.", tag = "set-up",
    responses((status = 200, description = "The starting profile and the plugins' features", body = Catalogue)))]
pub async fn catalogue(State(state): App) -> Answer<Json<Catalogue>> {
    let source = state.paths.source().map_err(internal)?;
    let template = std::fs::read(source.join(layout::EXAMPLE)).map_err(internal)?;
    let found = plugins::load(&[
        source.join("plugins"),
        state.paths.config_dir.join("plugins"),
    ]);
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
#[derive(Deserialize, ToSchema)]
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

#[derive(Serialize, ToSchema)]
pub struct Generated {
    /// The host name or the password's hash.
    pub value: String,
    /// What to tell whoever asked.
    pub note: String,
    /// A password that was made up, for whoever asked to write down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

#[utoipa::path(post, path = "/v1/setup/generate", summary = "Make up a host name, or hash or make up a password.", tag = "set-up",
    request_body(content = Generate, description = "What to make"),
    responses((status = 200, description = "What was made", body = Generated),
              (status = 400, description = "It could not be made; the message says why", body = ErrorBody)))]
pub async fn generate(State(state): App, Json(asked): Json<Generate>) -> Answer<Json<Generated>> {
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
