//! Applying a host's configuration: the Ansible run of the installer role
//! against it over SSH.

use crate::{
    Result,
    host::Host,
    paths::{Paths, layout},
    plugins,
    profiles::Place,
};
use serde_json::{Value, json};
use std::{io::Write, process::Command};

/// A command ready to run, and the private files it reads. The files live as
/// long as this does, so keep it until the command has ended.
pub struct Prepared {
    pub command: Command,
    pub files: Vec<tempfile::NamedTempFile>,
    /// Lines for the output before the command's own, such as what of the
    /// profile was left out.
    pub notes: Vec<String>,
}

/// Where each plugin's package is on this machine, by id, for the builder
/// of seeds and releases, which carries the packages.
pub fn plugin_dirs(plugins: &[plugins::Plugin]) -> Value {
    let dirs: serde_json::Map<String, Value> = plugins
        .iter()
        .map(|plugin| (plugin.id.clone(), json!(plugin.package_dir)))
        .collect();
    Value::Object(dirs)
}

/// What the role is told of each plugin, by id: the folder of its
/// `rookery` target, and its label.
pub fn plugin_vars(plugins: &[plugins::Plugin]) -> Value {
    let dirs: serde_json::Map<String, Value> = plugins
        .iter()
        .map(|plugin| (plugin.id.clone(), json!(plugin.dir)))
        .collect();
    let labels: serde_json::Map<String, Value> = plugins
        .iter()
        .map(|plugin| (plugin.id.clone(), json!(plugin.label)))
        .collect();
    json!({"rookery_plugin_dirs": dirs, "rookery_plugin_labels": labels})
}

/// Variables for a direct Ansible run, matching what the seed builder embeds.
/// `own` are the public keys of the rookery that applies them, as
/// [`crate::keys::operator_keys`] gives them. The role keeps those apart from
/// the profile's, so that no later apply takes them away.
pub fn ansible_vars(config: &Value, plugin_vars: &Value, own: &[String]) -> Value {
    let mut config = config.clone();
    rookery_api::profile::retire_qemu(&mut config);
    let config = &config;
    let mut vars = config["ansible_vars"].clone();
    if !vars.is_object() {
        vars = json!({});
    }
    // How the host joins the tailnet is rookery's to say, not the profile's:
    // the key command runs on this machine.
    if let Some(vars) = vars.as_object_mut() {
        for name in crate::tailnet::ROLE_VARS {
            vars.remove(*name);
        }
        // Ansible's own variables would say how and where commands run, which
        // rookery says from `access`; a profile saved before this was checked
        // may still hold them.
        vars.retain(|name, _| rookery_api::profile::role_variable(name));
        if let Some(Value::Array(features)) = vars.get_mut("rookery_plugin_features") {
            features.retain(|feature| {
                feature
                    .as_str()
                    .is_some_and(rookery_api::profile::feature_name)
            });
        }
    }
    for (source, dest) in [
        ("agent_username", "rookery_user"),
        ("agent_password_hash", "rookery_agent_password_hash"),
        ("agent_passwordless_sudo", "rookery_agent_passwordless_sudo"),
        ("agent_privileges", "rookery_agent_privileges"),
        ("operator_username", "rookery_operator_user"),
        ("operator_password_hash", "rookery_operator_password_hash"),
    ] {
        if let Some(value) = config.get(source) {
            vars[dest] = value.clone();
        }
    }
    // The role writes each account's keys as one block, a key to a line.
    vars["rookery_agent_ssh_public_key"] =
        json!(rookery_api::profile::account_keys(config, "agent").join("\n"));
    vars["rookery_operator_ssh_public_key"] =
        json!(rookery_api::profile::account_keys(config, "operator").join("\n"));
    let own: Vec<&str> = own
        .iter()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect();
    vars["rookery_ssh_public_key"] = json!(own.join("\n"));
    // The role names the target by its older name.
    vars["rookery_profile"] = config["target"].clone();
    for name in ["rookery_plugin_dirs", "rookery_plugin_labels"] {
        vars[name] = match &plugin_vars[name] {
            Value::Null => json!({}),
            value => value.clone(),
        };
    }
    // A node takes the role's path without the desktop, and only the features its profile names.
    if rookery_api::profile::host_type(config) == rookery_api::HostType::Node {
        vars["rookery_level"] = json!("node");
        vars["rookery_profile"] = json!(rookery_api::profile::node_target(config));
        if !vars["rookery_plugin_features"].is_array() {
            vars["rookery_plugin_features"] = json!([]);
        }
        for account in ["rookery_user", "rookery_operator_user"] {
            if let Some(vars) = vars.as_object_mut() {
                vars.remove(account);
            }
        }
        if let Some(user) = config["access"]["user"]
            .as_str()
            .filter(|user| !user.is_empty())
        {
            vars["rookery_node_user"] = json!(user);
        }
    }
    vars
}

