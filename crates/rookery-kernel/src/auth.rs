//! Who a request is from: a device session or an access token, read from the store.

use std::time::Duration;

use rookery_store::auth::{SESSION_PREFIX, TOKEN_PREFIX};

use crate::kernel::Kernel;
use crate::methods::Caller;

/// The cookie that carries a browser's device session.
pub const SESSION_COOKIE: &str = "rookery_session";

/// A store user as the wire has it.
pub fn wire_user(user: &rookery_store::users::User) -> rookery_proto::User {
    rookery_proto::User {
        id: user.id.to_string(),
        name: user.name.clone(),
        email: user.email.clone(),
        admin: user.is_admin(),
    }
}

/// The caller a token signs in as, or `None` when it does not work.
pub async fn authenticate(kernel: &Kernel, token: &str) -> Option<Caller> {
    authenticate_with_expiry(kernel, token)
        .await
        .map(|(caller, _)| caller)
}

pub(crate) async fn authenticate_with_expiry(
    kernel: &Kernel,
    token: &str,
) -> Option<(Caller, Option<Duration>)> {
    let token = token.trim();
    if token.starts_with(TOKEN_PREFIX) {
        return match kernel.store.auth().resolve_token(token).await {
            Ok(found) => found.and_then(|(record, user)| {
                (!user.is_disabled()).then(|| {
                    (
                        Caller::User {
                            user: wire_user(&user),
                            session: None,
                            token: Some(record.id),
                        },
                        record.remaining(),
                    )
                })
            }),
            Err(e) => {
                tracing::warn!(error = %e, "reading an access token failed");
                None
            }
        };
    }
    if token.starts_with(SESSION_PREFIX) {
        return match kernel.store.auth().resolve_session(token).await {
            Ok(found) => found.and_then(|(session, user)| {
                (!user.is_disabled()).then(|| {
                    (
                        Caller::User {
                            user: wire_user(&user),
                            session: Some(session.id),
                            token: None,
                        },
                        session.remaining(),
                    )
                })
            }),
            Err(e) => {
                tracing::warn!(error = %e, "reading a device session failed");
                None
            }
        };
    }
    None
}

/// The token of an `Authorization: Bearer` header.
pub fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    value.strip_prefix("Bearer ").map(|t| t.trim().to_owned())
}

/// The device session in the cookie.
pub fn cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, header};

    #[test]
    fn tokens_from_the_header_and_the_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; rookery_session=rookery_ses_abc; b=2"),
        );
        assert_eq!(cookie(&headers).as_deref(), Some("rookery_ses_abc"));
        assert_eq!(bearer(&headers), None);
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer rookery_pat_xyz"),
        );
        assert_eq!(bearer(&headers).as_deref(), Some("rookery_pat_xyz"));
    }
}
