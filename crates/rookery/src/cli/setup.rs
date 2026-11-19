//! What the set-up of a host does, as commands: a new host through the same
//! flow, and the keys, secrets, registries, plugins and settings of the
//! server.

use super::args::{self, Global, Key, Plugin, Registry, Secret};
use super::ask;
use super::client::{Client, Subject};
use super::fail::{Fail, Result};
use super::follow;
use super::host;
use super::out::{self, Style, Table, styled};
use super::settings;
use rookery_api::Act;
use rookery_setup::fields::{Action, Step};
use rookery_setup::flow::{self, Catalogue, Flow, Given, Hash, Maker, Stop};
use rookery_setup::setup::Feature;
use serde_json::{Value, json};

/// The keys of `--set`, for the help of `rookery host create` and `rookery host set`.
pub fn keys_help() -> String {
    let mut text = String::from("How it is installed, with --method:\n");
    for action in Action::SERVED {
        text.push_str(&format!("  {:<12} {}\n", action.key(), action.label()));
    }
    text.push_str(
        "\nThe fields of a profile, with --set KEY=VALUE. A switch takes on or off. feature.PLUGIN/FEATURE=on|off, secret.NAME=VARIABLE|PATH and registry.NAME=on|off give a host those:\n",
    );
    let wide = flow::keys()
        .iter()
        .map(|(key, _, _)| key.len())
        .max()
        .unwrap_or(0);
    for (key, label, help) in flow::keys() {
        let first = help
            .split(". ")
            .next()
            .unwrap_or(help)
            .trim_end_matches('.');
        let _ = label;
        text.push_str(&format!("  {key:<wide$}  {first}\n"));
    }
    text
}

/// What the server gives the set-up of a new host.
pub fn catalogue(client: &Client) -> Result<Catalogue> {
    let found = client.call("setup.catalogue", json!({}))?;
    let features = found["features"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|feature| {
            let text = |key: &str| feature[key].as_str().unwrap_or("").to_owned();
            Feature {
                key: text("key"),
                label: text("label"),
                group: text("group"),
                help: text("help"),
                types: rookery_setup::setup::feature_types(feature),
            }
        })
        .collect();
    let secrets = client
        .call("installation.secrets.list", json!({}))
        .map(|found| names(&found["names"]))
        .unwrap_or_default();
    let own_key = client
        .call("keys.list", json!({}))
        .ok()
        .and_then(|found| {
            found["keys"]
                .as_array()?
                .iter()
                .find(|key| key["name"] == "rookery")
                .map(|key| key["public"].as_str().unwrap_or("").to_owned())
        })
        .unwrap_or_default();
    Ok(Catalogue {
        template: found["template"].clone(),
        features,
        secrets,
        own_key,
        provisioners: vec![],
    })
}

fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect()
}

/// The server makes the hashes of the accounts' passwords, and keeps the
/// passwords it makes up.
struct Server<'a>(&'a Client, bool);

impl Maker for Server<'_> {
    fn hash(&mut self, host: &str, hash: Hash) -> std::result::Result<String, String> {
        let asked = match hash {
            Hash::Generate { user } => {
                self.1 = true;
                json!({"what": "password", "host": host, "user": user})
            }
            Hash::Of(password) => json!({"what": "password", "password": password}),
        };
        let made = self
            .0
            .call("setup.generate", asked)
            .map_err(|fail| fail.message)?;
        Ok(made["value"].as_str().unwrap_or("").to_owned())
    }
}

/// The profile `--from` names: a host's, a file, or standard input.
fn starting_profile(client: &Client, from: &str) -> Result<Value> {
    if from == "-" || std::path::Path::new(from).is_file() {
        let bytes = host::read_file(from)?;
        return serde_json::from_slice(&bytes)
            .map_err(|error| Fail::usage(format!("{from} holds no JSON profile: {error}")));
    }
    let mut profile = host::profile_of(client, from)
        .map_err(|fail| fail.context(&format!("--from {from} is no file and no host")))?;
    // What belongs to the other host itself stays with it.
    if let Some(profile) = profile.as_object_mut() {
        for key in ["hostname", "operator_password_hash", "agent_password_hash"] {
            profile.remove(key);
        }
    }
    if let Some(access) = profile.get_mut("access").and_then(Value::as_object_mut) {
        access.remove("address");
    }
    Ok(profile)
}

