use super::common::defs;
use rookery_proto::ErrorCode;
use rookery_store::Caller;
use rookery_store::collections::{
    BatchOp, Query, conditions::BatchCondition, operations::FieldOps,
};
use rookery_store::testing::TestDb;
use serde_json::{Value, json};
use uuid::Uuid;

fn schema() -> Vec<rookery_store::collections::CollectionDef> {
    defs(json!([
        {"name":"assets","scope":"global","history":true,"trash":true,"fields":[
            {"name":"key","type":"text","unique":true,"caseInsensitive":true},
            {"name":"n","type":"integer","default":0,"min":0,"max":1000},
            {"name":"tags","type":"refs","to":"tag"},
            {"name":"state","type":"enum","values":["new","ready","done"],"default":"new","transitions":{"new":["ready"],"ready":["done"]}}
        ]}
    ]))
}
fn admin() -> Caller {
    Caller::plugin("fixture").as_admin()
}
fn row_id(row: &Value) -> Uuid {
    Uuid::parse_str(row["id"].as_str().unwrap()).unwrap()
}
fn object(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().unwrap().clone()
}
fn field_ops(v: Value) -> FieldOps {
    serde_json::from_value(v).unwrap()
}

#[tokio::test]
async fn binary_additive_ddl_roundtrip_limits_history_and_filters() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    let c = admin();
    let mut defs = schema();
    s.collections().apply("datafix", &defs).await.unwrap();
    let before = s
        .data("datafix")
        .insert(&c, "assets", json!({"key":"before"}))
        .await
        .unwrap()
        .row
        .unwrap();
    defs[0]
        .fields
        .push(serde_json::from_value(json!({"name":"raw","type":"bytes","maxBytes":4})).unwrap());
    s.collections().apply("datafix", &defs).await.unwrap();
    assert_eq!(
        s.data("datafix")
            .get(&c, "assets", row_id(&before))
            .await
            .unwrap()
            .unwrap()["raw"],
        Value::Null
    );
    let row = s
        .data("datafix")
        .insert(&c, "assets", json!({"key":"binary","raw":"AAD/gA=="}))
        .await
        .unwrap()
        .row
        .unwrap();
    let id = row_id(&row);
    assert_eq!(row["raw"], "AAD/gA==");
    assert_eq!(
        s.data("datafix")
            .get(&c, "assets", id)
            .await
            .unwrap()
            .unwrap()["raw"],
        row["raw"]
    );
    assert!(
        s.data("datafix")
            .insert(&c, "assets", json!({"raw":"AAAAAAA="}))
            .await
            .unwrap_err()
            .details()
            .unwrap()["constraint"]
            == "maxBytes"
    );
    let hist = s
        .data("datafix")
        .history(&c, "assets", id, 10)
        .await
        .unwrap();
    assert_eq!(hist[0].changes["raw"], json!({"changed":true}));
    let q: Query = serde_json::from_value(json!({"where":{"raw":{"ne":null}}})).unwrap();
    let page = s.data("datafix").find(&c, "assets", &q).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.rows[0].get("raw").is_none());
    let bad: Query = serde_json::from_value(json!({"where":{"raw":"AAD/gA=="}})).unwrap();
    assert!(s.data("datafix").find(&c, "assets", &bad).await.is_err());
    let migrations: Vec<rookery_store::collections::PluginMigration> = serde_json::from_value(json!([{"version":1,"steps":[{"op":"changeType","collection":"assets","field":{"name":"raw","type":"text"}}]}])).unwrap();
    assert!(
        s.collections()
            .migrate(
                "datafix",
                &migrations,
                &rookery_store::collections::NoTransforms
            )
            .await
            .is_err()
    );
    assert_eq!(
        s.data("datafix")
            .get(&c, "assets", id)
            .await
            .unwrap()
            .unwrap()["raw"],
        row["raw"]
    );
    db.close().await;
}

