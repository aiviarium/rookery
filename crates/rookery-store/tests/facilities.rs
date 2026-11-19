//! The facilities' tables: access, settings, secrets, kv, connections, blobs, layouts, jobs,
//! leases, topics and notifications.

use std::time::Duration;

use rookery_store::access::Who;
use rookery_store::jobs::{NewJob, Outcome};
use rookery_store::secrets::Scope;
use rookery_store::test_db;
use rookery_store::users::Login;
use serde_json::json;
use uuid::Uuid;

async fn person(store: &rookery_store::Store, subject: &str) -> Uuid {
    let login = Login {
        issuer: "https://id.example.test".into(),
        subject: subject.into(),
        name: subject.into(),
        email: None,
        admin: false,
    };
    store.users().upsert_login(&login).await.unwrap().0.id
}

#[tokio::test]
async fn levels_come_from_grants_groups_and_owners() {
    let db = test_db!();
    let access = db.store.access();
    let (maria, ops) = (
        person(&db.store, "maria").await,
        person(&db.store, "ops").await,
    );
    let who = |user: Uuid| (format!("user:{user}"), user);
    let level = |user: Uuid| {
        let (principal, user) = who(user);
        let access = db.store.access();
        async move {
            access
                .level(
                    "env://studio",
                    &Who {
                        principal: &principal,
                        user: Some(user),
                    },
                )
                .await
                .unwrap()
        }
    };
    assert_eq!(level(ops).await, 0);
    access.set_owner("env://studio", maria).await.unwrap();
    assert_eq!(level(maria).await, 3);
    let group = access
        .create_group("platform", Some("platform"))
        .await
        .unwrap();
    access
        .grant("env://studio", &format!("group:{}", group.id), 2, "user:x")
        .await
        .unwrap();
    assert_eq!(level(ops).await, 0);
    access.add_member(group.id, ops).await.unwrap();
    assert_eq!(level(ops).await, 2);
    access
        .grant("env://studio", "everyone", 1, "user:x")
        .await
        .unwrap();
    let anyone = person(&db.store, "anyone").await;
    assert_eq!(level(anyone).await, 1);
    // A claim the provider no longer names takes the person out of the group.
    access.sync_claims(ops, &[]).await.unwrap();
    assert_eq!(level(ops).await, 1);
    let (principal, _) = who(maria);
    let seen = access
        .filter(
            &["env://studio".into(), "env://forge".into()],
            &Who {
                principal: &principal,
                user: Some(maria),
            },
            2,
        )
        .await
        .unwrap();
    assert_eq!(seen, ["env://studio"]);
    assert!(access.grant("studio", "everyone", 1, "x").await.is_err());
    assert!(
        access
            .grant("env://studio", "someone", 1, "x")
            .await
            .is_err()
    );
    // A plugin is a principal of its own: its grant is its own and no person's.
    access
        .grant("env://studio", "plugin:fleet", 2, "user:x")
        .await
        .unwrap();
    let as_plugin = Who {
        principal: "plugin:fleet",
        user: None,
    };
    assert_eq!(access.level("env://studio", &as_plugin).await.unwrap(), 2);
    assert_eq!(level(anyone).await, 1);
    assert!(
        access
            .grant("env://studio", "plugin:Fleet", 1, "x")
            .await
            .is_err()
    );
    assert!(
        access
            .grant("env://studio", "plugin:rookery", 1, "x")
            .await
            .is_err()
    );
    db.close().await;
}

#[tokio::test]
async fn settings_resolve_user_then_global_then_default() {
    let db = test_db!();
    let settings = db.store.settings();
    let user = person(&db.store, "maria").await;
    use rookery_store::settings::Scope::*;
    settings
        .set("fleet", "theme", Default, &json!("light"), "plugin:fleet")
        .await
        .unwrap();
    assert_eq!(
        settings
            .get("fleet", "theme", Some(user))
            .await
            .unwrap()
            .unwrap()
            .scope,
        "default"
    );
    settings
        .set("fleet", "theme", Global, &json!("dark"), "user:a")
        .await
        .unwrap();
    settings
        .set("fleet", "theme", User(user), &json!("blue"), "user:a")
        .await
        .unwrap();
    let mine = settings
        .get("fleet", "theme", Some(user))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((mine.value, mine.scope.as_str()), (json!("blue"), "user"));
    assert_eq!(
        settings
            .get("fleet", "theme", None)
            .await
            .unwrap()
            .unwrap()
            .value,
        json!("dark")
    );
    settings.reset("fleet", "theme", User(user)).await.unwrap();
    assert_eq!(
        settings.list("fleet", Some(user)).await.unwrap()[0].value,
        json!("dark")
    );
    db.close().await;
}

