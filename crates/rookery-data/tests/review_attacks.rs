//! Review: attacks on the access rules of data.* (not for merge).

use std::time::Duration;

use rookery_data::plugins::{self, PluginData};
use rookery_kernel::testing::{Inbox, TestKernel};
use rookery_kernel::{Caller, Methods, RpcError, test_kernel};
use rookery_store::Caller as StoreCaller;
use rookery_store::spaces::NewSpace;
use serde_json::{Value, json};

fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_data::register(&mut methods);
    methods
}

fn uid(caller: &Caller) -> uuid::Uuid {
    caller.user().unwrap().id.parse().unwrap()
}

fn code(result: Result<Value, RpcError>) -> String {
    match result {
        Ok(_) => "ok".into(),
        Err(e) => e.error_code().as_str().to_string(),
    }
}

async fn apply(k: &TestKernel, plugin: &str, collections: Value) {
    plugins::apply(
        &k.kernel,
        &PluginData {
            plugin: plugin.into(),
            collections,
            ..Default::default()
        },
        &plugins::NoTransforms,
    )
    .await
    .expect("apply");
}

async fn next_change(inbox: &mut Inbox, secs: u64) -> Option<Value> {
    tokio::time::timeout(
        Duration::from_secs(secs),
        inbox.notification("data.changed"),
    )
    .await
    .ok()
}

