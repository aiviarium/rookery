//! One-time sign-in links of local accounts.
//!
//! `GET /auth/link/CODE` says what a link does and asks for a click, without using it, so that a link
//! preview in a chat leaves it alone. `POST` uses it up and starts a device session: a browser gets the
//! cookie and goes to `/`; a client that asks for JSON gets the session's token as well, to sign in with.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use rookery_kernel::{Kernel, RpcError};
use rookery_proto::auth::{AuthMode, LinkKind};
use rookery_store::links::Link;
use rookery_store::users::Login;
use uuid::Uuid;

use crate::pages::{escape, page, problem};
use crate::session::{Device, same_origin, session_cookie, start_session, user_uri};

/// The issuer of local accounts is this and the deployment id.
pub const ISSUER_PREFIX: &str = "rookery-local:";

pub fn link_url(kernel: &Kernel, code: &str) -> String {
    format!("{}/auth/link/{code}", kernel.config.public_url)
}

/// Why a link signed nobody in.
#[derive(Debug)]
pub enum Refused {
    /// The server signs in through an identity provider.
    Off,
    /// Unknown, used, expired or revoked.
    Gone,
    Disabled,
    Bad(String),
    Internal(String),
}

impl Refused {
    fn status(&self) -> StatusCode {
        match self {
            Refused::Off | Refused::Disabled => StatusCode::FORBIDDEN,
            Refused::Gone => StatusCode::GONE,
            Refused::Bad(_) => StatusCode::BAD_REQUEST,
            Refused::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn says(&self) -> (&'static str, String) {
        match self {
            Refused::Off => ("Sign-in links are off", "This server signs in through an identity provider.".into()),
            Refused::Gone => (
                "This sign-in link does not work",
                "It has expired, was used or was revoked. Ask an admin for a new one, or run rookery server setup-key on the server.".into(),
            ),
            Refused::Disabled => ("This account is disabled", "Ask an admin to turn it on again.".into()),
            Refused::Bad(m) => ("Sign-in failed", m.clone()),
            Refused::Internal(m) => ("Sign-in failed", m.clone()),
        }
    }

    fn page(&self) -> Response {
        let (title, message) = self.says();
        problem(self.status(), title, &message)
    }

    fn json(&self) -> Response {
        let (_, message) = self.says();
        let error = match self {
            Refused::Off | Refused::Disabled => RpcError::forbidden(message),
            Refused::Gone => RpcError::not_found(message),
            Refused::Bad(_) => RpcError::invalid(message),
            Refused::Internal(_) => RpcError::internal(message),
        };
        (
            self.status(),
            Json(serde_json::json!({ "error": error.body() })),
        )
            .into_response()
    }
}

fn internal(e: impl std::fmt::Display) -> Refused {
    Refused::Internal(e.to_string())
}

/// A name as typed: trimmed, 1 to 100 characters, no control characters.
pub fn clean_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty() && name.chars().count() <= 100 && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// The name a new account gets: what the person typed on a setup link, else what the link says.
fn account_name(link: &Link, typed: Option<&str>) -> Result<String, Refused> {
    let typed = typed.map(str::trim).filter(|n| !n.is_empty());
    if link.kind() == LinkKind::Setup
        && let Some(typed) = typed
    {
        return clean_name(typed)
            .ok_or_else(|| Refused::Bad("A name is 1 to 100 characters.".into()));
    }
    Ok(match (link.name.as_deref(), link.kind()) {
        (Some(name), _) => name.to_owned(),
        (None, LinkKind::Setup) => "Admin".into(),
        (None, _) => "New user".into(),
    })
}

/// Uses a link up: the user it signs in, made first for a setup or an invite link.
pub async fn redeem(
    kernel: &Kernel,
    code: &str,
    typed_name: Option<&str>,
) -> Result<Uuid, Refused> {
    if kernel.config.auth != AuthMode::Local {
        return Err(Refused::Off);
    }
    let links = kernel.store.links();
    let Some(peeked) = links.peek(code).await.map_err(internal)? else {
        return Err(Refused::Gone);
    };
    // What can be wrong is checked before the link is spent.
    let name = match peeked.kind() {
        LinkKind::Device => None,
        _ => Some(account_name(&peeked, typed_name)?),
    };
    let (user, created) = if peeked.kind() == LinkKind::Setup {
        let deployment = kernel
            .store
            .state()
            .deployment_id()
            .await
            .map_err(internal)?;
        let subject = rookery_store::util::random_token("");
        let user = links
            .consume_setup(
                code,
                &format!("{ISSUER_PREFIX}{deployment}"),
                &subject,
                name.as_deref().unwrap_or_default(),
            )
            .await
            .map_err(internal)?
            .ok_or(Refused::Gone)?;
        (user, true)
    } else {
        let link = links
            .consume(code)
            .await
            .map_err(internal)?
            .ok_or(Refused::Gone)?;
        match link.kind() {
            LinkKind::Device => {
                let user = match link.user_id {
                    Some(id) => kernel.store.users().get(id).await.map_err(internal)?,
                    None => None,
                };
                match user {
                    Some(user) if !user.is_disabled() => (user.id, false),
                    Some(_) => return Err(Refused::Disabled),
                    None => return Err(Refused::Gone),
                }
            }
            _ => {
                let deployment = kernel
                    .store
                    .state()
                    .deployment_id()
                    .await
                    .map_err(internal)?;
                let login = Login {
                    issuer: format!("{ISSUER_PREFIX}{deployment}"),
                    subject: rookery_store::util::random_token(""),
                    name: name.unwrap_or_default(),
                    email: link.email.clone(),
                    admin: false,
                };
                let (user, _) = kernel
                    .store
                    .users()
                    .upsert_login(&login)
                    .await
                    .map_err(internal)?;
                (user.id, true)
            }
        }
    };
    let details = serde_json::json!({ "kind": rookery_store::links::kind_name(peeked.kind()), "link": peeked.id, "created": created });
    let _ = kernel
        .store
        .audit()
        .record(
            &format!("user:{user}"),
            "link.signin",
            Some(&user_uri(user)),
            details,
        )
        .await;
    Ok(user)
}

/// `GET /auth/link/CODE`.
pub async fn show(State(kernel): State<Kernel>, Path(code): Path<String>) -> Response {
    if kernel.config.auth != AuthMode::Local {
        return Refused::Off.page();
    }
    let link = match kernel.store.links().peek(&code).await {
        Ok(Some(link)) => link,
        Ok(None) => return Refused::Gone.page(),
        Err(e) => return internal(e).page(),
    };
    let says = match link.kind() {
        LinkKind::Setup => "This makes you the first admin of this server.".to_owned(),
        LinkKind::Invite => format!(
            "You are invited to this server as {}.",
            escape(link.name.as_deref().unwrap_or("a new user"))
        ),
        LinkKind::Device => {
            let who = match link.user_id {
                Some(id) => kernel
                    .store
                    .users()
                    .get(id)
                    .await
                    .ok()
                    .flatten()
                    .map(|u| u.name),
                None => None,
            };
            format!(
                "This signs {} in on this device.",
                escape(&who.unwrap_or_else(|| "you".into()))
            )
        }
    };
    let name = match link.kind() {
        LinkKind::Setup => {
            r#"<label for="name">Your name</label><input id="name" name="name" maxlength="100" autocomplete="name">"#
        }
        _ => "",
    };
    let body = format!(
        r#"<p>{says}</p><form method="post" action="/auth/link/{}">{name}<button type="submit">Sign in</button></form>"#,
        escape(&code)
    );
    page(StatusCode::OK, "Sign in to this server", &body)
}

fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains("application/json"))
}

