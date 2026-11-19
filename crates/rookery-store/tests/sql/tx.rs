//! SQL transactions with a lease: they commit only while the caller still holds the
//! lease with its token, and their writes roll back when it is refused. The lease table is the kernel's, so the
//! check runs on the kernel's own connection, never the plugin's role's.

use std::time::Duration;

use rookery_proto::ErrorCode;
use rookery_store::sql::exec::LeaseFence;
use rookery_store::testing::TestDb;
use serde_json::{Value, json};

use super::fixtures::setup;

const LONG: Duration = Duration::from_secs(30);
const TTL: Duration = Duration::from_millis(300);

/// The holder a plugin's own worker is: `plugin:<id>/<worker>`, as `leases.*` names it.
fn host_holder(plugin: &str) -> String {
    format!("plugin:{plugin}/host")
}

/// A lease as Rookery's `leases.*` holds it: `<plugin>/<key>`, with its fence as the token.
struct Grant {
    token: i64,
    holder: String,
}

async fn acquire(db: &TestDb, plugin: &str, key: &str, holder: &str, ttl: Duration) -> Grant {
    let lease = db
        .store
        .leases()
        .acquire(&format!("{plugin}/{key}"), holder, ttl)
        .await
        .expect("the lease is free");
    Grant {
        token: lease.fence,
        holder: lease.holder.unwrap(),
    }
}

/// Releases `<plugin>/<key>` as `holder` at `token`.
async fn release(
    db: &rookery_store::Store,
    plugin: &str,
    key: &str,
    token: i64,
    holder: &str,
) -> bool {
    db.leases()
        .release(&format!("{plugin}/{key}"), holder, token)
        .await
        .unwrap()
}

fn fence(key: &str, grant: &Grant) -> LeaseFence {
    LeaseFence {
        key: key.into(),
        token: grant.token,
        holder: grant.holder.clone(),
    }
}

/// The rows of `x_<plugin>.t`, read as the kernel (the plugin's transaction is the one being tested).
async fn n(db: &TestDb, plugin: &str) -> i64 {
    let schema = db.store.sql().names(plugin).unwrap().raw_schema;
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select count(*) from {schema}.t"
    )))
    .fetch_one(db.store.pool())
    .await
    .unwrap()
}

/// The messages on a topic, as the kernel sees them.
async fn messages(db: &TestDb, topic: &str) -> i64 {
    sqlx::query_scalar("select count(*) from rookery.topic_messages where topic = $1")
        .bind(topic)
        .fetch_one(db.store.pool())
        .await
        .unwrap()
}

/// The plugin's own table, made as the plugin's role.
async fn table(db: &TestDb, plugin: &str) {
    db.store
        .sql()
        .execute(plugin, "create table if not exists t (n int)", &[])
        .await
        .unwrap();
}

#[tokio::test]
async fn a_transaction_under_a_held_lease_commits() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    table(&db, "ledger").await;
    let holder = host_holder("ledger");
    let grant = acquire(&db, "ledger", "indexer", &holder, LONG).await;
    let tx = db
        .store
        .sql()
        .begin("ledger", Some(fence("indexer", &grant)))
        .await
        .unwrap();
    db.store
        .sql()
        .tx_execute("ledger", &tx, "insert into t values (1)", &[])
        .await
        .unwrap();
    // Not visible before the commit, and the lease row is untouched by the commit on the plugin's connection.
    assert_eq!(n(&db, "ledger").await, 0);
    db.store.sql().commit("ledger", &tx).await.unwrap();
    assert_eq!(n(&db, "ledger").await, 1);
    // The lease is still the caller's after a fenced commit, and can be used again.
    let again = db
        .store
        .sql()
        .begin("ledger", Some(fence("indexer", &grant)))
        .await
        .unwrap();
    db.store
        .sql()
        .tx_execute("ledger", &again, "insert into t values (2)", &[])
        .await
        .unwrap();
    db.store.sql().commit("ledger", &again).await.unwrap();
    assert_eq!(n(&db, "ledger").await, 2);
    db.close().await;
}

