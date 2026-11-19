//! SQL migrations of a plugin (`migrations/sql/NNNN_name.sql`): exactly once, in order, as the plugin's role,
//! and a failing file leaves nothing behind and is named.

use rookery_proto::ErrorCode;
use rookery_store::sql::migrations::{self, SqlMigration};
use rookery_store::testing::TestDb;

use super::fixtures::setup;

fn files(list: &[(&str, &str)]) -> Vec<SqlMigration> {
    migrations::order(
        list.iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect(),
    )
    .expect("file names")
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(db.store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn migrations_run_in_order_once_and_as_the_role() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let f = files(&[
        (
            "0002_seed.sql",
            "insert into things (n) select g from generate_series(1, 3) g; create index on things (n);",
        ),
        (
            "0001_init.sql",
            "create table things (n int); create function who() returns text language sql as 'select current_user::text';",
        ),
    ]);
    assert_eq!(db.store.sql().migrate("alpha", &f).await.unwrap(), [1, 2]);
    assert_eq!(count(&db, "select count(*) from x_alpha.things").await, 3);
    // The objects are the role's own, so are reachable by it and by nobody else.
    let owner: String = sqlx::query_scalar(
        "select pg_get_userbyid(relowner)::text from pg_class where oid = 'x_alpha.things'::regclass",
    )
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    assert_eq!(owner, db.store.sql().names("alpha").unwrap().role);
    let q = db
        .store
        .sql()
        .query("alpha", "select who() as w", &[])
        .await
        .unwrap();
    assert_eq!(q.rows[0]["w"], owner);
    // Again: nothing is repeated (a repeat would fail on `create table`, and add rows).
    assert!(
        db.store
            .sql()
            .migrate("alpha", &f)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(count(&db, "select count(*) from x_alpha.things").await, 3);
    // A later file runs alone.
    let mut g = f.clone();
    g.extend(files(&[(
        "0003_more.sql",
        "insert into things values (99)",
    )]));
    assert_eq!(db.store.sql().migrate("alpha", &g).await.unwrap(), [3]);
    assert_eq!(count(&db, "select count(*) from x_alpha.things").await, 4);
    assert_eq!(
        count(
            &db,
            "select count(*) from rookery.plugin_sql_migrations where plugin = 'alpha'"
        )
        .await,
        3
    );
    db.close().await;
}

#[tokio::test]
async fn two_kernels_applying_the_same_migrations_apply_them_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let other = db.second_store().await.unwrap();
    let f = files(&[
        ("0001_init.sql", "create table things (n int)"),
        (
            "0002_slow.sql",
            "select pg_sleep(0.5); insert into things values (1)",
        ),
    ]);
    let (s1, s2) = (db.store.sql(), other.sql());
    let (a, b) = tokio::join!(s1.migrate("alpha", &f), s2.migrate("alpha", &f));
    let (mut ran, mut also) = (a.unwrap(), b.unwrap());
    ran.append(&mut also);
    ran.sort();
    assert_eq!(ran, [1, 2], "each file ran in exactly one of the kernels");
    assert_eq!(count(&db, "select count(*) from x_alpha.things").await, 1);
    other.close().await;
    db.close().await;
}

#[tokio::test]
async fn a_failing_migration_is_rolled_back_named_and_retried_when_fixed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let bad = files(&[
        ("0001_init.sql", "create table things (n int)"),
        (
            "0002_broken.sql",
            "create table half (n int); insert into things values (1); insert into missing values (1)",
        ),
        ("0003_after.sql", "create table after (n int)"),
    ]);
    let e = db.store.sql().migrate("alpha", &bad).await.unwrap_err();
    assert!(e.to_string().contains("0002_broken.sql"), "{e}");
    assert!(!e.to_string().contains("0001_init"), "{e}");
    assert_eq!(e.code(), ErrorCode::Invalid);
    // File 1 stands; file 2 left nothing; file 3 never ran.
    assert_eq!(
        count(
            &db,
            "select count(*) from pg_tables where schemaname = 'x_alpha' and tablename = 'things'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &db,
            "select count(*) from pg_tables where schemaname = 'x_alpha' and tablename in ('half', 'after')"
        )
        .await,
        0
    );
    assert_eq!(count(&db, "select count(*) from x_alpha.things").await, 0);
    assert_eq!(
        count(
            &db,
            "select count(*) from rookery.plugin_sql_migrations where plugin = 'alpha'"
        )
        .await,
        1
    );
    // The same failure again, since nothing was recorded for it.
    assert!(
        db.store
            .sql()
            .migrate("alpha", &bad)
            .await
            .unwrap_err()
            .to_string()
            .contains("0002_broken.sql")
    );
    // A fixed file at that version runs.
    let fixed = files(&[
        ("0001_init.sql", "create table things (n int)"),
        ("0002_broken.sql", "create table half (n int)"),
        ("0003_after.sql", "create table after (n int)"),
    ]);
    assert_eq!(
        db.store.sql().migrate("alpha", &fixed).await.unwrap(),
        [2, 3]
    );
    db.close().await;
}

#[tokio::test]
async fn a_changed_applied_migration_is_refused_by_name() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    db.store
        .sql()
        .migrate(
            "alpha",
            &files(&[("0001_init.sql", "create table things (n int)")]),
        )
        .await
        .unwrap();
    let e = db
        .store
        .sql()
        .migrate(
            "alpha",
            &files(&[("0001_init.sql", "create table things (n int, extra int)")]),
        )
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("0001_init.sql") && e.to_string().contains("changed"),
        "{e}"
    );
    db.close().await;
}

#[tokio::test]
async fn a_migration_that_committed_but_was_not_recorded_is_not_run_again() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let f = files(&[(
        "0001_init.sql",
        "create table things (n int); insert into things values (1)",
    )]);
    db.store.sql().migrate("alpha", &f).await.unwrap();
    // The kernel stopped between the file's commit and its record.
    sqlx::query("delete from rookery.plugin_sql_migrations")
        .execute(db.store.pool())
        .await
        .unwrap();
    assert!(
        db.store
            .sql()
            .migrate("alpha", &f)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(count(&db, "select count(*) from x_alpha.things").await, 1);
    assert_eq!(
        count(&db, "select count(*) from rookery.plugin_sql_migrations").await,
        1
    );
    db.close().await;
}

#[tokio::test]
async fn a_migration_has_the_roles_rights_and_no_more() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    for (i, sql) in [
        "create table rookery.evil (n int)",
        "insert into rookery.settings default values",
        "select * from rookery.plugin_sql",
        "create role intruder",
        "create extension dblink",
        "set role postgres",
        "alter table p_alpha.notes add column x int",
        "drop schema v_alpha cascade",
    ]
    .iter()
    .enumerate()
    {
        let name = format!("{:04}_bad.sql", i + 1);
        let e = db
            .store
            .sql()
            .migrate("alpha", &files(&[(&name, sql)]))
            .await
            .unwrap_err();
        assert!(e.to_string().contains(&name), "{sql}: {e}");
        assert_eq!(
            count(
                &db,
                "select count(*) from pg_namespace where nspname = 'v_alpha'"
            )
            .await,
            1,
            "{sql}"
        );
    }
    assert_eq!(
        count(
            &db,
            "select count(*) from pg_tables where tablename = 'evil'"
        )
        .await,
        0
    );
    db.close().await;
}

#[tokio::test]
async fn migrations_of_a_plugin_without_raw_sql_are_refused() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let e = db
        .store
        .sql()
        .migrate("alpha", &files(&[("0001_init.sql", "select 1")]))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::NotFound);
    db.close().await;
}
