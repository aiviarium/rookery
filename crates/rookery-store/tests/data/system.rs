//! Rookery's own data as collections of the plugin `rookery`: typed tables read through the same
//! queries, access rules, change feed and history as a plugin's collections, and storage and layouts
//! as native collections.

use std::time::Duration;

use rookery_proto::ErrorCode;
use rookery_store::collections::Query;
use rookery_store::feed::{FeedEvent, Subscription};
use rookery_store::kv::KvScope;
use rookery_store::testing::TestDb;
use rookery_store::{Caller, Level};
use serde_json::{Value, json};

async fn next_change(sub: &mut Subscription) -> std::sync::Arc<rookery_store::feed::Change> {
    match tokio::time::timeout(Duration::from_secs(10), sub.next())
        .await
        .expect("the feed timed out")
        .unwrap()
        .unwrap()
    {
        FeedEvent::Change(c) => c,
        FeedEvent::Reset { head } => panic!("unexpected reset at {head}"),
    }
}

async fn enqueue(db: &TestDb, kind: &str, subject: Option<&str>, by: &str) -> uuid::Uuid {
    db.store
        .jobs()
        .enqueue(rookery_store::jobs::NewJob {
            kind,
            params: &json!({}),
            max_attempts: 1,
            run_after: None,
            subject,
            enqueued_by: by,
        })
        .await
        .unwrap()
        .id
}

fn kinds(rows: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .map(|r| r["kind"].as_str().unwrap().to_owned())
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn typed_tables_are_read_with_the_access_rules_of_collections() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ops = db.user("ops").await;
    let dev = db.user("dev").await;
    enqueue(&db, "apply", Some("env://studio"), "system:rookery").await;
    enqueue(&db, "apply", Some("env://forge"), "system:rookery").await;
    enqueue(&db, "mine", None, &format!("user:{dev}")).await;
    let data = db.store.data("rookery");
    let find = |caller: Caller| {
        let data = db.store.data("rookery");
        async move {
            data.find(&caller, "jobs", &Query::default())
                .await
                .map(|p| kinds(&p.rows))
        }
    };
    // An admin sees every job; a person the jobs they enqueued and those on what they hold view on.
    assert_eq!(
        find(Caller::system()).await.unwrap(),
        ["apply", "apply", "mine"]
    );
    assert_eq!(find(Caller::user(dev)).await.unwrap(), ["mine"]);
    assert!(find(Caller::user(ops)).await.unwrap().is_empty());
    db.store
        .access()
        .grant(
            "env://studio",
            &format!("user:{ops}"),
            Level::View.as_i16(),
            "system:test",
        )
        .await
        .unwrap();
    assert_eq!(find(Caller::user(ops)).await.unwrap(), ["apply"]);
    let filtered = data
        .find(
            &Caller::system(),
            "jobs",
            &serde_json::from_value(json!({ "where": { "subject": "env://forge" } })).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(filtered.rows.len(), 1);
    assert_eq!(
        data.count(&Caller::system(), "jobs", &Query::default())
            .await
            .unwrap(),
        3
    );
    // Writes stay with the table's own code.
    let refused = data
        .insert(&Caller::system(), "jobs", json!({ "kind": "x" }))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), ErrorCode::Forbidden);
    // A table for admins refuses everyone else; one of a person's own shows them their own rows.
    assert_eq!(
        data.find(&Caller::user(ops), "users", &Query::default())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    assert_eq!(
        data.find(&Caller::system(), "users", &Query::default())
            .await
            .unwrap()
            .rows
            .len(),
        2
    );
    db.store
        .notify()
        .send(rookery_store::notify::New {
            user: ops,
            kind: "apply",
            title: "Apply studio?",
            body: None,
            data: None,
            actions: None,
            sent_by: "system:rookery",
        })
        .await
        .unwrap();
    let mine = data
        .find(&Caller::user(ops), "notifications", &Query::default())
        .await
        .unwrap();
    assert_eq!(mine.rows.len(), 1);
    assert_eq!(mine.rows[0]["title"], "Apply studio?");
    assert_eq!(mine.rows[0]["userId"], ops.to_string());
    assert!(
        data.find(&Caller::user(dev), "notifications", &Query::default())
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    // Sealed columns are no fields.
    let connections = db
        .store
        .collections()
        .get("rookery", "connections")
        .unwrap();
    assert!(connections.def.field("ciphertext").is_none());
    assert!(connections.def.field("config").is_some());
    db.close().await;
}

#[tokio::test]
async fn writes_to_typed_tables_reach_the_change_feed_and_history() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ops = db.user("ops").await;
    let mut sub = db.store.feed().subscribe().await.unwrap();
    let job = enqueue(&db, "apply", Some("env://studio"), "system:rookery").await;
    let inserted = next_change(&mut sub).await;
    assert_eq!(
        (inserted.plugin.as_str(), inserted.collection.as_str()),
        ("rookery", "jobs")
    );
    assert_eq!(inserted.id, job);
    assert_eq!(inserted.row.as_ref().unwrap()["state"], "queued");
    db.store.jobs().cancel(job).await.unwrap();
    let cancelled = next_change(&mut sub).await;
    assert_eq!(cancelled.id, job);
    assert_eq!(cancelled.op, rookery_store::feed::ChangeOp::Update);
    assert!(cancelled.rev > inserted.rev);
    assert_eq!(cancelled.row.as_ref().unwrap()["state"], "cancelled");
    // A change of a plugin's collection and one of a typed table share one order.
    db.store.users().set_role(ops, true).await.unwrap();
    let role = next_change(&mut sub).await;
    assert_eq!(role.collection, "users");
    assert!(role.seq > cancelled.seq);
    let history = db
        .store
        .data("rookery")
        .history(&Caller::system(), "users", ops, 10)
        .await
        .unwrap();
    assert_eq!(history[0].op, "update");
    assert_eq!(history[0].changes["role"], json!(["member", "admin"]));
    db.close().await;
}

