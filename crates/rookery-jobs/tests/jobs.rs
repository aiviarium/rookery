//! Jobs with handlers inside the server, leases, topics, events and notifications, through their methods.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use rookery_jobs::{JobHandler, JobRun, Options};
use rookery_kernel::testing::{Inbox, TestKernel};
use rookery_kernel::{Caller, Methods, RpcError, test_kernel};
use serde_json::{Value, json};

/// Writes two lines and returns its params.
struct Echo;
impl JobHandler for Echo {
    fn kind(&self) -> &'static str {
        "test.echo"
    }
    fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        Box::pin(async move {
            job.info("one").await?;
            job.progress(json!({"done": 1, "of": 2})).await?;
            job.info("two").await?;
            Ok(job.params.clone())
        })
    }
}

/// Fails its first attempt.
struct Flaky(Arc<AtomicU32>);
impl JobHandler for Flaky {
    fn kind(&self) -> &'static str {
        "test.flaky"
    }
    fn run(&self, _: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        let tries = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if tries == 0 {
                Err(RpcError::unavailable("not yet"))
            } else {
                Ok(json!("done"))
            }
        })
    }
}

/// Fails, and is not safe to repeat, as an apply is not.
struct Apply;
impl JobHandler for Apply {
    fn kind(&self) -> &'static str {
        "test.apply"
    }
    fn repeatable(&self) -> bool {
        false
    }
    fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        Box::pin(async move {
            job.log("error", "the playbook failed", Some(json!({"exit": 2})))
                .await?;
            Err(RpcError::internal("ansible-playbook exited with 2"))
        })
    }
}

/// Waits until it is cancelled.
struct Waits;
impl JobHandler for Waits {
    fn kind(&self) -> &'static str {
        "test.waits"
    }
    fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        Box::pin(async move {
            job.info("waiting").await?;
            job.cancelled().await;
            job.info("stopping").await?;
            Ok(Value::Null)
        })
    }
}

fn handlers() -> Vec<Arc<dyn JobHandler>> {
    vec![
        Arc::new(Echo),
        Arc::new(Flaky(Arc::default())),
        Arc::new(Apply),
        Arc::new(Waits),
    ]
}

fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    let options = Options {
        backoff: Duration::ZERO,
        ..Options::default()
    };
    rookery_jobs::register(&mut methods, handlers(), options);
    methods
}

fn code(result: Result<Value, RpcError>) -> &'static str {
    result
        .err()
        .map(|e| e.error_code().as_str())
        .unwrap_or("ok")
}

/// Follows a job to its end: its log lines and the outcome.
async fn follow(k: &TestKernel, inbox: &mut Inbox, id: &Value, after: i64) -> Vec<Value> {
    let opened = k
        .call_on(inbox, "jobs.follow", json!({"id": id, "after": after}))
        .await
        .unwrap();
    inbox.lines(opened["stream"].as_u64().unwrap() as u32).await
}

