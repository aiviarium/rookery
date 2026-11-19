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
        token: None,
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
            json!({"name": "sync", "ttlMs": 30000, "worker": "w1"}),
        )
        .await
        .unwrap();
    assert_eq!(
        (lease["fence"].as_i64(), lease["holder"].as_str()),
        (Some(1), Some("plugin:fleet/w1"))
    );
    // An admin acting for the plugin is another holder.
    assert_eq!(
        code(
            k.call(
                &admin,
                "leases.acquire",
                json!({"plugin": "fleet", "name": "sync", "ttlMs": 1000, "worker": "w1"})
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
                json!({"plugin": "fleet", "name": "sync", "ttlMs": 1000, "worker": "w1"})
            )
            .await
        ),
        "FORBIDDEN"
    );
    // Another plugin's lease of the same name is its own.
    k.call(
        &Caller::Plugin("agents".into()),
        "leases.acquire",
        json!({"name": "sync", "ttlMs": 1000, "worker": "w1"}),
    )
    .await
    .unwrap();
    k.call(
        &fleet,
        "leases.renew",
        json!({"name": "sync", "ttlMs": 30000, "fence": 1, "worker": "w1"}),
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            k.call(
                &fleet,
                "leases.renew",
                json!({"name": "sync", "ttlMs": 30000, "fence": 7, "worker": "w1"})
            )
            .await
        ),
        "CONFLICT"
    );
    k.call(
        &fleet,
        "leases.release",
        json!({"name": "sync", "fence": 1, "worker": "w1"}),
    )
    .await
    .unwrap();
    let taken = k
        .call(
            &admin,
            "leases.acquire",
            json!({"plugin": "fleet", "name": "sync", "ttlMs": 1000, "worker": "w1"}),
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
        format!("user:{}/w1", admin.user().unwrap().id)
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
    assert_eq!(sent, json!({"delivered": 2, "dropped": 0}));
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
            // Any spelling of the person's id.
            json!({"name": "host.changed", "user": ops.user().unwrap().id.to_uppercase()}),
        )
        .await
        .unwrap();
    assert_eq!(only, json!({"delivered": 1, "dropped": 0}));
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
        json!({"delivered": 0, "dropped": 0})
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
    let send = json!({"user": ops_id.replace('-', ""), "kind": "apply", "title": "Apply studio?", "actions": ["approve", "deny"], "data": {"host": "studio"}});
    let sent = k.call(&fleet, "notify.send", send.clone()).await.unwrap();
    let id = sent["notification"]["id"].clone();
    let new = tokio::time::timeout(Duration::from_secs(5), live.notification("notify.new"))
        .await
        .expect("the person's connection hears of it, whatever the spelling of their id");
    assert_eq!(new["id"], id);
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

#[tokio::test]
async fn a_notification_answer_is_retryable_if_topic_publication_fails() {
    let k: TestKernel = test_kernel!(messaging());
    let fleet = Caller::Plugin("fleet".into());
    let ops = k.person("Ops", false).await;
    let sent = k
        .call(
            &fleet,
            "notify.send",
            json!({"user": ops.user().unwrap().id, "kind": "apply", "title": "Apply studio?", "actions": ["approve"]}),
        )
        .await
        .unwrap();
    let answer = json!({"id": sent["notification"]["id"], "answer": "approve"});
    let pool = k.db.store.pool();
    // Publishing the answer fails as a fault of the server would.
    sqlx::raw_sql(
        "CREATE FUNCTION rookery.reject_answers() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN IF NEW.topic = 'fleet/notify.answers' THEN RAISE EXCEPTION 'publishing failed'; END IF; RETURN NEW; END $$;
         CREATE TRIGGER reject_answers BEFORE INSERT ON rookery.topic_messages FOR EACH ROW EXECUTE FUNCTION rookery.reject_answers();",
    )
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        code(k.call(&ops, "notify.answer", answer.clone()).await),
        "INTERNAL"
    );
    sqlx::query("DROP TRIGGER reject_answers ON rookery.topic_messages")
        .execute(pool)
        .await
        .unwrap();
    let retried = k.call(&ops, "notify.answer", answer).await;
    k.close().await;
    assert_eq!(retried.unwrap()["answer"], "approve");
}

