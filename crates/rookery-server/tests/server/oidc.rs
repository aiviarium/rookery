//! Signing in through an identity provider: a made-up one in the test, with the code flow and PKCE.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2_free::sha256;

use crate::common::{http, set_cookie};
use crate::served;

/// SHA-256 from ring, which the test signs with anyway.
mod sha2_free {
    pub fn sha256(data: &[u8]) -> Vec<u8> {
        ring::digest::digest(&ring::digest::SHA256, data)
            .as_ref()
            .to_vec()
    }
}

#[derive(Clone)]
struct Provider {
    issuer: String,
    key: Arc<jsonwebtoken::EncodingKey>,
    public: Vec<u8>,
    /// code → (nonce, challenge)
    codes: Arc<Mutex<HashMap<String, (String, String)>>>,
    claims: Value,
}

async fn discovery(State(p): State<Provider>) -> axum::Json<Value> {
    axum::Json(json!({
        "issuer": p.issuer,
        "authorization_endpoint": format!("{}/authorize", p.issuer),
        "token_endpoint": format!("{}/token", p.issuer),
        "jwks_uri": format!("{}/jwks", p.issuer),
    }))
}

async fn jwks(State(p): State<Provider>) -> axum::Json<Value> {
    axum::Json(
        json!({"keys": [{"kty": "OKP", "crv": "Ed25519", "kid": "k1", "alg": "EdDSA", "x": URL_SAFE_NO_PAD.encode(&p.public)}]}),
    )
}

/// Says yes at once, as if the person had signed in.
async fn authorize(
    State(p): State<Provider>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["code_challenge_method"], "S256");
    let code = format!("code-{}", q["state"]);
    p.codes.lock().unwrap().insert(
        code.clone(),
        (q["nonce"].clone(), q["code_challenge"].clone()),
    );
    Redirect::to(&format!(
        "{}?code={code}&state={}",
        q["redirect_uri"], q["state"]
    ))
}

async fn token(State(p): State<Provider>, body: Bytes) -> impl IntoResponse {
    let f: HashMap<String, String> = form_urlencoded::parse(&body).into_owned().collect();
    let Some((nonce, challenge)) = p.codes.lock().unwrap().remove(&f["code"]) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    };
    if URL_SAFE_NO_PAD.encode(sha256(f["code_verifier"].as_bytes())) != challenge {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant", "error_description": "PKCE"})),
        )
            .into_response();
    }
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
    header.kid = Some("k1".into());
    let now = jsonwebtoken::get_current_timestamp();
    let mut claims = json!({"iss": p.issuer, "aud": f["client_id"], "sub": "maria-1", "exp": now + 300, "iat": now, "nonce": nonce});
    for (k, v) in p.claims.as_object().unwrap() {
        claims[k] = v.clone();
    }
    let id_token = jsonwebtoken::encode(&header, &claims, &p.key).unwrap();
    axum::Json(json!({"access_token": "made-up", "token_type": "Bearer", "id_token": id_token}))
        .into_response()
}

async fn provider(claims: Value) -> String {
    use ring::signature::KeyPair;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let random = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&random).unwrap();
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let state = Provider {
        issuer: issuer.clone(),
        key: Arc::new(jsonwebtoken::EncodingKey::from_ed_der(pkcs8.as_ref())),
        public: pair.public_key().as_ref().to_vec(),
        codes: Arc::default(),
        claims,
    };
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    issuer
}

fn location(headers: &[(String, String)]) -> String {
    crate::common::header(headers, "location")[0].to_owned()
}

