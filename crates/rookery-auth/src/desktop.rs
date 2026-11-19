//! Signing in a program on a person's own machine, such as the `rookery` CLI or a desktop app, through
//! their browser: a loopback redirect with PKCE (RFC 8252).
//!
//! 1. The program listens on `http://127.0.0.1:PORT/…` and opens
//!    `GET /auth/desktop?redirect_uri=…&state=…&code_challenge=…&code_challenge_method=S256`.
//! 2. A browser that is signed in is asked to confirm; one that is not signs in first (with the identity
//!    provider, or a sign-in link of a local account) and comes back.
//! 3. The confirmation sends the browser to the program's address with a one-time `code` and the `state`.
//! 4. The program posts `{code, code_verifier}` to `/auth/desktop/token` and gets a device session of its
//!    own, which shows in the person's devices and is revoked like any.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use rookery_kernel::auth::{authenticate, cookie};
use rookery_kernel::{Caller, Kernel};
use rookery_proto::auth::AuthMode;
use serde::Deserialize;
use uuid::Uuid;

use crate::oidc::challenge;
use crate::pages::{escape, page, problem};
use crate::session::{Device, same_origin, start_session};

/// How long a code waits to be exchanged.
const CODE_FOR: Duration = Duration::from_secs(300);

struct Granted {
    user: Uuid,
    challenge: String,
    redirect_uri: String,
    device: String,
    at: Instant,
}

/// The codes given out and not exchanged yet. They live on the leader alone: one a standby never saw
/// is asked for again.
#[derive(Default)]
pub struct Codes(Mutex<HashMap<String, Granted>>);

#[derive(Debug, Deserialize)]
pub struct Asked {
    redirect_uri: String,
    state: String,
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: Option<String>,
    /// What the program calls itself, for the list of devices.
    #[serde(default)]
    device: Option<String>,
}

/// Whether `uri` is the program's own address on this machine: `http://127.0.0.1`, `[::1]` or
/// `localhost`, with a port.
pub fn loopback(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => return false,
    };
    matches!(host, "127.0.0.1" | "[::1]" | "localhost")
}

fn checked(asked: &Asked) -> Result<(), Box<Response>> {
    let bad = |why: &str| {
        Box::new(problem(
            StatusCode::BAD_REQUEST,
            "This sign-in cannot go on",
            why,
        ))
    };
    if !loopback(&asked.redirect_uri) {
        return Err(bad("The program's address is not one on this machine."));
    }
    if asked.code_challenge_method.as_deref().unwrap_or("S256") != "S256" {
        return Err(bad("The challenge is S256 alone."));
    }
    if asked.code_challenge.len() < 43 || asked.state.is_empty() || asked.state.len() > 200 {
        return Err(bad("The challenge or the state is missing."));
    }
    Ok(())
}

fn query_of(asked: &Asked) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("redirect_uri", &asked.redirect_uri)
        .append_pair("state", &asked.state)
        .append_pair("code_challenge", &asked.code_challenge)
        .append_pair("code_challenge_method", "S256");
    if let Some(device) = &asked.device {
        query.append_pair("device", device);
    }
    query.finish()
}

async fn signed_in(kernel: &Kernel, headers: &HeaderMap) -> Option<rookery_proto::User> {
    let token = cookie(headers)?;
    match authenticate(kernel, &token).await {
        Some(Caller::User { user, .. }) => Some(user),
        _ => None,
    }
}

fn device_name(asked: &Asked) -> String {
    asked
        .device
        .as_deref()
        .map(|name| name.chars().filter(|c| !c.is_control()).take(80).collect())
        .filter(|name: &String| !name.trim().is_empty())
        .unwrap_or_else(|| "rookery".into())
}