/// Add what joining the tailnet needs from this machine: rookery's login
/// server, and the way to a key or why there is none, which
/// names the host as `rookery tailnet-key` finds it again: by its name, or by
/// the path of its file. That command runs as a child of the apply, with its
/// environment, so it looks in the same store.
pub fn add_tailnet(vars: &mut Value, paths: &Paths, config: &Value, place: &Place) -> Result<()> {
    let rookery = std::env::current_exe()?;
    if let (Some(vars), Value::Object(more)) = (
        vars.as_object_mut(),
        crate::tailnet::apply_vars(paths, config, &place.argument(), &rookery),
    ) {
        vars.extend(more);
    }
    Ok(())
}

/// `ansible-playbook` of the source's own environment when it has one, with
/// the source's Ansible settings.
pub fn playbook(source: &std::path::Path) -> Command {
    let local = source.join(".venv/bin/ansible-playbook");
    let mut command = Command::new(if local.is_file() {
        local
    } else {
        "ansible-playbook".into()
    });
    command.env("ANSIBLE_CONFIG", source.join(layout::ANSIBLE_CONFIG));
    crate::runner::without_server_env(&mut command);
    command
}

/// The Ansible run that applies `host`'s profile to it over SSH. With a
/// password it becomes root with it, taken from a private file; without one a
/// host that asks for a password is asked for on the terminal. Only a node
/// or a workstation is applied: every way to apply comes through here.
pub fn prepare(paths: &Paths, host: &Host, become_password: Option<&str>) -> Result<Prepared> {
    crate::host_type::guard(host, crate::host_type::Capability::Apply)?;
    let plugins = plugins::installed(&paths.config_dir);
    crate::node::check_features(&plugins, &host.name, &host.config)?;
    crate::node::check_removal(paths, host)?;
    let dirs = plugin_dirs(&plugins);
    // Without its own key in the operator's authorized_keys, rookery could be
    // locked out of the host it sets up, so it does not go on without one.
    let own = crate::keys::operator_keys(paths, &host.config)?;
    let mut vars = ansible_vars(&host.config, &plugin_vars(&plugins), &own);
    add_tailnet(&mut vars, paths, &host.config, &host.place)?;
    // The role keeps this on the host, for the plugins the host has, so
    // rookery can tell later whether the host is behind.
    vars["rookery_applied"] = json!({
        "rookery": rookery_api::VERSION,
        "plugins": plugins::records(&plugins),
    });
    // The role keeps the profile on the host too, for a rookery that lacks it.
    vars["rookery_kept_profile"] = crate::kept_profile::for_host(paths, host);
    // Only the certificates go to the host; their keys stay in rookery's store.
    let store = crate::registry::Store::own(crate::store::shared(paths)?);
    vars["rookery_client_certificates"] =
        crate::node::client_certificates(&plugins, &store, &host.config)?;
    let parrot_files = crate::parrot::stage(paths, &host.config, &plugins, &mut vars)?;
    let mut prepared = run(paths, host, layout::PLAYBOOK, vars, become_password)?;
    prepared.files.extend(parrot_files);
    let left = rookery_api::profile::left_out(&host.config);
    if !left.is_empty() {
        prepared.notes.push(format!(
            "! {}'s ansible_vars hold {}, which the role does not take: it is applied without {}. Remove {} from the profile",
            host.name,
            left.join(", "),
            if left.len() == 1 { "it" } else { "them" },
            if left.len() == 1 { "it" } else { "them" },
        ));
    }
    prepared.command.env("ROOKERY_PLUGINS", dirs.to_string());
    prepared
        .command
        .env("ROOKERY_SSH_PUBLIC_KEY", own.join("\n"));
    Ok(prepared)
}

