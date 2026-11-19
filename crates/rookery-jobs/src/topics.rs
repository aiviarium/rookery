//! `topics.*`: durable topics of a plugin. A subscription gets its messages in order, one at a time,
//! as JSON lines on the stream of `topics.subscribe`: the next comes once the last was acked or nacked.
//! A message nacked too often is a dead letter, which can be delivered again or discarded.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rookery_kernel::streams::Initiator;
use rookery_kernel::{Call, Empty, Kernel, Level, Methods, RpcError};
use rookery_proto::jobs::{
    DeadLetter, DeadLetters, Nacked, Published, SubscriptionInfo, TopicDelivery, TopicInfo,
    TopicMessage, TopicPublish, TopicRef, TopicSubscribe,
};
use rookery_proto::stream::StreamOpened;
use rookery_store::topics::{From, Next};
use tokio::sync::Notify;

use crate::{scoped, time};

/// The wait before a nacked message comes again, doubled for each delivery.
pub const BACKOFF: Duration = Duration::from_secs(1);

/// Wakes the streams of a topic when something changes for them.
#[derive(Default)]
pub struct Signals(Mutex<HashMap<String, Arc<Notify>>>);

impl Signals {
    fn of(&self, topic: &str) -> Arc<Notify> {
        self.0
            .lock()
            .unwrap()
            .entry(topic.to_owned())
            .or_default()
            .clone()
    }
}

/// Wakes the streams of a topic, after code inside the server published to it.
pub fn wake_topic(kernel: &Kernel, topic: &str) {
    wake(kernel, topic)
}

fn wake(kernel: &Kernel, topic: &str) {
    kernel.facility::<Signals>().of(topic).notify_waiters();
}

fn check_subscription(name: &str) -> Result<(), RpcError> {
    if name.is_empty() || name.len() > 200 {
        return Err(RpcError::invalid("a subscription's name is 1 to 200 bytes"));
    }
    Ok(())
}

/// Delivers a subscription's messages on a stream until it closes.
async fn deliver(
    kernel: Kernel,
    topic: String,
    subscription: String,
    stream: rookery_kernel::streams::Stream,
) {
    let (_reader, writer) = stream.split();
    let signal = kernel.facility::<Signals>().of(&topic);
    let topics = kernel.store.topics();
    let mut sent: Option<(i64, i32)> = None;
    loop {
        let woken = signal.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        let next = match topics.next(&topic, &subscription).await {
            Ok(next) => next,
            Err(e) => {
                writer.close(Some(RpcError::from(e).body()));
                return;
            }
        };
        let wait = match next {
            // The same delivery waits for its ack or nack.
            Next::Message(m, attempt) if sent == Some((m.seq, attempt)) => None,
            Next::Message(m, attempt) => {
                let message = TopicMessage {
                    seq: m.seq,
                    payload: m.payload,
                    published_by: m.published_by,
                    published_at: time(m.published_at),
                    attempt,
                };
                let mut line = serde_json::to_vec(&message).expect("a message serializes");
                line.push(b'\n');
                if writer.write(&line).await.is_err() {
                    return;
                }
                sent = Some((m.seq, attempt));
                None
            }
            Next::Wait(at) => Some((at - chrono::Utc::now()).to_std().unwrap_or(Duration::ZERO)),
            Next::Empty => None,
        };
        tokio::select! {
            _ = &mut woken => {}
            _ = tokio::time::sleep(wait.unwrap_or(Duration::from_secs(3600))), if wait.is_some() => {}
            _ = writer.closed() => return,
            _ = kernel.shutdown.cancelled() => {
                writer.close(Some(RpcError::unavailable("the server stops").body()));
                return;
            }
        }
    }
}

