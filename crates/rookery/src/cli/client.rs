//! The server a command talks to: the one named by `--server` or
//! `ROOKERY_SERVER`, the one `rookery login` kept, or the `rookery serve` on
//! this machine, by its socket. Every command is a call of its API over
//! `/ws`, through rookery-client.

use super::args::Global;
use super::fail::{Fail, Result};
use super::settings::{self, Settings};
use rookery_client::net;
use rookery_proto::hosts::Acted;
use serde_json::{Value, json};
use std::future::Future;
use std::sync::OnceLock;

pub struct Client {
    /// The server's address, as `--server` takes it.
    pub url: String,
    /// The token it was opened with, to open it again.
    token: String,
    inner: rookery_client::Client,
}

/// The runtime the connections run on; the command line stays on the main
/// thread.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the runtime starts")
    })
}

/// The socket of a `rookery serve` on this machine, when there is one:
/// `ROOKERY_SOCKET`, the one in the user's runtime folder, or the system's.
pub fn local_socket() -> Option<String> {
    if !cfg!(unix) {
        return None;
    }
    rookery_api::socket::found(&variable).map(|path| format!("unix:{}", path.display()))
}

/// The settings of this user that `rookery login` keeps.
pub fn settings() -> Settings {
    Settings::new(settings::folder(|name| std::env::var(name).ok()))
}