#[tokio::test]
async fn secrets_are_sealed_in_their_rows_and_rekeyed() {
    let db = test_db!();
    let user = person(&db.store, "maria").await;
    let secrets = db.store.secrets();
    let info = secrets
        .set("fleet", "token", Scope::Global, b"made-up-token")
        .await
        .unwrap();
    secrets
        .set("fleet", "token", Scope::User(user), b"mine")
        .await
        .unwrap();
    assert_eq!(
        secrets
            .get("fleet", "token", Scope::Global)
            .await
            .unwrap()
            .unwrap(),
        b"made-up-token"
    );
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM rookery.secrets WHERE id = $1")
            .bind(info.id)
            .fetch_one(db.store.pool())
            .await
            .unwrap();
    assert!(
        !stored.windows(5).any(|w| w == b"made-"),
        "the value is sealed"
    );
    // Copied into another row, a sealed value does not open.
    sqlx::query("UPDATE rookery.secrets s SET nonce = g.nonce, ciphertext = g.ciphertext FROM rookery.secrets g WHERE s.scope = 'user' AND g.scope = 'global'")
        .execute(db.store.pool())
        .await
        .unwrap();
    assert!(
        secrets
            .get("fleet", "token", Scope::User(user))
            .await
            .is_err()
    );
    secrets
        .rename("fleet", "token", Scope::Global, "api-token")
        .await
        .unwrap();
    assert_eq!(
        secrets
            .get("fleet", "api-token", Scope::Global)
            .await
            .unwrap()
            .unwrap(),
        b"made-up-token"
    );
    secrets
        .delete("fleet", "token", Scope::User(user))
        .await
        .unwrap();

    let connection = db
        .store
        .connections()
        .create(
            "registry",
            "ghcr",
            Scope::Global,
            &json!({"url": "https://registry.example.test"}),
            Some(b"pw"),
            "user:a",
        )
        .await
        .unwrap();
    let old = db.store.key().unwrap().clone();
    let new = rookery_store::seal::Key::generate();
    let moved = secrets.rekey(&old, &new).await.unwrap();
    assert_eq!((moved.secrets, moved.connections), (1, 1));
    assert!(
        secrets
            .get("fleet", "api-token", Scope::Global)
            .await
            .is_err(),
        "the old key opens nothing now"
    );
    let rekeyed = db.store.with_key(Some(new.clone()));
    assert_eq!(
        rekeyed
            .secrets()
            .get("fleet", "api-token", Scope::Global)
            .await
            .unwrap()
            .unwrap(),
        b"made-up-token"
    );
    assert_eq!(
        rekeyed
            .connections()
            .credential(connection.id)
            .await
            .unwrap()
            .unwrap(),
        b"pw"
    );
    // A second rekey to the same key moves nothing.
    assert_eq!(
        rekeyed.secrets().rekey(&new, &new).await.unwrap().secrets,
        0
    );
    // Without a key: refused, and what needs none still works.
    let keyless = db.store.with_key(None);
    let refused = keyless
        .secrets()
        .get("fleet", "api-token", Scope::Global)
        .await
        .unwrap_err();
    assert!(matches!(refused, rookery_store::Error::Unavailable(_)));
    assert_eq!(
        keyless
            .secrets()
            .list(Some("fleet"), None)
            .await
            .unwrap()
            .len(),
        1
    );
    db.close().await;
}

