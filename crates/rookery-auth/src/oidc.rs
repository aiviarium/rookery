//! OpenID Connect: the authorization code flow with PKCE.
//!
//! `GET /auth/login` sends the browser to the identity provider with a state, a nonce and a PKCE
//! challenge, and binds the sign-in to the browser with the cookie `rookery_oidc`. `GET /auth/callback`
//! trades the code for tokens, checks the ID token's signature, issuer, audience, expiry and nonce, signs
//! the person in and goes back to where they started. The provider's metadata and keys are cached, and
//! the keys fetched again when a token names one that is not known.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use rand::RngCore;
use rookery_kernel::Kernel;
use rookery_kernel::config::Oidc;
use rookery_proto::auth::AuthMode;
use rookery_store::users::Login;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::pages::problem;
use crate::session::{Device, session_cookie, sign_in};

/// The cookie that ties a callback to the browser that started the sign-in.
pub const STATE_COOKIE: &str = "rookery_oidc";
/// How long a sign-in may take.
const PENDING_FOR: Duration = Duration::from_secs(600);
/// Sign-ins kept at once; more are refused, so that nobody can fill the memory.
const MAX_PENDING: usize = 2000;
/// How long the metadata and keys are kept.
const REFRESH_EVERY: Duration = Duration::from_secs(3600);
/// A token with an unknown key fetches the keys at most this often.
const MIN_REFRESH: Duration = Duration::from_secs(10);
const ALGORITHMS: [Algorithm; 7] = [
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

/// The provider's metadata, as discovery answers it.
#[derive(Debug, Clone, Deserialize)]
pub struct Metadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

struct Pending {
    verifier: String,
    nonce: String,
    back_to: String,
    started: Instant,
}

struct Known {
    metadata: Metadata,
    keys: JwkSet,
    fetched: Instant,
}

/// The kernel's OIDC state: what it knows of the provider, and the sign-ins under way.
#[derive(Default)]
pub struct OidcState {
    known: tokio::sync::Mutex<Option<Known>>,
    pending: Mutex<HashMap<String, Pending>>,
}

fn random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// The PKCE challenge of a verifier (S256).
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

async fn get_json(url: String) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let mut response = ureq::get(&url).call().map_err(|e| format!("{url}: {e}"))?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{url}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn post_form(url: String, form: Vec<(&'static str, String)>) -> Result<Value, String> {
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .build()
            .into();
        let mut response = agent
            .post(&url)
            .send_form(form.iter().map(|(k, v)| (*k, v.as_str())))
            .map_err(|e| format!("{url}: {e}"))?;
        let status = response.status();
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{url}: {e}"))?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|_| format!("the token endpoint answered {status} with invalid JSON"))?;
        if !status.is_success() {
            return Err(format!("the token endpoint answered {status}"));
        }
        Ok(body)
    })
    .await
    .map_err(|e| e.to_string())?
}

impl OidcState {
    /// The metadata and keys, fetched when they are not known or are old, or when `refresh` asks.
    async fn known(&self, config: &Oidc, refresh: bool) -> Result<(Metadata, JwkSet), String> {
        let mut known = self.known.lock().await;
        let fresh = known.as_ref().is_some_and(|k| {
            let age = k.fetched.elapsed();
            age < REFRESH_EVERY && (!refresh || age < MIN_REFRESH)
        });
        if !fresh {
            let url = format!(
                "{}/.well-known/openid-configuration",
                config.issuer.trim_end_matches('/')
            );
            let metadata: Metadata = serde_json::from_value(get_json(url).await?)
                .map_err(|e| format!("the provider's metadata: {e}"))?;
            if metadata.issuer.trim_end_matches('/') != config.issuer.trim_end_matches('/') {
                return Err(format!(
                    "the provider says its issuer is {}, not {}",
                    metadata.issuer, config.issuer
                ));
            }
            let keys: JwkSet = serde_json::from_value(get_json(metadata.jwks_uri.clone()).await?)
                .map_err(|e| format!("the provider's keys: {e}"))?;
            *known = Some(Known {
                metadata,
                keys,
                fetched: Instant::now(),
            });
        }
        let k = known.as_ref().expect("known");
        Ok((k.metadata.clone(), k.keys.clone()))
    }

    fn remember(&self, state: String, pending: Pending) -> Result<(), String> {
        let mut map = self.pending.lock().unwrap();
        map.retain(|_, p| p.started.elapsed() < PENDING_FOR);
        if map.len() >= MAX_PENDING {
            return Err("too many sign-ins are under way; try again in a few minutes".into());
        }
        map.insert(state, pending);
        Ok(())
    }

    fn take(&self, state: &str) -> Option<Pending> {
        let pending = self.pending.lock().unwrap().remove(state)?;
        (pending.started.elapsed() < PENDING_FOR).then_some(pending)
    }
}