#[tokio::test]
async fn a_job_runs_in_the_server_and_its_log_streams() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    assert_eq!(
        code(
            k.call(&ops, "jobs.enqueue", json!({"kind": "test.echo"}))
                .await
        ),
        "FORBIDDEN"
    );
    assert_eq!(
        code(
            k.call(&admin, "jobs.enqueue", json!({"kind": "test.none"}))
                .await
        ),
        "INVALID"
    );
    let job = k
        .call(
            &admin,
            "jobs.enqueue",
            json!({"kind": "test.echo", "params": {"host": "studio"}, "subject": "env://studio"}),
        )
        .await
        .unwrap();
    let mut inbox = k.connect(&admin);
    let lines = follow(&k, &mut inbox, &job["id"], 0).await;
    let texts: Vec<&str> = lines.iter().filter_map(|l| l["text"].as_str()).collect();
    assert_eq!(texts, ["one", "two"]);
    assert_eq!(
        lines.last().unwrap(),
        &json!({"state": "succeeded", "result": {"host": "studio"}, "error": null})
    );
    // From a cursor: only what came after it.
    let lines = follow(&k, &mut inbox, &job["id"], 1).await;
    assert_eq!(lines.len(), 2);
    let done = k
        .call(&admin, "jobs.get", json!({"id": job["id"]}))
        .await
        .unwrap();
    assert_eq!(
        (done["state"].as_str(), done["progress"].clone()),
        (Some("succeeded"), json!({"done": 1, "of": 2}))
    );
    // Someone else's job is not there for a person.
    assert_eq!(
        code(k.call(&ops, "jobs.get", json!({"id": job["id"]})).await),
        "NOT_FOUND"
    );
    assert_eq!(
        k.call(&ops, "jobs.list", json!({})).await.unwrap()["jobs"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let listed = k
        .call(&admin, "jobs.list", json!({"subject": "env://studio"}))
        .await
        .unwrap();
    assert_eq!(listed["jobs"].as_array().unwrap().len(), 1);
    let output = k
        .call(&admin, "jobs.output", json!({"id": job["id"]}))
        .await
        .unwrap();
    assert_eq!(
        (
            output["items"].as_array().unwrap().len(),
            output["state"].as_str()
        ),
        (2, Some("succeeded"))
    );
    k.close().await;
}

#[tokio::test]
async fn a_failed_attempt_runs_again_unless_the_handler_is_not_safe_to_repeat() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let mut inbox = k.connect(&admin);
    let flaky = k
        .call(&admin, "jobs.enqueue", json!({"kind": "test.flaky"}))
        .await
        .unwrap();
    assert_eq!(flaky["maxAttempts"], 3);
    let end = follow(&k, &mut inbox, &flaky["id"], 0).await;
    assert_eq!(end.last().unwrap()["state"], "succeeded");
    assert_eq!(
        k.call(&admin, "jobs.get", json!({"id": flaky["id"]}))
            .await
            .unwrap()["attempts"],
        2
    );

    let apply = k
        .call(
            &admin,
            "jobs.enqueue",
            json!({"kind": "test.apply", "subject": "env://studio"}),
        )
        .await
        .unwrap();
    assert_eq!(apply["maxAttempts"], 1);
    let lines = follow(&k, &mut inbox, &apply["id"], 0).await;
    assert_eq!(lines[0]["data"], json!({"exit": 2}));
    let end = lines.last().unwrap();
    assert_eq!(
        (end["state"].as_str(), end["error"]["code"].as_str()),
        (Some("failed"), Some("INTERNAL"))
    );
    // A person decides to run it again.
    let again = k
        .call(&admin, "jobs.retry", json!({"id": apply["id"]}))
        .await
        .unwrap();
    assert_eq!(
        (again["state"].as_str(), again["maxAttempts"].as_i64()),
        (Some("queued"), Some(2))
    );
    let end = follow(&k, &mut inbox, &apply["id"], 1).await;
    assert_eq!(end.last().unwrap()["state"], "failed");
    assert_eq!(
        k.call(&admin, "jobs.get", json!({"id": apply["id"]}))
            .await
            .unwrap()["attempts"],
        2
    );
    k.close().await;
}

#[tokio::test]
async fn cancelling_stops_a_running_handler() {
    let k: TestKernel = test_kernel!(methods());
    let admin = k.person("Maria", true).await;
    let mut inbox = k.connect(&admin);
    let job = k
        .call(&admin, "jobs.enqueue", json!({"kind": "test.waits"}))
        .await
        .unwrap();
    let opened = k
        .call_on(&inbox, "jobs.follow", json!({"id": job["id"]}))
        .await
        .unwrap();
    let stream = opened["stream"].as_u64().unwrap() as u32;
    // Once it says it waits, it runs.
    let first = inbox.frame(stream).await;
    assert!(String::from_utf8_lossy(&first).contains("waiting"));
    let cancelled = k
        .call(&admin, "jobs.cancel", json!({"id": job["id"]}))
        .await
        .unwrap();
    assert_eq!(cancelled["cancelRequested"], true);
    let rest = inbox.lines(stream).await;
    assert_eq!(rest.last().unwrap()["state"], "cancelled");
    assert!(rest.iter().any(|l| l["text"] == "stopping"));
    k.close().await;
}