#[tokio::test]
async fn a_released_lease_refuses_the_commit_and_rolls_the_writes_back() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    table(&db, "ledger").await;
    let holder = host_holder("ledger");
    let grant = acquire(&db, "ledger", "indexer", &holder, LONG).await;
    let tx = db
        .store
        .sql()
        .begin("ledger", Some(fence("indexer", &grant)))
        .await
        .unwrap();
    db.store
        .sql()
        .tx_execute("ledger", &tx, "insert into t values (1)", &[])
        .await
        .unwrap();
    assert!(release(&db.store, "ledger", "indexer", grant.token, &holder).await);
    let e = db.store.sql().commit("ledger", &tx).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Conflict, "{e}");
    // The same shape a fenced batch gives.
    let details = e.details().expect("details");
    assert_eq!(details["condition"], json!("lease"));
    assert_eq!(details["index"], json!(0));
    assert_eq!(n(&db, "ledger").await, 0, "the writes are gone");
    // A transaction rolled back this way is closed: it cannot be committed later.
    assert_eq!(
        db.store
            .sql()
            .commit("ledger", &tx)
            .await
            .unwrap_err()
            .code(),
        ErrorCode::NotFound
    );
    db.close().await;
}

#[tokio::test]
async fn an_expired_lease_refuses_the_commit_even_when_nobody_took_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    table(&db, "ledger").await;
    let holder = host_holder("ledger");
    let grant = acquire(&db, "ledger", "indexer", &holder, TTL).await;
    let tx = db
        .store
        .sql()
        .begin("ledger", Some(fence("indexer", &grant)))
        .await
        .unwrap();
    db.store
        .sql()
        .tx_execute("ledger", &tx, "insert into t values (1)", &[])
        .await
        .unwrap();
    tokio::time::sleep(TTL + Duration::from_millis(100)).await;
    let e = db.store.sql().commit("ledger", &tx).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Conflict, "{e}");
    assert_eq!(n(&db, "ledger").await, 0);
    db.close().await;
}

#[tokio::test]
async fn a_lease_taken_over_by_another_holder_refuses_the_commit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    table(&db, "ledger").await;
    let first = acquire(&db, "ledger", "indexer", &host_holder("ledger"), TTL).await;
    let tx = db
        .store
        .sql()
        .begin("ledger", Some(fence("indexer", &first)))
        .await
        .unwrap();
    db.store
        .sql()
        .tx_execute("ledger", &tx, "insert into t values (1)", &[])
        .await
        .unwrap();
    tokio::time::sleep(TTL + Duration::from_millis(100)).await;
    let second = acquire(&db, "ledger", "indexer", "plugin:ledger/r9", LONG).await;
    assert!(second.token > first.token);
    let e = db.store.sql().commit("ledger", &tx).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Conflict, "{e}");
    assert_eq!(n(&db, "ledger").await, 0);
    db.close().await;
}

#[tokio::test]
async fn a_lease_that_is_not_the_callers_is_not_opened() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    let grant = acquire(&db, "ledger", "indexer", "plugin:ledger/r1", LONG).await;
    for wrong in [
        // Another holder's token.
        LeaseFence {
            key: "indexer".into(),
            token: grant.token,
            holder: host_holder("ledger"),
        },
        // A token nobody holds.
        LeaseFence {
            key: "indexer".into(),
            token: grant.token + 1,
            holder: "plugin:ledger/r1".into(),
        },
        // A key nobody holds.
        LeaseFence {
            key: "nightly".into(),
            token: grant.token,
            holder: "plugin:ledger/r1".into(),
        },
    ] {
        let e = db
            .store
            .sql()
            .begin("ledger", Some(wrong))
            .await
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Conflict, "{e}");
        assert_eq!(e.details().unwrap()["condition"], json!("lease"));
    }
    // No transaction was left open by a refused begin.
    db.store
        .sql()
        .begin("ledger", None)
        .await
        .expect("nothing is open");
    db.close().await;
}

