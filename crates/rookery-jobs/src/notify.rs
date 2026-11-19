//! `notify.*`: an inbox for each person. A new notification reaches the person's connections as a
//! `notify.new` notification. An answer to one a plugin sent is published on the plugin's durable topic
//! `notify.answers`, where the plugin reads it.

use rookery_kernel::{Call, Empty, Level, Methods, RpcError};
use rookery_proto::jobs::{
    Count, NotificationInfo, NotificationList, NotifyAnswer, NotifyPref, NotifyQuery, NotifyRead,
    NotifySend, NotifySent,
};
use rookery_store::notify::{New, Notification};
use uuid::Uuid;

use crate::time;

pub fn info(n: Notification) -> NotificationInfo {
    let actions = n
        .actions
        .as_ref()
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    NotificationInfo {
        id: n.id.to_string(),
        kind: n.kind,
        title: n.title,
        body: n.body,
        data: n.data,
        actions,
        answer: n.answer,
        created_at: time(n.created_at),
        read_at: n.read_at.map(time),
    }
}

fn uuid(what: &str, id: &str) -> Result<Uuid, RpcError> {
    Uuid::parse_str(id).map_err(|_| RpcError::invalid(format!("{id:?} is not the id of a {what}")))
}

pub fn register(methods: &mut Methods) {
    methods
        .add("notify.send", |call: Call, p: NotifySend| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            if p.title.trim().is_empty() || p.title.len() > 500 {
                return Err(RpcError::invalid(
                    "a notification's title is 1 to 500 bytes",
                ));
            }
            let user = uuid("user", &p.user)?;
            let kind = crate::scoped(&plugin, &p.kind)?;
            let sent = call
                .store()
                .notify()
                .send(New {
                    user,
                    kind: &kind,
                    title: &p.title,
                    body: p.body.as_deref(),
                    data: p.data.as_ref(),
                    actions: p.actions.as_deref(),
                    sent_by: &call.audit_name(),
                })
                .await?
                .map(info);
            if let Some(sent) = &sent {
                call.kernel
                    .conns
                    .notify_user(&user.to_string(), "notify.new", sent);
            }
            Ok(NotifySent { notification: sent })
        })
        .summary("Puts a notification in a person's inbox, unless they muted its kind.")
        .level(Level::Plugin);
    methods
        .add("notify.list", |call: Call, p: NotifyQuery| async move {
            let user = call.user_id()?;
            let before = p
                .before
                .as_deref()
                .map(|b| uuid("notification", b))
                .transpose()?;
            let notify = call.store().notify();
            let notifications = notify
                .list(user, p.unread, before, p.limit.unwrap_or(50))
                .await?;
            Ok(NotificationList {
                notifications: notifications.into_iter().map(info).collect(),
                unread: notify.unread(user).await?,
            })
        })
        .summary("The caller's notifications, newest first, and how many are unread.")
        .level(Level::Own);
    methods
        .add("notify.read", |call: Call, p: NotifyRead| async move {
            let user = call.user_id()?;
            let ids = p
                .ids
                .as_ref()
                .map(|ids| {
                    ids.iter()
                        .map(|id| uuid("notification", id))
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?;
            let count = call.store().notify().read(user, ids.as_deref()).await?;
            Ok(Count { count })
        })
        .summary("Marks some of the caller's notifications read, or all of them.")
        .level(Level::Own);
    methods
        .add("notify.answer", |call: Call, p: NotifyAnswer| async move {
            let user = call.user_id()?;
            let answered = call
                .store()
                .notify()
                .answer(user, uuid("notification", &p.id)?, &p.answer)
                .await?;
            if let Some(topic) = answered.answer_topic() {
                crate::topics::wake_topic(&call.kernel, &topic);
            }
            Ok(info(answered))
        })
        .summary("Gives one of a notification's answers, once.")
        .level(Level::Own);
    methods
        .add("notify.setPref", |call: Call, p: NotifyPref| async move {
            call.store()
                .notify()
                .set_pref(call.user_id()?, &p.kind, p.muted)
                .await?;
            Ok(Empty {})
        })
        .summary("Mutes a kind of notification for the caller, or hears it again.")
        .level(Level::Own);
}
