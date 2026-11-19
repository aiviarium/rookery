//! `rookery login URL`: use the rookery server at URL. A browser signs this
//! machine in (`/auth/desktop`, a loopback address and PKCE), or a one-time
//! sign-in link does, pasted or given as the URL, or a token given with
//! `--token-file` or `--token-stdin`. The server and the device session or
//! token are kept as this user's settings.

use super::args::{Global, Login};
use super::ask;
use super::client::{self, runtime};
use super::fail::{Fail, Result};
use super::out;
use super::settings;
use rookery_client::net;
use serde_json::{Value, json};
use std::time::Duration;

/// How long the browser has to sign in.
const BROWSER_FOR: Duration = Duration::from_secs(300);

pub fn login(global: &Global, login: &Login) -> Result<()> {
    let given = net::address(&login.url);
    // A sign-in link given as the address: the server is what comes before it.
    let (url, link) = match given.find("/auth/link/") {
        Some(at) => (given[..at].to_owned(), Some(given.clone())),
        None => (given.clone(), None),
    };
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Fail::usage(format!(
            "{} is no address of a rookery server: https://HOST or http://HOST:PORT",
            login.url
        )));
    }
    let settings = client::settings();
    let token = if global.token_file.is_some() || global.token_stdin {
        read_token(global)?
    } else if let Some(link) = link {
        redeem(&url, &link)?
    } else if login.link_stdin {
        let link = ask::secret_from("Sign-in link", None, true, "--link-stdin")?;
        redeem(&url, link.trim())?
    } else if login.no_browser || !out::get().ask {
        let link = ask::secret(
            "Sign-in link, from rookery server setup-key or an admin",
            "--link-stdin",
        )?;
        redeem(&url, link.trim())?
    } else {
        browser(&url)?
    };
    let version = check(&url, &token)?;
    settings
        .set(&[(settings::SERVER, &url), (settings::TOKEN, &token)])
        .map_err(|error| Fail::failed(error.to_string()))?;
    out::done(format!(
        "rookery uses the rookery server at {url} now, rookery {version}"
    ));
    if std::env::var_os("ROOKERY_SERVER").is_some() {
        out::warn("ROOKERY_SERVER is set in this shell and is used before the setting");
    }
    Ok(())
}

/// The token opens the server: its version.
fn check(url: &str, token: &str) -> Result<String> {
    super::client::warn_plain(url);
    let me = runtime().block_on(async {
        let client = rookery_client::Client::connect(url, Some(token)).await?;
        client.call("auth.me", json!({})).await
    });
    match me {
        Ok(me) => Ok(me["version"].as_str().unwrap_or("").to_owned()),
        Err(error)
            if matches!(
                error.code(),
                rookery_proto::ErrorCode::Unauthenticated | rookery_proto::ErrorCode::Forbidden
            ) =>
        {
            Err(Fail::refused(format!(
                "The server at {url} did not take the sign-in: {error}"
            )))
        }
        Err(error) => Err(Fail::unreachable(format!(
            "The rookery server at {url} cannot be reached: {error}"
        ))),
    }
}

fn read_token(global: &Global) -> Result<String> {
    if let Some(file) = &global.token_file {
        return super::client::token_file(file);
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
    Ok(text.trim().to_owned())
}

/// Use up a one-time sign-in link for a device session of this machine.
fn redeem(url: &str, link: &str) -> Result<String> {
    let path = match link.find("/auth/link/") {
        Some(at) => link[at..].to_owned(),
        None => {
            return Err(Fail::usage(
                "That is no sign-in link: one has /auth/link/ in it",
            ));
        }
    };
    let name = std::env::var("USER").unwrap_or_default();
    let body = serde_json::to_vec(&json!({"name": name}))?;
    let answer = runtime()
        .block_on(net::send("POST", url, &path, "", body))
        .map_err(Fail::unreachable)?;
    let said: Value = serde_json::from_slice(&answer.body).unwrap_or(Value::Null);
    match said["token"].as_str() {
        Some(token) => Ok(token.to_owned()),
        None => Err(Fail::refused(format!(
            "The server did not take the link: {}",
            said["error"]["message"]
                .as_str()
                .or(said["message"].as_str())
                .unwrap_or("it is wrong, used up or expired")
        ))),
    }
}

fn random(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    // The system's own source on every system; nothing weaker stands in for it.
    getrandom::fill(&mut buffer)
        .map_err(|e| Fail::failed(format!("no random bytes from the system: {e}")))?;
    Ok(base64url(&buffer))
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).into()
}

