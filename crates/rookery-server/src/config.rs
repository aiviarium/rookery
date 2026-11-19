//! The server's configuration, from `ROOKERY_*`.

use std::net::SocketAddr;
use std::path::PathBuf;

use rookery_kernel::config::Oidc;
use rookery_proto::auth::AuthMode;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `ROOKERY_DATABASE_URL`.
    pub database_url: String,
    /// `ROOKERY_DATABASE_LOCK_URL`: a direct connection for the leader lock, when the main one goes
    /// through a pooler.
    pub lock_url: String,
    /// `ROOKERY_LISTEN`, 127.0.0.1:7777 unless set.
    pub listen: SocketAddr,
    /// `ROOKERY_PUBLIC_URL`, `http://` and the listen address unless set.
    pub public_url: String,
    /// `ROOKERY_SECRET_KEY` or the file of `ROOKERY_SECRET_KEY_FILE`: 64 hex digits.
    pub secret_key: Option<String>,
    /// `ROOKERY_BLOBS`: the folder blobs are kept in.
    pub blobs: Option<PathBuf>,
    pub auth: AuthMode,
    pub oidc: Option<Oidc>,
    /// `ROOKERY_ADMINS`, apart by commas.
    pub admins: Vec<String>,
    /// `ROOKERY_SHELL_DIR`: a built shell, served at `/`.
    pub shell_dir: Option<PathBuf>,
    /// The Unix socket of the CLI on this machine: `ROOKERY_SOCKET`, or `server.sock` in rookery's
    /// runtime folder.
    pub socket: Option<PathBuf>,
}

/// What is wrong with the configuration, said for a person.
#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn fail<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(message.into()))
}

