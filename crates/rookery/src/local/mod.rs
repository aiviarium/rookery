//! What rookery does on the machine its server runs on, which is Linux:
//! `rookery serve`, and what only makes sense there: its sign-in links and
//! tokens, its secrets' key, the import of an older store, and the commands
//! an apply runs that read the server's tables.

mod deploy;
pub mod import;

pub use deploy::server_context;

use crate::Result;
use crate::cli::args::{self, Global};
use crate::cli::fail::{Fail, Result as Done};
use crate::cli::{client, out};
use rookery_core::paths::Paths;
use serde_json::{Value, json};

/// ssh runs rookery as its askpass helper, the program of SSH_ASKPASS, with
/// the prompt as its one argument and the variable that says where the
/// password is; the answer is all that may go to standard output. Whether
/// this run was one.
pub fn askpass(args: &[String]) -> Result<bool> {
    let Ok(source) = std::env::var(rookery_core::keys::askpass::SOURCE) else {
        return Ok(false);
    };
    let prompt = match args {
        [_, word, prompt @ ..] if word == "askpass" => prompt.join(" "),
        [_, prompt] => prompt.clone(),
        _ => return Ok(false),
    };
    let answer = rookery_core::keys::askpass::answer(&prompt, &source, None)?;
    println!("{answer}");
    Ok(true)
}

/// What only works with the installation on this machine is refused when
/// rookery is set to use a server elsewhere.
fn here_only(global: &Global, what: &str) -> Done<Paths> {
    if let Some(server) = client::named(global)
        && !rookery_client::net::local(&server)
    {
        return Err(Fail::refused(format!(
            "{what} works where the rookery server runs, and this rookery uses the one at {server}. Run it there"
        )));
    }
    attach()?;
    Ok(Paths::locate()?)
}

/// The server's tables, from `ROOKERY_DATABASE_URL` and its key, for a command of the server's
/// machine that reads them as the server does.
fn attach() -> Done<()> {
    let config = rookery_server::ServerConfig::from_env().map_err(|e| {
        Fail::usage(format!(
            "this works where rookery serve runs, with its ROOKERY_DATABASE_URL: {e}"
        ))
    })?;
    rookery_hosts::attach(&config.database_url, config.secret_key).map_err(Fail::failed)
}

/// Whether this rookery uses the server of this machine, on its socket.
fn uses_this_machine(global: &Global) -> bool {
    client::named(global).is_none_or(|server| rookery_client::net::local(&server))
}

/// `rookery host inventory [NAME…]`: the hosts as an Ansible inventory, with the server's key as a
/// file of this machine, from the server on its socket.
pub fn inventory(global: &Global, names: &[String]) -> Done<()> {
    if !uses_this_machine(global) {
        return Err(Fail::refused(
            "rookery host inventory names files of the server's machine: run it where rookery serve runs",
        ));
    }
    let answer = socket_call("hosts.inventory", json!({"hosts": names}))?;
    out::json(&answer["inventory"]);
    Ok(())
}

/// The enrollment key an apply asks for, for a host not on the tailnet yet; it goes to the role on
/// standard output. The apply runs on the server's machine, which gives it over its socket.
pub fn tailnet_key(host: &str) -> Done<()> {
    let answer = socket_call("hosts.tailnetKey", json!({"host": host}))?;
    println!("{}", answer["key"].as_str().unwrap_or_default());
    Ok(())
}

/// `rookery ssh HOST`: plain SSH with the server's key on the server's machine, and a terminal
/// through the server anywhere else.
pub fn ssh(global: &Global, ssh: &args::Ssh) -> Done<()> {
    if !uses_this_machine(global) {
        return crate::cli::things::ssh(&client::Client::open(global)?, ssh);
    }
    let login = socket_call("hosts.ssh", json!({"host": ssh.host, "agent": ssh.agent}))?;
    let args: Vec<String> = serde_json::from_value(login["args"].clone()).unwrap_or_default();
    let mut command = std::process::Command::new("ssh");
    command.args(&args).args(&ssh.command);
    if let Some(env) = login["env"].as_object() {
        for (key, value) in env {
            command.env(key, value.as_str().unwrap_or_default());
        }
    }
    let status = command.status()?;
    match status.code() {
        Some(0) => Ok(()),
        code => Err(Fail::exit(code.unwrap_or(1))),
    }
}

/// `rookery registry publish`: upload the release archive built here, and
/// make it the release address of a host through the server.
pub fn publish(
    global: &Global,
    destination: &str,
    host: Option<&str>,
    login: Option<&str>,
) -> Done<()> {
    let paths = here_only(
        global,
        "rookery registry publish, which builds the release here,",
    )?;
    std::env::set_current_dir(paths.source()?)?;
    let url = rookery_core::registry::publish(&paths, destination)?;
    out::done(format!("Published {url}"));
    let Some(host) = host else {
        return Ok(());
    };
    let client = client::Client::open(global)?;
    let mut profile = crate::cli::host_profile(&client, host)?;
    profile["release_url"] = json!(url);
    profile["release_store"] = json!(destination);
    let login = login.map(|name| match name {
        "" => destination.split('/').next().unwrap_or(""),
        name => name,
    });
    profile["release_private"] = json!(login.is_some());
    match login {
        Some(name) => {
            profile["release_login"] = json!(name);
            out::done(format!(
                "{host} downloads it from there with the login of {name}"
            ));
        }
        None => {
            if let Some(profile) = profile.as_object_mut() {
                profile.remove("release_login");
            }
            out::done(format!("{host} downloads it from there without a login"));
        }
    }
    crate::cli::save_host(&client, host, &profile)
}