#[tokio::test]
async fn kv_and_layouts_write_by_revision() {
    let db = test_db!();
    let kv = db.store.kv();
    assert_eq!(
        kv.set("fleet", None, "a", &json!(1), Some(0))
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        kv.set("fleet", None, "a", &json!(2), Some(0)).await,
        Err(rookery_store::Error::Conflict(_))
    ));
    assert_eq!(
        kv.set("fleet", None, "a", &json!(2), Some(1))
            .await
            .unwrap(),
        2
    );
    assert!(
        kv.set("fleet", None, "a", &json!(3), Some(1))
            .await
            .is_err()
    );
    assert_eq!(
        kv.set("fleet", None, "a", &json!(3), None).await.unwrap(),
        3
    );
    kv.set("fleet", None, "ab", &json!(4), None).await.unwrap();
    kv.set("fleet", None, "b", &json!(5), None).await.unwrap();
    let listed: Vec<String> = kv
        .list("fleet", None, "a", None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.key)
        .collect();
    assert_eq!(listed, ["a", "ab"]);
    assert!(kv.delete("fleet", None, "a", Some(2)).await.is_err());
    assert!(kv.delete("fleet", None, "a", Some(3)).await.unwrap());
    let user = person(&db.store, "maria").await;
    kv.set("fleet", Some(user), "b", &json!("mine"), None)
        .await
        .unwrap();
    assert_eq!(
        kv.get("fleet", None, "b").await.unwrap().unwrap().value,
        json!(5)
    );

    let layout = db.store.layout();
    assert_eq!(
        layout
            .put(user, "default", &json!({"panes": 1}), Some(0))
            .await
            .unwrap(),
        1
    );
    assert!(
        layout
            .put(user, "default", &json!({}), Some(0))
            .await
            .is_err()
    );
    assert!(
        layout
            .put(user, "other", &json!({}), Some(4))
            .await
            .is_err()
    );
    assert!(
        layout.get(user, "other").await.unwrap().is_none(),
        "a refused write leaves nothing"
    );
    assert_eq!(
        layout
            .put(user, "default", &json!({"panes": 2}), Some(1))
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        layout.get(user, "default").await.unwrap().unwrap(),
        (json!({"panes": 2}), 2)
    );
    db.close().await;
}

#[tokio::test]
async fn blobs_share_files_of_the_same_bytes() {
    let db = test_db!();
    let blobs = db.store.blobs();
    let one = blobs
        .put(b"recording", "text/plain", Some("a.txt"), "user:a")
        .await
        .unwrap();
    let two = blobs
        .put(b"recording", "text/plain", None, "user:b")
        .await
        .unwrap();
    assert_eq!(one.sha256, two.sha256);
    assert_eq!(blobs.read(&one).await.unwrap(), b"recording");
    blobs.delete(one.id).await.unwrap();
    assert_eq!(
        blobs.read(&two).await.unwrap(),
        b"recording",
        "the file stays while another blob has it"
    );
    blobs.delete(two.id).await.unwrap();
    assert!(blobs.read(&two).await.is_err());
    db.close().await;
}

fn job<'a>(kind: &'a str, params: &'a serde_json::Value, attempts: i32) -> NewJob<'a> {
    NewJob {
        kind,
        params,
        max_attempts: attempts,
        run_after: None,
        subject: Some("env://studio"),
        enqueued_by: "user:a",
        token: None,
    }
}

