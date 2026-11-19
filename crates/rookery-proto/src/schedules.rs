//! Schedules and webhooks: what enqueues jobs besides a call. A schedule enqueues a job by cron
//! expression or interval; a delivery to `/hooks/<plugin>/<name>` enqueues the job of the webhook a
//! plugin declares, once it is verified.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A schedule that runs as the plugin's service principal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScheduleContribution {
    /// `<plugin>.<name>`.
    pub id: String,
    /// The job it enqueues, one of the plugin's.
    pub job: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    /// A cron expression (5 fields, or 6 with seconds), in `timezone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cron: Option<String>,
    /// Or a fixed interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub every_secs: Option<u32>,
    /// An IANA time zone. Default UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub timezone: Option<String>,
    /// Runs missed while the server was down: `once` (default), `skip` or `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub missed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "Record<string, unknown>"))]
    pub params: Option<Value>,
    /// Whether it starts running. Default true; an admin pauses and resumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub enabled: Option<bool>,
}

/// An incoming webhook at `/hooks/<plugin>/<name>`. Each accepted delivery enqueues `job`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WebhookContribution {
    /// `<plugin>.<name>`.
    pub id: String,
    pub job: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    /// How deliveries prove where they come from. Required; `{ "scheme": "none" }` accepts anything.
    pub verify: WebhookVerify,
    /// The largest body accepted, in bytes. Default 1 MiB, at most 16 MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub max_bytes: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum WebhookScheme {
    /// GitHub: `X-Hub-Signature-256: sha256=<hex>` over the body; replays by `X-GitHub-Delivery`.
    Github,
    /// Stripe: `Stripe-Signature: t=<unix>,v1=<hex>` over `<t>.<body>`.
    Stripe,
    /// Slack: `X-Slack-Signature: v0=<hex>` over `v0:<timestamp>:<body>`.
    Slack,
    /// Standard Webhooks (Svix and others): `webhook-signature: v1,<base64>` over `<id>.<timestamp>.<body>`.
    Standard,
    /// An HMAC of the body (or `<timestamp>.<body>`) in a header of your choosing.
    Hmac,
    /// No verification. Only for hooks that may be called by anyone.
    None,
}

/// Where a webhook's secret comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", untagged)]
pub enum SecretSource {
    /// A global secret of the plugin, by name (set by an admin).
    Secret { secret: String },
    /// A field of the credential of the plugin's global connection of this type.
    Connection {
        connection: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        field: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WebhookVerify {
    pub scheme: WebhookScheme,
    /// The secret. Every scheme but `none` needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub secret: Option<SecretSource>,
    /// `hmac`: `sha256` (default), `sha1` or `sha512`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub algorithm: Option<String>,
    /// `hmac`: the header with the signature. Default `X-Signature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub header: Option<String>,
    /// `hmac`: text before the signature, such as `sha256=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub prefix: Option<String>,
    /// `hmac`: `hex` (default) or `base64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub encoding: Option<String>,
    /// `hmac`: a header with a unix timestamp; the signature is then over `<timestamp>.<body>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub timestamp_header: Option<String>,
    /// `hmac`: a header with a delivery id, for replay protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub delivery_header: Option<String>,
    /// How old a timestamp may be, in seconds. Default 300.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub tolerance_secs: Option<u32>,
}

/// `schedules.create`, from a plugin inside a handler: a schedule on behalf of the call's user.
/// Creating one with a name the user already has replaces it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScheduleCreate {
    /// The plugin it is of: a plugin's own, or one an admin names; `rookery` for a kind of the server's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub plugin: Option<String>,
    pub name: String,
    /// The kind of job: `<plugin>/<job>`, or one of the server's, such as `apply`.
    pub job: String,
    /// The resource the job is about, such as `env://studio`, whose access the job's follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub every_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub missed: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, type = "Record<string, unknown>"))]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInfo {
    pub id: String,
    pub plugin: String,
    pub name: String,
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    /// The job each firing enqueues.
    pub job: String,
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
    pub every_secs: Option<i64>,
    pub timezone: String,
    /// `once`, `skip` or `all`.
    pub missed: String,
    pub enabled: bool,
    /// Declared in the manifest; otherwise created at runtime.
    pub declared: bool,
    /// The user it runs on behalf of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub next_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_job_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScheduleList {
    pub schedules: Vec<ScheduleInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ScheduleListQuery {
    /// Admins: every schedule. Otherwise the caller's own (and, for a plugin, its declared ones).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "ts", ts(optional, type = "boolean"))]
    pub all: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub plugin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScheduleRef {
    pub id: String,
}

/// `schedules.setEnabled`: pause (`false`) or resume (`true`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScheduleSetEnabled {
    pub id: String,
    pub enabled: bool,
}

/// `schedules.fire`: run a schedule's job now, as a firing would (on behalf of its user), with `params`
/// merged into the schedule's. A plugin fires its own schedules (a webhook that starts what a person set
/// up), a person theirs ("run now").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScheduleFire {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional, type = "Record<string, unknown>"))]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WebhookInfo {
    pub plugin: String,
    /// `<plugin>.<name>`.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    /// `/hooks/<plugin>/<name>`.
    pub path: String,
    pub job: String,
    /// The verification scheme: `github`, `hmac`, `stripe`, `slack`, `standard`, `none`.
    pub scheme: String,
    /// Where the secret comes from, for people: `secret webhook` or `connection github`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub secret: Option<String>,
    /// Whether the secret is set.
    pub ready: bool,
    pub deliveries: Vec<DeliveryInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WebhookList {
    pub webhooks: Vec<WebhookInfo>,
}

/// `webhooks.setSecret` (admins): stores the secret a webhook declared as `{ "secret": "<name>" }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WebhookSecretSet {
    /// `<plugin>.<name>`.
    pub id: String,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DeliveryInfo {
    pub id: String,
    /// `accepted`, `duplicate` or `refused`.
    pub outcome: String,
    pub status: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub job_id: Option<String>,
    pub bytes: i32,
    pub received_at: String,
}