pub fn register(methods: &mut Methods) {
    methods
        .add("topics.publish", |call: Call, p: TopicPublish| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let topic = scoped(&plugin, &p.topic)?;
            let seq = call
                .store()
                .topics()
                .publish(&topic, &p.payload, &call.audit_name())
                .await?;
            wake(&call.kernel, &topic);
            Ok(Published { seq })
        })
        .summary("Publishes a message to a topic of the plugin. It is kept for every subscription.")
        .level(Level::Plugin);
    methods
        .add("topics.subscribe", |call: Call, p: TopicSubscribe| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let topic = scoped(&plugin, &p.topic)?;
            check_subscription(&p.subscription)?;
            let from = if p.from_start { From::Start } else { From::Latest };
            call.store().topics().subscribe(&topic, &p.subscription, from).await?;
            let stream = call.conn()?.streams().open(Initiator::Peer);
            let id = stream.id();
            tokio::spawn(deliver(call.kernel.clone(), topic, p.subscription, stream));
            Ok(StreamOpened { stream: id, offset: None })
        })
        .summary("A subscription's messages in order on a stream, one at a time until each is acked or nacked.")
        .stream()
        .level(Level::Plugin);
    methods
        .add("topics.ack", |call: Call, p: TopicDelivery| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let topic = scoped(&plugin, &p.topic)?;
            call.store()
                .topics()
                .ack(&topic, &p.subscription, p.seq)
                .await?;
            wake(&call.kernel, &topic);
            Ok(Empty {})
        })
        .summary("The subscription is done with a message; the next one comes.")
        .level(Level::Plugin);
    methods
        .add("topics.nack", |call: Call, p: TopicDelivery| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let topic = scoped(&plugin, &p.topic)?;
            let dead = call.store().topics().nack(&topic, &p.subscription, p.seq, p.error.as_deref(), BACKOFF).await?;
            wake(&call.kernel, &topic);
            Ok(Nacked { dead })
        })
        .summary("The subscription could not handle a message: it comes again later, or becomes a dead letter.")
        .level(Level::Plugin);
    methods
        .add("topics.inspect", |call: Call, p: TopicRef| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let topic = scoped(&plugin, &p.topic)?;
            let (head, messages, subscriptions, dead_letters) =
                call.store().topics().inspect(&topic).await?;
            Ok(TopicInfo {
                topic: p.topic,
                head,
                messages,
                subscriptions: subscriptions
                    .into_iter()
                    .map(|s| SubscriptionInfo {
                        subscription: s.subscription,
                        acked: s.acked,
                        lag: s.lag,
                        attempts: s.attempts,
                    })
                    .collect(),
                dead_letters,
            })
        })
        .summary("A topic's newest message, its subscriptions and their lag, and its dead letters.")
        .level(Level::Plugin);
    methods
        .add("topics.deadLetters", |call: Call, p: TopicRef| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let topic = scoped(&plugin, &p.topic)?;
            let dead = call
                .store()
                .topics()
                .dead_letters(&topic, p.subscription.as_deref())
                .await?;
            Ok(DeadLetters {
                dead_letters: dead
                    .into_iter()
                    .map(|d| DeadLetter {
                        subscription: d.subscription,
                        seq: d.seq,
                        attempts: d.attempts,
                        error: d.error,
                        retrying: d.retry,
                        payload: d.payload,
                        at: time(d.at),
                    })
                    .collect(),
            })
        })
        .summary("The dead letters of a topic, or of one subscription.")
        .level(Level::Plugin);
    methods
        .add("topics.retry", |call: Call, p: TopicDelivery| async move {
            let plugin = call.plugin(p.plugin.as_deref())?;
            let topic = scoped(&plugin, &p.topic)?;
            call.store()
                .topics()
                .retry(&topic, &p.subscription, p.seq)
                .await?;
            wake(&call.kernel, &topic);
            Ok(Empty {})
        })
        .summary("Delivers a dead letter to its subscription again, before newer messages.")
        .level(Level::Plugin);
    methods
        .add(
            "topics.discard",
            |call: Call, p: TopicDelivery| async move {
                let plugin = call.plugin(p.plugin.as_deref())?;
                let topic = scoped(&plugin, &p.topic)?;
                if !call
                    .store()
                    .topics()
                    .discard(&topic, &p.subscription, p.seq)
                    .await?
                {
                    return Err(RpcError::not_found(format!("no dead letter {}", p.seq)));
                }
                Ok(Empty {})
            },
        )
        .summary("Throws a dead letter away.")
        .level(Level::Plugin);
}