#[tokio::test]
async fn a_new_leader_ends_what_the_old_one_left_running() {
    let k: TestKernel = test_kernel!(Methods::default());
    let jobs = k.db.store.jobs();
    let params = json!({});
    let new = |kind| rookery_store::jobs::NewJob {
        kind,
        params: &params,
        max_attempts: 1,
        run_after: None,
        subject: None,
        enqueued_by: "user:x",
    };
    let apply = jobs.enqueue(new("test.apply")).await.unwrap();
    let kinds = vec!["test.apply".to_owned()];
    jobs.claim(&kinds, "server:old", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    let mut echo = new("test.echo");
    echo.max_attempts = 3;
    let echo = jobs.enqueue(echo).await.unwrap();
    jobs.claim(
        &["test.echo".to_owned()],
        "server:old",
        Duration::from_secs(30),
    )
    .await
    .unwrap()
    .unwrap();

    // A new kernel on the same database takes over.
    let kernel = rookery_kernel::Kernel::new(
        rookery_kernel::Config::new("http://127.0.0.1:7777"),
        k.db.store.clone(),
        methods(),
    );
    kernel.methods.started(&kernel);
    let (conn, rx) = rookery_kernel::conn::ConnHandle::detached(&kernel, Caller::Local);
    let mut inbox = Inbox { conn, rx };
    let call = rookery_kernel::Call::new(kernel.clone(), Caller::Local, Some(inbox.conn.clone()));
    let opened = kernel
        .methods
        .dispatch(call, "jobs.follow", json!({"id": echo.id.to_string()}))
        .await
        .unwrap();
    let lines = inbox.lines(opened["stream"].as_u64().unwrap() as u32).await;
    assert_eq!(
        lines.last().unwrap()["state"],
        "succeeded",
        "one that may repeat runs again"
    );
    let apply = jobs.get(apply.id).await.unwrap().unwrap();
    assert_eq!(
        (apply.state.as_str(), apply.error.unwrap()["code"].as_str()),
        ("failed", Some("UNAVAILABLE"))
    );
    kernel.shutdown.cancel();
    k.close().await;
}

fn messaging() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_jobs::register(&mut methods, Vec::new(), Options::default());
    methods
}

#[tokio::test]
async fn leases_go_to_one_holder_at_a_time() {
    let k: TestKernel = test_kernel!(messaging());
    let fleet = Caller::Plugin("fleet".into());
    let admin = k.person("Maria", true).await;
    let ops = k.person("Ops", false).await;
    let lease = k
        .call(
            &fleet,
            "leases.acquire",
            json!({"name": "sync", "ttlMs": 30000}),
        )
        .await
        .unwrap();
    assert_eq!(
        (lease["fence"].as_i64(), lease["holder"].as_str()),
        (Some(1), Some("plugin:fleet"))
    );
    // An admin acting for the plugin is another holder.
    assert_eq!(
        code(
            k.call(
                &admin,
                "leases.acquire",
                json!({"plugin": "fleet", "name": "sync", "ttlMs": 1000})
            )
            .await
        ),
        "CONFLICT"
    );
    assert_eq!(
        code(
            k.call(
                &ops,
                "leases.acquire",
                json!({"plugin": "fleet", "name": "sync", "ttlMs": 1000})
            )
            .await
        ),
        "FORBIDDEN"
    );
    // Another plugin's lease of the same name is its own.
    k.call(
        &Caller::Plugin("agents".into()),
        "leases.acquire",
        json!({"name": "sync", "ttlMs": 1000}),
    )
    .await
    .unwrap();
    k.call(
        &fleet,
        "leases.renew",
        json!({"name": "sync", "ttlMs": 30000, "fence": 1}),
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            k.call(
                &fleet,
                "leases.renew",
                json!({"name": "sync", "ttlMs": 30000, "fence": 7})
            )
            .await
        ),
        "CONFLICT"
    );
    k.call(
        &fleet,
        "leases.release",
        json!({"name": "sync", "fence": 1}),
    )
    .await
    .unwrap();
    let taken = k
        .call(
            &admin,
            "leases.acquire",
            json!({"plugin": "fleet", "name": "sync", "ttlMs": 1000}),
        )
        .await
        .unwrap();
    assert_eq!(taken["fence"], 2);
    let seen = k
        .call(&fleet, "leases.inspect", json!({"name": "sync"}))
        .await
        .unwrap();
    assert_eq!(
        seen["lease"]["holder"],
        format!("user:{}", admin.user().unwrap().id)
    );
    k.close().await;
}

