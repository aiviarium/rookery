//! Signing in to `rookery serve`.
//!
//! - **Local accounts**, the default: one-time sign-in links and no passwords. `rookery server setup-key`
//!   makes the first admin's link over the Unix socket; admins invite others, and anyone makes a link to
//!   sign themselves in on another device.
//! - **OpenID Connect** with `ROOKERY_AUTH=oidc`: the code flow with PKCE. `ROOKERY_ADMINS` names admins by
//!   email or `group:NAME`. Links are off.
//! - Either ends in a **device session**: a random token in the `HttpOnly` cookie `rookery_session`, kept
//!   hashed. **Access tokens** (`rookery_pat_…`) are for scripts.

pub mod links;
pub mod methods;
pub mod oidc;
pub mod pages;
pub mod session;

use axum::Router;
use axum::routing::{get, post};
use rookery_kernel::{Kernel, Methods};

pub fn register(methods: &mut Methods) {
    methods::register(methods);
}

/// `/auth/*`: the session, signing out, link redemption and the OIDC flow.
pub fn routes() -> Router<Kernel> {
    Router::new()
        .route("/auth/session", get(session::current))
        .route("/auth/logout", post(session::logout))
        .route(
            "/auth/link/{code}",
            get(links::show).post(links::redeem_route),
        )
        .route("/auth/login", get(oidc::login))
        .route("/auth/callback", get(oidc::callback))
}
