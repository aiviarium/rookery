//! The network under the command line: TCP or TLS to a `rookery serve`, its
//! socket on this machine, a server in this program's own process, and plain
//! HTTP requests to it.

use rookery_api::client::{self, Error};
use std::sync::{Arc, OnceLock};
use tokio::net::TcpStream;

pub use rookery_api::client::Stream;

/// The address of the server in this program's own process, which the
/// command line starts when no other is set.
pub const HERE: &str = "here:";

/// Whether the server is on this machine, reached without a token: the one
/// in this program, or the socket of a `rookery serve` of this user.
pub fn local(server: &str) -> bool {
    server == HERE || server.starts_with("unix:")
}

/// A server's address as typed, made whole: without a scheme it is https,
/// or http for this machine itself.
pub fn address(typed: &str) -> String {
    let typed = typed.trim().trim_end_matches('/');
    if typed.is_empty()
        || typed.starts_with("http://")
        || typed.starts_with("https://")
        || local(typed)
    {
        return typed.to_owned();
    }
    let local = ["localhost", "127.", "[::1]"]
        .iter()
        .any(|name| typed.starts_with(name));
    format!("{}://{typed}", if local { "http" } else { "https" })
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

/// How a server is reached.
pub enum Way {
    /// Over TCP, with TLS when `secure`.
    Network {
        name: String,
        port: u16,
        secure: bool,
    },
    /// The socket of a `rookery serve` on this machine.
    Socket(std::path::PathBuf),
    /// The server in this process.
    Here,
}

/// Where a request goes: how the server is reached, the authority to name in
/// requests, and the path.
pub struct Target {
    pub way: Way,
    pub authority: String,
    pub path: String,
}

/// The server in this process, when the program runs one.
static HERE_DIAL: std::sync::RwLock<Option<client::Dial>> = std::sync::RwLock::new(None);

/// Reach `here:` through `dial`, until another is given.
pub fn serve_here(dial: client::Dial) {
    *HERE_DIAL
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(dial);
}

/// Where `path` of `server` is: `http(s)://HOST[:PORT]`, `unix:/path` or `here:`.
pub fn target(server: &str, path: &str) -> Result<Target, String> {
    if server == HERE {
        return Ok(Target {
            way: Way::Here,
            authority: "rookery".into(),
            path: path.to_owned(),
        });
    }
    if let Some(socket) = server.strip_prefix("unix:") {
        return Ok(Target {
            way: Way::Socket(socket.into()),
            authority: "rookery".into(),
            path: path.to_owned(),
        });
    }
    let (secure, rest) = if let Some(rest) = server.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = server.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("{server} is no http:// or https:// address"));
    };
    // What follows the authority is the server's own prefix, before the path.
    let (authority, prefix) = match rest.find('/') {
        Some(at) => (&rest[..at], rest[at..].trim_end_matches('/')),
        None => (rest, ""),
    };
    let default = if secure { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        // Not the colons of an IPv6 address in brackets.
        Some((host, port)) if !port.contains(']') => (
            host,
            port.parse().map_err(|_| format!("{port} is no port"))?,
        ),
        _ => (authority, default),
    };
    if host.is_empty() {
        return Err(format!("{server} names no host"));
    }
    Ok(Target {
        way: Way::Network {
            name: host.trim_matches(['[', ']']).to_owned(),
            port,
            secure,
        },
        authority: format!("{host}:{port}"),
        path: format!("{prefix}{path}"),
    })
}

fn tls() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            // The system's own as well, so that a server with a certificate of
            // an authority this machine trusts, such as a company's, is taken.
            // A certificate the system store cannot give is left out.
            let _ = roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring has the default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

/// Connect to where `target` points, with TLS when it says so.
pub async fn connect(target: &Target) -> Result<Stream, String> {
    let (name, port, secure) = match &target.way {
        Way::Here => {
            let dial = HERE_DIAL
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .ok_or("There is no server in this program")?;
            return dial().await.map_err(|error| describe(&error));
        }
        #[cfg(unix)]
        Way::Socket(path) => {
            let stream = tokio::net::UnixStream::connect(path)
                .await
                .map_err(|error| format!("{}: {error}", path.display()))?;
            return Ok(Box::new(stream));
        }
        #[cfg(not(unix))]
        Way::Socket(path) => {
            return Err(format!(
                "{} is a socket, which this system has none of",
                path.display()
            ));
        }
        Way::Network { name, port, secure } => (name, *port, *secure),
    };
    let stream = TcpStream::connect((name.as_str(), port))
        .await
        .map_err(|error| format!("{}: {error}", target.authority))?;
    let _ = stream.set_nodelay(true);
    if !secure {
        return Ok(Box::new(stream));
    }
    let name =
        rustls::pki_types::ServerName::try_from(name.clone()).map_err(|error| error.to_string())?;
    let stream = tokio_rustls::TlsConnector::from(tls())
        .connect(name, stream)
        .await
        .map_err(|error| format!("{}: {error}", target.authority))?;
    Ok(Box::new(stream))
}

