//! `jobs.*`, `leases.*`, `topics.*`, `events.*` and `notify.*`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ErrorBody;
use crate::facilities::wire;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

wire! {
    /// `jobs.enqueue`: a job of a kind a handler in the server runs.
    pub struct JobEnqueue {
        pub kind: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub params: Value,
        /// A resource the job is about, such as `env://studio`.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub subject: Option<String>,
    }
}

wire! {
    pub struct JobInfo {
        pub id: String,
        pub kind: String,
        pub state: JobState,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub params: Value,
        pub subject: Option<String>,
        pub attempts: i32,
        pub max_attempts: i32,
        pub cancel_requested: bool,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub progress: Option<Value>,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub result: Option<Value>,
        pub error: Option<ErrorBody>,
        pub enqueued_by: String,
        pub created_at: String,
        pub started_at: Option<String>,
        pub finished_at: Option<String>,
    }
}

wire! {
    /// `jobs.get`, `jobs.cancel` and `jobs.retry`.
    pub struct JobRef {
        pub id: String,
    }
}

wire! {
    /// `jobs.list`: the newest first. Admins see every job, others those they enqueued.
    pub struct JobQuery {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub state: Option<JobState>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub kind: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub subject: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub before: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub limit: Option<i64>,
    }
}

wire! {
    pub struct JobList {
        pub jobs: Vec<JobInfo>,
    }
}

wire! {
    /// `jobs.follow` and `jobs.output`: the log after `after`.
    pub struct JobFollow {
        pub id: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub after: i64,
    }
}

wire! {
    /// One line of a job's output. On the stream of `jobs.follow` each is a JSON line, and the last
    /// line is a [`JobEnd`].
    pub struct LogItem {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub seq: i64,
        pub at: String,
        pub attempt: i32,
        pub level: String,
        pub text: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub data: Option<Value>,
    }
}

wire! {
    /// The last line on the stream of `jobs.follow`.
    pub struct JobEnd {
        pub state: JobState,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub result: Option<Value>,
        pub error: Option<ErrorBody>,
    }
}

wire! {
    pub struct LogItems {
        pub items: Vec<LogItem>,
        pub state: JobState,
    }
}

wire! {
    /// `jobs.log`: a plugin's worker writes a line to a job of its plugin.
    pub struct JobLog {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub id: String,
        #[serde(default = "info")]
        pub level: String,
        pub text: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
        pub data: Option<Value>,
    }
}

fn info() -> String {
    "info".into()
}

wire! {
    /// `jobs.progress`: a plugin's worker says how far a job of its plugin is.
    pub struct JobProgress {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub id: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub progress: Value,
    }
}

wire! {
    /// `leases.acquire` and `leases.renew`: a lease of the plugin, for `ttlMs`.
    pub struct LeaseAcquire {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub name: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub ttl_ms: u64,
        /// For `leases.renew`: the fence it was acquired at.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub fence: Option<i64>,
        /// The worker that holds it, such as a replica's id. Without one, the connection on `/ws` is the
        /// worker, and its leases are released when it closes.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub worker: Option<String>,
    }
}

wire! {
    /// `leases.release` and `leases.inspect`.
    pub struct LeaseRef {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub name: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub fence: Option<i64>,
        /// For `leases.release`: the worker that acquired it, as `leases.acquire` named it.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub worker: Option<String>,
    }
}

wire! {
    pub struct LeaseInfo {
        pub name: String,
        pub holder: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub fence: i64,
        pub expires_at: Option<String>,
    }
}

wire! {
    pub struct LeaseState {
        pub lease: Option<LeaseInfo>,
    }
}

wire! {
    /// `topics.publish`: to a topic of the plugin.
    pub struct TopicPublish {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub topic: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub payload: Value,
    }
}

wire! {
    pub struct Published {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub seq: i64,
    }
}

wire! {
    /// `topics.subscribe`: a stream of the subscription's messages, one JSON line each, one at a time:
    /// the next comes once the last is acked or nacked.
    pub struct TopicSubscribe {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub topic: String,
        pub subscription: String,
        /// For a new subscription: from the first message kept, instead of the next one published.
        #[serde(default)]
        pub from_start: bool,
    }
}

wire! {
    /// A message on the stream of `topics.subscribe`.
    pub struct TopicMessage {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub seq: i64,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub payload: Value,
        pub published_by: String,
        pub published_at: String,
        /// The how-manyth delivery this is.
        pub attempt: i32,
    }
}

wire! {
    /// `topics.ack`, `topics.nack`, `topics.retry` and `topics.discard`.
    pub struct TopicDelivery {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub topic: String,
        pub subscription: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub seq: i64,
        /// For `topics.nack`: why.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub error: Option<String>,
    }
}

wire! {
    /// The result of `topics.nack`.
    pub struct Nacked {
        /// The message is a dead letter now.
        pub dead: bool,
    }
}

