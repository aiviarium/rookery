//! What rookery does on the machine its installation is on, which is Linux:
//! `rookery serve`, the server its commands use when no other is set, and
//! what only makes sense where the server runs: its setup key, its store,
//! its API documents, and the commands that need the installation's own
//! files, such as `rookery ssh` with its key.

mod deploy;
pub mod serve;
mod store_command;

pub use deploy::server_context;

use crate::Result;
use crate::cli::args::{self, Global};
use crate::cli::fail::{Fail, Result as Done};
use crate::cli::{client, out};
use rookery_core::{host::Host, paths::Paths};
use serde_json::{Value, json};

/// The server of this machine: a `rookery serve` of this user, or one in this
/// process, whose jobs the second part waits for. With `ask`, a host whose
/// sudo asks for a password has it asked for on the terminal when the
/// server starts, as `rookery serve` does.
pub fn server(ask: bool) -> Result<Option<crate::Local>> {
    let (server, here) = serve::here::local(&Paths::locate()?, ask)?;
    if let Some(here) = &here {
        crate::cli::net::serve_here(here.dial());
    }
    Ok(Some((server, Box::new(move || serve::here::finish(here)))))
}

/// Give the server in this process a host's sudo password for this run, as
/// `rookery serve` takes one on its terminal at the start. Whether there is
/// such a server.
pub fn give_sudo_password(name: &str, password: &str) -> bool {
    serve::here::give_sudo_password(name, password)
}

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

/// Put back the plugins rookery's store has and this machine's plugin
/// directory lacks, as on a machine restored from the store, then install
/// the plugins that come with this rookery, and say so on standard error.
pub(crate) fn restore_plugins(paths: &Paths) {
    match rookery_core::plugin_state::restore(paths) {
        Ok(notices) => notices.iter().for_each(|notice| eprintln!("{notice}")),
        Err(error) => {
            eprintln!("rookery: the plugins in rookery's store were not checked: {error}")
        }
    }
    match rookery_core::eggs::builtins(paths) {
        Ok(notices) => notices.iter().for_each(|notice| eprintln!("{notice}")),
        Err(error) => {
            eprintln!("rookery: the plugins that come with rookery were not installed: {error}")
        }
    }
}

/// What only works with the installation on this machine is refused when
/// rookery is set to use a server elsewhere.
fn here_only(global: &Global, what: &str) -> Done<Paths> {
    if let Some(server) = client::named(global)
        && !crate::cli::net::local(&server)
    {
        return Err(Fail::refused(format!(
            "{what} works where the rookery server runs, and this rookery uses the one at {server}. Run it there"
        )));
    }
    Ok(Paths::locate()?)
}

/// `rookery ssh HOST [--agent] [-- COMMAND…]`: plain SSH with the
/// installation's key, for a host whose controller cannot be reached.
pub fn ssh(global: &Global, ssh: &args::Ssh) -> Done<()> {
    let paths = here_only(
        global,
        "rookery ssh, which logs in with the server's own key,",
    )
    .map_err(|fail| {
        Fail::refused(format!(
            "{} rookery session attach {} reaches a host through any server",
            fail.message, ssh.host
        ))
    })?;
    let host = Host::load(&paths, &ssh.host)?;
    if !host.access().reachable() {
        return Err(Fail::failed(format!("{} has no SSH address yet", ssh.host)));
    }
    let mut command = host.ssh(&paths)?;
    command.arg(host.target());
    if ssh.agent {
        // Root login and the agent's own key are not a given, so it goes
        // through the SSH user and sudo; agent-env adds the display and tools.
        let agent = host.agent();
        if agent.starts_with('-')
            || !agent
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return Err(Fail::failed("The agent's account name is not a valid one"));
        }
        command.arg("-t").arg(format!(
            "sudo -iu {agent} sh -c 'command -v agent-env >/dev/null && exec agent-env; exec bash -l'"
        ));
    }
    command.args(&ssh.command);
    let status = command.status()?;
    match status.code() {
        Some(0) => Ok(()),
        code => Err(Fail::exit(code.unwrap_or(1))),
    }
}