/// `rookery host create`: the set-up flow, asked inline
/// or taken from the flags, then saved, then run as its way says.
pub fn create(client: &Client, create: &args::HostCreate) -> Result<()> {
    let name = &create.name;
    if client.call("hosts.get", json!({"host": name})).is_ok() {
        // A duplicate's machine is planned with its provisioner already.
        let planned =
            host::machine_of(client, name)?.filter(|machine| machine["state"] == "planned");
        return match (&create.provisioner, planned) {
            (Some(_), _) | (None, Some(_)) => make_saved(client, create),
            (None, None) => Err(Fail::failed(format!(
                "There is a host {name} already. Change it with rookery host set {name} KEY=VALUE, or see it with rookery host show {name}"
            ))),
        };
    }
    let mut catalogue = catalogue(client)?;
    if let Some(from) = &create.from {
        catalogue.template = starting_profile(client, from)?;
    }
    let method = match &create.method {
        Some(key) => Some(Action::by_key(key).ok_or_else(|| {
            let known: Vec<&str> = Action::SERVED.iter().map(|action| action.key()).collect();
            Fail::usage(format!("--method is {}, not {key}", known.join(", ")))
        })?),
        None => None,
    };
    if method.is_none_or(|method| method == Action::Machine) {
        catalogue.provisioners = provisioners(client)?;
    }
    let mut given = Given {
        method,
        generate_passwords: create.generate_passwords,
        provisioner: create.provisioner.clone(),
        ..Given::default()
    };
    if let Some(host_type) = &create.host_type {
        given
            .set(&[format!("type={host_type}")])
            .map_err(Fail::usage)?;
    }
    given.set(&create.set).map_err(Fail::usage)?;
    given
        .features
        .extend(create.feature.iter().map(|key| (key.clone(), true)));
    given
        .features
        .extend(create.no_feature.iter().map(|key| (key.clone(), false)));
    for word in &create.arguments {
        let (key, value) = word.split_once('=').ok_or_else(|| {
            Fail::usage(format!(
                "The provisioner's arguments are NAME=VALUE, not {word}"
            ))
        })?;
        given.arguments.push((key.to_owned(), value.to_owned()));
    }
    if !given.arguments.is_empty() && given.provisioner.is_none() && method != Some(Action::Machine)
    {
        return Err(Fail::usage(
            "NAME=VALUE arguments go to a provisioner: add --provisioner NAME",
        ));
    }
    let mut flow = Flow::new(name, catalogue);
    let mut maker = Server(client, false);
    flow.run(&given, &mut ask::asker(), &mut maker)
        .map_err(stopped)?;
    host::save(client, name, &flow.config)?;
    out::done(format!("Saved {name}"));
    if maker.1 {
        out::hint(format!("Its passwords: rookery host credentials {name}"));
    }
    run_way(client, &flow, create)
}

/// The provisioners that can make a host, as the Install tab lists them.
fn provisioners(client: &Client) -> Result<Vec<rookery_api::ProvisionerInfo>> {
    let listed = client.call("provisioners.list", json!({}))?;
    Ok(serde_json::from_value(listed)?)
}

/// Make the machine of a host that is saved and has none, with its profile
/// as it is, as the Run tab of its set-up does.
fn make_saved(client: &Client, create: &args::HostCreate) -> Result<()> {
    let name = &create.name;
    let machine = host::machine_of(client, name)?;
    if let Some(machine) = machine
        .as_ref()
        .filter(|machine| !machine["external_id"].as_str().unwrap_or("").is_empty())
    {
        return Err(Fail::failed(format!(
            "{name} has a machine already, {} of {}. rookery host show {name} says how it is",
            machine["external_id"].as_str().unwrap_or(""),
            machine["provisioner"].as_str().unwrap_or("")
        )));
    }
    // One planned, as for a duplicate, is made as it was planned unless told otherwise.
    let planned = machine.filter(|machine| machine["state"] == "planned");
    let catalogue = Catalogue {
        provisioners: provisioners(client)?,
        ..Catalogue::default()
    };
    let mut flow = Flow::new(name, catalogue);
    flow.config = host::profile_of(client, name)?;
    let mut given = Given {
        provisioner: create.provisioner.clone().or_else(|| {
            planned
                .as_ref()
                .and_then(|machine| machine["provisioner"].as_str())
                .map(str::to_owned)
        }),
        ..Given::default()
    };
    for word in &create.arguments {
        let (key, value) = word.split_once('=').ok_or_else(|| {
            Fail::usage(format!(
                "The provisioner's arguments are NAME=VALUE, not {word}"
            ))
        })?;
        given.arguments.push((key.to_owned(), value.to_owned()));
    }
    if let Some(planned) =
        planned.filter(|machine| machine["provisioner"].as_str() == given.provisioner.as_deref())
    {
        for (key, value) in planned["arguments"].as_object().into_iter().flatten() {
            if !given.arguments.iter().any(|(known, _)| known == key) {
                given
                    .arguments
                    .push((key.clone(), value.as_str().unwrap_or("").to_owned()));
            }
        }
    }
    flow.machine_questions(&given, &mut ask::asker())
        .map_err(stopped)?;
    run_way(client, &flow, create)
}