#[tokio::test]
async fn a_plugin_cannot_use_a_nested_plugins_namespace() {
    let k: TestKernel = test_kernel!(messaging());
    let parent = Caller::Plugin("registry".into());
    let child = Caller::Plugin("registry/fleet".into());
    k.call(
        &child,
        "topics.publish",
        json!({"topic": "hosts", "payload": "fixture"}),
    )
    .await
    .unwrap();
    let own = k
        .call(&child, "topics.inspect", json!({"topic": "hosts"}))
        .await
        .unwrap();
    let refused = k
        .call(&parent, "topics.inspect", json!({"topic": "fleet/hosts"}))
        .await;
    k.close().await;
    assert_eq!(own["messages"], 1);
    assert_eq!(code(refused), "INVALID");
}

#[tokio::test]
async fn a_previous_attempt_cannot_write_to_the_next_one() {
    struct Observe(tokio::sync::mpsc::UnboundedSender<JobRun>);
    impl JobHandler for Observe {
        fn kind(&self) -> &'static str {
            "test.observe"
        }
        fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
            self.0.send(job.clone()).unwrap();
            Box::pin(async move {
                if job.attempt == 1 {
                    return Err(RpcError::unavailable("try again"));
                }
                job.cancelled().await;
                Ok(Value::Null)
            })
        }
    }
    let (tx, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let mut methods = Methods::default();
    rookery_jobs::register(
        &mut methods,
        vec![Arc::new(Observe(tx))],
        Options {
            backoff: Duration::ZERO,
            ..Options::default()
        },
    );
    let k: TestKernel = test_kernel!(methods);
    let admin = k.person("Maria", true).await;
    k.call(&admin, "jobs.enqueue", json!({"kind": "test.observe"}))
        .await
        .unwrap();
    let first = runs.recv().await.unwrap();
    let second = runs.recv().await.unwrap();
    assert_eq!((first.attempt, second.attempt), (1, 2));
    assert_eq!(
        first.info("stale").await.unwrap_err().error_code().as_str(),
        "CONFLICT"
    );
    assert_eq!(
        first
            .progress(json!("stale"))
            .await
            .unwrap_err()
            .error_code()
            .as_str(),
        "CONFLICT"
    );
    second.info("current").await.unwrap();
    second.progress(json!("current")).await.unwrap();
    k.close().await;
}

/// Runs a while, longer than a few renewals of its claim.
struct Slow;
impl JobHandler for Slow {
    fn kind(&self) -> &'static str {
        "test.slow"
    }
    fn repeatable(&self) -> bool {
        false
    }
    fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            job.info("done").await?;
            Ok(json!("done"))
        })
    }
}

