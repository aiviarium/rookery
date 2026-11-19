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
    /// `ROOKERY_SECRET_KEY` or the file of `ROOKERY_SECRET_KEY_FILE`: 64 hex digits, as Rookery's key
    /// file has them, or 32 bytes in base64.
    pub secret_key: Option<rookery_store::seal::Key>,
    /// `ROOKERY_BLOBS`: the folder blobs are kept in.
    pub blobs: Option<PathBuf>,
    /// `ROOKERY_DATABASE_ROLE_TAG`: a namespace of this deployment's raw SQL roles, when several deployments
    /// share a Postgres server.
    pub sql_role_tag: Option<String>,
    /// `ROOKERY_SERVICE_DATABASE_URL`: where plugins' services reach Postgres, when not where the server does.
    pub service_database_url: Option<String>,
    pub auth: AuthMode,
    pub oidc: Option<Oidc>,
    /// `ROOKERY_ADMINS`, apart by commas.
    pub admins: Vec<String>,
    /// `ROOKERY_OUTBOUND_ALLOW`: internal addresses and ranges, apart by commas, that the server may
    /// reach for a member, such as `10.20.0.0/16` for a registry on the installation's own network.
    pub outbound_allow: Vec<rookery_kernel::outbound::AddrRange>,
    /// `ROOKERY_SHELL_DIR`: a built shell, served at `/`.
    pub shell_dir: Option<PathBuf>,
    /// `ROOKERY_PORTS_URL`: where hosts' ports are served, each on a name of its own before it, such
    /// as `https://ports.rookery.example` for `https://3000-HOSTID.ports.rookery.example`. Without
    /// it, a server on the loopback serves them under `localhost`, and any other refuses them.
    pub ports_url: Option<String>,
    /// The Unix socket of the CLI on this machine: `ROOKERY_SOCKET`, or `server.sock` in rookery's
    /// runtime folder.
    pub socket: Option<PathBuf>,
    /// Rookery's folders of configuration and data, which the hosts' files are written to: the key
    /// ring, known hosts and the plugins. Without them the server has no hosts.
    pub paths: Option<rookery_core::paths::Paths>,
    /// How long provisioners may take and how a new machine is waited for, which tests make quick.
    pub provision: fn() -> rookery_core::provision::Options,
    /// How plugins' host halves run: `ROOKERY_BUN` (the Bun program, `bun` by default),
    /// `ROOKERY_EXTHOST` (the extension host's entry: `host/dist/main.js` of the provisioning source
    /// unless it is set, or `host/src/main.ts` of a checkout; without one no host half runs) and
    /// `ROOKERY_EXTHOST_MEMORY_MB` (the memory a host may hold, 512 by default).
    pub exthost: rookery_plugins::HostOptions,
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
    rookery_api::socket::bound(var)
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
        let secret_key = secret_key
            .map(|text| {
                rookery_store::seal::Key::parse(&text)
                    .map_err(|e| ConfigError(format!("the secret key: {e}")))
            })
            .transpose()?;
        let admins = get("ROOKERY_ADMINS")
            .map(|list| {
                list.split(',')
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let outbound_allow = get("ROOKERY_OUTBOUND_ALLOW")
            .map(|list| rookery_kernel::outbound::AddrRange::parse_list(&list))
            .transpose()
            .map_err(|e| ConfigError(format!("ROOKERY_OUTBOUND_ALLOW: {e}")))?
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
        let ports_url = get("ROOKERY_PORTS_URL");
        if let Some(url) = &ports_url {
            rookery_hosts::proxy::Ports::parse(url)
                .map_err(|e| ConfigError(format!("ROOKERY_PORTS_URL: {e}")))?;
        }
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
            sql_role_tag: get("ROOKERY_DATABASE_ROLE_TAG"),
            service_database_url: get("ROOKERY_SERVICE_DATABASE_URL"),
            auth,
            oidc,
            admins,
            outbound_allow,
            shell_dir: get("ROOKERY_SHELL_DIR").map(PathBuf::from),
            ports_url,
            socket: default_socket(var),
            paths: None,
            provision: Default::default,
            exthost: rookery_plugins::HostOptions {
                bun: get("ROOKERY_BUN")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("bun")),
                entry: get("ROOKERY_EXTHOST").map(PathBuf::from),
                memory_mb: match get("ROOKERY_EXTHOST_MEMORY_MB") {
                    Some(mb) => mb.trim().parse().map_err(|_| {
                        ConfigError(format!(
                            "ROOKERY_EXTHOST_MEMORY_MB is a number of MiB, not {mb}"
                        ))
                    })?,
                    None => rookery_plugins::HostOptions::default().memory_mb,
                },
            },
        })
    }

    /// From the environment, with the folders of the user it runs as.
    pub fn from_env() -> Result<ServerConfig, ConfigError> {
        let mut config = ServerConfig::from_vars(&|name| std::env::var(name).ok())?;
        let paths = rookery_core::paths::Paths::locate()
            .map_err(|e| ConfigError(format!("rookery's folders: {e}")))?;
        // Without ROOKERY_EXTHOST, host halves run with the bundle the provisioning source carries.
        if config.exthost.entry.is_none() {
            config.exthost.entry = paths
                .source()
                .ok()
                .map(|source| source.join("host/dist/main.js"))
                .filter(|entry| entry.is_file());
        }
        config.paths = Some(paths);
        Ok(config)
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
        assert!(config.outbound_allow.is_empty());
        let allowing = read(&[
            ("ROOKERY_DATABASE_URL", "postgres://db"),
            ("ROOKERY_OUTBOUND_ALLOW", "10.20.0.0/16, fd00::1"),
        ])
        .unwrap();
        assert_eq!(allowing.outbound_allow.len(), 2);
        assert!(
            read(&[
                ("ROOKERY_DATABASE_URL", "postgres://db"),
                ("ROOKERY_OUTBOUND_ALLOW", "intranet")
            ])
            .is_err()
        );

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

    #[test]
    fn the_key_file_of_rookery_reads_as_it_is() {
        let dir = std::env::temp_dir().join(format!("rookery-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("store.key");
        std::fs::write(&file, format!("{}\n", "0f".repeat(32))).unwrap();
        let base = ("ROOKERY_DATABASE_URL", "postgres://db");
        let config = read(&[base, ("ROOKERY_SECRET_KEY_FILE", file.to_str().unwrap())]).unwrap();
        assert_eq!(config.secret_key.unwrap().to_hex(), "0f".repeat(32));
        assert!(read(&[base, ("ROOKERY_SECRET_KEY", "short")]).is_err());
        assert!(read(&[base]).unwrap().secret_key.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