/// What "Sign in with …" names: the issuer's host.
pub fn display_name(config: &Oidc) -> String {
    let rest = config
        .issuer
        .split_once("://")
        .map_or(config.issuer.as_str(), |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_owned()
}

/// Whether the provider's metadata and keys can be had.
pub async fn ready(kernel: &Kernel) -> bool {
    match configured(kernel) {
        Some(config) => kernel
            .facility::<OidcState>()
            .known(config, false)
            .await
            .is_ok(),
        None => false,
    }
}

pub fn configured(kernel: &Kernel) -> Option<&Oidc> {
    config(kernel)
}

fn config(kernel: &Kernel) -> Option<&Oidc> {
    match kernel.config.auth {
        AuthMode::Oidc => kernel.config.oidc.as_ref(),
        AuthMode::Local => None,
    }
}

fn redirect_uri(kernel: &Kernel) -> String {
    format!("{}/auth/callback", kernel.config.public_url)
}

/// Only a path on this server, so that a sign-in cannot be made to end on another site.
fn safe_path(path: Option<&str>) -> String {
    match path {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && !p.chars().any(char::is_control) =>
        {
            p.to_owned()
        }
        _ => "/".into(),
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    #[serde(default, rename = "return_to", alias = "return")]
    back_to: Option<String>,
}

/// `GET /auth/login`.
pub async fn login(State(kernel): State<Kernel>, Query(query): Query<LoginQuery>) -> Response {
    let Some(config) = config(&kernel) else {
        return problem(
            StatusCode::NOT_FOUND,
            "No identity provider",
            "This server signs in with links, not an identity provider.",
        );
    };
    let state = kernel.facility::<OidcState>();
    let (metadata, _) = match state.known(config, false).await {
        Ok(known) => known,
        Err(e) => {
            return problem(
                StatusCode::BAD_GATEWAY,
                "The identity provider does not answer",
                &e,
            );
        }
    };
    let verifier = random(32);
    let nonce = random(16);
    let key = random(16);
    let pending = Pending {
        verifier: verifier.clone(),
        nonce: nonce.clone(),
        back_to: safe_path(query.back_to.as_deref()),
        started: Instant::now(),
    };
    if let Err(e) = state.remember(key.clone(), pending) {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "Sign-in is busy", &e);
    }
    let query: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &redirect_uri(&kernel))
        .append_pair("scope", "openid profile email")
        .append_pair("state", &key)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .finish();
    let separator = if metadata.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let to = format!("{}{separator}{query}", metadata.authorization_endpoint);
    let secure = if kernel.config.is_secure() {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{STATE_COOKIE}={key}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        PENDING_FOR.as_secs()
    );
    ([(header::SET_COOKIE, cookie)], Redirect::to(&to)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn state_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == STATE_COOKIE)
        .map(|(_, value)| value.to_owned())
}