#[tokio::test]
async fn concurrent_upsert_is_one_row_and_updates_always_advance_revision() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    s.collections().apply("datafix", &schema()).await.unwrap();
    let c = admin();
    let on = vec!["key".into()];
    let data = s.data("datafix");
    let (a, b) = tokio::join!(
        data.upsert(&c, "assets", json!({"key":"A"}), &on),
        data.upsert(&c, "assets", json!({"key":"a"}), &on)
    );
    let a = a.unwrap().row.unwrap();
    let b = b.unwrap().row.unwrap();
    assert_eq!(a["id"], b["id"]);
    assert_eq!(
        s.data("datafix")
            .count(&c, "assets", &Query::default())
            .await
            .unwrap(),
        1
    );
    let same = s
        .data("datafix")
        .upsert(&c, "assets", json!({"key":"a"}), &on)
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(same["rev"], 3);
    assert!(
        data.upsert(&c, "assets", json!({"n":1}), &["n".into()])
            .await
            .is_err()
    );
    assert!(
        data.upsert(&c, "assets", json!({"key":"a","state":"done"}), &on)
            .await
            .is_err()
    );
    db.close().await;
}

#[tokio::test]
async fn concurrent_operations_do_not_lose_increments_and_constraints_apply() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    let c = admin();
    s.collections().apply("datafix", &schema()).await.unwrap();
    let row = s
        .data("datafix")
        .insert(&c, "assets", json!({"key":"counter"}))
        .await
        .unwrap()
        .row
        .unwrap();
    let id = row_id(&row);
    let ops = field_ops(json!({"n":{"inc":1},"tags":{"add":["a"]}}));
    let data = s.data("datafix");
    let (a, b) = tokio::join!(
        data.update_ops(&c, "assets", id, json!({}), None, &ops),
        data.update_ops(&c, "assets", id, json!({}), None, &ops)
    );
    a.unwrap();
    b.unwrap();
    let row = data.get(&c, "assets", id).await.unwrap().unwrap();
    assert_eq!(row["n"], 2);
    assert_eq!(row["tags"], json!(["tag://a"]));
    let e = data
        .update_ops(
            &c,
            "assets",
            id,
            json!({}),
            None,
            &field_ops(json!({"n":{"inc":-3}})),
        )
        .await
        .unwrap_err();
    assert_eq!(e.details().unwrap()["constraint"], "min");
    let e = data
        .update_ops(&c, "assets", id, json!({}), Some(1), &ops)
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Conflict);
    db.close().await;
}