#[tokio::test]
async fn jobs_are_claimed_once_retried_and_recovered() {
    let db = test_db!();
    let jobs = db.store.jobs();
    let kinds = vec!["apply".to_owned()];
    let lease = Duration::from_secs(30);
    let params = json!({"host": "studio"});
    let made = jobs.enqueue(job("apply", &params, 2)).await.unwrap();
    let (a, b) = tokio::join!(
        jobs.claim(&kinds, "w1", lease),
        jobs.claim(&kinds, "w2", lease)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(a.is_some() != b.is_some(), "one worker gets it");
    let worker = if a.is_some() { "w1" } else { "w2" };
    jobs.log(made.id, "info", "started", None).await.unwrap();
    assert!(!jobs.heartbeat(made.id, worker, lease).await.unwrap());
    // A failed attempt with attempts left goes back to the queue.
    let again = jobs
        .finish(
            made.id,
            worker,
            Outcome::Failed {
                error: json!({"message": "x"}),
                retry_in: Some(Duration::ZERO),
            },
        )
        .await
        .unwrap();
    assert_eq!((again.state.as_str(), again.attempts), ("queued", 1));
    jobs.claim(&kinds, "w1", lease).await.unwrap().unwrap();
    jobs.log(made.id, "error", "failed again", None)
        .await
        .unwrap();
    let failed = jobs
        .finish(
            made.id,
            "w1",
            Outcome::Failed {
                error: json!({"message": "y"}),
                retry_in: Some(Duration::ZERO),
            },
        )
        .await
        .unwrap();
    assert_eq!(failed.state, "failed");
    let log = jobs.log_after(made.id, 0, 100).await.unwrap();
    assert_eq!(
        log.iter().map(|i| (i.seq, i.attempt)).collect::<Vec<_>>(),
        [(1, 1), (2, 2)]
    );
    assert_eq!(jobs.log_after(made.id, 1, 100).await.unwrap().len(), 1);
    let retried = jobs.retry(made.id).await.unwrap();
    assert_eq!(
        (retried.state.as_str(), retried.max_attempts),
        ("queued", 3)
    );

    // Cancelling: a queued job ends now, a running one is asked.
    let queued = jobs.enqueue(job("apply", &params, 1)).await.unwrap();
    assert_eq!(jobs.cancel(queued.id).await.unwrap().state, "cancelled");
    let running = jobs.claim(&kinds, "w1", lease).await.unwrap().unwrap();
    assert_eq!(jobs.cancel(running.id).await.unwrap().state, "running");
    assert!(
        jobs.heartbeat(running.id, "w1", lease).await.unwrap(),
        "the handler sees it was asked to stop"
    );
    assert_eq!(
        jobs.finish(running.id, "w1", Outcome::Cancelled)
            .await
            .unwrap()
            .state,
        "cancelled"
    );

    // A server that stops leaves its attempts running: one that may repeat runs again, one that may not fails.
    let repeat = jobs.enqueue(job("apply", &params, 3)).await.unwrap();
    let once = jobs.enqueue(job("provision", &params, 1)).await.unwrap();
    let all = vec!["apply".to_owned(), "provision".to_owned()];
    jobs.claim(&all, "old", lease).await.unwrap().unwrap();
    jobs.claim(&all, "old", lease).await.unwrap().unwrap();
    let recovered = jobs
        .recover(&all, &["provision".to_owned()], "new")
        .await
        .unwrap();
    let state = |id| recovered.iter().find(|j| j.id == id).unwrap().state.clone();
    assert_eq!(
        (state(repeat.id).as_str(), state(once.id).as_str()),
        ("queued", "failed")
    );
    assert!(
        jobs.finish(once.id, "old", Outcome::Succeeded(json!(null)))
            .await
            .is_err(),
        "the old claim is gone"
    );
    db.close().await;
}

#[tokio::test]
async fn leases_are_fenced() {
    let db = test_db!();
    let leases = db.store.leases();
    let ttl = Duration::from_secs(30);
    let first = leases
        .acquire("fleet/sync", "plugin:fleet/a", ttl)
        .await
        .unwrap();
    assert_eq!(first.fence, 1);
    assert!(
        leases
            .acquire("fleet/sync", "plugin:fleet/b", ttl)
            .await
            .is_err()
    );
    assert_eq!(
        leases
            .acquire("fleet/sync", "plugin:fleet/a", ttl)
            .await
            .unwrap()
            .fence,
        1,
        "the holder keeps its fence"
    );
    leases
        .renew("fleet/sync", "plugin:fleet/a", 1, ttl)
        .await
        .unwrap();
    assert!(
        leases
            .release("fleet/sync", "plugin:fleet/a", 1)
            .await
            .unwrap()
    );
    let second = leases
        .acquire("fleet/sync", "plugin:fleet/b", ttl)
        .await
        .unwrap();
    assert_eq!(second.fence, 2);
    assert!(
        leases
            .renew("fleet/sync", "plugin:fleet/a", 1, ttl)
            .await
            .is_err(),
        "an old fence renews nothing"
    );
    // An expired lease goes to the next with a new fence.
    sqlx::query("UPDATE rookery.leases SET expires_at = now() - interval '1 second'")
        .execute(db.store.pool())
        .await
        .unwrap();
    assert_eq!(
        leases.inspect("fleet/sync").await.unwrap().unwrap().holder,
        None
    );
    assert_eq!(
        leases
            .acquire("fleet/sync", "plugin:fleet/a", ttl)
            .await
            .unwrap()
            .fence,
        3
    );
    db.close().await;
}

#[tokio::test]
async fn topics_deliver_in_order_with_dead_letters() {
    use rookery_store::topics::{From, MAX_ATTEMPTS, Next};
    let db = test_db!();
    let topics = db.store.topics();
    topics
        .publish("hosts/studio", &json!(0), "system:x")
        .await
        .unwrap();
    topics
        .subscribe("hosts/studio", "inbox", From::Latest)
        .await
        .unwrap();
    assert_eq!(
        topics.next("hosts/studio", "inbox").await.unwrap(),
        Next::Empty
    );
    let one = topics
        .publish("hosts/studio", &json!(1), "system:x")
        .await
        .unwrap();
    let two = topics
        .publish("hosts/studio", &json!(2), "system:x")
        .await
        .unwrap();
    assert!(topics.ack("hosts/studio", "inbox", two).await.is_err());
    assert!(
        topics
            .nack("hosts/studio", "inbox", two, None, Duration::ZERO)
            .await
            .is_err()
    );
    let Next::Message(m, 1) = topics.next("hosts/studio", "inbox").await.unwrap() else {
        panic!("the first message")
    };
    assert_eq!(m.seq, one);
    // A nack waits, then delivers the same message again.
    assert!(
        !topics
            .nack(
                "hosts/studio",
                "inbox",
                one,
                Some("busy"),
                Duration::from_secs(60)
            )
            .await
            .unwrap()
    );
    assert!(matches!(
        topics.next("hosts/studio", "inbox").await.unwrap(),
        Next::Wait(_)
    ));
    sqlx::query("UPDATE rookery.topic_subscriptions SET not_before = NULL")
        .execute(db.store.pool())
        .await
        .unwrap();
    let Next::Message(m, 2) = topics.next("hosts/studio", "inbox").await.unwrap() else {
        panic!("the first message again")
    };
    assert_eq!(m.seq, one);
    for _ in 2..MAX_ATTEMPTS {
        topics
            .nack("hosts/studio", "inbox", one, Some("broken"), Duration::ZERO)
            .await
            .unwrap();
    }
    assert!(
        topics
            .nack("hosts/studio", "inbox", one, Some("broken"), Duration::ZERO)
            .await
            .unwrap(),
        "dead after the last delivery"
    );
    let Next::Message(m, 1) = topics.next("hosts/studio", "inbox").await.unwrap() else {
        panic!("the second message")
    };
    assert_eq!(m.seq, two);
    topics.ack("hosts/studio", "inbox", two).await.unwrap();
    assert_eq!(
        topics.next("hosts/studio", "inbox").await.unwrap(),
        Next::Empty
    );
    let dead = topics.dead_letters("hosts/studio", None).await.unwrap();
    assert_eq!(
        (
            dead.len(),
            dead[0].payload.clone(),
            dead[0].error.as_deref()
        ),
        (1, json!(1), Some("broken"))
    );
    // Delivered again to its subscription, it comes before anything newer.
    topics.retry("hosts/studio", "inbox", one).await.unwrap();
    topics
        .publish("hosts/studio", &json!(3), "system:x")
        .await
        .unwrap();
    let Next::Message(m, _) = topics.next("hosts/studio", "inbox").await.unwrap() else {
        panic!("the dead letter")
    };
    assert_eq!(m.seq, one);
    topics.ack("hosts/studio", "inbox", one).await.unwrap();
    assert!(
        topics
            .dead_letters("hosts/studio", None)
            .await
            .unwrap()
            .is_empty()
    );
    let (head, count, subscriptions, dead) = topics.inspect("hosts/studio").await.unwrap();
    assert_eq!((count, subscriptions[0].lag, dead), (4, 1, 0));
    assert!(head > two);
    // A subscription from the start sees everything kept.
    topics
        .subscribe("hosts/studio", "audit", From::Start)
        .await
        .unwrap();
    let Next::Message(m, _) = topics.next("hosts/studio", "audit").await.unwrap() else {
        panic!("the first kept")
    };
    assert_eq!(m.payload, json!(0));
    db.close().await;
}

#[tokio::test]
async fn notifications_mute_and_answer() {
    use rookery_store::notify::New;
    let db = test_db!();
    let user = person(&db.store, "maria").await;
    let notify = db.store.notify();
    let actions = ["approve".to_owned(), "deny".to_owned()];
    let new = |kind| New {
        user,
        kind,
        title: "Apply studio?",
        body: None,
        data: None,
        actions: Some(&actions),
        sent_by: "system:jobs",
    };
    let sent = notify.send(new("apply")).await.unwrap().unwrap();
    assert_eq!(notify.unread(user).await.unwrap(), 1);
    assert!(notify.answer(user, sent.id, "maybe").await.is_err());
    let answered = notify.answer(user, sent.id, "approve").await.unwrap();
    assert!(answered.read_at.is_some());
    assert!(notify.answer(user, sent.id, "deny").await.is_err(), "once");
    notify.set_pref(user, "digest", true).await.unwrap();
    assert!(notify.send(new("digest")).await.unwrap().is_none());
    notify.send(new("apply")).await.unwrap().unwrap();
    assert_eq!(notify.read(user, None).await.unwrap(), 1);
    assert_eq!(notify.list(user, true, None, 10).await.unwrap().len(), 0);
    assert_eq!(notify.list(user, false, None, 10).await.unwrap().len(), 2);
    db.close().await;
}

#[tokio::test]
async fn a_cancel_request_wins_before_the_attempt_finishes() {
    let db = test_db!();
    let jobs = db.store.jobs();
    let made = jobs.enqueue(job("apply", &json!({}), 1)).await.unwrap();
    jobs.claim(&["apply".into()], "w1", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    jobs.cancel(made.id).await.unwrap();
    let done = jobs
        .finish(made.id, "w1", Outcome::Succeeded(json!("done")))
        .await
        .unwrap();
    db.close().await;
    assert_eq!(done.state, "cancelled");
    assert!(done.result.is_none());
}

#[tokio::test]
async fn concurrent_log_writes_each_leave_a_line() {
    let db = test_db!();
    let jobs = db.store.jobs();
    let made = jobs.enqueue(job("apply", &json!({}), 1)).await.unwrap();
    let mut writers = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let store = db.store.clone();
        writers.spawn(async move { store.jobs().log(made.id, "info", "a line", None).await });
    }
    let mut lines = Vec::new();
    while let Some(result) = writers.join_next().await {
        lines.push(result.unwrap());
    }
    let log = jobs.log_after(made.id, 0, 100).await.unwrap();
    db.close().await;
    assert!(lines.iter().all(Result::is_ok), "{lines:?}");
    assert_eq!(log.len(), 32);
}

#[tokio::test]
async fn rekey_authenticates_rows_even_when_key_ids_match() {
    use rookery_store::{seal::Key, secrets::Scope};

    let db = test_db!();
    let fake = |n: u32| Key::from_bytes(n.to_be_bytes().repeat(8).try_into().unwrap());
    let old = fake(47333);
    let new = fake(113831);
    assert_eq!(old.id(), new.id());
    let old_store = rookery_store::Store::connect(rookery_store::StoreOptions {
        secret_key: Some(old.clone()),
        ..rookery_store::StoreOptions::new(&db.url)
    })
    .await
    .unwrap();
    let new_store = rookery_store::Store::connect(rookery_store::StoreOptions {
        secret_key: Some(new.clone()),
        ..rookery_store::StoreOptions::new(&db.url)
    })
    .await
    .unwrap();
    old_store
        .secrets()
        .set("fleet", "api", Scope::Global, b"fixture")
        .await
        .unwrap();
    let moved = old_store.secrets().rekey(&old, &new).await.unwrap();
    let opened = new_store.secrets().get("fleet", "api", Scope::Global).await;
    old_store.close().await;
    new_store.close().await;
    db.close().await;
    assert_eq!(moved.secrets, 1);
    assert_eq!(opened.unwrap().as_deref(), Some(b"fixture".as_slice()));
}

#[tokio::test]
async fn topic_publications_commit_in_cursor_order() {
    use rookery_store::topics::{From, Next};
    let db = test_db!();
    let pool = db.store.pool();
    let topics = db.store.topics();
    topics
        .subscribe("fleet/hosts", "inbox", From::Start)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE FUNCTION rookery.delay_first_message() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 IF NEW.payload = '1'::jsonb THEN PERFORM pg_advisory_xact_lock(1234); END IF;
                 RETURN NEW;
             END
         $$;
         CREATE TRIGGER delay_first_message BEFORE INSERT ON rookery.topic_messages
         FOR EACH ROW EXECUTE FUNCTION rookery.delay_first_message()",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(1234)")
        .execute(&mut *gate)
        .await
        .unwrap();
    let first = {
        let store = db.store.clone();
        tokio::spawn(async move {
            store
                .topics()
                .publish("fleet/hosts", &json!(1), "plugin:fleet")
                .await
                .unwrap()
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND objid = 1234 AND NOT granted
                 AND database = (SELECT oid FROM pg_database WHERE datname = current_database()))",
            ).fetch_one(pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let mut second = {
        let store = db.store.clone();
        tokio::spawn(async move {
            store
                .topics()
                .publish("fleet/hosts", &json!(2), "plugin:fleet")
                .await
                .unwrap()
        })
    };
    let premature = tokio::time::timeout(Duration::from_millis(500), &mut second).await;
    let visible = topics.next("fleet/hosts", "inbox").await.unwrap();
    gate.commit().await.unwrap();
    let first = first.await.unwrap();
    let second = match premature {
        Ok(second) => second.unwrap(),
        Err(_) => second.await.unwrap(),
    };
    db.close().await;
    assert!(first < second);
    assert_eq!(visible, Next::Empty);
}
