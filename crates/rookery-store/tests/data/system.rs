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