/// `POST /auth/link/CODE`, with the name of a setup link's admin as a form field or in JSON.
pub async fn redeem_route(
    State(kernel): State<Kernel>,
    Path(code): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let json = wants_json(&headers);
    let refuse = |refused: Refused| if json { refused.json() } else { refused.page() };
    if !same_origin(&kernel, &headers) {
        return refuse(Refused::Bad(
            "A link is used from a page of this server.".into(),
        ));
    }
    let is_json_body = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("application/json"));
    let name = if is_json_body {
        serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_owned))
    } else {
        form_urlencoded::parse(&body)
            .find(|(k, _)| k == "name")
            .map(|(_, v)| v.into_owned())
    };
    let user = match redeem(&kernel, &code, name.as_deref()).await {
        Ok(user) => user,
        Err(refused) => return refuse(refused),
    };
    let signed = match start_session(&kernel, user, &Device::from_headers(&headers)).await {
        Ok(signed) => signed,
        Err(e) => return refuse(Refused::Internal(e.message)),
    };
    let cookie = [
        (header::SET_COOKIE, session_cookie(&kernel, &signed.token)),
        (header::CACHE_CONTROL, "no-store".into()),
    ];
    if json {
        let body = serde_json::json!({ "token": signed.token, "user": crate::session::wire(&signed.user) });
        (cookie, Json(body)).into_response()
    } else {
        (cookie, Redirect::to("/")).into_response()
    }
}