/// `rookery registry push-image`: push rookery's container image from here.
pub fn push_image(global: &Global, destination: &str, tag: &str) -> Done<()> {
    let paths = here_only(
        global,
        "rookery registry push-image, which pushes from here,",
    )?;
    let image = rookery_core::registry::push_image(&paths, destination, tag)?;
    out::done(format!("Pushed {image}"));
    Ok(())
}

/// `rookery serve`: the server on the Postgres of `ROOKERY_DATABASE_URL`, set up by the `ROOKERY_*`
/// variables.
pub fn serve() -> Done<()> {
    let config =
        rookery_server::ServerConfig::from_env().map_err(|e| Fail::usage(e.to_string()))?;
    // Read once: no program the server starts gets the database or its keys.
    for name in rookery_core::runner::SERVER_ONLY {
        // SAFETY: no other thread runs yet; the runtime is made below.
        unsafe { std::env::remove_var(name) };
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Fail::failed(e.to_string()))?;
    runtime
        .block_on(rookery_server::serve(config))
        .map_err(Fail::failed)
}

/// A call of the server on this machine, over its socket.
fn socket_call(method: &str, params: Value) -> Done<Value> {
    let socket = rookery_server::config::default_socket(&|name| std::env::var(name).ok())
        .filter(|socket| socket.exists())
        .ok_or_else(|| {
            Fail::unreachable(
                "No rookery serve runs on this machine for this user: its socket is not there (ROOKERY_SOCKET names another)",
            )
        })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Fail::failed(e.to_string()))?;
    runtime
        .block_on(rookery_server::client::call(&socket, method, params))
        .map_err(Fail::failed)
}

/// `rookery server secrets rekey`: every sealed row of rookery serve on Postgres moves to a new key.
fn rekey(
    new_key_file: Option<std::path::PathBuf>,
    write_new_key: Option<std::path::PathBuf>,
) -> Done<()> {
    use rookery_server::rookery_store::seal::Key;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let config =
        rookery_server::ServerConfig::from_env().map_err(|e| Fail::usage(e.to_string()))?;
    let new = match (&new_key_file, &write_new_key) {
        (Some(file), None) => {
            let text = std::fs::read_to_string(file)
                .map_err(|e| Fail::failed(format!("reading {}: {e}", file.display())))?;
            Key::parse(&text).map_err(|e| Fail::usage(format!("{}: {e}", file.display())))?
        }
        (None, Some(_)) => Key::generate(),
        _ => {
            return Err(Fail::usage(
                "give the new key with --new-key-file FILE, or make one with --write-new-key FILE",
            ));
        }
    };
    if config
        .secret_key
        .as_ref()
        .is_some_and(|old| old.id() == new.id())
    {
        return Err(Fail::usage("the new key is the key in use"));
    }
    // The new key is kept before any row needs it.
    if let Some(file) = &write_new_key {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(file)
            .map_err(|e| Fail::failed(format!("writing {}: {e}", file.display())))?;
        writeln!(out, "{}", new.to_hex()).map_err(|e| Fail::failed(e.to_string()))?;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Fail::failed(e.to_string()))?;
    let moved = runtime
        .block_on(rookery_server::rekey(&config, &new))
        .map_err(Fail::failed)?;
    out::done(format!(
        "Sealed {} secrets, {} credentials and {} keys of the key ring again under the key {}. Start rookery serve with it as ROOKERY_SECRET_KEY_FILE.",
        moved.secrets,
        moved.connections,
        moved.host_keys,
        new.id()
    ));
    Ok(())
}

/// `rookery server …`: the server of this machine itself.
pub fn server_command(command: args::Server) -> Done<()> {
    match command {
        args::Server::SetupKey => {
            let made = socket_call("auth.createLink", json!({"kind": "setup"}))?;
            println!("{}", made["url"].as_str().unwrap_or(""));
            out::done(
                "The link signs in the first admin once, within a day. Open it in a browser, or pass it to rookery login.",
            );
            Ok(())
        }
        args::Server::Token { user, name, admin } => {
            let made = socket_call(
                "admin.createToken",
                json!({"user": user, "name": name, "admin": admin}),
            )?;
            println!("{}", made["token"].as_str().unwrap_or(""));
            out::done(format!(
                "An access token of {user}, shown once. Scripts send it as ROOKERY_TOKEN; rookery login URL --token-stdin keeps it"
            ));
            Ok(())
        }
        args::Server::Import {
            from,
            from_file,
            if_not_imported,
            key_file,
            owner,
        } => {
            let from = import::store_of(from.as_deref(), from_file.as_deref())?;
            import::import(
                &from,
                key_file.as_deref(),
                owner.as_deref(),
                if_not_imported,
            )
        }
        args::Server::Secrets(args::Secrets::Rekey {
            new_key_file,
            write_new_key,
        }) => rekey(new_key_file, write_new_key),
        args::Server::Image { tag } => deploy::image(tag),
        args::Server::Deploy(deploy) => deploy::deploy(&deploy),
    }
}