/// The groups in a claim: a list of names, or names in one string apart by spaces or commas.
pub fn groups(claims: &Value, claim: &str) -> Vec<String> {
    let mut value = claims;
    for part in claim.split('.') {
        match value.get(part) {
            Some(v) => value = v,
            None => return Vec::new(),
        }
    }
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Value::String(text) => text
            .split([' ', ','])
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Checks an ID token and returns its claims.
pub async fn verify(
    kernel: &Kernel,
    config: &Oidc,
    token: &str,
    nonce: &str,
) -> Result<Value, String> {
    let header = jsonwebtoken::decode_header(token).map_err(|e| format!("the ID token: {e}"))?;
    if !ALGORITHMS.contains(&header.alg) {
        return Err("the ID token is signed in a way rookery does not take".into());
    }
    let state = kernel.facility::<OidcState>();
    let find = |keys: &JwkSet| match header.kid.as_deref() {
        Some(kid) => keys.find(kid).cloned(),
        None if keys.keys.len() == 1 => keys.keys.first().cloned(),
        None => None,
    };
    let (metadata, keys) = state.known(config, false).await?;
    let key = match find(&keys) {
        Some(key) => key,
        None => find(&state.known(config, true).await?.1)
            .ok_or("the ID token is signed with a key the provider does not list")?,
    };
    let key = DecodingKey::from_jwk(&key).map_err(|e| format!("the provider's key: {e}"))?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[metadata.issuer.as_str()]);
    validation.set_audience(&[&config.client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.leeway = 60;
    let claims = jsonwebtoken::decode::<Value>(token, &key, &validation)
        .map_err(|e| format!("the ID token: {e}"))?
        .claims;
    if let Some(Value::Array(audiences)) = claims.get("aud")
        && audiences.iter().any(|audience| {
            !audience.as_str().is_some_and(|audience| {
                audience == config.client_id || config.audiences.iter().any(|a| a == audience)
            })
        })
    {
        return Err("the ID token names an untrusted audience".into());
    }
    if claims
        .get("azp")
        .is_some_and(|azp| azp.as_str() != Some(&config.client_id))
    {
        return Err("the ID token is authorized for a different client".into());
    }
    if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
        return Err("the ID token is not for this sign-in".into());
    }
    Ok(claims)
}

/// `GET /auth/callback`.
pub async fn callback(
    State(kernel): State<Kernel>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(config) = config(&kernel) else {
        return problem(
            StatusCode::NOT_FOUND,
            "No identity provider",
            "This server signs in with links, not an identity provider.",
        );
    };
    if let Some(error) = &query.error {
        let said = query.error_description.as_deref().unwrap_or(error);
        return problem(StatusCode::FORBIDDEN, "The identity provider said no", said);
    }
    let (Some(code), Some(key)) = (query.code.as_deref(), query.state.as_deref()) else {
        return problem(
            StatusCode::BAD_REQUEST,
            "Sign-in failed",
            "The callback has no code.",
        );
    };
    if state_cookie(&headers).as_deref() != Some(key) {
        return problem(
            StatusCode::BAD_REQUEST,
            "Sign-in failed",
            "This sign-in was started in another browser. Start it again here.",
        );
    }
    let state = kernel.facility::<OidcState>();
    let Some(pending) = state.take(key) else {
        return problem(
            StatusCode::BAD_REQUEST,
            "Sign-in failed",
            "This sign-in is unknown or took too long. Start it again.",
        );
    };
    let fail = |e: String| problem(StatusCode::BAD_GATEWAY, "Sign-in failed", &e);
    let (metadata, _) = match state.known(config, false).await {
        Ok(known) => known,
        Err(e) => return fail(e),
    };
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code.to_owned()),
        ("redirect_uri", redirect_uri(&kernel)),
        ("client_id", config.client_id.clone()),
        ("code_verifier", pending.verifier),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let tokens = match post_form(metadata.token_endpoint.clone(), form).await {
        Ok(tokens) => tokens,
        Err(e) => return fail(e),
    };
    let Some(id_token) = tokens.get("id_token").and_then(Value::as_str) else {
        return fail("the provider sent no ID token".into());
    };
    let claims = match verify(&kernel, config, id_token, &pending.nonce).await {
        Ok(claims) => claims,
        Err(e) => return problem(StatusCode::FORBIDDEN, "Sign-in failed", &e),
    };
    let text = |name: &str| claims.get(name).and_then(Value::as_str).map(str::to_owned);
    let subject = text("sub").unwrap_or_default();
    let email = text("email")
        .filter(|_| claims.get("email_verified").and_then(Value::as_bool) != Some(false));
    let verified_email = email
        .as_deref()
        .filter(|_| claims.get("email_verified").and_then(Value::as_bool) == Some(true));
    let groups = groups(&claims, &config.groups_claim);
    let name = text("name")
        .or_else(|| text("preferred_username"))
        .or_else(|| email.clone())
        .unwrap_or_else(|| subject.clone());
    let login = Login {
        issuer: config.issuer.clone(),
        subject,
        name,
        admin: kernel.config.names_admin(verified_email, &groups),
        email,
    };
    match sign_in(&kernel, login, &Device::from_headers(&headers)).await {
        Ok(signed) => {
            let cleared = format!("{STATE_COOKIE}=; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=0");
            let cookies = [
                (header::SET_COOKIE, session_cookie(&kernel, &signed.token)),
                (header::SET_COOKIE, cleared),
            ];
            (
                axum::response::AppendHeaders(cookies),
                Redirect::to(&pending.back_to),
            )
                .into_response()
        }
        Err(e) => problem(StatusCode::FORBIDDEN, "Sign-in failed", &e.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_endpoint_errors_do_not_expose_response_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let app = axum::Router::new().route(
            "/token",
            axum::routing::post(|| async {
                (StatusCode::BAD_GATEWAY, "private provider response")
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let error = post_form(endpoint, vec![]).await.unwrap_err();
        server.abort();
        assert!(!error.contains("private provider response"));
    }

    #[test]
    fn pkce_and_paths() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(safe_path(Some("/hosts/studio")), "/hosts/studio");
        assert_eq!(safe_path(Some("//evil.example.test")), "/");
        assert_eq!(safe_path(Some("https://evil.example.test")), "/");
        assert_eq!(safe_path(None), "/");
        assert_eq!(safe_path(Some("/a\nb")), "/");
        assert_eq!(safe_path(Some("/a\tb")), "/");
    }

    #[test]
    fn groups_from_lists_strings_and_paths() {
        let claims = serde_json::json!({"groups": ["ops", "dev"], "roles": "a b,c", "realm": {"groups": ["x"]}});
        assert_eq!(groups(&claims, "groups"), ["ops", "dev"]);
        assert_eq!(groups(&claims, "roles"), ["a", "b", "c"]);
        assert_eq!(groups(&claims, "realm.groups"), ["x"]);
        assert!(groups(&claims, "none").is_empty());
    }
}