/// Where the CLI finds the server's socket on this machine.
pub fn default_socket(var: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(path) = var("ROOKERY_SOCKET").filter(|p| !p.trim().is_empty()) {
        return Some(PathBuf::from(path));
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return Some(PathBuf::from("/run/rookery/server.sock"));
    }
    var("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(|dir| PathBuf::from(dir).join("rookery/server.sock"))
}

fn is_public(addr: &SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

impl ServerConfig {
    /// Reads `ROOKERY_*` through `var`, which is `std::env::var` outside tests.
    pub fn from_vars(var: &dyn Fn(&str) -> Option<String>) -> Result<ServerConfig, ConfigError> {
        let get = |name: &str| {
            var(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let flag =
            |name: &str| get(name).is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"));
        let Some(database_url) = get("ROOKERY_DATABASE_URL") else {
            return fail("ROOKERY_DATABASE_URL names the Postgres database of rookery serve");
        };
        let lock_url = get("ROOKERY_DATABASE_LOCK_URL").unwrap_or_else(|| database_url.clone());
        let listen: SocketAddr = match get("ROOKERY_LISTEN") {
            Some(text) => text
                .parse()
                .map_err(|_| ConfigError(format!("ROOKERY_LISTEN is ADDRESS:PORT, not {text}")))?,
            None => "127.0.0.1:7777".parse().expect("an address"),
        };
        if is_public(&listen) && !flag("ROOKERY_ALLOW_PUBLIC") {
            return fail(format!(
                "ROOKERY_LISTEN is {listen}, an address other machines reach: set ROOKERY_ALLOW_PUBLIC=1 to serve there"
            ));
        }
        let public_url = get("ROOKERY_PUBLIC_URL").unwrap_or_else(|| format!("http://{listen}"));
        if !(public_url.starts_with("http://") || public_url.starts_with("https://")) {
            return fail(format!(
                "ROOKERY_PUBLIC_URL is http:// or https:// and an address, not {public_url}"
            ));
        }
        let secret_key = match (get("ROOKERY_SECRET_KEY"), get("ROOKERY_SECRET_KEY_FILE")) {
            (Some(_), Some(_)) => {
                return fail("set ROOKERY_SECRET_KEY or ROOKERY_SECRET_KEY_FILE, not both");
            }
            (Some(key), None) => Some(key),
            (None, Some(file)) => Some(
                std::fs::read_to_string(&file)
                    .map_err(|e| {
                        ConfigError(format!("reading ROOKERY_SECRET_KEY_FILE {file}: {e}"))
                    })?
                    .trim()
                    .to_owned(),
            ),
            (None, None) => None,
        };
        let admins = get("ROOKERY_ADMINS")
            .map(|list| {
                list.split(',')
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let oidc = match get("ROOKERY_OIDC_ISSUER") {
            Some(issuer) => {
                let Some(client_id) = get("ROOKERY_OIDC_CLIENT_ID") else {
                    return fail("ROOKERY_OIDC_ISSUER needs ROOKERY_OIDC_CLIENT_ID");
                };
                let client_secret = match get("ROOKERY_OIDC_CLIENT_SECRET_FILE") {
                    Some(file) => Some(
                        std::fs::read_to_string(&file)
                            .map_err(|e| {
                                ConfigError(format!(
                                    "reading ROOKERY_OIDC_CLIENT_SECRET_FILE {file}: {e}"
                                ))
                            })?
                            .trim()
                            .to_owned(),
                    ),
                    None => get("ROOKERY_OIDC_CLIENT_SECRET"),
                };
                let audiences = get("ROOKERY_OIDC_AUDIENCES")
                    .map(|list| {
                        list.split(',')
                            .map(str::trim)
                            .filter(|a| !a.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Oidc {
                    issuer,
                    client_id,
                    client_secret,
                    audiences,
                    groups_claim: get("ROOKERY_OIDC_GROUPS_CLAIM")
                        .unwrap_or_else(|| "groups".into()),
                })
            }
            None => None,
        };
        let auth = match get("ROOKERY_AUTH").as_deref() {
            None if oidc.is_some() => AuthMode::Oidc,
            None | Some("local") => AuthMode::Local,
            Some("oidc") if oidc.is_some() => AuthMode::Oidc,
            Some("oidc") => {
                return fail(
                    "ROOKERY_AUTH=oidc needs ROOKERY_OIDC_ISSUER and ROOKERY_OIDC_CLIENT_ID",
                );
            }
            Some(other) => return fail(format!("ROOKERY_AUTH is local or oidc, not {other}")),
        };
        if auth == AuthMode::Local && oidc.is_some() && get("ROOKERY_AUTH").is_some() {
            return fail("ROOKERY_AUTH=local and ROOKERY_OIDC_ISSUER do not go together");
        }
        Ok(ServerConfig {
            database_url,
            lock_url,
            listen,
            public_url: public_url.trim_end_matches('/').to_owned(),
            secret_key,
            blobs: get("ROOKERY_BLOBS").map(PathBuf::from),
            auth,
            oidc,
            admins,
            shell_dir: get("ROOKERY_SHELL_DIR").map(PathBuf::from),
            socket: default_socket(var),
        })
    }

    pub fn from_env() -> Result<ServerConfig, ConfigError> {
        ServerConfig::from_vars(&|name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn read(pairs: &[(&str, &str)]) -> Result<ServerConfig, ConfigError> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ServerConfig::from_vars(&|name| vars.get(name).cloned())
    }

    #[test]
    fn defaults_and_refusals() {
        assert!(read(&[]).is_err());
        let config =
            read(&[("ROOKERY_DATABASE_URL", "postgres://db.example.test/rookery")]).unwrap();
        assert_eq!(config.listen.to_string(), "127.0.0.1:7777");
        assert_eq!(config.public_url, "http://127.0.0.1:7777");
        assert_eq!(config.lock_url, config.database_url);
        assert_eq!(config.auth, AuthMode::Local);

        let public = [
            ("ROOKERY_DATABASE_URL", "postgres://db"),
            ("ROOKERY_LISTEN", "0.0.0.0:7777"),
        ];
        assert!(
            read(&public)
                .unwrap_err()
                .0
                .contains("ROOKERY_ALLOW_PUBLIC")
        );
        let allowed = [
            public[0],
            public[1],
            ("ROOKERY_ALLOW_PUBLIC", "1"),
            ("ROOKERY_PUBLIC_URL", "https://rookery.example.test/"),
        ];
        assert_eq!(
            read(&allowed).unwrap().public_url,
            "https://rookery.example.test"
        );
    }

    #[test]
    fn an_issuer_turns_oidc_on() {
        let base = ("ROOKERY_DATABASE_URL", "postgres://db");
        assert!(read(&[base, ("ROOKERY_AUTH", "oidc")]).is_err());
        assert!(read(&[base, ("ROOKERY_OIDC_ISSUER", "https://id.example.test")]).is_err());
        let config = read(&[
            base,
            ("ROOKERY_OIDC_ISSUER", "https://id.example.test"),
            ("ROOKERY_OIDC_CLIENT_ID", "rookery"),
            ("ROOKERY_ADMINS", "ops@example.test, group:platform"),
        ])
        .unwrap();
        assert_eq!(config.auth, AuthMode::Oidc);
        assert_eq!(config.admins, ["ops@example.test", "group:platform"]);
        assert_eq!(config.oidc.unwrap().groups_claim, "groups");
    }
}