#[tokio::test]
async fn batches_commit_or_rollback_writes_messages_and_jobs() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    let c = admin();
    s.collections().apply("datafix", &schema()).await.unwrap();
    let condition: BatchCondition =
        serde_json::from_value(json!({"lease":{"key":"indexer","token":42}})).unwrap();
    sqlx::query("INSERT INTO rookery.leases (name,holder,fence,expires_at) VALUES ('datafix/indexer','holder',42,now()+interval '1 minute')").execute(s.pool()).await.unwrap();
    let job = rookery_store::collections::BatchJob {
        kind: "datafix/work".into(),
        params: json!({"id":1}),
        subject: None,
        max_attempts: 1,
    };
    let ops = vec![
        BatchOp::Insert {
            collection: "assets".into(),
            values: object(json!({"key":"batch"})),
        },
        BatchOp::Publish {
            topic: "datafix/indexed".into(),
            payload: json!({"id":1}),
        },
        BatchOp::Enqueue { job },
        BatchOp::Update {
            collection: "assets".into(),
            id: Uuid::nil(),
            patch: object(json!({"n":1})),
            rev: Some(7),
            ops: Default::default(),
        },
    ];
    let head = s.feed().head();
    // Missing row fails after all three mutations: the transaction still leaves nothing behind.
    assert!(
        s.data("datafix")
            .batch_if(&c, &ops, Some(&condition), "holder")
            .await
            .is_err()
    );
    assert_eq!(
        s.data("datafix")
            .count(&c, "assets", &Query::default())
            .await
            .unwrap(),
        0
    );
    for table in ["topic_messages", "jobs"] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM rookery.{table}"
        )))
        .fetch_one(s.pool())
        .await
        .unwrap();
        assert_eq!(n, 0);
    }
    assert_eq!(s.feed().head(), head);
    let e = s
        .data("datafix")
        .batch_if(&c, &ops[..3], Some(&condition), "other holder")
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Conflict);
    assert_eq!(e.details().unwrap()["index"], 0);
    let out = s
        .data("datafix")
        .batch_if(&c, &ops[..3], Some(&condition), "holder")
        .await
        .unwrap();
    assert_eq!(out.rows.len(), 3);
    assert_eq!(out.changes.len(), 1);
    assert_eq!(out.jobs.len(), 1);
    for table in ["topic_messages", "jobs"] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM rookery.{table}"
        )))
        .fetch_one(s.pool())
        .await
        .unwrap();
        assert_eq!(n, 1);
    }
    // A revision condition fails after an insert, publish and enqueue too.
    let mut fail = ops.clone();
    if let BatchOp::Update { id, .. } = &mut fail[3] {
        *id = row_id(out.rows[0].as_ref().unwrap());
    }
    if let BatchOp::Insert { values, .. } = &mut fail[0] {
        values.insert("key".into(), json!("rolledback"));
    }
    let e = s
        .data("datafix")
        .batch_if(&c, &fail, None, "holder")
        .await
        .unwrap_err();
    assert_eq!(e.details().unwrap()["index"], 3);
    assert_eq!(
        s.data("datafix")
            .count(&c, "assets", &Query::default())
            .await
            .unwrap(),
        1
    );
    db.close().await;
}

#[tokio::test]
async fn vectors_require_extension_and_roundtrip_with_hnsw_index() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    let c = admin();
    let available: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name='vector')",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    let mut defs = schema();
    s.collections().apply("datafix", &defs).await.unwrap();
    defs[0].fields.push(
        serde_json::from_value(
            json!({"name":"embedding","type":"vector","dimensions":3,"metric":"cosine","index":"hnsw"}),
        )
        .unwrap(),
    );
    if !available {
        let e = s.collections().apply("datafix", &defs).await.unwrap_err();
        let message = e.to_string();
        for part in ["datafix", "assets", "embedding", "pgvector"] {
            assert!(message.contains(part));
        }
        // A failed vector declaration leaves an ordinary collection usable.
        s.data("datafix")
            .insert(&c, "assets", json!({"key":"still usable"}))
            .await
            .unwrap();
        eprintln!("SKIP vector roundtrip: pgvector genuinely unavailable in test database");
        db.close().await;
        return;
    }
    let collections = s.collections();
    let (own, other) = tokio::join!(
        collections.apply("datafix", &defs),
        collections.apply("otherfix", &defs)
    );
    own.unwrap();
    other.unwrap();
    let row = s
        .data("datafix")
        .insert(
            &c,
            "assets",
            json!({"key":"vector","embedding":[0.25,-1,3.5]}),
        )
        .await
        .unwrap()
        .row
        .unwrap();
    assert_eq!(row["embedding"], json!([0.25, -1.0, 3.5]));
    assert_eq!(
        s.data("datafix")
            .get(&c, "assets", row_id(&row))
            .await
            .unwrap()
            .unwrap()["embedding"],
        row["embedding"]
    );
    assert!(
        s.data("datafix")
            .insert(&c, "assets", json!({"embedding":[1,2]}))
            .await
            .is_err()
    );
    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE schemaname='p_datafix'")
            .fetch_all(s.pool())
            .await
            .unwrap();
    assert!(
        indexes
            .iter()
            .any(|s| s.contains("USING hnsw") && s.contains("vector_cosine_ops"))
    );
    let mut changed = defs.clone();
    if let rookery_store::collections::FieldType::Vector { dimensions, .. } =
        &mut changed[0].fields.last_mut().unwrap().ty
    {
        *dimensions = 4;
    }
    assert!(s.collections().apply("datafix", &changed).await.is_err());
    eprintln!("pgvector DDL, migration, storage and HNSW test ran");
    db.close().await;
}