#[tokio::test]
async fn an_attempt_outlasts_a_renewal_that_failed_once() {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    let options = Options {
        lease: Duration::from_millis(300),
        ..Options::default()
    };
    rookery_jobs::register(&mut methods, vec![Arc::new(Slow)], options);
    let k: TestKernel = test_kernel!(methods);
    // The first renewal of a claim fails, as one that lost a deadlock or its connection does.
    sqlx::raw_sql(
        "CREATE SEQUENCE renewals;
         CREATE FUNCTION fail_first_renewal() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             IF nextval('renewals') = 1 THEN
                 RAISE EXCEPTION 'deadlock detected' USING ERRCODE = '40P01';
             END IF;
             RETURN NEW;
         END $$;
         CREATE TRIGGER fail_first_renewal BEFORE UPDATE ON rookery.jobs FOR EACH ROW
             WHEN (OLD.state = 'running' AND NEW.state = 'running' AND NEW.claimed_until IS DISTINCT FROM OLD.claimed_until)
             EXECUTE FUNCTION fail_first_renewal();",
    )
    .execute(k.kernel.store.pool())
    .await
    .unwrap();
    let job = rookery_jobs::jobs::enqueue(&k.kernel, "test.slow", json!({}), None, "system:test")
        .await
        .unwrap();
    let jobs = k.kernel.store.jobs();
    let mut state = String::new();
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        state = jobs.get(job.id).await.unwrap().unwrap().state;
        if state != "queued" && state != "running" {
            break;
        }
    }
    let renewals: i64 = sqlx::query_scalar("SELECT last_value FROM renewals")
        .fetch_one(k.kernel.store.pool())
        .await
        .unwrap();
    k.close().await;
    assert!(renewals > 1, "the claim was renewed after the failure");
    assert_eq!(state, "succeeded", "the attempt ran to its end");
}

#[tokio::test]
async fn two_workers_of_a_plugin_hold_a_lease_one_at_a_time() {
    let k: TestKernel = test_kernel!(messaging());
    let fleet = Caller::Plugin("fleet".into());
    let first = k.connect(&fleet);
    let second = k.connect(&fleet);
    let acquire = json!({"name": "sync", "ttlMs": 30000});
    let held = k
        .call_on(&first, "leases.acquire", acquire.clone())
        .await
        .unwrap();
    assert_eq!(held["fence"], 1);
    assert!(
        held["holder"]
            .as_str()
            .unwrap()
            .starts_with("plugin:fleet/conn:"),
        "{held}"
    );
    // The same worker keeps its fence; another connection of the plugin is another worker.
    assert_eq!(
        k.call_on(&first, "leases.acquire", acquire.clone())
            .await
            .unwrap()["fence"],
        1
    );
    assert_eq!(
        code(k.call_on(&second, "leases.acquire", acquire.clone()).await),
        "CONFLICT"
    );
    assert_eq!(
        code(
            k.call_on(
                &second,
                "leases.renew",
                json!({"name": "sync", "ttlMs": 30000, "fence": 1})
            )
            .await
        ),
        "CONFLICT"
    );
    assert_eq!(
        code(
            k.call(
                &fleet,
                "leases.acquire",
                json!({"name": "sync", "ttlMs": 30000, "worker": "replica-2"})
            )
            .await
        ),
        "CONFLICT",
        "a named worker is another holder too"
    );
    assert_eq!(
        code(k.call(&fleet, "leases.acquire", acquire.clone()).await),
        "INVALID",
        "with no connection, a call names its worker"
    );

    // The first worker's connection closes: its lease is released, and the next holder gets a new fence.
    k.kernel.methods.connection_closed(&k.kernel, &first.conn);
    let taken = loop {
        match k.call_on(&second, "leases.acquire", acquire.clone()).await {
            Ok(taken) => break taken,
            Err(e) if e.error_code().as_str() == "CONFLICT" => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => panic!("{e:?}"),
        }
    };
    assert_eq!(taken["fence"], 2);
    let renewed = k
        .call(
            &fleet,
            "leases.acquire",
            json!({"name": "sync", "ttlMs": 10, "worker": "replica-2"}),
        )
        .await;
    assert_eq!(code(renewed), "CONFLICT");
    k.call_on(
        &second,
        "leases.release",
        json!({"name": "sync", "fence": 2}),
    )
    .await
    .unwrap();
    let named = k
        .call(
            &fleet,
            "leases.acquire",
            json!({"name": "sync", "ttlMs": 30000, "worker": "replica-2"}),
        )
        .await
        .unwrap();
    assert_eq!(
        (named["fence"].as_i64(), named["holder"].as_str()),
        (Some(3), Some("plugin:fleet/replica-2"))
    );
    k.close().await;
}