#[tokio::test]
async fn storage_and_layouts_are_rows_of_rookerys_collections() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ops = db.user("ops").await;
    let space = db.store.spaces().personal(ops).await.unwrap().id;
    let kv = db.store.kv();
    let mut sub = db.store.feed().subscribe().await.unwrap();
    assert_eq!(
        kv.set("fleet", None, "cursor", &json!(1), Some(0))
            .await
            .unwrap(),
        1
    );
    let change = next_change(&mut sub).await;
    assert_eq!(
        (change.plugin.as_str(), change.collection.as_str()),
        ("rookery", "storage")
    );
    assert_eq!(change.row.as_ref().unwrap()["value"], 1);
    assert_eq!(
        kv.set("fleet", None, "cursor", &json!(2), Some(1))
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        kv.set("fleet", None, "cursor", &json!(3), Some(1))
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
    // Each scope keeps its own keys.
    kv.set(
        "fleet",
        KvScope::Space(space),
        "cursor",
        &json!("space"),
        None,
    )
    .await
    .unwrap();
    kv.set("fleet", Some(ops), "cursor", &json!("mine"), None)
        .await
        .unwrap();
    assert_eq!(
        kv.get("fleet", None, "cursor")
            .await
            .unwrap()
            .unwrap()
            .value,
        2
    );
    assert_eq!(
        kv.get("fleet", KvScope::Space(space), "cursor")
            .await
            .unwrap()
            .unwrap()
            .value,
        "space"
    );
    assert_eq!(
        kv.get("fleet", Some(ops), "cursor")
            .await
            .unwrap()
            .unwrap()
            .value,
        "mine"
    );
    assert!(kv.get("agents", None, "cursor").await.unwrap().is_none());
    // The rows are a collection like any other, for admins.
    let rows = db
        .store
        .data("rookery")
        .find(
            &Caller::system().in_space(space),
            "storageSpace",
            &Query::default(),
        )
        .await
        .unwrap();
    assert_eq!(rows.rows[0]["key"], "cursor");
    assert_eq!(
        db.store
            .data("rookery")
            .find(&Caller::user(ops), "storage", &Query::default())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    let layout = db.store.layout();
    assert_eq!(
        layout
            .put(ops, "default", &json!({"panes": 1}), Some(0))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        layout
            .put(ops, "default", &json!({"panes": 2}), Some(1))
            .await
            .unwrap(),
        2
    );
    assert!(
        layout
            .put(ops, "default", &json!({"panes": 3}), Some(1))
            .await
            .is_err()
    );
    assert_eq!(
        layout.get(ops, "default").await.unwrap().unwrap(),
        (json!({"panes": 2}), 2)
    );
    db.close().await;
}

#[tokio::test]
async fn the_old_key_value_and_layout_tables_move_into_the_collections() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ops = db.user("ops").await;
    sqlx::raw_sql(
        "CREATE TABLE rookery.kv (plugin text, scope text, user_id uuid, key text, value jsonb, revision bigint, updated_at timestamptz DEFAULT now());
         CREATE TABLE rookery.layouts (user_id uuid, name text, value jsonb, revision bigint, updated_at timestamptz DEFAULT now());",
    )
    .execute(db.store.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO rookery.kv VALUES ('box', 'global', NULL, 'seen', '\"yes\"', 4), ('box', 'user', $1, 'mine', '1', 2)")
        .bind(ops)
        .execute(db.store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO rookery.layouts VALUES ($1, 'default', '{\"panes\": 1}', 3)")
        .bind(ops)
        .execute(db.store.pool())
        .await
        .unwrap();
    db.store.collections().install_system(&[]).await.unwrap();
    let kv = db.store.kv();
    let seen = kv.get("box", None, "seen").await.unwrap().unwrap();
    assert_eq!((seen.value, seen.revision), (json!("yes"), 4));
    assert_eq!(
        kv.get("box", Some(ops), "mine")
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        db.store
            .layout()
            .get(ops, "default")
            .await
            .unwrap()
            .unwrap()
            .1,
        3
    );
    let gone: Option<String> = sqlx::query_scalar("SELECT to_regclass('rookery.kv')::text")
        .fetch_one(db.store.pool())
        .await
        .unwrap();
    assert!(gone.is_none());
    db.close().await;
}

/// At a realistic size, 100k jobs over 5,000 hosts: one statement writes or deletes 20k rows with the trigger
/// in a moment, and a member with view on 50 hosts reads and counts theirs without the other rows' cost.
#[tokio::test]
async fn typed_tables_scale_to_many_rows_and_bulk_writes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let ops = db.user("ops").await;
    let pool = db.store.pool();
    let insert = |n: i32| {
        sqlx::query(
            "INSERT INTO rookery.jobs (id, kind, params, state, subject, enqueued_by)
             SELECT gen_random_uuid(), 'apply', '{}', 'succeeded', 'env://h' || (i % 5000), 'system:rookery'
             FROM generate_series(1, $1) i",
        )
        .bind(n)
        .execute(pool)
    };
    let head = db.store.feed().head();
    let t = std::time::Instant::now();
    insert(20_000).await.unwrap();
    let bulk = t.elapsed();
    let t = std::time::Instant::now();
    sqlx::query("DELETE FROM rookery.jobs")
        .execute(pool)
        .await
        .unwrap();
    let delete = t.elapsed();
    assert!(
        bulk < Duration::from_secs(5) && delete < Duration::from_secs(5),
        "a bulk insert took {bulk:?} and a bulk delete {delete:?}"
    );
    let logged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rookery.changes WHERE plugin = 'rookery' AND collection = 'jobs' AND seq > $1",
    )
    .bind(head)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(logged, 40_000, "each row written is in the change log");
    insert(100_000).await.unwrap();
    sqlx::query(
        "INSERT INTO rookery.grants (resource, principal, level, created_by)
         SELECT 'env://h' || i, $1, 1, 'system:test' FROM generate_series(1, 50) i",
    )
    .bind(format!("user:{ops}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO rookery.grants (resource, principal, level, created_by)
         SELECT 'env://h' || (i % 5000), 'user:' || gen_random_uuid(), 1, 'system:test'
         FROM generate_series(1, 20000) i ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE").execute(pool).await.unwrap();
    let data = db.store.data("rookery");
    let member = Caller::user(ops);
    let query = |v: Value| serde_json::from_value::<Query>(v).unwrap();
    let t = std::time::Instant::now();
    let page = data
        .find(&member, "jobs", &query(json!({ "limit": 50 })))
        .await
        .unwrap();
    let count = data
        .count(&member, "jobs", &Query::default())
        .await
        .unwrap();
    let one = data
        .find(
            &member,
            "jobs",
            &query(json!({ "limit": 50, "where": { "subject": "env://h7" } })),
        )
        .await
        .unwrap();
    let reads = t.elapsed();
    println!("bulk insert {bulk:?}, bulk delete {delete:?}, three reads {reads:?}");
    assert_eq!(page.rows.len(), 50);
    // 100k jobs over 5,000 hosts: 20 a host, 50 hosts.
    assert_eq!(count, 1_000);
    assert_eq!(one.rows.len(), 20);
    assert!(
        reads < Duration::from_secs(1),
        "three reads of a member took {reads:?}"
    );
    db.close().await;
}

/// A typed table's history names who wrote, from the call's actor, and the system without one.
#[tokio::test]
async fn the_history_of_typed_tables_names_the_actor() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let settings = db.store.settings();
    rookery_store::actor::scope("user:ada".into(), async {
        db.store.access().create_group("ops", None).await.unwrap();
    })
    .await;
    settings
        .set(
            "notes",
            "theme",
            rookery_store::settings::Scope::Global,
            &json!("dark"),
            "user:bob",
        )
        .await
        .unwrap();
    db.store.access().create_group("dev", None).await.unwrap();
    let principals: Vec<(String, String)> = sqlx::query_as(
        "SELECT collection, principal FROM rookery.history WHERE plugin = 'rookery'
         AND collection IN ('groups', 'settings') ORDER BY id",
    )
    .fetch_all(db.store.pool())
    .await
    .unwrap();
    assert_eq!(
        principals,
        [
            ("groups".to_string(), "user:ada".to_string()),
            ("settings".to_string(), "user:bob".to_string()),
            ("groups".to_string(), "system:rookery".to_string()),
        ]
    );
    db.close().await;
}

/// People written into the table without signing in, as an upgrade or an import leaves them, get their
/// entity, its grant and their personal space, and the entity holds no email.
#[tokio::test]
async fn people_from_before_get_their_entity_and_space() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO rookery.users (id, issuer, subject, name, email, role) VALUES ($1, 'test', 'old', 'Old', 'old@example.test', 'member')",
    )
    .bind(id)
    .execute(db.store.pool())
    .await
    .unwrap();
    assert_eq!(db.store.users().backfill().await.unwrap(), 1);
    assert_eq!(db.store.users().backfill().await.unwrap(), 0);
    let entity = db
        .store
        .entities()
        .get(&Caller::user(db.user("eve").await), &format!("user://{id}"))
        .await
        .unwrap()
        .expect("everyone sees a person's entity");
    assert_eq!(entity.title, "Old");
    assert_eq!(entity.subtitle, None);
    let personal = db.store.spaces().personal(id).await.unwrap();
    assert_eq!(personal.owner_id, Some(id));
    db.close().await;
}

#[tokio::test]
async fn a_typed_change_committed_before_listen_is_delivered() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = db.store.pool();
    // Hold the pool while the subscriber starts its listener so LISTEN cannot race this insert.
    let mut connections = Vec::new();
    for _ in 0..pool.options().get_max_connections() {
        connections.push(pool.acquire().await.unwrap());
    }
    let mut sub = db.store.feed().subscribe_from(db.store.feed().head());
    let job: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO rookery.jobs (id, kind, params, state, enqueued_by)
         VALUES (gen_random_uuid(), 'apply', '{}', 'queued', 'system:rookery') RETURNING id",
    )
    .fetch_one(&mut **connections.last_mut().unwrap())
    .await
    .unwrap();
    drop(connections);
    let change = next_change(&mut sub).await;
    assert_eq!(change.id, job);
    assert_eq!(change.collection, "jobs");
    db.close().await;
}