#[tokio::test]
async fn a_vector_field_without_index_gets_hnsw_and_none_stays_unindexed() {
    use rookery_store::collections::VectorIndex;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    let available: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name='vector')",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    if !available {
        eprintln!("SKIP vector index default: pgvector genuinely unavailable in test database");
        db.close().await;
        return;
    }
    let wanted = defs(json!([{"name":"assets","scope":"global","fields":[
        {"name":"auto","type":"vector","dimensions":4},
        {"name":"exact","type":"vector","dimensions":4,"index":"none"}]}]));
    // `index` stays out of the stored definition, and resolves to hnsw for the vector field.
    assert!(
        serde_json::to_value(&wanted[0].fields[0])
            .unwrap()
            .get("index")
            .is_none()
    );
    s.collections().apply("indexfix", &wanted).await.unwrap();
    assert_eq!(wanted[0].fields[0].vector_index(), Some(VectorIndex::Hnsw));
    assert_eq!(wanted[0].fields[1].vector_index(), Some(VectorIndex::None));
    let hnsw = || async {
        let indexes: Vec<String> =
            sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE schemaname='p_indexfix'")
                .fetch_all(s.pool())
                .await
                .unwrap();
        indexes
            .into_iter()
            .filter(|i| i.contains("USING hnsw"))
            .collect::<Vec<_>>()
    };
    let found = hnsw().await;
    assert_eq!(
        found.len(),
        1,
        "only the field without `index` is indexed: {found:?}"
    );
    assert!(
        found[0].contains("(auto "),
        "the default index is on `auto`: {}",
        found[0]
    );

    // Applying the same manifest again changes nothing.
    let again = s.collections().apply("indexfix", &wanted).await.unwrap();
    assert!(
        again.created.is_empty() && again.altered.is_empty(),
        "{again:?}"
    );

    // An existing collection whose stored definition kept an explicit `hnsw` is not re-indexed when the
    // manifest drops the key, and one stored before the key existed (no `index` at all) is not either.
    let legacy = defs(json!([{"name":"assets","scope":"global","fields":[
        {"name":"auto","type":"vector","dimensions":4,"index":"hnsw"},
        {"name":"exact","type":"vector","dimensions":4,"index":"none"}]}]));
    sqlx::query("UPDATE rookery.plugin_schemas SET collections = $1 WHERE plugin = 'indexfix'")
        .bind(serde_json::to_value(&legacy).unwrap())
        .execute(s.pool())
        .await
        .unwrap();
    let with_key = s.collections().apply("indexfix", &wanted).await.unwrap();
    assert!(
        with_key.created.is_empty() && with_key.altered.is_empty(),
        "{with_key:?}"
    );
    sqlx::query(
        "UPDATE rookery.plugin_schemas SET collections = jsonb_set(collections, '{0,fields,0}', \
         (collections->0->'fields'->0) - 'index') WHERE plugin = 'indexfix'",
    )
    .execute(s.pool())
    .await
    .unwrap();
    let older = s.collections().apply("indexfix", &wanted).await.unwrap();
    assert!(
        older.created.is_empty() && older.altered.is_empty(),
        "{older:?}"
    );
    assert_eq!(hnsw().await.len(), 1);
    eprintln!("vector index default and none test ran");
    db.close().await;
}