/// The Ansible run that puts rookery's controller alone on a host that is not
/// a workstation: rookeryd and box, what they read, and the record of what
/// was installed. The SSH user is the account sessions run as.
pub fn prepare_controller(
    paths: &Paths,
    host: &Host,
    become_password: Option<&str>,
) -> Result<Prepared> {
    crate::host_type::guard(host, crate::host_type::Capability::InstallController)?;
    // The role calls the type managed by its older name.
    let mut vars = json!({
        "rookery_level": "controller",
        "rookery_applied": {"rookery": rookery_api::VERSION, "plugins": []},
    });
    let user = host.access().user;
    if !user.is_empty() {
        vars["rookery_controller_user"] = json!(user);
    }
    run(
        paths,
        host,
        layout::CONTROLLER_PLAYBOOK,
        vars,
        become_password,
    )
}

/// The run of `playbook` against `host` with these variables, the passwords
/// it needs in them.
fn run(
    paths: &Paths,
    host: &Host,
    playbook_path: &str,
    mut vars: Value,
    become_password: Option<&str>,
) -> Result<Prepared> {
    let source = paths.source()?;
    if let Some(password) = become_password {
        vars["ansible_become_password"] = json!(password);
    }
    // A host logged in to with a password: Ansible hands it to ssh through
    // an askpass helper of its own, and takes it from this private file.
    let login = crate::keys::login_of(paths, host)?;
    let ssh_password = match login.auth {
        rookery_api::profile::Auth::Password => {
            crate::registry::Store::own(crate::store::shared(paths)?)
                .secret(&crate::keys::password_secret(&host.place))?
        }
        _ => None,
    };
    if let Some(password) = &ssh_password {
        vars["ansible_password"] = json!(password);
    }
    // Secrets go to private files rather than argv.
    let mut extra = tempfile::NamedTempFile::new()?;
    extra.write_all(untemplated(&vars).as_bytes())?;
    extra.flush()?;
    let mut inventory = tempfile::NamedTempFile::new()?;
    inventory.write_all(untemplated(&login.inventory()?).as_bytes())?;
    inventory.flush()?;
    let mut command = playbook(&source);
    command
        .current_dir(&source)
        .args([playbook_path, "--become", "-e"])
        .arg(format!("@{}", extra.path().display()))
        .arg("-i")
        .arg(inventory.path());
    if login.auth == rookery_api::profile::Auth::Password && ssh_password.is_none() {
        command.arg("--ask-pass");
    }
    if become_password.is_none() && host.asks_password() {
        command.arg("--ask-become-pass");
    }
    // A password is Ansible's to hand to ssh, through its own helper; no
    // other program, such as the desktop's dialog, is ever asked for one.
    command.env("SSH_ASKPASS_REQUIRE", "never");
    // A job's output is a log, not a terminal; its colours are kept for
    // whoever shows it on one, and taken out for whoever does not.
    command.env("ANSIBLE_FORCE_COLOR", "1");
    command.env("ROOKERY_BUILD_DIR", paths.build_dir());
    Ok(Prepared {
        command,
        files: vec![extra, inventory],
        notes: vec![],
    })
}

/// `value` as YAML that Ansible reads with every text marked `!unsafe`, so
/// that none of it is a template: Ansible renders `{{ … }}` in variables
/// and inventories, and a lookup in one would run on this machine.
pub fn untemplated(value: &Value) -> String {
    let mut out = String::new();
    write_untemplated(value, &mut out);
    out
}