#[tokio::test]
async fn a_claim_that_ran_out_is_reclaimed_by_the_leader() {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    let options = Options {
        backoff: Duration::ZERO,
        lease: Duration::from_millis(300),
        ..Options::default()
    };
    rookery_jobs::register(&mut methods, handlers(), options);
    let k: TestKernel = test_kernel!(methods);
    let admin = k.person("Maria", true).await;
    let mut inbox = k.connect(&admin);
    // A first job ends, so the worker is past its start and what follows is a reclaim.
    let first = k
        .call(&admin, "jobs.enqueue", json!({"kind": "test.echo"}))
        .await
        .unwrap();
    follow(&k, &mut inbox, &first["id"], 0).await;

    // Two jobs a worker claimed and then stopped renewing, as one whose server hung.
    let jobs = k.db.store.jobs();
    let params = json!({"n": 1});
    let mut stale = Vec::new();
    for (kind, max_attempts) in [("test.echo", 3), ("test.apply", 1)] {
        let job = jobs
            .enqueue(rookery_store::jobs::NewJob {
                kind,
                params: &params,
                max_attempts,
                run_after: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                subject: None,
                enqueued_by: "user:x",
                token: None,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE rookery.jobs SET state = 'running', attempts = 1, claimed_by = 'server:hung',
                 claimed_until = now() - interval '1 second', started_at = now() WHERE id = $1",
        )
        .bind(job.id)
        .execute(k.db.store.pool())
        .await
        .unwrap();
        stale.push(job.id);
    }

    // One safe to repeat runs again within its attempts.
    let echo = Value::String(stale[0].to_string());
    let lines = follow(&k, &mut inbox, &echo, 0).await;
    assert_eq!(lines.last().unwrap()["state"], "succeeded", "{lines:?}");
    let echo = jobs.get(stale[0]).await.unwrap().unwrap();
    assert_eq!((echo.attempts, echo.result), (2, Some(params.clone())));

    // One not safe to repeat fails as interrupted, for a person to run again.
    let apply = Value::String(stale[1].to_string());
    let lines = follow(&k, &mut inbox, &apply, 0).await;
    let end = lines.last().unwrap();
    assert_eq!(end["state"], "failed", "{lines:?}");
    assert!(
        end["error"]["message"]
            .as_str()
            .unwrap()
            .contains("interrupted"),
        "{end}"
    );
    assert_eq!(jobs.get(stale[1]).await.unwrap().unwrap().attempts, 1);
    k.close().await;
}

#[tokio::test]
async fn a_burst_of_events_drops_what_does_not_fit_and_keeps_the_connection() {
    let k: TestKernel = test_kernel!(messaging());
    let ops = k.person("Ops", false).await;
    let fleet = Caller::Plugin("fleet".into());
    let mut slow = k.connect(&ops);
    let subscribed = k
        .call_on(&slow, "events.subscribe", json!({"names": ["fleet/*"]}))
        .await
        .unwrap();
    // Every host goes offline at once, and nobody reads the socket meanwhile.
    let (mut delivered, mut dropped) = (0, 0);
    for n in 0..200 {
        let sent = k
            .call(
                &fleet,
                "events.publish",
                json!({"name": "host.changed", "payload": {"host": format!("host-{n}"), "online": false}}),
            )
            .await
            .unwrap();
        delivered += sent["delivered"].as_u64().unwrap();
        dropped += sent["dropped"].as_u64().unwrap();
    }
    assert_eq!(delivered + dropped, 200);
    assert!(delivered > 0 && dropped > 0, "{delivered} {dropped}");
    assert!(!slow.conn.is_closed(), "the connection stays");
    // A reply or a control notification still has room.
    slow.conn.notify("test.control", json!({}));

    // The client reads what came, then hears how many it missed.
    let (mut events, mut missed, mut control) = (0, 0, false);
    while missed < dropped {
        let next = tokio::time::timeout(Duration::from_secs(5), slow.rx.recv())
            .await
            .expect("the drops are told")
            .expect("the connection is open");
        let rookery_kernel::conn::Outbound::Text(text) = next else {
            continue;
        };
        let Ok(rookery_proto::Message::Notification(n)) = rookery_proto::Message::parse(&text)
        else {
            continue;
        };
        match n.method.as_str() {
            "events.event" => events += 1,
            "test.control" => control = true,
            "events.dropped" => {
                assert_eq!(n.params["subscription"], subscribed["subscription"]);
                missed += n.params["dropped"].as_u64().unwrap();
            }
            other => panic!("{other}"),
        }
    }
    assert_eq!((events, missed, control), (delivered, dropped, true));
    // Afterwards events reach it again.
    let sent = k
        .call(
            &fleet,
            "events.publish",
            json!({"name": "host.changed", "payload": {}}),
        )
        .await
        .unwrap();
    assert_eq!(
        (sent["delivered"].as_u64(), sent["dropped"].as_u64()),
        (Some(1), Some(0))
    );
    k.close().await;
}

#[tokio::test]
async fn no_plugin_acts_as_rookery() {
    let k: TestKernel = test_kernel!(messaging());
    for id in ["rookery", "rookery.hosts"] {
        let posing = Caller::Plugin(id.into());
        assert_eq!(
            code(
                k.call(
                    &posing,
                    "leases.acquire",
                    json!({"name": "locks/studio", "ttlMs": 30000, "worker": "w1"}),
                )
                .await
            ),
            "FORBIDDEN",
            "{id}"
        );
    }
}

/// Holds its own job's row locked across awaits for a while, as a handler in the middle of a
/// transaction does, then logs a line.
struct HoldsItsRow(Arc<std::sync::Mutex<Option<sqlx::PgPool>>>);
impl JobHandler for HoldsItsRow {
    fn kind(&self) -> &'static str {
        "test.holds"
    }
    fn run(&self, job: JobRun) -> BoxFuture<'static, Result<Value, RpcError>> {
        let pool = self
            .0
            .lock()
            .unwrap()
            .clone()
            .expect("the pool is given before a job runs");
        Box::pin(async move {
            let fail = |e: sqlx::Error| RpcError::internal(e.to_string());
            let mut tx = pool.begin().await.map_err(fail)?;
            sqlx::query("SELECT 1 FROM rookery.jobs WHERE id = $1 FOR UPDATE")
                .bind(job.id)
                .execute(&mut *tx)
                .await
                .map_err(fail)?;
            // Several beats of the claim come due while the row is held.
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            tx.commit().await.map_err(fail)?;
            job.info("held and let go").await?;
            Ok(json!({}))
        })
    }
}

#[tokio::test]
async fn a_handler_that_holds_its_row_is_not_stopped_by_its_heartbeat() {
    // A heartbeat waited on in the same poll as the handler would wait on the handler's row lock
    // forever, while the handler, not polled, never lets it go.
    let pool = Arc::new(std::sync::Mutex::new(None));
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    let options = Options {
        lease: Duration::from_millis(150),
        ..Options::default()
    };
    rookery_jobs::register(
        &mut methods,
        vec![Arc::new(HoldsItsRow(pool.clone()))],
        options,
    );
    let k: TestKernel = test_kernel!(methods);
    *pool.lock().unwrap() = Some(k.db.store.pool().clone());
    let admin = k.person("Maria", true).await;
    let mut inbox = k.connect(&admin);
    let job = k
        .call(&admin, "jobs.enqueue", json!({"kind": "test.holds"}))
        .await
        .unwrap();
    let lines = tokio::time::timeout(
        Duration::from_secs(20),
        follow(&k, &mut inbox, &job["id"], 0),
    )
    .await
    .expect("the job ended in time: a heartbeat waited on its handler's row lock");
    assert_eq!(lines.last().unwrap()["state"], "succeeded", "{lines:?}");
}