#[tokio::test]
async fn blob_fields_keep_live_blobs_release_on_delete_and_reacquire_on_restore() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    let c = admin();
    let mut defs = schema();
    s.collections().apply("datafix", &defs).await.unwrap();
    defs[0]
        .fields
        .push(serde_json::from_value(json!({"name":"source","type":"blob"})).unwrap());
    s.collections().apply("datafix", &defs).await.unwrap();
    let blob = s
        .blobs()
        .put(
            b"\x00\xffblob",
            "application/octet-stream",
            None,
            "system:test",
        )
        .await
        .unwrap();
    let row = s
        .data("datafix")
        .insert(&c, "assets", json!({"key":"blob","source":blob.id}))
        .await
        .unwrap()
        .row
        .unwrap();
    let id = row_id(&row);
    assert!(s.blobs().referenced(blob.id).await.unwrap());
    s.data("datafix").delete(&c, "assets", id).await.unwrap();
    assert!(!s.blobs().referenced(blob.id).await.unwrap());
    s.data("datafix").restore(&c, "assets", id).await.unwrap();
    assert!(s.blobs().referenced(blob.id).await.unwrap());
    s.data("datafix")
        .update(&c, "assets", id, json!({"source":null}), None)
        .await
        .unwrap();
    assert!(!s.blobs().referenced(blob.id).await.unwrap());
    s.data("datafix")
        .update(&c, "assets", id, json!({"source":blob.id}), None)
        .await
        .unwrap();
    s.data("datafix").delete(&c, "assets", id).await.unwrap();
    // A blob that is gone by the time its row comes back out of the trash refuses the restore.
    sqlx::query("DELETE FROM rookery.blobs WHERE id = $1")
        .bind(blob.id)
        .execute(s.pool())
        .await
        .unwrap();
    assert_eq!(
        s.data("datafix")
            .restore(&c, "assets", id)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
    s.data("datafix").purge(&c, "assets", id).await.unwrap();
    assert!(
        s.data("datafix")
            .get_trashed(&c, "assets", id)
            .await
            .unwrap()
            .is_none()
    );
    db.close().await;
}

#[tokio::test]
async fn lease_expiry_while_waiting_for_change_ordering_rolls_everything_back() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let s = db.store.clone();
    let c = admin();
    s.collections().apply("datafix", &schema()).await.unwrap();
    let mut blocker = s.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(rookery_store::feed::SEQ_LOCK)
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("INSERT INTO rookery.leases (name,holder,fence,expires_at) VALUES ('datafix/indexer','holder',42,clock_timestamp()+interval '2 seconds')").execute(s.pool()).await.unwrap();
    let store = s.clone();
    let batch = tokio::spawn(async move {
        let condition: BatchCondition =
            serde_json::from_value(json!({"lease":{"key":"indexer","token":42}})).unwrap();
        store
            .data("datafix")
            .batch_if(
                &c,
                &[
                    BatchOp::Insert {
                        collection: "assets".into(),
                        values: object(json!({"key":"expired"})),
                    },
                    BatchOp::Publish {
                        topic: "datafix/indexed".into(),
                        payload: json!({}),
                    },
                    BatchOp::Enqueue {
                        job: rookery_store::collections::BatchJob {
                            kind: "datafix/work".into(),
                            params: json!({}),
                            subject: None,
                            max_attempts: 1,
                        },
                    },
                ],
                Some(&condition),
                "holder",
            )
            .await
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event='advisory')").fetch_one(s.pool()).await.unwrap();
        if waiting {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "batch never waited for ordering lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    sqlx::query("SELECT pg_sleep(greatest(0, extract(epoch FROM (SELECT expires_at FROM rookery.leases))-extract(epoch FROM clock_timestamp()))+0.01)").execute(s.pool()).await.unwrap();
    blocker.commit().await.unwrap();
    assert_eq!(
        batch.await.unwrap().unwrap_err().code(),
        ErrorCode::Conflict
    );
    for table in ["topic_messages", "jobs", "changes"] {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM rookery.{table}"
        )))
        .fetch_one(s.pool())
        .await
        .unwrap();
        assert_eq!(count, 0);
    }
    assert_eq!(
        s.data("datafix")
            .count(&admin(), "assets", &Query::default())
            .await
            .unwrap(),
        0
    );
    db.close().await;
}
