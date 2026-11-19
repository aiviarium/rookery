//! Device sessions: starting one, the cookie, `GET /auth/session` and signing out.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use rookery_kernel::auth::{SESSION_COOKIE, authenticate, bearer, cookie, wire_user};
use rookery_kernel::{Caller, Kernel, RpcError};
use rookery_proto::auth::{AuthMode, SignedOut};
use rookery_store::users::{Login, User};
use uuid::Uuid;

/// How long a device session lasts. The cookie's `Max-Age` is the same.
pub const SESSION_DAYS: i64 = 30;

/// The device a session starts on, for the list of devices.
#[derive(Debug, Clone, Default)]
pub struct Device {
    pub label: Option<String>,
    pub user_agent: Option<String>,
}

impl Device {
    pub fn from_headers(headers: &HeaderMap) -> Device {
        let user_agent = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|ua| ua.chars().take(300).collect::<String>());
        Device {
            label: user_agent.as_deref().map(device_label),
            user_agent,
        }
    }
}

/// A short name for a device from its user agent, such as "Firefox on Linux".
pub fn device_label(user_agent: &str) -> String {
    let browser = [
        ("Edg/", "Edge"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("Safari/", "Safari"),
        ("curl/", "curl"),
        ("rookery/", "rookery"),
    ]
    .iter()
    .find(|(k, _)| user_agent.contains(k))
    .map(|(_, v)| *v);
    let system = [
        ("Android", "Android"),
        ("iPhone", "iOS"),
        ("iPad", "iPadOS"),
        ("Mac OS X", "macOS"),
        ("Windows", "Windows"),
        ("Linux", "Linux"),
    ]
    .iter()
    .find(|(k, _)| user_agent.contains(k))
    .map(|(_, v)| *v);
    match (browser, system) {
        (Some(b), Some(s)) => format!("{b} on {s}"),
        (Some(b), None) => b.to_owned(),
        (None, Some(s)) => s.to_owned(),
        (None, None) => "Unknown device".into(),
    }
}

/// A started session: its token, shown once, and who it is for.
pub struct SignedIn {
    pub token: String,
    pub user: User,
}

/// Signs in a person an identity provider vouched for: makes them on their first sign-in, brings their
/// name and email up to date, makes them an admin when `ROOKERY_ADMINS` names them, and starts a session.
pub async fn sign_in(kernel: &Kernel, login: Login, device: &Device) -> Result<SignedIn, RpcError> {
    let (user, new) = kernel.store.users().upsert_login(&login).await?;
    if user.is_disabled() {
        return Err(RpcError::forbidden("this account is disabled"));
    }
    if new {
        let details = serde_json::json!({ "issuer": user.issuer });
        let _ = kernel
            .store
            .audit()
            .record(
                &format!("user:{}", user.id),
                "user.create",
                Some(&user_uri(user.id)),
                details,
            )
            .await;
    }
    session_for(kernel, user, device).await
}

/// Starts a session for a person who is known.
pub async fn start_session(
    kernel: &Kernel,
    user: Uuid,
    device: &Device,
) -> Result<SignedIn, RpcError> {
    let user = kernel
        .store
        .users()
        .get(user)
        .await?
        .ok_or_else(|| RpcError::not_found("no such user"))?;
    if user.is_disabled() {
        return Err(RpcError::forbidden("this account is disabled"));
    }
    session_for(kernel, user, device).await
}

async fn session_for(kernel: &Kernel, user: User, device: &Device) -> Result<SignedIn, RpcError> {
    let expires = Utc::now() + chrono::Duration::days(SESSION_DAYS);
    let (token, _) = kernel
        .store
        .auth()
        .create_session(
            user.id,
            device.label.as_deref(),
            device.user_agent.as_deref(),
            Some(expires),
        )
        .await?;
    Ok(SignedIn { token, user })
}

pub fn user_uri(id: Uuid) -> String {
    format!("user://{id}")
}

/// The `Set-Cookie` value of a session: `Secure` when the server is reached over https.
pub fn session_cookie(kernel: &Kernel, token: &str) -> String {
    let secure = if kernel.config.is_secure() {
        "; Secure"
    } else {
        ""
    };
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        SESSION_DAYS * 86400
    )
}

pub fn cleared_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// Whether a browser's request comes from a page of this server: one with no `Origin` does, as does one
/// whose `Origin` is the public address or the `Host` it asked.
pub fn same_origin(kernel: &Kernel, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if origin == kernel.config.public_url {
        return true;
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    origin
        .split_once("://")
        .is_some_and(|(_, rest)| rest == host)
}

/// `GET /auth/session`: who the browser is signed in as, or how it can sign in.
pub async fn current(State(kernel): State<Kernel>, headers: HeaderMap) -> Response {
    let token = cookie(&headers).or_else(|| bearer(&headers));
    if let Some(token) = token
        && let Some(Caller::User { user, .. }) = authenticate(&kernel, &token).await
    {
        return Json(serde_json::json!({ "user": user })).into_response();
    }
    let login_url = (kernel.config.auth == AuthMode::Oidc).then(|| "/auth/login".to_owned());
    (
        StatusCode::UNAUTHORIZED,
        Json(SignedOut {
            auth: kernel.config.auth,
            login_url,
        }),
    )
        .into_response()
}

/// `POST /auth/logout`: ends the session and clears the cookie.
pub async fn logout(State(kernel): State<Kernel>, headers: HeaderMap) -> Response {
    if !same_origin(&kernel, &headers) {
        return (
            StatusCode::FORBIDDEN,
            "signing out is for pages of this server",
        )
            .into_response();
    }
    if let Some(token) = cookie(&headers).or_else(|| bearer(&headers)) {
        match kernel.store.auth().revoke_session_token(&token).await {
            Ok(Some(session)) => kernel.conns.revoke_session(session),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "signing out failed"),
        }
    }
    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, cleared_cookie())],
    )
        .into_response()
}

/// The wire's user of a store user.
pub fn wire(user: &User) -> rookery_proto::User {
    wire_user(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_labels() {
        assert_eq!(
            device_label("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"),
            "Firefox on Linux"
        );
        assert_eq!(device_label("curl/8.5.0"), "curl");
        assert_eq!(device_label(""), "Unknown device");
    }
}