wire! {
    /// `topics.inspect` and `topics.deadLetters`.
    pub struct TopicRef {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub topic: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub subscription: Option<String>,
    }
}

wire! {
    pub struct SubscriptionInfo {
        pub subscription: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub acked: i64,
        /// Messages after its cursor.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub lag: i64,
        pub attempts: i32,
    }
}

wire! {
    pub struct TopicInfo {
        pub topic: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub head: i64,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub messages: i64,
        pub subscriptions: Vec<SubscriptionInfo>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub dead_letters: i64,
    }
}

wire! {
    pub struct DeadLetter {
        pub subscription: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub seq: i64,
        pub attempts: i32,
        pub error: Option<String>,
        /// Delivered again, and not yet acked.
        pub retrying: bool,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub payload: Value,
        pub at: String,
    }
}

wire! {
    pub struct DeadLetters {
        pub dead_letters: Vec<DeadLetter>,
    }
}

wire! {
    /// `events.publish`: at most once, to the connections subscribed now. With `user`, to that person's
    /// connections alone.
    pub struct EventPublish {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub name: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub payload: Value,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub user: Option<String>,
    }
}

wire! {
    /// `events.subscribe`: events whose name is one of `names`, or starts with one ending in `*`, as
    /// `events.event` notifications on this connection.
    pub struct EventSubscribe {
        pub names: Vec<String>,
    }
}

wire! {
    pub struct EventSubscribed {
        pub subscription: String,
    }
}

wire! {
    /// `events.unsubscribe`.
    pub struct EventUnsubscribe {
        pub subscription: String,
    }
}

wire! {
    /// The params of an `events.event` notification.
    pub struct EventNotice {
        pub subscription: String,
        /// `<plugin>/<name>`.
        pub name: String,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub payload: Value,
    }
}

wire! {
    /// The params of an `events.dropped` notification: events of a subscription that did not fit on a
    /// slow connection were dropped. Events are at most once; a client that needs what it missed reads
    /// it again, or subscribes again.
    pub struct EventsDropped {
        pub subscription: String,
        /// How many were dropped since the last `events.dropped` of the subscription.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub dropped: u64,
    }
}

wire! {
    pub struct Delivered {
        /// How many connections it reached.
        pub delivered: usize,
        /// How many subscribed connections it did not reach, as they were too slow.
        #[serde(default)]
        pub dropped: usize,
    }
}

wire! {
    /// `notify.send`: a plugin, or an admin, puts a notification in a person's inbox.
    pub struct NotifySend {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub plugin: Option<String>,
        pub user: String,
        pub kind: String,
        pub title: String,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub body: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "unknown"))]
        pub data: Option<Value>,
        /// The answers it offers, such as approve and deny.
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub actions: Option<Vec<String>>,
    }
}

wire! {
    pub struct NotificationInfo {
        pub id: String,
        pub kind: String,
        pub title: String,
        pub body: Option<String>,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        pub data: Option<Value>,
        pub actions: Vec<String>,
        pub answer: Option<String>,
        pub created_at: String,
        pub read_at: Option<String>,
    }
}

wire! {
    /// The result of `notify.send`: none when the person muted the kind.
    pub struct NotifySent {
        pub notification: Option<NotificationInfo>,
    }
}

wire! {
    /// Notification `notify.new`: one of a person's notifications, on their connections.
    pub struct NotifyNew {
        pub notification: NotificationInfo,
        pub unread: i64,
        /// Whether the person asked for this kind natively, from `notify.setPref`.
        pub native: bool,
    }
}

wire! {
    /// Notification `notify.changed`: notifications changed on every connection of the person,
    /// including the one that made the change. `allRead` means every notification is read now.
    pub struct NotifyChanged {
        pub notifications: Vec<NotificationInfo>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[cfg_attr(feature = "ts", ts(optional, type = "boolean"))]
        pub all_read: bool,
        pub unread: i64,
    }
}

wire! {
    /// `notify.list`: the caller's, newest first.
    pub struct NotifyQuery {
        #[serde(default)]
        pub unread: bool,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub before: Option<String>,
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional, type = "number"))]
        pub limit: Option<i64>,
    }
}

wire! {
    pub struct NotificationList {
        pub notifications: Vec<NotificationInfo>,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub unread: i64,
    }
}

wire! {
    /// `notify.read`: these, or with none every one of the caller's.
    pub struct NotifyRead {
        #[serde(default)]
        #[cfg_attr(feature = "ts", ts(optional))]
        pub ids: Option<Vec<String>>,
    }
}

wire! {
    pub struct Count {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        pub count: u64,
    }
}

wire! {
    /// `notify.answer`.
    pub struct NotifyAnswer {
        pub id: String,
        pub answer: String,
    }
}

wire! {
    /// `notify.setPref`: mute a kind, or hear it again, and whether it comes as a native notification.
    pub struct NotifyPref {
        pub kind: String,
        pub muted: bool,
        /// Whether the person wants this kind natively. Left out, it stays as it is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional, type = "boolean"))]
        pub native: Option<bool>,
    }
}