#[tokio::test]
async fn a_subscription_gets_its_messages_in_order_until_acked() {
    let k: TestKernel = test_kernel!(messaging());
    let fleet = Caller::Plugin("fleet".into());
    let admin = k.person("Maria", true).await;
    // A plugin's stream needs a connection; the admin holds one for it here.
    let mut inbox = k.connect(&admin);
    let subscribe = json!({"plugin": "fleet", "topic": "hosts", "subscription": "inbox"});
    let opened = k
        .call_on(&inbox, "topics.subscribe", subscribe)
        .await
        .unwrap();
    let stream = opened["stream"].as_u64().unwrap() as u32;
    let one = k
        .call(
            &fleet,
            "topics.publish",
            json!({"topic": "hosts", "payload": {"host": "studio"}}),
        )
        .await
        .unwrap();
    k.call(
        &fleet,
        "topics.publish",
        json!({"topic": "hosts", "payload": {"host": "forge"}}),
    )
    .await
    .unwrap();
    let line = |bytes: Vec<u8>| {
        serde_json::from_slice::<Value>(bytes.strip_suffix(b"\n").unwrap()).unwrap()
    };
    let first = line(inbox.frame(stream).await);
    assert_eq!(
        (
            first["seq"].clone(),
            first["payload"]["host"].as_str(),
            first["attempt"].as_i64()
        ),
        (one["seq"].clone(), Some("studio"), Some(1))
    );
    let delivery = |seq: &Value| json!({"plugin": "fleet", "topic": "hosts", "subscription": "inbox", "seq": seq, "error": "busy"});
    // Nacked, it comes again first.
    assert_eq!(
        k.call(&admin, "topics.nack", delivery(&first["seq"]))
            .await
            .unwrap(),
        json!({"dead": false})
    );
    sqlx::query("UPDATE rookery.topic_subscriptions SET not_before = NULL")
        .execute(k.db.store.pool())
        .await
        .unwrap();
    k.call(
        &fleet,
        "topics.publish",
        json!({"topic": "hosts", "payload": {"host": "third"}}),
    )
    .await
    .unwrap();
    let again = line(inbox.frame(stream).await);
    assert_eq!(
        (again["seq"].clone(), again["attempt"].as_i64()),
        (first["seq"].clone(), Some(2))
    );
    k.call(&admin, "topics.ack", delivery(&again["seq"]))
        .await
        .unwrap();
    let second = line(inbox.frame(stream).await);
    assert_eq!(second["payload"]["host"], "forge");
    let info = k
        .call(&fleet, "topics.inspect", json!({"topic": "hosts"}))
        .await
        .unwrap();
    assert_eq!(
        (
            info["messages"].as_i64(),
            info["subscriptions"][0]["lag"].as_i64()
        ),
        (Some(3), Some(2))
    );
    assert_eq!(
        code(
            k.call(
                &Caller::Plugin("agents".into()),
                "topics.inspect",
                json!({"plugin": "fleet", "topic": "hosts"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    k.close().await;
}

#[tokio::test]
async fn events_reach_those_subscribed_now() {
    let k: TestKernel = test_kernel!(messaging());
    let fleet = Caller::Plugin("fleet".into());
    let ops = k.person("Ops", false).await;
    let eve = k.person("Eve", false).await;
    let mut mine = k.connect(&ops);
    let mut theirs = k.connect(&eve);
    k.call_on(&mine, "events.subscribe", json!({"names": ["fleet/*"]}))
        .await
        .unwrap();
    let sub = k
        .call_on(
            &theirs,
            "events.subscribe",
            json!({"names": ["fleet/host.changed"]}),
        )
        .await
        .unwrap();
    let sent = k
        .call(
            &fleet,
            "events.publish",
            json!({"name": "host.changed", "payload": {"host": "studio"}}),
        )
        .await
        .unwrap();
    assert_eq!(sent, json!({"delivered": 2}));
    assert_eq!(
        mine.notification("events.event").await["payload"]["host"],
        "studio"
    );
    assert_eq!(
        theirs.notification("events.event").await["name"],
        "fleet/host.changed"
    );
    // To one person only.
    let only = k
        .call(
            &fleet,
            "events.publish",
            json!({"name": "host.changed", "user": ops.user().unwrap().id}),
        )
        .await
        .unwrap();
    assert_eq!(only, json!({"delivered": 1}));
    k.call_on(
        &theirs,
        "events.unsubscribe",
        json!({"subscription": sub["subscription"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            k.call_on(
                &mine,
                "events.unsubscribe",
                json!({"subscription": sub["subscription"]})
            )
            .await
        ),
        "NOT_FOUND"
    );
    mine.conn.close_detached(&k.kernel);
    assert_eq!(
        k.call(&fleet, "events.publish", json!({"name": "x"}))
            .await
            .unwrap(),
        json!({"delivered": 0})
    );
    assert_eq!(
        code(
            k.call(
                &ops,
                "events.publish",
                json!({"plugin": "fleet", "name": "x"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    k.close().await;
}

#[tokio::test]
async fn notifications_reach_a_persons_inbox_and_answers_the_plugin() {
    let k: TestKernel = test_kernel!(messaging());
    let fleet = Caller::Plugin("fleet".into());
    let ops = k.person("Ops", false).await;
    let ops_id = ops.user().unwrap().id.clone();
    let mut live = k.connect(&ops);
    let send = json!({"user": ops_id, "kind": "apply", "title": "Apply studio?", "actions": ["approve", "deny"], "data": {"host": "studio"}});
    let sent = k.call(&fleet, "notify.send", send.clone()).await.unwrap();
    let id = sent["notification"]["id"].clone();
    assert_eq!(live.notification("notify.new").await["id"], id);
    let listed = k
        .call(&ops, "notify.list", json!({"unread": true}))
        .await
        .unwrap();
    assert_eq!(
        (
            listed["unread"].as_i64(),
            listed["notifications"][0]["kind"].as_str()
        ),
        (Some(1), Some("fleet/apply"))
    );
    assert_eq!(
        code(
            k.call(&ops, "notify.answer", json!({"id": id, "answer": "maybe"}))
                .await
        ),
        "INVALID"
    );
    k.call(
        &ops,
        "notify.answer",
        json!({"id": id, "answer": "approve"}),
    )
    .await
    .unwrap();
    let answers = k
        .call(&fleet, "topics.inspect", json!({"topic": "notify.answers"}))
        .await
        .unwrap();
    assert_eq!(
        answers["messages"], 1,
        "the answer waits for the plugin on its topic"
    );
    k.call(
        &ops,
        "notify.setPref",
        json!({"kind": "fleet/apply", "muted": true}),
    )
    .await
    .unwrap();
    assert_eq!(
        k.call(&fleet, "notify.send", send).await.unwrap(),
        json!({"notification": null})
    );
    assert_eq!(
        k.call(&ops, "notify.read", json!({})).await.unwrap(),
        json!({"count": 0})
    );
    assert_eq!(
        code(
            k.call(
                &ops,
                "notify.send",
                json!({"plugin": "fleet", "user": ops_id, "kind": "x", "title": "x"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    k.close().await;
}