/// A topic message in a transaction: written and
/// numbered when the transaction commits, at most once, with the rest of the transaction; gone on rollback, and
/// not written at all when a lease refuses the commit. The write runs on the kernel's connection, never the
/// plugin role's.
#[tokio::test]
async fn a_published_message_is_written_when_the_transaction_commits_and_never_otherwise() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    table(&db, "ledger").await;
    let topic = "ledger.audit";
    let sql = db.store.sql();
    // Committed: the message and the row are there, numbered as `topics.publish` numbers its first.
    let tx = sql.begin("ledger", None).await.unwrap();
    sql.tx_execute("ledger", &tx, "insert into t values (1)", &[])
        .await
        .unwrap();
    sql.tx_publish("ledger", &tx, topic, &json!({ "n": 1 }), "plugin:ledger")
        .await
        .unwrap();
    assert_eq!(messages(&db, topic).await, 0, "nothing before the commit");
    sql.commit("ledger", &tx).await.unwrap();
    assert_eq!(messages(&db, topic).await, 1);
    assert_eq!(n(&db, "ledger").await, 1);
    let (seq, payload, by): (i64, Value, String) = sqlx::query_as(
        "select seq, payload, published_by from rookery.topic_messages where topic = $1",
    )
    .bind(topic)
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    assert_eq!(
        (seq, payload, by),
        (1, json!({ "n": 1 }), "plugin:ledger".into())
    );
    // Rolled back: the message is dropped with the transaction, and the sequence does not move.
    let tx = sql.begin("ledger", None).await.unwrap();
    sql.tx_execute("ledger", &tx, "insert into t values (2)", &[])
        .await
        .unwrap();
    sql.tx_publish("ledger", &tx, topic, &json!({ "n": 2 }), "plugin:ledger")
        .await
        .unwrap();
    sql.rollback("ledger", &tx).await.unwrap();
    assert_eq!(messages(&db, topic).await, 1);
    // A lease that refuses the commit publishes nothing, and the row is rolled back with it.
    let grant = acquire(&db, "ledger", "indexer", &host_holder("ledger"), LONG).await;
    let tx = sql
        .begin("ledger", Some(fence("indexer", &grant)))
        .await
        .unwrap();
    sql.tx_execute("ledger", &tx, "insert into t values (3)", &[])
        .await
        .unwrap();
    sql.tx_publish("ledger", &tx, topic, &json!({ "n": 3 }), "plugin:ledger")
        .await
        .unwrap();
    release(&db.store, "ledger", "indexer", grant.token, &grant.holder).await;
    assert_eq!(
        sql.commit("ledger", &tx).await.unwrap_err().code(),
        ErrorCode::Conflict
    );
    assert_eq!(messages(&db, topic).await, 1);
    assert_eq!(n(&db, "ledger").await, 1);
    // A second held commit continues the topic's sequence, one message per publish.
    let grant = acquire(&db, "ledger", "indexer", &host_holder("ledger"), LONG).await;
    let tx = sql
        .begin("ledger", Some(fence("indexer", &grant)))
        .await
        .unwrap();
    sql.tx_publish("ledger", &tx, topic, &json!({ "n": 4 }), "plugin:ledger")
        .await
        .unwrap();
    sql.tx_publish("ledger", &tx, topic, &json!({ "n": 5 }), "plugin:ledger")
        .await
        .unwrap();
    sql.commit("ledger", &tx).await.unwrap();
    assert_eq!(messages(&db, topic).await, 3);
    let seqs: Vec<i64> =
        sqlx::query_scalar("select seq from rookery.topic_messages where topic = $1 order by seq")
            .bind(topic)
            .fetch_all(db.store.pool())
            .await
            .unwrap();
    assert_eq!(seqs, vec![1, 2, 3]);
    db.close().await;
}

/// The most messages one transaction may queue, and the most bytes.
#[tokio::test]
async fn a_transaction_caps_the_messages_it_carries() {
    use rookery_store::sql::exec::{MAX_TX_PUBLISH_BYTES, MAX_TX_PUBLISHES};
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    let sql = db.store.sql();
    let tx = sql.begin("ledger", None).await.unwrap();
    for i in 0..MAX_TX_PUBLISHES {
        sql.tx_publish(
            "ledger",
            &tx,
            "ledger.audit",
            &json!({ "n": i }),
            "plugin:ledger",
        )
        .await
        .unwrap();
    }
    let e = sql
        .tx_publish(
            "ledger",
            &tx,
            "ledger.audit",
            &json!({ "n": 0 }),
            "plugin:ledger",
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid, "{e}");
    sql.rollback("ledger", &tx).await.unwrap();
    let tx = sql.begin("ledger", None).await.unwrap();
    let big = "x".repeat(MAX_TX_PUBLISH_BYTES / 3 + 1);
    for _ in 0..2 {
        sql.tx_publish("ledger", &tx, "ledger.audit", &json!(big), "plugin:ledger")
            .await
            .unwrap();
    }
    let e = sql
        .tx_publish("ledger", &tx, "ledger.audit", &json!(big), "plugin:ledger")
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid, "{e}");
    sql.rollback("ledger", &tx).await.unwrap();
    db.close().await;
}