fn write_untemplated(value: &Value, out: &mut String) {
    match value {
        // Ansible reads an empty text marked unsafe as null, and an empty one is no template.
        Value::String(text) if text.is_empty() => out.push_str("\"\""),
        Value::String(text) => {
            out.push_str("!unsafe ");
            quoted(text, out);
        }
        // YAML 1.1 reads a number such as 1e5 as text unless it is told otherwise.
        Value::Number(number) if number.is_f64() => {
            out.push_str("!!float ");
            out.push_str(&number.to_string());
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => out.push_str(&value.to_string()),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_untemplated(item, out);
            }
            out.push(']');
        }
        Value::Object(fields) => {
            out.push('{');
            for (index, (key, item)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                quoted(key, out);
                out.push_str(": ");
                write_untemplated(item, out);
            }
            out.push('}');
        }
    }
}

/// A text in YAML's double quotes, with everything but printable ASCII
/// escaped, as YAML refuses some characters even in quotes.
fn quoted(text: &str, out: &mut String) {
    use std::fmt::Write as _;
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(ch),
            ch if (ch as u32) <= 0xffff => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => {
                let _ = write!(out, "\\U{:08x}", ch as u32);
            }
        }
    }
    out.push('"');
}

/// After a successful apply of a workstation that rookery reaches as its
/// operator: the role gave the operator sudo without a password, SSH with
/// keys only and rookery's keys, so the saved profile says so now. A password
/// kept for the first apply is not needed again and is removed.
pub fn settle(paths: &Paths, host: &Host) -> Result<()> {
    settle_with(paths, host, &mut std::io::sink())
}

/// [`settle`], saying on `out` what it did for a node: the provisioners its
/// features offer are registered, and those they no longer offer forgotten.
/// A node that does not say is named on `out`, and its profile is settled
/// all the same.
pub fn settle_with(paths: &Paths, host: &Host, out: &mut dyn Write) -> Result<()> {
    match settle_node(paths, host) {
        Ok(lines) => {
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
        }
        Err(error) => {
            let _ = writeln!(out, "! {error}");
        }
    }
    for line in settle_tailnet(paths, host) {
        let _ = writeln!(out, "{line}");
    }
    let Some(mut profile) = host.place.load(paths)? else {
        return Ok(());
    };
    let before = rookery_api::profile::Access::of(&profile);
    if !rookery_api::profile::settle_after_apply(&mut profile) {
        return Ok(());
    }
    host.place.save(paths, &profile)?;
    let store = crate::store::shared(paths)?;
    let secrets = crate::registry::Store::own(store.clone());
    // A node's sudo is as it was; only a workstation's operator was given one that never asks.
    if crate::host_type::of(&profile) != crate::host_type::HostType::Node {
        secrets.set_secret(&crate::keys::sudo_secret(&host.place), None)?;
    }
    if before.auth == rookery_api::profile::Auth::Password {
        secrets.set_secret(&crate::keys::password_secret(&host.place), None)?;
    }
    // The role checked that the operator takes the key: a refusal of it
    // remembered from before the apply no longer holds.
    if let Ok(login) = crate::keys::login(paths, &store, &host.place, &profile) {
        login.noted(None);
    }
    Ok(())
}

/// After a successful apply of a host that joins the tailnet: record its
/// device, and rename it when a rename asked for that. Only with the API
/// key, as rookery does nothing with the device without it; a host applied
/// before the key was set is looked up by its address or hostname instead.
fn settle_tailnet(paths: &Paths, host: &Host) -> Vec<String> {
    if !rookery_api::profile::joins_tailnet(&host.config)
        || crate::tailnet::Control::configured(paths)
            .device_problem()
            .is_some()
    {
        return vec![];
    }
    let reported = match crate::node::applied(paths, host) {
        Ok(applied) => applied.and_then(|applied| applied.tailnet),
        Err(error) => {
            return vec![format!(
                "! {} did not say which device it is on the tailnet, so rookery has not recorded it: {error}",
                host.name
            )];
        }
    };
    crate::tailnet::after_apply(paths, host, reported).unwrap_or_else(|error| {
        vec![format!(
            "! rookery did not record {}'s device on the tailnet: {error}",
            host.name
        )]
    })
}