/// `GET /auth/desktop`: the browser confirms, or signs in first.
pub async fn ask(
    State(kernel): State<Kernel>,
    headers: HeaderMap,
    Query(asked): Query<Asked>,
) -> Response {
    if let Err(refused) = checked(&asked) {
        return *refused;
    }
    let Some(user) = signed_in(&kernel, &headers).await else {
        let back = format!("/auth/desktop?{}", query_of(&asked));
        return match kernel.config.auth {
            AuthMode::Oidc => {
                let to = format!(
                    "/auth/login?{}",
                    form_urlencoded::Serializer::new(String::new())
                        .append_pair("return_to", &back)
                        .finish()
                );
                Redirect::to(&to).into_response()
            }
            AuthMode::Local => page(
                StatusCode::OK,
                "Sign in to this server first",
                "<p>This browser is not signed in to this server. Open a sign-in link of yours in it, then run the sign-in of the program again. Or paste the link into the program instead.</p>",
            ),
        };
    };
    let device = device_name(&asked);
    let body = format!(
        r#"<p>{} asks to sign in to this server as {}, on this machine.</p><form method="post" action="/auth/desktop?{}"><button type="submit">Sign it in</button></form>"#,
        escape(&device),
        escape(&user.name),
        escape(&query_of(&asked))
    );
    page(StatusCode::OK, "Sign in a program", &body)
}

/// `POST /auth/desktop`: the browser confirmed; it goes on to the program with a code.
pub async fn grant(
    State(kernel): State<Kernel>,
    headers: HeaderMap,
    Query(asked): Query<Asked>,
) -> Response {
    if let Err(refused) = checked(&asked) {
        return *refused;
    }
    if !same_origin(&kernel, &headers) {
        return problem(
            StatusCode::FORBIDDEN,
            "This sign-in cannot go on",
            "It is confirmed from a page of this server.",
        );
    }
    let Some(user) = signed_in(&kernel, &headers).await else {
        return problem(
            StatusCode::UNAUTHORIZED,
            "Sign in to this server first",
            "This browser is not signed in to this server.",
        );
    };
    let Ok(user_id) = Uuid::parse_str(&user.id) else {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "A user's id",
            "is not a UUID",
        );
    };
    let code = rookery_store::util::random_token("");
    {
        let codes = kernel.facility::<Codes>();
        let mut codes = codes.0.lock().unwrap();
        codes.retain(|_, granted| granted.at.elapsed() < CODE_FOR);
        codes.insert(
            code.clone(),
            Granted {
                user: user_id,
                challenge: asked.code_challenge.clone(),
                redirect_uri: asked.redirect_uri.clone(),
                device: device_name(&asked),
                at: Instant::now(),
            },
        );
    }
    let separator = if asked.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    let to = format!(
        "{}{separator}{}",
        asked.redirect_uri,
        form_urlencoded::Serializer::new(String::new())
            .append_pair("code", &code)
            .append_pair("state", &asked.state)
            .finish()
    );
    Redirect::to(&to).into_response()
}

#[derive(Debug, Deserialize)]
pub struct Exchange {
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

/// `POST /auth/desktop/token`: the program's code and verifier for a device session.
pub async fn token(State(kernel): State<Kernel>, Json(asked): Json<Exchange>) -> Response {
    let refused = |why: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_grant", "error_description": why})),
        )
            .into_response()
    };
    let granted = kernel
        .facility::<Codes>()
        .0
        .lock()
        .unwrap()
        .remove(&asked.code);
    let Some(granted) = granted else {
        return refused("the code is unknown or used");
    };
    if granted.at.elapsed() >= CODE_FOR {
        return refused("the code expired");
    }
    if granted.redirect_uri != asked.redirect_uri
        || !same(&challenge(&asked.code_verifier), &granted.challenge)
    {
        return refused("the verifier or the address does not match");
    }
    let device = Device {
        label: Some(granted.device.clone()),
        user_agent: Some(granted.device),
    };
    match start_session(&kernel, granted.user, &device).await {
        Ok(signed) => (
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "token": signed.token,
                "user": crate::session::wire(&signed.user),
            })),
        )
            .into_response(),
        Err(e) => refused(&e.message),
    }
}

/// Whether two texts are the same, in a time that does not say how much of them is.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |differ, (x, y)| differ | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::loopback;

    #[test]
    fn only_this_machine_takes_the_code() {
        assert!(loopback("http://127.0.0.1:43123/callback"));
        assert!(loopback("http://[::1]:43123/"));
        assert!(loopback("http://localhost:8080"));
        for wrong in [
            "https://127.0.0.1:43123/",
            "http://127.0.0.1/",
            "http://192.0.2.10:43123/",
            "http://127.0.0.1.example.test:80/",
            "http://localhost:/",
        ] {
            assert!(!loopback(wrong), "{wrong}");
        }
    }
}