/// Why the flow stopped, as the command line says it: with the flags that
/// give what is missing.
fn stopped(stop: Stop) -> Fail {
    match stop {
        Stop::Missing(question) => Fail::usage(format!(
            "{} is missing: pass {}",
            question.label, question.flag
        )),
        Stop::Problems(problems) => {
            // Only a secret the server lacks stops it: the command line was right.
            let refused = problems
                .iter()
                .all(|problem| problem.field == Some(rookery_setup::fields::Field::Tailscale));
            let said: Vec<String> = problems
                .iter()
                .map(|problem| match problem.field {
                    Some(field) if field.is_password_hash() => {
                        format!(
                            "{} (--generate-passwords, or {})",
                            problem.text,
                            flow::flag_of(field)
                        )
                    }
                    Some(field) => format!("{} ({})", problem.text, flow::flag_of(field)),
                    None => problem.text.clone(),
                })
                .collect();
            let said = format!("Not saved: {}", said.join("; "));
            match refused {
                true => Fail::refused(said),
                false => Fail::usage(said),
            }
        }
        Stop::Refused(why) if why == "Stopped" => Fail::exit(130),
        Stop::Refused(why) => Fail::usage(why),
    }
}

/// The Run tab: create the host with its provisioner, or install it the
/// way chosen, when that was asked for.
fn run_way(client: &Client, flow: &Flow, create: &args::HostCreate) -> Result<()> {
    let name = &flow.name;
    let host_type = rookery_api::profile::host_type(&flow.config);
    if flow.action == Action::Machine {
        let request = flow.request().map_err(Fail::usage)?;
        let made = client.call("resources.create", serde_json::to_value(&request)?)?;
        let made: Made = serde_json::from_value(made)?;
        let acted = rookery_proto::hosts::Acted::of_job(made.job);
        let title = format!("Creating {name} with {}", request.provisioner);
        follow::acted(client, acted, &title, create.detach.detach, false)?;
        return Ok(());
    }
    let (question, then) = match (flow.action, host_type) {
        (Action::Iso, _) => (
            "Build the USB installer now?",
            format!("rookery host iso {name} --write DEVICE"),
        ),
        (Action::CloudSeed, _) => {
            return {
                out::hint(format!(
                    "Its cloud seed: rookery host seed {name} > user-data"
                ));
                Ok(())
            };
        }
        (Action::Reach, _) | (Action::Ssh, rookery_api::HostType::Ssh) => {
            out::hint(format!("Log in to it once: rookery key trust {name}"));
            return Ok(());
        }
        (_, rookery_api::HostType::Managed) => (
            "Install the controller on it now?",
            format!("rookery host apply {name}"),
        ),
        _ => ("Apply it now?", format!("rookery host apply {name}")),
    };
    let run = create.run
        || (out::get().ask
            && matches!(
                ask::ask(&rookery_setup::flow::Question {
                    step: Step::Run,
                    label: question.into(),
                    help: String::new(),
                    flag: "--run".into(),
                    asked: rookery_setup::flow::Asked::Confirm { default: false },
                    required: false,
                })?,
                rookery_setup::flow::Answer::Confirm(true)
            ));
    if !run {
        out::hint(format!("Install it with: {then}"));
        return Ok(());
    }
    match flow.action {
        Action::Iso => {
            let acted = client.act(&Subject::Host(name.clone()), "installer", &Act::default())?;
            follow::acted(
                client,
                acted,
                &format!("Building the installer of {name}"),
                create.detach.detach,
                false,
            )?;
            Ok(())
        }
        _ => host::apply(
            client,
            &args::HostApply {
                name: name.clone(),
                detach: create.detach.clone(),
            },
        ),
    }
}

// ── keys ────────────────────────────────────────────────────────────────────