/// After a successful apply of a node: register the provisioners its
/// features offer, and forget those they no longer do. Says what it did, a
/// line each.
pub fn settle_node(paths: &Paths, host: &Host) -> Result<Vec<String>> {
    if crate::host_type::of(&host.config) != crate::host_type::HostType::Node {
        return Ok(vec![]);
    }
    let offers = crate::node::reported(paths, host)
        .map_err(|error| format!("{} did not say what its features offer: {error}", host.name))?;
    let store = crate::registry::Store::own(crate::store::shared(paths)?);
    crate::node::register(paths, &store, host, &offers)
}

/// How long a controller just installed has to answer.
const CONTROLLER_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// Install rookery's controller alone on a host from where rookery runs, its
/// output to `out`, and wait for it to answer: whether all went well. A
/// host of the type ssh becomes managed in its profile once the controller
/// is there.
pub fn install_controller(
    paths: &Paths,
    host: &Host,
    become_password: Option<&str>,
    out: &mut crate::runner::Output,
) -> bool {
    let fail = |out: &mut crate::runner::Output, error: &dyn std::fmt::Display| {
        let _ = writeln!(out, "✗ {error}");
        false
    };
    // The profile here may say less than the host is, as for a workstation
    // added by its address; the role refuses it too.
    let intended = crate::host_type::of(&host.config);
    let asked = crate::keys::refusal::now();
    let observed = crate::host_type::observe(paths, host, become_password);
    // A host that refused the login just now would refuse Ansible's too.
    if observed.is_none()
        && let Ok(login) = crate::keys::login_of(paths, host)
        && let Some(refused) = login.refusal()
        && refused.at >= asked
    {
        return fail(out, &refused.sentence(&host.name, refused.at));
    }
    if let Some(why) = crate::host_type::refuse_install(&host.name, intended, observed) {
        return fail(out, &why);
    }
    let prepared = match prepare_controller(paths, host, become_password) {
        Ok(prepared) => prepared,
        Err(error) => return fail(out, &error),
    };
    let program = crate::runner::Program {
        command: prepared.command,
        input: None,
        files: prepared.files,
    };
    match crate::runner::run_program(program, out) {
        Some(0) => {}
        Some(code) => {
            return fail(
                out,
                &format!("Installing the controller failed (ansible-playbook ended with {code})"),
            );
        }
        None => return fail(out, &"Installing the controller was ended by a signal"),
    }
    if crate::host_type::of(&host.config) != crate::host_type::HostType::Managed {
        let mut config = host.config.clone();
        rookery_api::profile::set_host_type(&mut config, crate::host_type::HostType::Managed);
        if let Err(error) = host.place.save(paths, &config) {
            return fail(
                out,
                &format!("The controller is installed, but the profile was not saved: {error}"),
            );
        }
        let _ = writeln!(out, "✓ {} is a managed host now", host.name);
    }
    match answers(paths, host, become_password) {
        Ok(version) => {
            let _ = writeln!(
                out,
                "✓ rookery's controller answers on {}: rookeryd {version}",
                host.name
            );
            true
        }
        Err(error) => fail(
            out,
            &format!(
                "rookeryd is installed on {}, but it does not answer ({error}). On a host without systemd something there has to start /usr/local/bin/rookeryd",
                host.name
            ),
        ),
    }
}