fn variable(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The token of this run: from a file, standard input, `ROOKERY_TOKEN`, or
/// the one `rookery login` kept for that server.
/// A token from `--token-file`, with a warning when others on this machine can read the file.
pub fn token_file(file: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(file)
        .map_err(|error| Fail::usage(format!("--token-file {}: {error}", file.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(file)
            && meta.permissions().mode() & 0o077 != 0
        {
            super::out::warn(format!(
                "{} can be read by others on this machine, and the token in it gives its access to them: chmod 600 {}",
                file.display(),
                file.display()
            ));
        }
    }
    Ok(text.trim().to_owned())
}

/// Warns before a token goes in plain text to a server other than this machine.
pub fn warn_plain(url: &str) {
    let Some(rest) = url.strip_prefix("http://") else {
        return;
    };
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    let host = match host.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => host,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !loopback {
        super::out::warn(format!(
            "{url} is plain HTTP: the token goes to it unencrypted. Use the server's https:// address"
        ));
    }
}

fn token(global: &Global, url: &str, kept: &Settings) -> Result<String> {
    if let Some(file) = &global.token_file {
        return token_file(file);
    }
    if global.token_stdin {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        return Ok(text.trim().to_owned());
    }
    if let Some(token) = variable("ROOKERY_TOKEN") {
        return Ok(token);
    }
    let same = kept
        .get(settings::SERVER)
        .is_some_and(|server| server.trim_end_matches('/') == url);
    Ok(match same {
        true => kept.get(settings::TOKEN).unwrap_or_default(),
        false => String::new(),
    })
}

/// The server this run uses, if one is named: by a flag, the environment,
/// or the settings.
pub fn named(global: &Global) -> Option<String> {
    named_in(global, &settings())
}

fn named_in(global: &Global, kept: &Settings) -> Option<String> {
    global
        .server
        .clone()
        .filter(|server| !server.trim().is_empty())
        .or_else(|| variable("ROOKERY_SERVER"))
        .or_else(|| kept.get(settings::SERVER))
        .map(|server| net::address(&server))
}

impl Client {
    /// Reach the server this run uses.
    pub fn open(global: &Global) -> Result<Client> {
        let kept = settings();
        Client::open_in(global, &kept)
    }

    /// Reach the server this run uses, with these settings.
    pub fn open_in(global: &Global, kept: &Settings) -> Result<Client> {
        let url = match named_in(global, kept).or_else(local_socket) {
            Some(url) => url,
            None => {
                return Err(Fail::unreachable(
                    "No rookery server is set. Name one with rookery login URL, or with --server URL for one command",
                ));
            }
        };
        let token = match net::local(&url) {
            true => String::new(),
            false => token(global, &url, kept)?,
        };
        if !token.is_empty() {
            warn_plain(&url);
        }
        Client::reach(url, token)
    }

    /// The same, as `open_in`, kept for callers that ask the server whatever its version.
    pub fn open_any(global: &Global, kept: &Settings) -> Result<Client> {
        Client::open_in(global, kept)
    }

    fn reach(url: String, token: String) -> Result<Client> {
        let inner = runtime()
            .block_on(rookery_client::Client::connect(&url, Some(&token)))
            .map_err(|error| reached(&url, error))?;
        Ok(Client { url, token, inner })
    }

    /// The same server again, as when it was replaced and answers anew.
    pub fn reopen(&self) -> Result<Client> {
        Client::reach(self.url.clone(), self.token.clone())
    }

    /// The server's version and the number of its API.
    pub fn server_version(&self) -> Result<(String, u32)> {
        let said = self.call("auth.me", json!({}))?;
        let version = said["version"].as_str().unwrap_or("").to_owned();
        let api = said["apiVersion"].as_u64().unwrap_or(0) as u32;
        Ok((version, api))
    }

    pub fn wait<F: Future>(&self, future: F) -> F::Output {
        runtime().block_on(future)
    }

    /// One method of the server, waited for.
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.wait(self.inner.call(method, params))
            .map_err(Fail::from)
    }

    /// A method that opens a stream, and the stream.
    pub fn stream(&self, method: &str, params: Value) -> Result<rookery_client::Stream> {
        self.wait(self.inner.open(method, params))
            .map_err(Fail::from)
    }

    /// The connection, for work that runs on the runtime itself.
    pub fn inner(&self) -> &rookery_client::Client {
        &self.inner
    }

    /// Run an action of a subject: a host, a provisioner, a resource, or
    /// the installation.
    pub fn act(&self, subject: &Subject, action: &str, act: &rookery_api::Act) -> Result<Acted> {
        let mut params = serde_json::to_value(act)?;
        let method = match subject {
            Subject::Host(name) => {
                params["host"] = json!(name);
                "hosts.act"
            }
            Subject::Provisioner(name) => {
                params["name"] = json!(name);
                "provisioners.act"
            }
            Subject::Resource(id) => {
                params["id"] = json!(id);
                "resources.act"
            }
            Subject::Installation => "installation.act",
        };
        params["action"] = json!(action);
        Ok(serde_json::from_value(self.call(method, params)?)?)
    }

    /// The actions of a subject.
    pub fn actions(&self, subject: &Subject) -> Result<Vec<rookery_api::Action>> {
        let (method, params) = match subject {
            Subject::Host(name) => ("hosts.actions", json!({"host": name})),
            Subject::Provisioner(name) => ("provisioners.actions", json!({"name": name})),
            Subject::Resource(id) => ("resources.actions", json!({"id": id})),
            Subject::Installation => ("installation.actions", json!({})),
        };
        let listed = self.call(method, params)?;
        Ok(serde_json::from_value(listed["actions"].clone())?)
    }
}

/// Why the server at `url` was not reached, for a person.
fn reached(url: &str, error: rookery_client::Error) -> Fail {
    use rookery_proto::ErrorCode::*;
    match error.code() {
        Unauthenticated | Forbidden => Fail::refused(format!(
            "The rookery server at {url} did not take the token. Log in again with: rookery login {url}"
        )),
        _ => Fail::unreachable(format!(
            "The rookery server at {url} cannot be reached: {error}"
        )),
    }
}

/// What an action is done to.
#[derive(Debug, Clone, PartialEq)]
pub enum Subject {
    Host(String),
    Provisioner(String),
    Resource(String),
    Installation,
}

impl Subject {
    /// How a job of it is named in its record, as `host:studio`.
    pub fn shown(&self) -> String {
        match self {
            Subject::Host(name) => name.clone(),
            Subject::Provisioner(name) => format!("provisioner {name}"),
            Subject::Resource(id) => format!("resource {id}"),
            Subject::Installation => "the installation".into(),
        }
    }
}

/// Params of a method: `args` with the fields of `data` added.
pub fn merged(mut args: Value, data: Value) -> Value {
    if let (Some(args), Value::Object(data)) = (args.as_object_mut(), data) {
        for (key, value) in data {
            args.insert(key, value);
        }
    }
    args
}

/// Params of a method of a session on a host from `{"id": ID, …}`: the rest as its query.
pub fn terminal(host: &str, args: Value) -> Value {
    let mut query = serde_json::Map::new();
    let mut id = String::new();
    for (key, value) in args.as_object().cloned().unwrap_or_default() {
        match (key.as_str(), value) {
            ("id", Value::String(given)) => id = given,
            (_, Value::String(text)) => {
                query.insert(key, json!(text));
            }
            (_, Value::Null) => {}
            (_, other) => {
                query.insert(key, json!(other.to_string()));
            }
        }
    }
    json!({"host": host, "terminal": id, "query": query})
}