pub fn key(client: &Client, command: Key) -> Result<()> {
    match command {
        Key::List => {
            let found = client.call("keys.list", json!({}))?;
            out::data(&found, || {
                let mut table = Table::new(&["name", "fingerprint", "hosts"]);
                for key in found["keys"].as_array().into_iter().flatten() {
                    let mut name = key["name"].as_str().unwrap_or("").to_owned();
                    if key["pending"].is_object() {
                        name.push_str(&styled(" (rotating)", Style::Warn));
                    }
                    table.row(vec![
                        name,
                        key["fingerprint"].as_str().unwrap_or("").to_owned(),
                        names(&key["hosts"]).join(", "),
                    ]);
                }
                table.print("No keys");
            });
            Ok(())
        }
        Key::Show { name } => {
            let found = client.call("keys.list", json!({}))?;
            let key = found["keys"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|key| key["name"] == name.as_str())
                .cloned()
                .ok_or_else(|| {
                    Fail::failed(format!("No key {name}. rookery key list names them"))
                })?;
            out::data(&key, || {
                println!("{}", key["public"].as_str().unwrap_or(""))
            });
            Ok(())
        }
        Key::Create { name } => {
            let made = client.call("keys.add", json!({"name": name}))?;
            out::data(&made, || {
                out::done(format!("Made the key {name}"));
                println!("{}", made["public"].as_str().unwrap_or(""));
            });
            Ok(())
        }
        Key::Import { name, file } => {
            let private = String::from_utf8(host::read_file(&file)?)
                .map_err(|_| Fail::usage(format!("{file} is no OpenSSH private key")))?;
            let made = client.call("keys.add", json!({"name": name, "private": private}))?;
            out::data(&made, || {
                out::done(format!(
                    "Added the key {name}: {}",
                    made["fingerprint"].as_str().unwrap_or("")
                ))
            });
            Ok(())
        }
        Key::Remove { name } => {
            ask::confirm(&format!(
                "This removes the key {name} from rookery's key ring."
            ))?;
            client.call("keys.remove", json!({"name": name}))?;
            out::done(format!("Removed the key {name}"));
            Ok(())
        }
        Key::Rotate { name, detach } => {
            ask::confirm(&format!(
                "This replaces the key {name} on every host that takes it."
            ))?;
            let acted: rookery_proto::hosts::Acted =
                serde_json::from_value(client.call("keys.rotate", json!({"name": name}))?)?;
            follow::acted(
                client,
                acted,
                &format!("Rotating the key {name}"),
                detach.detach,
                false,
            )?;
            Ok(())
        }
        Key::Trust {
            host,
            key,
            password_stdin,
        } => {
            let password = match password_stdin {
                true => Some(ask::secret_from(
                    "Password",
                    None,
                    true,
                    "--password-stdin",
                )?),
                false => None,
            };
            host::trust(client, &host, key.as_deref(), password)
        }
    }
}

// ── secrets ─────────────────────────────────────────────────────────────────

pub fn secret(client: &Client, command: Secret) -> Result<()> {
    match command {
        Secret::List => {
            let found = client.call("installation.secrets.list", json!({}))?;
            out::data(&found, || {
                let all = names(&found["names"]);
                if all.is_empty() {
                    out::hint("No secrets. Set one with rookery secret set NAME");
                }
                all.iter().for_each(|name| println!("{name}"));
            });
            Ok(())
        }
        Secret::Set { name, value_file } => {
            let value = ask::secret_from(
                &format!("Value of {name}"),
                value_file.as_deref(),
                false,
                "--value-file FILE",
            )?;
            client.call(
                "installation.secrets.set",
                super::client::merged(json!({"name": name}), json!({"value": value})),
            )?;
            out::done(format!("Set the secret {name}"));
            Ok(())
        }
        Secret::Rename { name, new } => {
            client.call(
                "installation.secrets.rename",
                super::client::merged(
                    json!({"name": name}),
                    json!({"value": json!({"name": new})["name"].clone()}),
                ),
            )?;
            out::done(format!("{name} is called {new} now"));
            Ok(())
        }
        Secret::Remove { name } => {
            ask::confirm(&format!("This removes the secret {name}."))?;
            client.call("installation.secrets.remove", json!({"name": name}))?;
            out::done(format!("Removed the secret {name}"));
            Ok(())
        }
    }
}

// ── registries ──────────────────────────────────────────────────────────────