/// One request with a bearer: the status and the body of the answer.
pub async fn http(
    method: &str,
    server: &str,
    path: &str,
    bearer: &str,
) -> Result<(u16, String), String> {
    let target = target(server, path)?;
    let stream = connect(&target).await?;
    let mut client = client::Client::over(stream)
        .await
        .map_err(|error| describe(&error))?;
    let authorization = format!("Bearer {bearer}");
    let headers = [
        ("host", target.authority.as_str()),
        ("accept", "application/json"),
        ("authorization", authorization.as_str()),
    ];
    let answer = client
        .send(method, &target.path, &headers, Default::default())
        .await
        .map_err(|error| describe(&error))?;
    Ok((
        answer.status,
        String::from_utf8_lossy(&answer.body).into_owned(),
    ))
}

/// One request with a bearer and a JSON body, such as one with more bytes
/// than the portal carries: the answer as it came.
pub async fn send(
    method: &str,
    server: &str,
    path: &str,
    bearer: &str,
    body: Vec<u8>,
) -> Result<client::Response, String> {
    let target = target(server, path)?;
    let stream = connect(&target).await?;
    let mut client = client::Client::over(stream)
        .await
        .map_err(|error| describe(&error))?;
    let authorization = format!("Bearer {bearer}");
    let mut headers = vec![
        ("host", target.authority.as_str()),
        ("accept", "application/json"),
        ("content-type", "application/json"),
    ];
    if !bearer.is_empty() {
        headers.push(("authorization", authorization.as_str()));
    }
    client
        .send(method, &target.path, &headers, body.into())
        .await
        .map_err(|error| describe(&error))
}

pub fn describe(error: &Error) -> String {
    match error {
        Error::Api { message, .. } => message.clone(),
        Error::Transport(message) => message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(target: &Target) -> (&str, u16, bool) {
        match &target.way {
            Way::Network { name, port, secure } => (name, *port, *secure),
            _ => panic!("not over the network"),
        }
    }

    #[test]
    fn addresses_are_taken_apart() {
        let at = target("https://studio.example", "/v1/portal").unwrap();
        assert_eq!(network(&at), ("studio.example", 443, true));
        assert_eq!(
            (at.authority.as_str(), at.path.as_str()),
            ("studio.example:443", "/v1/portal")
        );
        let at = target("http://192.0.2.10:7878", "/v1/access").unwrap();
        assert_eq!(network(&at), ("192.0.2.10", 7878, false));
        let at = target("http://[2001:db8::1]:7878/rookery/", "/v1/portal").unwrap();
        assert_eq!(network(&at), ("2001:db8::1", 7878, false));
        assert_eq!(at.path, "/rookery/v1/portal");
        let at = target("https://[2001:db8::1]", "/").unwrap();
        assert_eq!(network(&at).1, 443);
        let at = target("unix:/run/user/1000/rookery/serve.sock", "/v1/portal").unwrap();
        assert!(matches!(&at.way, Way::Socket(path) if path.ends_with("serve.sock")));
        assert!(matches!(
            target("here:", "/v1/portal").unwrap().way,
            Way::Here
        ));
        for wrong in [
            "studio.example",
            "ftp://studio.example",
            "https://",
            "http://studio:port",
        ] {
            assert!(target(wrong, "/").is_err(), "{wrong}");
        }
    }

    #[test]
    fn a_server_typed_is_made_whole() {
        assert_eq!(
            address("studio.example:8080"),
            "https://studio.example:8080"
        );
        assert_eq!(address("127.0.0.1:7878/"), "http://127.0.0.1:7878");
        assert_eq!(address(" https://192.0.2.10/ "), "https://192.0.2.10");
        assert_eq!(address("here:"), "here:");
        assert_eq!(
            address("unix:/run/rookery/serve.sock"),
            "unix:/run/rookery/serve.sock"
        );
        assert!(local(HERE) && local("unix:/run/rookery/serve.sock"));
        assert!(!local("https://studio.example"));
    }
}