async fn space_of(k: &TestKernel, owner: &Caller) -> uuid::Uuid {
    k.db.store
        .spaces()
        .create(
            &StoreCaller::user(uid(owner)),
            &NewSpace {
                kind: "project".into(),
                name: "Secret".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
}

/// A plugin reads another plugin's rows through `include`, because its caller is an admin.
#[tokio::test]
async fn attack_plugin_reads_another_plugins_rows_through_include() {
    let k: TestKernel = test_kernel!(methods());
    apply(
        &k,
        "notes",
        json!([{
            "name": "notes", "scope": "global",
            "entity": { "scheme": "note", "title": "title" },
            "access": { "read": "manage", "write": "manage" },
            "fields": [{ "name": "title", "type": "text" }, { "name": "secret", "type": "text" }]
        }]),
    )
    .await;
    apply(
        &k,
        "evil",
        json!([{
            "name": "probe", "scope": "global",
            "fields": [{ "name": "target", "type": "ref" }]
        }]),
    )
    .await;
    let notes = Caller::Plugin("notes".into());
    let evil = Caller::Plugin("evil".into());
    let row = k
        .call(
            &notes,
            "data.insert",
            json!({ "collection": "notes", "values": { "title": "t", "secret": "hunter2" } }),
        )
        .await
        .unwrap()["row"]
        .clone();
    let uri = format!("note://{}", row["id"].as_str().unwrap());
    // Direct reads of notes' collection are refused.
    assert_eq!(
        code(
            k.call(
                &evil,
                "data.find",
                json!({ "plugin": "notes", "collection": "notes" })
            )
            .await
        ),
        "FORBIDDEN"
    );
    let ins = k
        .call(
            &evil,
            "data.insert",
            json!({ "collection": "probe", "values": { "target": uri } }),
        )
        .await;
    println!("evil insert: {ins:?}");
    let page = k
        .call(
            &evil,
            "data.find",
            json!({ "collection": "probe", "query": { "include": ["target"] } }),
        )
        .await
        .unwrap();
    println!("evil page: {page}");
    assert!(
        page["included"].get(&uri).is_none(),
        "LEAK: evil read notes' row {}",
        page["included"][&uri]
    );
    k.close().await;
}

/// A member follows Rookery's admin-only collections by passing `after`, which skips the first read.
#[tokio::test]
async fn attack_live_with_after_skips_the_admin_check() {
    let k: TestKernel = test_kernel!(methods());
    let eve = k.person("Eve", false).await;
    let mut inbox = k.connect(&eve);
    let started = k
        .call_on(
            &inbox,
            "data.live",
            json!({ "plugin": "rookery", "collection": "users", "after": 0 }),
        )
        .await;
    println!("live users after 0: {started:?}");
    let without = k
        .call_on(
            &k.connect(&eve),
            "data.live",
            json!({ "plugin": "rookery", "collection": "users" }),
        )
        .await;
    println!("live users without after: {}", code(without));
    let _bob = k.person("Bob", false).await;
    let got = next_change(&mut inbox, 3).await;
    println!("eve got: {got:?}");
    assert!(
        started.is_err() && got.is_none(),
        "LEAK: a member follows rookery/users"
    );
    k.close().await;
}

/// A member follows jobs on hosts they hold nothing on: every change's id, op, rev and time.
#[tokio::test]
async fn attack_live_on_jobs_streams_metadata_of_invisible_rows() {
    let k: TestKernel = test_kernel!(methods());
    let eve = k.person("Eve", false).await;
    let mut inbox = k.connect(&eve);
    k.call_on(
        &inbox,
        "data.live",
        json!({ "plugin": "rookery", "collection": "jobs" }),
    )
    .await
    .unwrap();
    k.db.store
        .jobs()
        .enqueue(rookery_store::jobs::NewJob {
            kind: "apply",
            params: &json!({}),
            max_attempts: 1,
            run_after: None,
            subject: Some("env://secret"),
            enqueued_by: "system:rookery",
        })
        .await
        .unwrap();
    let got = next_change(&mut inbox, 3).await;
    println!("eve got: {got:?}");
    assert!(got.is_none(), "LEAK: metadata of a job Eve may not see");
    k.close().await;
}

/// A member removed from a space keeps receiving its changes.
#[tokio::test]
async fn attack_live_keeps_streaming_after_revocation() {
    let k: TestKernel = test_kernel!(methods());
    apply(
        &k,
        "notes",
        json!([{
            "name": "notes", "scope": "space",
            "access": { "read": "view", "write": "use" },
            "fields": [{ "name": "title", "type": "text" }]
        }]),
    )
    .await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    let s = space_of(&k, &ada).await;
    k.db.store
        .spaces()
        .add_member(
            &StoreCaller::user(uid(&ada)),
            s,
            &format!("user:{}", uid(&bob)),
            rookery_store::Level::View,
        )
        .await
        .unwrap();
    let at = json!({ "plugin": "notes", "collection": "notes", "space": s.to_string() });
    let mut inbox = k.connect(&bob);
    k.call_on(&inbox, "data.live", at.clone()).await.unwrap();
    k.db.store
        .access()
        .revoke(&format!("space://{s}"), &format!("user:{}", uid(&bob)))
        .await
        .unwrap();
    let mut ins = at.clone();
    ins["values"] = json!({ "title": "after the revoke" });
    k.call(&ada, "data.insert", ins).await.unwrap();
    let got = next_change(&mut inbox, 3).await;
    println!("bob got after revoke: {got:?}");
    assert!(got.is_none(), "LEAK: a revoked member keeps following");
    k.close().await;
}

/// A collection whose rows need manage: a member with view on the space sees them anyway through the
/// entity index (search, entities.*, activity, text).
#[tokio::test]
async fn attack_entity_index_ignores_the_collections_read_level() {
    let k: TestKernel = test_kernel!(methods());
    apply(
        &k,
        "hr",
        json!([{
            "name": "reviews", "scope": "space",
            "entity": { "scheme": "review", "title": "title" },
            "access": { "read": "manage", "write": "manage" },
            "history": true,
            "fields": [
                { "name": "title", "type": "text", "search": true },
                { "name": "body", "type": "longtext", "search": true }
            ]
        }]),
    )
    .await;
    let ada = k.person("Ada", false).await;
    let bob = k.person("Bob", false).await;
    let s = space_of(&k, &ada).await;
    k.db.store
        .spaces()
        .add_member(
            &StoreCaller::user(uid(&ada)),
            s,
            &format!("user:{}", uid(&bob)),
            rookery_store::Level::View,
        )
        .await
        .unwrap();
    let at = json!({ "plugin": "hr", "collection": "reviews", "space": s.to_string() });
    let mut ins = at.clone();
    ins["values"] = json!({ "title": "Bob underperforms", "body": "fire on friday" });
    let row = k.call(&ada, "data.insert", ins).await.unwrap()["row"].clone();
    let uri = format!("review://{}", row["id"].as_str().unwrap());
    println!(
        "bob data.find: {}",
        code(k.call(&bob, "data.find", at.clone()).await)
    );
    let mut get = at.clone();
    get["id"] = row["id"].clone();
    println!("bob data.get: {:?}", k.call(&bob, "data.get", get).await);
    let mut leaks = Vec::new();
    for (m, p) in [
        ("entities.resolve", json!({ "uri": uri })),
        ("entities.search", json!({ "text": "underp" })),
        ("search.query", json!({ "text": "friday" })),
        ("entities.text", json!({ "uris": [uri] })),
        ("activity.recent", json!({ "space": s.to_string() })),
        ("recents.touch", json!({ "kind": "entity", "key": uri })),
    ] {
        let r = k.call(&bob, m, p).await;
        let s = format!("{r:?}");
        println!("bob {m}: {s}");
        if s.contains("underperforms")
            || s.contains("fire on friday")
            || s.contains("\"recorded\": Bool(true)")
        {
            leaks.push(m);
        }
    }
    assert!(leaks.is_empty(), "LEAK through {leaks:?}");
    k.close().await;
}

/// Cost: a member reads rookery/jobs with 200k jobs over 5,000 hosts, holding view on 50 of them.
#[tokio::test]
async fn attack_cost_typed_table_reads_at_scale() {
    let k: TestKernel = test_kernel!(methods());
    let ops = k.person("Ops", false).await;
    let pool = k.db.store.pool();
    for n in [1000, 10000] {
        let t = std::time::Instant::now();
        sqlx::query(
            "INSERT INTO rookery.jobs (id, kind, params, state, subject, enqueued_by)
             SELECT gen_random_uuid(), 'apply', '{}', 'succeeded', 'env://h' || (i % 5000), 'system:rookery'
             FROM generate_series(1, $1) i",
        )
        .bind(n)
        .execute(pool)
        .await
        .unwrap();
        println!(
            "one INSERT of {n} jobs with the capture trigger: {:?}",
            t.elapsed()
        );
    }
    let t = std::time::Instant::now();
    sqlx::query("DELETE FROM rookery.jobs WHERE id IN (SELECT id FROM rookery.jobs LIMIT 10000)")
        .execute(pool)
        .await
        .unwrap();
    println!(
        "one DELETE of 10000 jobs with the capture trigger: {:?}",
        t.elapsed()
    );
    sqlx::raw_sql(
        "ALTER TABLE rookery.jobs DISABLE TRIGGER rookery_capture;
         INSERT INTO rookery.jobs (id, kind, params, state, subject, enqueued_by)
         SELECT gen_random_uuid(), 'apply', '{}', 'succeeded', 'env://h' || (i % 5000), 'system:rookery'
         FROM generate_series(1, 200000) i;
         ALTER TABLE rookery.jobs ENABLE TRIGGER rookery_capture;",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO rookery.grants (resource, principal, level, created_by)
         SELECT 'env://h' || i, $1, 1, 'system:test' FROM generate_series(1, 50) i",
    )
    .bind(format!("user:{}", uid(&ops)))
    .execute(pool)
    .await
    .unwrap();
    // Other people's grants, so the table is not tiny.
    sqlx::query(
        "INSERT INTO rookery.grants (resource, principal, level, created_by)
         SELECT 'env://h' || (i % 5000), 'user:' || gen_random_uuid(), 1, 'system:test' FROM generate_series(1, 20000) i
         ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE").execute(pool).await.unwrap();
    let rookery = |c: &str| json!({ "plugin": "rookery", "collection": c });
    for (m, p) in [
        (
            "data.find",
            json!({ "plugin": "rookery", "collection": "jobs", "query": { "limit": 50 } }),
        ),
        ("data.count", rookery("jobs")),
        (
            "data.find",
            json!({ "plugin": "rookery", "collection": "jobs", "query": { "limit": 50, "where": { "subject": "env://h7" } } }),
        ),
    ] {
        let t = std::time::Instant::now();
        let r = k.call(&ops, m, p.clone()).await;
        println!(
            "{m} {p}: {:?} -> {}",
            t.elapsed(),
            format!("{r:?}").chars().take(120).collect::<String>()
        );
    }
    let plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN (ANALYZE, BUFFERS) SELECT t.id FROM rookery.\"v_jobs\" t WHERE TRUE AND t.deleted_at IS NULL
         AND (t._owner IS NOT DISTINCT FROM $1::uuid AND t._owner IS NOT NULL OR (t._resource IS NOT NULL AND
         rookery.effective_level(t._resource, $1::uuid, (SELECT rookery.principals_of($1::uuid, $2))) >= 1))
         ORDER BY t.id LIMIT 51",
    )
    .bind(uid(&ops))
    .bind(format!("user:{}", uid(&ops)))
    .fetch_all(pool)
    .await
    .unwrap();
    println!("PLAN\n{}", plan.join("\n"));
    k.close().await;
}