pub fn registry(global: &Global, command: Registry) -> Result<()> {
    #[cfg(target_os = "linux")]
    match command {
        Registry::Publish {
            destination,
            host,
            login,
        } => return crate::local::publish(global, &destination, host.as_deref(), login.as_deref()),
        Registry::PushImage { destination, tag } => {
            return crate::local::push_image(global, &destination, &tag);
        }
        _ => {}
    }
    let client = Client::open(global)?;
    let registries = || -> Result<serde_json::Map<String, Value>> {
        let found = client.call("registries.list", json!({}))?;
        Ok(found["registries"].as_object().cloned().unwrap_or_default())
    };
    match command {
        Registry::List => {
            let found = client.call("registries.list", json!({}))?;
            out::data(&found, || {
                let mut table = Table::new(&["name", "type", "url", "user", "repositories"]);
                for (name, registry) in found["registries"].as_object().into_iter().flatten() {
                    let repositories: Vec<String> = registry["repositories"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|repo| {
                            format!(
                                "{} {}",
                                repo["format"].as_str().unwrap_or(""),
                                repo["name"].as_str().unwrap_or("")
                            )
                        })
                        .collect();
                    table.row(vec![
                        name.clone(),
                        registry["type"].as_str().unwrap_or("").to_owned(),
                        registry["url"].as_str().unwrap_or("").to_owned(),
                        registry["username"].as_str().unwrap_or("").to_owned(),
                        repositories.join(", "),
                    ]);
                }
                table.print("No registries. Add one with rookery registry add NAME TYPE URL");
            });
            Ok(())
        }
        Registry::Add {
            name,
            kind,
            url,
            user,
        } => {
            let mut registry = json!({"type": kind, "url": url.trim_end_matches('/')});
            if let Some(user) = user {
                registry["username"] = json!(user);
            }
            // Adding again changes the address or user, and keeps the repositories.
            if let Some(known) = registries()?.get(&name) {
                registry["repositories"] = known["repositories"].clone();
            }
            client.call(
                "registries.save",
                super::client::merged(json!({"name": name}), json!({"value": registry})),
            )?;
            out::done(format!("Saved the registry {name}"));
            out::hint(format!(
                "Keep its login with: rookery registry login {name}"
            ));
            Ok(())
        }
        Registry::AddRepo {
            name,
            format,
            repository,
            address,
        } => {
            let mut registry = registries()?
                .get(&name)
                .filter(|registry| registry["type"] == "nexus")
                .cloned()
                .ok_or_else(|| Fail::usage(format!("{name} is no Nexus registry")))?;
            let mut entry = json!({"format": format, "name": repository});
            if let Some(address) = address {
                entry["address"] = json!(address);
            }
            let mut repositories = registry["repositories"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            repositories.retain(|known| known["name"] != repository.as_str());
            repositories.push(entry);
            registry["repositories"] = json!(repositories);
            client.call(
                "registries.save",
                super::client::merged(json!({"name": name}), json!({"value": registry})),
            )?;
            out::done(format!("Added {repository} to {name}"));
            Ok(())
        }
        Registry::Login {
            name,
            password_stdin,
        } => {
            if !registries()?.contains_key(&name) {
                return Err(Fail::failed(format!(
                    "No registry {name}. rookery registry list names them"
                )));
            }
            let value = ask::secret_from(
                &format!("Password or token of {name}"),
                None,
                password_stdin,
                "--password-stdin",
            )?;
            client.call(
                "installation.secrets.set",
                super::client::merged(
                    json!({"name": format!("registry/{name}")}),
                    json!({"value": value}),
                ),
            )?;
            out::done(format!("Kept the login of {name}"));
            Ok(())
        }
        Registry::Check { name } => {
            let names: Vec<String> = match name {
                Some(name) => vec![name],
                None => registries()?.keys().cloned().collect(),
            };
            let mut all = serde_json::Map::new();
            let mut failed = false;
            for name in names {
                let checked = client.call("registries.check", json!({"name": name}))?;
                failed |= checked["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|line| line.as_str().is_some_and(|line| line.starts_with('✗')));
                all.insert(name, checked);
            }
            out::data(&Value::Object(all.clone()), || {
                for (name, checked) in &all {
                    println!("{}", styled(name, Style::Bold));
                    for line in names_of(&checked["lines"]) {
                        println!("  {line}");
                    }
                }
            });
            match failed {
                true => Err(Fail::exit(1)),
                false => Ok(()),
            }
        }
        Registry::Rename { name, new } => {
            client.call(
                "registries.rename",
                super::client::merged(
                    json!({"name": name}),
                    json!({"value": json!({"name": new})["name"].clone()}),
                ),
            )?;
            out::done(format!("{name} is called {new} now, with its login"));
            Ok(())
        }
        Registry::Remove { name } => {
            ask::confirm(&format!("This removes the registry {name} with its login."))?;
            client.call("registries.remove", json!({"name": name}))?;
            out::done(format!("Removed the registry {name}"));
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Registry::Publish { .. } | Registry::PushImage { .. } => unreachable!("handled above"),
    }
}

fn names_of(list: &Value) -> Vec<String> {
    names(list)
}

// ── plugins ─────────────────────────────────────────────────────────────────

fn report(found: &Value) {
    out::data(found, || {
        for line in names(&found["lines"]) {
            println!("{line}");
        }
        let behind: Vec<rookery_api::HostBehind> =
            serde_json::from_value(found["behind"].clone()).unwrap_or_default();
        for host in &behind {
            out::warn(format!(
                "{} needs applying: {}",
                host.name,
                host.behind.text()
            ));
        }
        if found["behind"]
            .as_array()
            .is_some_and(|behind| !behind.is_empty())
        {
            out::hint("rookery rollout brings them up to date");
        }
    });
}

pub fn plugin(client: &Client, command: Plugin) -> Result<()> {
    match command {
        Plugin::List => {
            let found = client.call("plugins.list", json!({}))?;
            out::data(&found, || {
                let mut table = Table::new(&["id", "version", "from", "features"]);
                let mut refused = vec![];
                for plugin in found["plugins"].as_array().into_iter().flatten() {
                    let features: Vec<String> = plugin["features"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|feature| feature["id"].as_str().map(str::to_owned))
                        .collect();
                    let origin = match &plugin["origin"] {
                        Value::String(origin) => origin.clone(),
                        other => other
                            .as_object()
                            .and_then(|origin| origin.keys().next().cloned())
                            .unwrap_or_default(),
                    };
                    let problem = plugin["problem"].as_str().unwrap_or("");
                    let id = plugin["id"].as_str().unwrap_or("").to_owned();
                    if !problem.is_empty() {
                        refused.push(format!("{id} is not used: {problem}"));
                    }
                    table.row(vec![
                        id,
                        plugin["version"].as_str().unwrap_or("").to_owned(),
                        origin,
                        match problem.is_empty() {
                            true => features.join(", "),
                            false => styled("not used", Style::Warn),
                        },
                    ]);
                }
                table.print("No plugins");
                for line in refused {
                    out::warn(line);
                }
            });
            Ok(())
        }
        Plugin::Show { id } => {
            let found = client.call("plugins.list", json!({}))?;
            let plugins: Vec<rookery_api::PluginInfo> =
                serde_json::from_value(found["plugins"].clone())?;
            let Some(plugin) = plugins.into_iter().find(|plugin| plugin.id == id) else {
                return Err(Fail::failed(format!(
                    "There is no plugin {id}. The plugins are listed by rookery plugin list"
                )));
            };
            out::data(&serde_json::to_value(&plugin)?, || {
                print!("{}", super::pages::plugins(std::slice::from_ref(&plugin)));
            });
            Ok(())
        }
        Plugin::Add {
            spec,
            git,
            path,
            git_ref,
            allow_young,
        } => {
            let spec = spec.unwrap_or_default();
            let local = std::path::Path::new(&spec);
            if git.is_some() || spec.is_empty() || !local.exists() {
                let asked = rookery_api::PluginSpec {
                    spec,
                    git,
                    path,
                    git_ref,
                    allow_young,
                };
                report(&client.call("plugins.add", serde_json::to_value(asked)?)?);
                return Ok(());
            }
            // A file or folder of this machine is sent as an egg.
            let package = match local.is_dir() {
                true => pack(local)?,
                false => std::fs::read(local)?,
            };
            if package.len() > rookery_api::ops::PORTAL_BYTES {
                return Err(Fail::usage(format!(
                    "{spec} is larger than the 8 MiB a server is sent"
                )));
            }
            let data = json!({"base64": rookery_api::b64(&package)});
            report(&client.call("plugins.upload", data)?);
            Ok(())
        }
        Plugin::Remove { id } => {
            // A plugin is named by its id or by its package.
            let found = client.call("plugins.list", json!({}))?;
            let id = found["plugins"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|plugin| plugin["package"] == id.as_str())
                .and_then(|plugin| plugin["id"].as_str())
                .map(str::to_owned)
                .unwrap_or(id);
            ask::confirm(&format!("This removes the plugin {id}."))?;
            report(&client.call("plugins.remove", json!({"id": id}))?);
            Ok(())
        }
        Plugin::Update { id, check: true } => {
            let found = client.call("plugins.check", json!({}))?;
            out::data(&found, || {
                let mut table = Table::new(&["id", "version", "latest"]);
                for plugin in found["plugins"].as_array().into_iter().flatten() {
                    if id.as_ref().is_some_and(|id| plugin["id"] != id.as_str()) {
                        continue;
                    }
                    let latest = match (
                        plugin["latest"].as_str(),
                        plugin["note"].as_str().unwrap_or(""),
                    ) {
                        (Some(latest), _) => styled(latest, Style::Warn),
                        (None, "") => "the latest".to_owned(),
                        (None, note) => note.to_owned(),
                    };
                    table.row(vec![
                        plugin["id"].as_str().unwrap_or("").to_owned(),
                        plugin["version"].as_str().unwrap_or("").to_owned(),
                        latest,
                    ]);
                }
                table.print("The server has no plugins of its own");
            });
            Ok(())
        }
        Plugin::Update { id, check: false } => {
            let body = match id {
                Some(id) => json!({"id": id}),
                None => json!({}),
            };
            report(&client.call("plugins.update", body)?);
            Ok(())
        }
        Plugin::Check { dir } => check_plugin(&dir),
        Plugin::Registry(command) => plugin_registry(client, command),
    }
}

fn plugin_registry(client: &Client, command: args::PluginRegistry) -> Result<()> {
    match command {
        args::PluginRegistry::List => {
            let found = client.call("plugins.registry.list", json!({}))?;
            out::data(&found, || {
                let listed: rookery_api::PluginRegistries =
                    serde_json::from_value(found.clone()).unwrap_or_default();
                let mut table =
                    Table::new(&["name", "kind", "location", "groups", "min age", "key"]);
                for registry in &listed.registries {
                    if registry.kind == "off" {
                        table.row(vec![
                            registry.location.clone(),
                            "off".into(),
                            "turned off".into(),
                            String::new(),
                            String::new(),
                            String::new(),
                        ]);
                        continue;
                    }
                    table.row(vec![
                        registry.name.clone(),
                        match registry.builtin {
                            true => "built in".into(),
                            false => registry.kind.clone(),
                        },
                        registry.location.clone(),
                        match registry.only.is_empty() {
                            true => "any".to_owned(),
                            false => registry.only.join(", "),
                        },
                        match registry.builtin {
                            true => "none".into(),
                            false => registry.min_age.clone().unwrap_or_else(|| "default".into()),
                        },
                        match (&registry.key, registry.unsigned_tags) {
                            (None, _) => "none".to_owned(),
                            (Some(_), false) => "signed eggs".to_owned(),
                            (Some(_), true) => "signed eggs, unsigned tags".to_owned(),
                        },
                    ]);
                }
                table.print("No plugin registries: rookery plugin registry add adds one");
                if !listed.file.is_empty() {
                    out::hint(format!("They are kept in {}", listed.file));
                }
            });
            Ok(())
        }
        args::PluginRegistry::Add {
            kind,
            location,
            name,
            only,
            key,
            min_age,
            unsigned_tags,
        } => {
            let asked = rookery_api::PluginRegistry {
                kind: match kind {
                    args::RegistryKind::Git => "git",
                    args::RegistryKind::Http => "http",
                    args::RegistryKind::Dir => "dir",
                }
                .into(),
                location,
                name: name.unwrap_or_default(),
                only,
                key,
                min_age,
                unsigned_tags,
                builtin: false,
            };
            report(&client.call("plugins.registry.add", serde_json::to_value(asked)?)?);
            Ok(())
        }
        args::PluginRegistry::Remove { location } => {
            ask::confirm(&format!("This removes the plugin registry {location}."))?;
            report(&client.call("plugins.registry.remove", json!({"location": location}))?);
            Ok(())
        }
        args::PluginRegistry::Disable { name } => {
            let asked = rookery_api::PluginRegistry {
                kind: "off".into(),
                location: name,
                ..rookery_api::PluginRegistry::default()
            };
            report(&client.call("plugins.registry.add", serde_json::to_value(asked)?)?);
            Ok(())
        }
    }
}

/// Check a package folder against the plugin contract, on this machine,
/// or each package of a registry folder.
pub fn check_plugin(dir: &std::path::Path) -> Result<()> {
    let catalog = dir.join("index.json");
    let (problems, kept) = match !dir.join("package.kdl").is_file() && catalog.is_file() {
        true => {
            let (count, problems) = check_registry(dir, &catalog)?;
            let kept = format!(
                "The {count} package{} of {} keep Rookery's plugin contract",
                if count == 1 { "" } else { "s" },
                dir.display()
            );
            (problems, kept)
        }
        false => (
            rookery_api::plugin_contract::check_dir(dir),
            format!("{} keeps Rookery's plugin contract", dir.display()),
        ),
    };
    if problems.is_empty() {
        out::done(kept);
        return Ok(());
    }
    for problem in &problems {
        eprintln!("{problem}");
    }
    Err(Fail::failed(format!(
        "{} breaks Rookery's plugin contract in {} place{}",
        dir.display(),
        problems.len(),
        if problems.len() == 1 { "" } else { "s" }
    )))
}

/// How many packages a registry folder lists, and the problems of each,
/// each line with the package's name.
fn check_registry(
    dir: &std::path::Path,
    catalog: &std::path::Path,
) -> Result<(usize, Vec<String>)> {
    let data = std::fs::read(catalog)
        .map_err(|error| Fail::failed(format!("{}: {error}", catalog.display())))?;
    let catalog: hatchery::index::Catalog = hatchery::index::from_json("index.json", &data)
        .map_err(|error| Fail::failed(format!("{}: {error}", dir.display())))?;
    let mut problems = vec![];
    for name in &catalog.packages {
        let package = dir.join(name.dir());
        match package.join("package.kdl").is_file() {
            true => problems.extend(
                rookery_api::plugin_contract::check_dir(&package)
                    .into_iter()
                    .map(|problem| format!("{name}: {problem}")),
            ),
            false => problems.push(format!(
                "{name}: the registry lists it, and {} has no package.kdl",
                package.display()
            )),
        }
    }
    Ok((catalog.packages.len(), problems))
}

/// A package folder as the base egg Hatchery makes of it, without a build
/// and checked as an install checks it.
fn pack(folder: &std::path::Path) -> Result<Vec<u8>> {
    let egg = hatchery::Egg::pack(folder, &[&rookery_api::plugin_contract::RookeryTarget])
        .map_err(|error| Fail::failed(format!("{}: {error}", folder.display())))?;
    Ok(egg.bytes().to_vec())
}
// ── settings ────────────────────────────────────────────────────────────────

/// The settings of rookery itself, on this machine, beside those of the
/// server: where the server is, and its token.
const CLIENT_SETTINGS: &[(&str, &str)] = &[
    (
        settings::SERVER,
        "the rookery server this rookery uses, kept by rookery login; empty for the one in rookery itself",
    ),
    (
        settings::TOKEN,
        "the token of that server, kept by rookery login and never shown",
    ),
];

/// The client's own settings, as the server's are listed.
fn client_settings() -> Vec<Value> {
    let kept = super::client::settings();
    CLIENT_SETTINGS
        .iter()
        .map(|(key, meaning)| {
            let value = match (kept.get(key), *key == settings::TOKEN) {
                (Some(_), true) => "(set)".to_owned(),
                (Some(value), false) => value,
                (None, _) => String::new(),
            };
            json!({"key": key, "value": value, "meaning": meaning, "where": "client"})
        })
        .collect()
}

fn is_client_setting(key: &str) -> bool {
    CLIENT_SETTINGS.iter().any(|(known, _)| *known == key)
}

/// Change one of the client's own settings.
fn set_client(key: &str, value: &str) -> Result<()> {
    if key == settings::TOKEN && !value.is_empty() {
        return Err(Fail::usage(
            "The token is not given on the command line, where others can read it. rookery login URL keeps one",
        ));
    }
    let mut changes = vec![(key, value)];
    // Another server takes another token.
    if key == settings::SERVER {
        changes.push((settings::TOKEN, ""));
    }
    super::client::settings()
        .set(&changes)
        .map_err(|error| Fail::failed(error.to_string()))
}

pub fn config(client: &Client, command: args::Config) -> Result<()> {
    let settings = || -> Result<Vec<Value>> {
        let found = client.call("installation.settings.list", json!({}))?;
        // A server's own server and token are not this rookery's: those are the client's.
        let mut all: Vec<Value> = found
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|setting| !is_client_setting(setting["key"].as_str().unwrap_or("")))
            .map(|mut setting| {
                setting["where"] = json!("server");
                setting
            })
            .collect();
        all.extend(client_settings());
        Ok(all)
    };
    let known = |key: &str| -> Result<Value> {
        settings()?
            .into_iter()
            .find(|setting| setting["key"] == key)
            .ok_or_else(|| {
                Fail::usage(format!(
                    "{key} is no setting. rookery config list names them"
                ))
            })
    };
    let set = |key: &str, value: &str| -> Result<()> {
        if is_client_setting(key) {
            return set_client(key, value);
        }
        known(key)?;
        client.call(
            "installation.settings.set",
            json!({"name": key, "value": json!({"value": value})["value"].clone()}),
        )?;
        Ok(())
    };
    match command {
        args::Config::List => {
            let all = settings()?;
            out::data(&Value::Array(all.clone()), || {
                let width = all
                    .iter()
                    .map(|setting| setting["key"].as_str().unwrap_or("").chars().count())
                    .max()
                    .unwrap_or(0);
                let mut place = "";
                for setting in &all {
                    let here = setting["where"].as_str().unwrap_or("");
                    if here != place {
                        place = here;
                        let heading = match here {
                            "client" => "This rookery, on this machine",
                            _ => "The server",
                        };
                        println!("{}", styled(heading, Style::Accent));
                    }
                    let value = setting["value"].as_str().unwrap_or("");
                    let shown = match value {
                        "" => styled("(not set)", Style::Dim),
                        value => value.to_owned(),
                    };
                    let key = setting["key"].as_str().unwrap_or("");
                    println!("  {key:<width$}  {shown}");
                    println!(
                        "  {:<width$}  {}",
                        "",
                        styled(setting["meaning"].as_str().unwrap_or(""), Style::Dim)
                    );
                }
            });
            Ok(())
        }
        args::Config::Get { key } => {
            if key == settings::TOKEN {
                return Err(Fail::usage(
                    "The token is never shown. rookery config list says whether there is one",
                ));
            }
            let setting = known(&key)?;
            out::data(&setting, || {
                println!("{}", setting["value"].as_str().unwrap_or(""))
            });
            Ok(())
        }
        args::Config::Set { key, value } => {
            set(&key, &value)?;
            out::done(format!("{key} = {value}"));
            if key == settings::SERVER {
                out::hint(format!("Claim its token with: rookery login {value}"));
            }
            Ok(())
        }
        args::Config::Unset { key } => {
            set(&key, "")?;
            out::done(format!("{key} is back to its default"));
            Ok(())
        }
    }
}

/// What `resources.create` answers: the resource and the job that makes it.
#[derive(serde::Deserialize)]
struct Made {
    job: rookery_proto::jobs::JobInfo,
}