/// The version of the host's rookeryd, asked until it answers or the
/// patience is spent.
fn answers(paths: &Paths, host: &Host, password: Option<&str>) -> Result<String> {
    let link = crate::link::Link::new(host.link(paths, password)?);
    let runtime = crate::link::runtime()?;
    let started = std::time::Instant::now();
    loop {
        let asked = runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(15), link.version()).await
        });
        match asked {
            Ok(Ok(version)) => return Ok(version.version),
            Ok(Err(error)) if started.elapsed() >= CONTROLLER_PATIENCE => {
                return Err(error.to_string().into());
            }
            Err(_) if started.elapsed() >= CONTROLLER_PATIENCE => {
                return Err("no answer in time".into());
            }
            _ => std::thread::sleep(std::time::Duration::from_secs(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_come_from_the_profile() {
        let config = json!({
            "target": "cloud", "agent_username": "bot", "agent_privileges": "limited",
            "ansible_vars": {"rookery_geometry": "1x1", "rookery_tailscale_key_command": ["sh", "-c", "id"]},
            "operator_ssh_public_key": "ssh-ed25519 ME",
        });
        let vars = ansible_vars(
            &config,
            &json!({"rookery_plugin_dirs": {"demo": "/p/demo/rookery"}, "rookery_plugin_labels": {"demo": "Demo"}}),
            &["ssh-ed25519 OWN".to_owned(), "ssh-ed25519 NEXT".to_owned()],
        );
        assert_eq!(vars["rookery_operator_ssh_public_key"], "ssh-ed25519 ME");
        assert_eq!(
            vars["rookery_ssh_public_key"],
            "ssh-ed25519 OWN\nssh-ed25519 NEXT"
        );
        // A profile from before the settings went away cannot turn either off.
        let old = json!({"rookery_ssh_key": false, "operator_passwordless_sudo": false});
        let vars_old = ansible_vars(&old, &json!({}), &["ssh-ed25519 OWN".to_owned()]);
        assert_eq!(vars_old["rookery_ssh_public_key"], "ssh-ed25519 OWN");
        assert!(vars_old.get("rookery_operator_passwordless_sudo").is_none());
        assert_eq!(vars["rookery_agent_ssh_public_key"], "");
        assert_eq!(vars["rookery_user"], "bot");
        assert_eq!(vars["rookery_agent_privileges"], "limited");
        assert_eq!(vars["rookery_profile"], "cloud");
        assert_eq!(vars["rookery_geometry"], "1x1");
        assert_eq!(vars["rookery_plugin_dirs"]["demo"], "/p/demo/rookery");
        assert_eq!(vars["rookery_plugin_labels"]["demo"], "Demo");
        // A profile of the qemu target there once was is applied as cloud, reached from the QEMU host.
        let qemu = ansible_vars(&json!({"target": "qemu"}), &json!({}), &[]);
        assert_eq!(qemu["rookery_profile"], "cloud");
        assert_eq!(qemu["rookery_bootstrap_ssh_cidr"], "10.0.2.2/32");
        // The tailnet's variables are rookery's, not the profile's.
        assert!(vars.get("rookery_tailscale_key_command").is_none());
        assert!(vars.get("rookery_tailscale_auth_key").is_none());
        assert!(vars.get("rookery_tailscale_login_server").is_none());
    }

    #[test]
    fn a_node_takes_the_path_without_the_desktop() {
        let node = json!({"type": "node", "access": {"address": "192.0.2.20", "user": "root"},
            "agent_username": "bot", "ansible_vars": {"rookery_user": "x"}});
        let vars = ansible_vars(&node, &json!({}), &["ssh-ed25519 OWN".to_owned()]);
        assert_eq!(vars["rookery_level"], "node");
        assert_eq!(vars["rookery_profile"], "cloud");
        assert_eq!(vars["rookery_node_user"], "root");
        // A node has only the features its profile names, and no accounts of a workstation.
        assert_eq!(vars["rookery_plugin_features"], json!([]));
        assert!(vars.get("rookery_user").is_none() && vars.get("rookery_operator_user").is_none());
        assert_eq!(vars["rookery_ssh_public_key"], "ssh-ed25519 OWN");
        let contained = json!({"type": "node", "target": "container",
            "ansible_vars": {"rookery_plugin_features": ["docker/engine"]}});
        let vars = ansible_vars(&contained, &json!({}), &[]);
        assert_eq!(vars["rookery_profile"], "container");
        assert_eq!(vars["rookery_plugin_features"], json!(["docker/engine"]));
        assert!(
            ansible_vars(&json!({"builder": {}}), &json!({}), &[])
                .get("rookery_level")
                .is_none()
        );
    }

    #[test]
    fn a_password_travels_in_the_private_file() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("source");
        std::fs::create_dir_all(source.join("provision/bin")).unwrap();
        std::fs::write(source.join(layout::BUILDER), "").unwrap();
        let paths = Paths {
            config_dir: home.path().join("config"),
            data_dir: home.path().join("data"),
        };
        paths.prepare().unwrap();
        paths.give_own_key();
        paths.set("source", source.to_str().unwrap()).unwrap();
        let host = Host {
            name: "box".into(),
            place: Place::own(&paths, "box").unwrap(),
            config: json!({"target": "cloud", "access": {"address": "box", "user": "ops", "sudo": "password"}}),
        };
        let prepared = prepare(&paths, &host, Some("hunter2")).unwrap();
        let args: Vec<String> = prepared
            .command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("hunter2") || arg == "--ask-become-pass")
        );
        assert_eq!(&args[..2], [layout::PLAYBOOK, "--become"]);
        let config = prepared
            .command
            .get_envs()
            .find(|(name, _)| *name == "ANSIBLE_CONFIG")
            .and_then(|(_, value)| value);
        let wanted = paths.source().unwrap().join(layout::ANSIBLE_CONFIG);
        assert_eq!(config, Some(wanted.as_os_str()));
        let extra = read_back(prepared.files[0].path());
        assert_eq!(extra["ansible_become_password"], "hunter2");
        let inventory = read_back(prepared.files[1].path());
        assert_eq!(
            inventory["all"]["hosts"]["rookery_target"]["ansible_user"],
            "ops"
        );
        // Without one, Ansible asks.
        let prepared = prepare(&paths, &host, None).unwrap();
        assert!(
            prepared
                .command
                .get_args()
                .any(|arg| arg == "--ask-become-pass")
        );
        // The key of this rookery goes to the operator, apart from the profile's,
        // and the host keeps its profile without how this rookery reaches it.
        assert_eq!(
            extra["rookery_ssh_public_key"],
            paths.own_public_key().unwrap()
        );
        assert_eq!(extra["rookery_kept_profile"]["host"], "box");
        assert_eq!(extra["rookery_kept_profile"]["profile"]["target"], "cloud");
        assert!(
            extra["rookery_kept_profile"]["profile"]
                .get("access")
                .is_none()
        );
    }

    #[test]
    fn an_apply_as_the_operator_settles_the_profile_and_forgets_the_passwords() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: home.path().join("config"),
            data_dir: home.path().join("data"),
        };
        paths.prepare().unwrap();
        paths.give_own_key();
        let place = Place::own(&paths, "studio").unwrap();
        let host = |config: Value| {
            place.save(&paths, &config).unwrap();
            Host {
                name: "studio".into(),
                place: place.clone(),
                config,
            }
        };
        let saved = || place.load(&paths).unwrap().unwrap();
        place.save(&paths, &json!({})).unwrap();
        let secrets = crate::registry::Store::own(crate::store::shared(&paths).unwrap());
        secrets.set_secret("sudo/studio", Some("hunter2")).unwrap();
        secrets.set_secret("ssh/studio", Some("hunter2")).unwrap();
        let first = json!({"operator_username": "ops", "builder": {},
            "access": {"address": "192.0.2.10", "user": "ops", "auth": "password", "sudo": "password"}});
        settle(&paths, &host(first)).unwrap();
        let access = rookery_api::profile::Access::of(&saved());
        assert_eq!(access.auth, rookery_api::profile::Auth::Rookery);
        assert_eq!(access.sudo, Some(false));
        assert_eq!(access.address, "192.0.2.10");
        assert_eq!(secrets.secret("sudo/studio").unwrap(), None);
        assert_eq!(secrets.secret("ssh/studio").unwrap(), None);
        // A key of the ring the role installed stays how rookery logs in.
        let keyed = json!({"builder": {}, "access": {"address": "192.0.2.10", "user": "ops", "auth": "key:admin"}});
        settle(&paths, &host(keyed)).unwrap();
        assert_eq!(
            rookery_api::profile::Access::of(&saved()).auth,
            rookery_api::profile::Auth::Key("admin".into())
        );
        // An account of the machine's own keeps a sudo that asks, and its password.
        secrets.set_secret("sudo/studio", Some("hunter2")).unwrap();
        let own = json!({"builder": {}, "access": {"address": "192.0.2.10", "user": "admin", "auth": "password", "sudo": "password"}});
        settle(&paths, &host(own.clone())).unwrap();
        assert_eq!(saved(), own);
        assert_eq!(
            secrets.secret("sudo/studio").unwrap().as_deref(),
            Some("hunter2")
        );
    }

    #[test]
    fn a_profile_says_nothing_of_how_ansible_runs() {
        let config = json!({"builder": {}, "ansible_vars": {
            "rookery_geometry": "1x1",
            "ansible_connection": "local",
            "ansible_ssh_common_args": "-o ProxyCommand=\"sh -c id\"",
            "ansible_python_interpreter": "/tmp/x",
            "rookery_plugin_features": ["claude/cli", "../../tmp/x", 7]}});
        let vars = ansible_vars(&config, &json!({}), &[]);
        assert_eq!(vars["rookery_geometry"], "1x1");
        for gone in [
            "ansible_connection",
            "ansible_ssh_common_args",
            "ansible_python_interpreter",
        ] {
            assert!(vars.get(gone).is_none(), "{gone}");
        }
        assert_eq!(vars["rookery_plugin_features"], json!(["claude/cli"]));
        assert_eq!(
            rookery_api::profile::left_out(&config),
            [
                "ansible_connection",
                "ansible_python_interpreter",
                "ansible_ssh_common_args",
                "the feature \"../../tmp/x\"",
                "the feature 7"
            ]
        );
    }

    /// A file [`untemplated`] wrote, without its tags, of texts without any.
    fn read_back(path: &std::path::Path) -> Value {
        let text = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&text.replace("!unsafe ", "").replace("!!float ", "")).unwrap()
    }

    #[test]
    fn what_goes_to_ansible_is_never_a_template() {
        let value = json!({"rookery_x": "{{ lookup('pipe', 'id') }}", "n": 3, "f": 1e20, "b": true,
            "z": null, "e": "", "l": ["a\"b\\c", "é\u{7f}\u{1F600}"], "d": {"{{ k }}": "v"}});
        assert_eq!(
            untemplated(&value),
            r#"{"b": true, "d": {"{{ k }}": !unsafe "v"}, "e": "", "f": !!float 1e+20, "l": [!unsafe "a\"b\\c", !unsafe "\u00e9\u007f\U0001f600"], "n": 3, "rookery_x": !unsafe "{{ lookup('pipe', 'id') }}", "z": null}"#
        );
        if let Ok(dir) = std::env::var("ROOKERY_UNTEMPLATED_TO") {
            std::fs::write(format!("{dir}/vars.yml"), untemplated(&value)).unwrap();
            let inventory = json!({"all": {"hosts": {"rookery_target": {
                "ansible_host": "{{ lookup('pipe', 'echo RAN') }}", "ansible_connection": "local",
                "ansible_ssh_common_args": "-o ControlMaster=auto"}}}});
            std::fs::write(format!("{dir}/inventory"), untemplated(&inventory)).unwrap();
        }
    }

    /// What the role is given for the made-up profiles in
    /// tests/fixtures/ansible, kept beside them. CI applies the role on a pull
    /// request only for changes there or to the role, so a change to what it
    /// gets has to show there.
    #[test]
    fn ansible_contract() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ansible");
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                name.strip_suffix(".profile.json").map(str::to_owned)
            })
            .collect();
        names.sort();
        assert!(!names.is_empty());
        let update = std::env::var_os("ROOKERY_UPDATE_DOCS").is_some();
        for name in names {
            let profile = read_back(&dir.join(format!("{name}.profile.json")));
            let vars = ansible_vars(
                &profile,
                &json!({
                    "rookery_plugin_dirs": {"docker": "/usr/lib/rookery/plugins/docker/rookery", "incus": "/usr/lib/rookery/plugins/incus/rookery"},
                    "rookery_plugin_labels": {"docker": "Docker", "incus": "Incus"},
                }),
                &["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIROOKERY rookery@studio".to_owned()],
            );
            let written = serde_json::to_string_pretty(&vars).unwrap() + "\n";
            let path = dir.join(format!("{name}.vars.json"));
            if update {
                std::fs::write(&path, &written).unwrap();
                continue;
            }
            let kept = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                kept == written,
                "{} is stale: write it again with ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-core ansible_contract",
                path.display()
            );
        }
    }
}