fn base64url(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn query_value(target: &str, key: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| rookery_api::client::decode(v))
    })
}

/// Sign in through the browser: listen here, open `/auth/desktop`, take the code the browser brings
/// back, and exchange it for a device session.
fn browser(url: &str) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let verifier = random(32)?;
    let state = random(16)?;
    let challenge = base64url(&sha256(verifier.as_bytes()));
    let token = runtime().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
        let device = format!(
            "rookery on {}",
            std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| "this machine".into())
        );
        let query: String = [
            ("redirect_uri", redirect.as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("device", device.as_str()),
        ]
        .iter()
        .map(|(k, v)| format!("{k}={}", rookery_api::client::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
        let page = format!("{url}/auth/desktop?{query}");
        if !open_browser(&page) {
            out::note(format!("Open this in a browser to sign in:\n  {page}"));
        } else {
            out::note(format!("Signing in in the browser. If none opened: {page}"));
        }
        let waited = tokio::time::timeout(BROWSER_FOR, async {
            loop {
                let (mut socket, _) = listener.accept().await?;
                let mut request = vec![0; 8192];
                let read = socket.read(&mut request).await?;
                let head = String::from_utf8_lossy(&request[..read]).into_owned();
                let target = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                let said = |text: &str| {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\nconnection: close\r\n\r\n{text}\n"
                    )
                };
                if !target.starts_with("/callback") {
                    let _ = socket.write_all(said("Not this address").as_bytes()).await;
                    continue;
                }
                let code = query_value(&target, "code");
                let back = query_value(&target, "state");
                if back.as_deref() != Some(state.as_str()) || code.is_none() {
                    let _ = socket.write_all(said("This sign-in is not the one rookery started.").as_bytes()).await;
                    continue;
                }
                let _ = socket
                    .write_all(said("rookery is signed in. You can close this page.").as_bytes())
                    .await;
                return Ok::<_, std::io::Error>(code.unwrap_or_default());
            }
        })
        .await;
        let code = match waited {
            Ok(code) => code?,
            Err(_) => return Err(Fail::failed("The browser did not sign in within five minutes")),
        };
        let body = serde_json::to_vec(&json!({
            "code": code, "code_verifier": verifier, "redirect_uri": redirect,
        }))?;
        let answer = net::send("POST", url, "/auth/desktop/token", "", body)
            .await
            .map_err(Fail::unreachable)?;
        let said: Value = serde_json::from_slice(&answer.body).unwrap_or(Value::Null);
        said["token"].as_str().map(str::to_owned).ok_or_else(|| {
            Fail::refused(format!(
                "The server did not sign this machine in: {}",
                said["error_description"].as_str().unwrap_or("it gave no session")
            ))
        })
    })?;
    Ok(token)
}

/// Open a page in the person's browser; false when there is none to open.
fn open_browser(page: &str) -> bool {
    let program = if cfg!(target_os = "macos") {
        ("open", vec![page])
    } else if cfg!(windows) {
        ("cmd", vec!["/C", "start", "", page])
    } else {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return false;
        }
        ("xdg-open", vec![page])
    };
    std::process::Command::new(program.0)
        .args(&program.1)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

pub fn logout() -> Result<()> {
    let settings = client::settings();
    let server = settings.get(settings::SERVER);
    settings
        .set(&[(settings::SERVER, ""), (settings::TOKEN, "")])
        .map_err(|error| Fail::failed(error.to_string()))?;
    match server {
        Some(server) => out::done(format!(
            "rookery no longer uses the rookery server at {server}"
        )),
        None => out::hint("rookery used no server of its own"),
    }
    Ok(())
}