/// The one window the design states: when the kernel's own transaction dies after the
/// plugin's transaction committed, the plugin's data stays committed while the caller sees a failed commit. The
/// test finds the kernel connection by the lock it holds on the lease row and ends it, with the plugin's COMMIT
/// held open by a deferred trigger, which is exactly the state a dying kernel would leave.
#[tokio::test]
async fn a_kernel_death_after_the_plugins_commit_leaves_its_data_committed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    table(&db, "ledger").await;
    let sql = db.store.sql();
    sql.execute(
        "ledger",
        "create function slow() returns trigger language plpgsql as $$ \
         begin perform pg_sleep(1.5); return new; end $$",
        &[],
    )
    .await
    .unwrap();
    sql.execute(
        "ledger",
        "create constraint trigger slow_commit after insert on t deferrable initially deferred \
         for each row execute function slow()",
        &[],
    )
    .await
    .unwrap();
    let grant = acquire(&db, "ledger", "indexer", &host_holder("ledger"), LONG).await;
    let tx = sql
        .begin("ledger", Some(fence("indexer", &grant)))
        .await
        .unwrap();
    sql.tx_execute("ledger", &tx, "insert into t values (1)", &[])
        .await
        .unwrap();
    let committing = {
        let store = db.store.clone();
        let tx = tx.clone();
        tokio::spawn(async move { store.sql().commit("ledger", &tx).await })
    };
    // The kernel's connection is the one holding a lock on `rookery.leases` until the two-step commit ends.
    let mut killer = None;
    for _ in 0..100 {
        let pid: Option<i32> = sqlx::query_scalar(
            "select pid from pg_locks where relation = 'rookery.leases'::regclass \
             and mode = 'RowShareLock' and pid <> pg_backend_pid() limit 1",
        )
        .fetch_optional(db.store.pool())
        .await
        .unwrap();
        if let Some(pid) = pid {
            killer = Some(pid);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = killer.expect("the kernel's transaction holds the lease row");
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(db.store.pool())
        .await
        .unwrap();
    let e = committing.await.unwrap().unwrap_err();
    assert_eq!(e.code(), ErrorCode::Unavailable, "{e}");
    assert!(
        e.to_string().contains("the transaction committed"),
        "the caller is told what happened: {e}"
    );
    // The plugin's write is there: the window, stated rather than hidden.
    assert_eq!(n(&db, "ledger").await, 1);
    db.close().await;
}

/// The window the design states: the lease row is locked by the kernel's transaction across the plugin's
/// commit, so a release or a takeover waits for it and cannot land between the check and the commit. Here the
/// lock is taken and released by hand, around a statement: while it is held, both wait.
#[tokio::test]
async fn the_leases_row_is_locked_for_the_whole_two_step_commit() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "ledger", true).await;
    let grant = acquire(&db, "ledger", "indexer", &host_holder("ledger"), LONG).await;
    let mut kernel = db.store.pool().acquire().await.unwrap();
    sqlx::query("begin").execute(&mut *kernel).await.unwrap();
    let live: bool = sqlx::query_scalar(
        "select expires_at > clock_timestamp() from rookery.leases \
         where name = $1 and fence = $2 and holder = $3 for update",
    )
    .bind("ledger/indexer")
    .bind(grant.token)
    .bind(&grant.holder)
    .fetch_one(&mut *kernel)
    .await
    .unwrap();
    assert!(live);
    // A release does not get past the lock.
    let s = db.store.clone();
    let holder = grant.holder.clone();
    let token = grant.token;
    let waiting =
        tokio::spawn(async move { release(&s, "ledger", "indexer", token, &holder).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished(), "the release waits for the commit");
    sqlx::query("commit").execute(&mut *kernel).await.unwrap();
    assert!(waiting.await.unwrap());
    drop(kernel);
    db.close().await;
}