/// Several hosts' inventories as one.
pub fn merge_inventory(hosts: &[(String, Value)]) -> Value {
    let mut all = serde_json::Map::new();
    for (name, inventory) in hosts {
        all.insert(
            name.clone(),
            inventory["all"]["hosts"]["rookery_target"].clone(),
        );
    }
    json!({"all": {"hosts": all}})
}

/// `rookery host inventory [NAME…]`: the hosts as an Ansible inventory, with
/// the installation's key as a file of this machine.
pub fn inventory(global: &Global, names: &[String]) -> Done<()> {
    let paths = here_only(global, "rookery host inventory")?;
    let hosts = match names {
        [] => Host::all(&paths)
            .into_iter()
            .filter(|host| !host.address().is_empty())
            .collect(),
        names => names
            .iter()
            .map(|name| Host::load(&paths, name))
            .collect::<Result<Vec<_>>>()?,
    };
    let mut entries = vec![];
    for host in hosts {
        let inventory = host
            .inventory(&paths)
            .map_err(|error| format!("{}: {error}", host.name))?;
        entries.push((host.name, inventory));
    }
    out::json(&merge_inventory(&entries));
    Ok(())
}

/// The enrollment key an apply asks for, for a host not on the tailnet yet;
/// it goes to the role on standard output.
pub fn tailnet_key(host: &str) -> Done<()> {
    let paths = Paths::locate()?;
    println!("{}", rookery_core::tailnet::key_for(&paths, host)?);
    Ok(())
}

/// `rookery simulated-provisioner VERB`: the provisioner of a simulated
/// server, which rookery runs as the plugin `sim`, with the request on
/// standard input.
pub fn simulated_provisioner(verb: &str) -> Done<()> {
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let answer = rookery_core::simulate::provision(verb, &input, &mut std::io::stderr())?;
    match answer {
        serde_json::Value::String(log) => print!("{log}"),
        answer => println!("{answer}"),
    }
    Ok(())
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

/// `rookery server …`: the server of this machine itself.
pub fn server_command(command: args::Server) -> Done<()> {
    // Before rookery's own directories are looked for, which it never uses.
    if let args::Server::Run(run) = &command
        && (run.simulate || serve::simulate::asked())
    {
        return Ok(serve::simulate::command(run)?);
    }
    let paths = Paths::locate()?;
    match command {
        args::Server::Run(run) => {
            let mut words = vec![];
            if let Some(listen) = &run.listen {
                words.extend(["--listen", listen.as_str()]);
            }
            if run.expose {
                words.push("--expose");
            }
            Ok(serve::command(&paths, &words)?)
        }
        args::Server::SetupKey { revoke } => {
            let words: &[&str] = if revoke { &["--remove"] } else { &[] };
            Ok(serve::setup_key::command(&paths, words)?)
        }
        args::Server::Store(store) => {
            let target;
            let file;
            let words: Vec<&str> = match &store {
                args::Store::Status => vec![],
                args::Store::Encrypt => vec!["encrypt"],
                args::Store::Rekey => vec!["rekey"],
                args::Store::Decrypt => {
                    crate::cli::ask::confirm("This writes the store's values plain again.")?;
                    vec!["decrypt", "--yes"]
                }
                args::Store::Copy {
                    target: given,
                    target_file,
                    into_existing,
                } => {
                    let mut words = vec!["copy"];
                    match (given, target_file) {
                        (Some(given), None) => {
                            target = given.clone();
                            words.push(&target);
                        }
                        (None, Some(path)) => {
                            file = path.display().to_string();
                            words.extend(["--target-file", file.as_str()]);
                        }
                        _ => {
                            return Err(Fail::usage(
                                "rookery server store copy takes a TARGET or --target-file FILE",
                            ));
                        }
                    }
                    if *into_existing {
                        words.push("--into-existing");
                    }
                    words
                }
            };
            Ok(store_command::command(&paths, &words)?)
        }
        args::Server::Api(api) => {
            let document = match api {
                args::Api::Openapi => serde_json::to_value(serve::openapi())?,
                args::Api::Asyncapi => serve::asyncapi(),
                args::Api::Operations => serve::operations(),
            };
            out::json(&document);
            Ok(())
        }
        args::Server::Image { tag } => deploy::image(tag),
        args::Server::Deploy(deploy) => deploy::deploy(&deploy),
    }
}
