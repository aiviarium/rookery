//! What the kernel is told at start-up. `rookery-server` reads it from `ROOKERY_*`.

use rookery_proto::auth::AuthMode;

#[derive(Debug, Clone)]
pub struct Config {
    /// The address clients use, without a trailing slash: `ROOKERY_PUBLIC_URL`, or the listen address.
    pub public_url: String,
    /// `ROOKERY_AUTH`.
    pub auth: AuthMode,
    /// `ROOKERY_ADMINS`: emails, and `group:NAME` for a group of the identity provider.
    pub admins: Vec<String>,
    /// The identity provider, when `auth` is `oidc`.
    pub oidc: Option<Oidc>,
    /// How long a call may take before it ends with `TIMEOUT`.
    pub call_timeout: std::time::Duration,
}

/// `ROOKERY_OIDC_*`.
#[derive(Debug, Clone, PartialEq)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    /// For a confidential client; a public one signs in with PKCE alone.
    pub client_secret: Option<String>,
    /// Audiences an ID token may name besides the client id.
    pub audiences: Vec<String>,
    /// The claim that lists a person's groups.
    pub groups_claim: String,
}

impl Config {
    pub fn new(public_url: impl Into<String>) -> Config {
        Config {
            public_url: public_url.into().trim_end_matches('/').to_owned(),
            auth: AuthMode::Local,
            admins: Vec::new(),
            oidc: None,
            call_timeout: std::time::Duration::from_secs(120),
        }
    }

    /// Whether `ROOKERY_ADMINS` names this email, or one of these groups.
    pub fn names_admin(&self, email: Option<&str>, groups: &[String]) -> bool {
        self.admins
            .iter()
            .any(|admin| match admin.strip_prefix("group:") {
                Some(group) => groups.iter().any(|g| g == group),
                None => email.is_some_and(|email| email.eq_ignore_ascii_case(admin)),
            })
    }

    pub fn is_secure(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}