#[tokio::test]
async fn a_person_signs_in_through_the_provider() {
    let issuer =
        provider(json!({"email": "maria@example.test", "name": "Maria", "groups": ["platform"]}))
            .await;
    let served = served!(move |config: &mut rookery_server::ServerConfig| {
        config.auth = rookery_proto::auth::AuthMode::Oidc;
        config.admins = vec!["group:platform".into()];
        config.oidc = Some(rookery_kernel::config::Oidc {
            issuer,
            client_id: "rookery".into(),
            client_secret: None,
            audiences: vec![],
            groups_claim: "groups".into(),
        });
    });
    let (status, body, _) = http("GET", &format!("{}/auth/session", served.url), &[], None).await;
    assert_eq!(status, 401);
    let signed_out: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (
            signed_out["error"]["code"].as_str(),
            signed_out["local"].as_bool()
        ),
        (Some("UNAUTHENTICATED"), Some(false))
    );
    assert_eq!(signed_out["sso"]["ready"], true);
    assert!(
        signed_out["sso"]["name"]
            .as_str()
            .unwrap()
            .starts_with("127.0.0.1:")
    );

    let platform = served
        .db
        .store
        .access()
        .create_group("Platform", Some("platform"))
        .await
        .unwrap();
    let other = served
        .db
        .store
        .access()
        .create_group("Other", Some("other"))
        .await
        .unwrap();
    let (status, _, headers) = http(
        "GET",
        &format!("{}/auth/login?return_to=/hosts/studio", served.url),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 303);
    let state = set_cookie(&headers, "rookery_oidc").expect("the state cookie");
    let (status, _, headers) = http("GET", &location(&headers), &[], None).await;
    assert_eq!(status, 303);
    let callback = location(&headers);
    assert!(
        callback.starts_with(&format!("{}/auth/callback?", served.url)),
        "{callback}"
    );

    // Without the browser's state cookie the callback is refused.
    let (status, _, _) = http("GET", &callback, &[], None).await;
    assert_eq!(status, 400);
    let (status, page, headers) = http("GET", &callback, &[("cookie", &state)], None).await;
    assert_eq!(status, 303, "{page}");
    assert_eq!(location(&headers), "/hosts/studio");
    let session = set_cookie(&headers, "rookery_session").expect("a session");
    let (_, body, _) = http(
        "GET",
        &format!("{}/auth/session", served.url),
        &[("cookie", &session)],
        None,
    )
    .await;
    let user = serde_json::from_str::<Value>(&body).unwrap()["user"].clone();
    assert_eq!(
        (
            user["name"].as_str(),
            user["email"].as_str(),
            user["admin"].as_bool()
        ),
        (Some("Maria"), Some("maria@example.test"), Some(true))
    );
    // The groups bound to the provider's claims have the person, and only those it names.
    let id: uuid::Uuid = user["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        served.db.store.access().members(platform.id).await.unwrap(),
        [id]
    );
    assert!(
        served
            .db
            .store
            .access()
            .members(other.id)
            .await
            .unwrap()
            .is_empty()
    );
    // The code was for one sign-in.
    let (status, _, _) = http("GET", &callback, &[("cookie", &state)], None).await;
    assert_eq!(status, 400);
    // Links are off with an identity provider.
    let refused = served
        .rpc(
            session.trim_start_matches("rookery_session="),
            "auth.createLink",
            json!({"kind": "device"}),
        )
        .await
        .unwrap_err();
    assert_eq!(crate::common::code(&refused), "UNAVAILABLE");
    served.stop().await;
}

#[tokio::test]
async fn a_token_for_another_audience_is_refused() {
    // An extra trusted audience is allowed alongside the client ID.
    let issuer = provider(json!({"aud": "other-service", "email": "maria@example.test"})).await;
    let served = served!(move |config: &mut rookery_server::ServerConfig| {
        config.auth = rookery_proto::auth::AuthMode::Oidc;
        config.oidc = Some(rookery_kernel::config::Oidc {
            issuer,
            client_id: "rookery".into(),
            client_secret: None,
            audiences: vec!["other-service".into()],
            groups_claim: "groups".into(),
        });
    });
    let (_, _, headers) = http("GET", &format!("{}/auth/login", served.url), &[], None).await;
    let state = set_cookie(&headers, "rookery_oidc").unwrap();
    let (_, _, headers) = http("GET", &location(&headers), &[], None).await;
    let (status, page, headers) =
        http("GET", &location(&headers), &[("cookie", &state)], None).await;
    assert_eq!(status, 403, "{page}");
    assert!(set_cookie(&headers, "rookery_session").is_none());
    served.stop().await;
}

#[tokio::test]
async fn email_must_be_verified_for_an_admin_role() {
    for verified in [None, Some(false), Some(true)] {
        let mut claims = json!({"email": "maria@example.test"});
        if let Some(verified) = verified {
            claims["email_verified"] = json!(verified);
        }
        let issuer = provider(claims).await;
        let served = served!(move |config: &mut rookery_server::ServerConfig| {
            config.auth = rookery_proto::auth::AuthMode::Oidc;
            config.admins = vec!["maria@example.test".into()];
            config.oidc = Some(rookery_kernel::config::Oidc {
                issuer,
                client_id: "rookery".into(),
                client_secret: None,
                audiences: vec![],
                groups_claim: "groups".into(),
            });
        });
        let (_, _, headers) = http("GET", &format!("{}/auth/login", served.url), &[], None).await;
        let state = set_cookie(&headers, "rookery_oidc").unwrap();
        let (_, _, headers) = http("GET", &location(&headers), &[], None).await;
        let (status, _, headers) =
            http("GET", &location(&headers), &[("cookie", &state)], None).await;
        assert_eq!(status, 303);
        let cookie = set_cookie(&headers, "rookery_session").unwrap();
        let (_, body, _) = http(
            "GET",
            &format!("{}/auth/session", served.url),
            &[("cookie", &cookie)],
            None,
        )
        .await;
        let user = &serde_json::from_str::<Value>(&body).unwrap()["user"];
        let is_admin = user["admin"].as_bool();
        served.stop().await;
        assert_eq!(is_admin, Some(verified == Some(true)));
    }
}
