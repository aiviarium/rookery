//! The role of a plugin with raw SQL: what it is, what it holds, and what it cannot do. Every item of
//! the list of what a role cannot do (docs/content/docs/(guide)/use/raw-sql.mdx, "The role") is run twice, through the kernel's pool
//! (`Sql::execute`) and on a direct connection with the role's own credential.

use rookery_proto::ErrorCode;
use rookery_store::Error;
use rookery_store::sql::{grants, names};
use rookery_store::testing::TestDb;
use sqlx::{Connection, Executor, PgConnection, Row};

use super::common::{World, defs};
use super::fixtures::{code, direct, setup};

async fn admin_scalar<T>(db: &TestDb, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(db.store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_plugin_that_does_not_opt_in_gets_nothing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    db.store
        .collections()
        .apply("plain", &super::fixtures::notes_defs())
        .await
        .unwrap();
    let tag = db.role_tag();
    let roles: i64 = admin_scalar(
        &db,
        &format!("SELECT count(*) FROM pg_roles WHERE rolname LIKE 'rookery\\_{tag}\\_x\\_%'"),
    )
    .await;
    let schemas: i64 = admin_scalar(
        &db,
        "SELECT count(*) FROM pg_namespace WHERE nspname IN ('v_plain', 'x_plain')",
    )
    .await;
    let rows: i64 = admin_scalar(&db, "SELECT count(*) FROM rookery.plugin_sql").await;
    assert_eq!((roles, schemas, rows), (0, 0, 0));
    let e = db
        .store
        .sql()
        .query("plain", "select 1", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::NotFound, "{e}");
    db.close().await;
}

#[tokio::test]
async fn the_role_is_what_the_specification_lists_and_holds_exactly_its_grants() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "demo-app", false).await;
    let n = db.store.sql().names("demo-app").unwrap();
    assert_eq!(n.role, format!("rookery_{}_x_demo_app", db.role_tag()));

    let r = sqlx::query(
        "SELECT rolsuper, rolcreaterole, rolcreatedb, rolinherit, rolreplication, rolbypassrls, rolcanlogin, rolconnlimit \
         FROM pg_roles WHERE rolname = $1",
    )
    .bind(&n.role)
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    let flags: Vec<bool> = [
        "rolsuper",
        "rolcreaterole",
        "rolcreatedb",
        "rolinherit",
        "rolreplication",
        "rolbypassrls",
        "rolcanlogin",
    ]
    .iter()
    .map(|c| r.get::<bool, _>(*c))
    .collect();
    assert_eq!(flags, [false, false, false, false, false, false, true]);
    assert_eq!(r.get::<i32, _>("rolconnlimit"), 10);

    // search_path, statement_timeout and idle_in_transaction_session_timeout, for this database.
    let settings: Vec<String> = sqlx::query_scalar(
        "SELECT unnest(setconfig) FROM pg_db_role_setting s JOIN pg_roles r ON r.oid = s.setrole \
         WHERE r.rolname = $1 AND s.setdatabase = (SELECT oid FROM pg_database WHERE datname = current_database()) ORDER BY 1",
    )
    .bind(&n.role)
    .fetch_all(db.store.pool())
    .await
    .unwrap();
    assert_eq!(
        settings,
        [
            "idle_in_transaction_session_timeout=15s",
            "lock_timeout=10s",
            "search_path=x_demo_app, v_demo_app, public",
            "statement_timeout=30s",
            // Where the kernel may set it (the test server's user is a superuser): the role cannot lift it.
            "temp_file_limit=2GB"
        ]
    );

    // Owner of x_, nothing else; the kernel's user is a member; the role is a member of nothing.
    let owner: String = sqlx::query_scalar(
        "SELECT pg_get_userbyid(nspowner)::text FROM pg_namespace WHERE nspname = $1",
    )
    .bind(&n.raw_schema)
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    assert_eq!(owner, n.role);
    let kernel_is_member: bool =
        sqlx::query_scalar("SELECT pg_has_role(current_user, $1, 'MEMBER')")
            .bind(&n.role)
            .fetch_one(db.store.pool())
            .await
            .unwrap();
    assert!(kernel_is_member);
    let memberships: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_auth_members WHERE member = (SELECT oid FROM pg_roles WHERE rolname = $1)",
    )
    .bind(&n.role)
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    assert_eq!(memberships, 0);

    // The grants held are exactly the grants due.
    let mut c = db.store.pool().acquire().await.unwrap();
    let held = rookery_store::sql::provision::current_grants(&mut c, &n.role)
        .await
        .unwrap();
    let views = names::views(
        &n,
        &db.store
            .collections()
            .list("demo-app")
            .iter()
            .map(|c| c.def.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let database: String = admin_scalar(&db, "SELECT current_database()::text").await;
    assert_eq!(held, grants::due(&n, &database, &views));
    let all: Vec<String> = held.iter().map(|g| g.grant_sql("r")).collect();
    assert_eq!(all.len(), 2 + 4, "{all:?}"); // CONNECT, USAGE v_, SELECT on four views

    // The schemas exist; the password is a SCRAM verifier, and the sealed one opens the role.
    let verifier: String =
        sqlx::query_scalar("SELECT rolpassword FROM pg_authid WHERE rolname = $1")
            .bind(&n.role)
            .fetch_one(db.store.pool())
            .await
            .unwrap();
    assert!(verifier.starts_with("SCRAM-SHA-256$"));
    let q = db
        .store
        .sql()
        .query(
            "demo-app",
            "select current_user::text as u, session_user::text as s",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(q.rows[0]["u"], n.role);
    assert_eq!(q.rows[0]["s"], n.role);
    db.close().await;
}

#[tokio::test]
async fn views_show_what_the_data_api_shows_the_plugin_and_follow_the_collections() {
    let Some(w) = World::new().await else { return };
    let s = w.db.store.clone();
    s.sql().enable("issues", false).await.unwrap();
    let c = w.as_user(w.owner);
    for t in ["one", "two", "three"] {
        s.data("issues")
            .insert(
                &c,
                "issues",
                serde_json::json!({ "title": t, "estimate": 1.5 }),
            )
            .await
            .unwrap();
    }
    let all = s
        .data("issues")
        .find(&c, "issues", &Default::default())
        .await
        .unwrap();
    let ids: std::collections::BTreeSet<String> = all
        .rows
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 3);
    let sql = s.sql();
    let rows = sql.query("issues", "select id::text as id, title, space_id::text as space, number, estimate from issues order by number", &[]).await.unwrap();
    assert_eq!(
        rows.rows
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>(),
        ids
    );
    assert_eq!(rows.rows[0]["space"], w.space.to_string());
    assert_eq!(rows.rows[0]["estimate"], 1.5);
    // Raw SQL applies neither scope nor access: the outsider's data API sees nothing, the view sees all.
    let outsider = w.as_user(w.outsider);
    assert!(
        s.data("issues")
            .find(&outsider, "issues", &Default::default())
            .await
            .map(|p| p.rows.len())
            .unwrap_or(0)
            == 0
    );
    // The hidden search column is not in the view; the system columns are.
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT attname::text FROM pg_attribute WHERE attrelid = 'v_issues.issues'::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum",
    )
    .fetch_all(s.pool())
    .await
    .unwrap();
    assert_eq!(
        &cols[..8],
        [
            "id",
            "space_id",
            "user_id",
            "created_at",
            "updated_at",
            "created_by",
            "updated_by",
            "rev"
        ]
    );
    assert!(!cols.contains(&"_search".to_string()) && !cols.contains(&"deleted_at".to_string()));

    // Trash: the live view loses the row, `__all` keeps it with deleted_at.
    let victim = all.rows[0]["id"].as_str().unwrap().parse().unwrap();
    s.data("issues").delete(&c, "issues", victim).await.unwrap();
    let live = sql
        .query("issues", "select count(*) as n from issues", &[])
        .await
        .unwrap();
    let every = sql
        .query(
            "issues",
            "select count(*) as n, count(deleted_at) as trashed from issues__all",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            live.rows[0]["n"].clone(),
            every.rows[0]["n"].clone(),
            every.rows[0]["trashed"].clone()
        ),
        (2.into(), 3.into(), 1.into())
    );

    // A new field is in the view as soon as the collection has it.
    let mut d = super::common::issues_defs();
    d[1].fields.push(
        serde_json::from_value(serde_json::json!({ "name": "risk", "type": "integer" })).unwrap(),
    );
    s.collections().apply("issues", &d).await.unwrap();
    sql.query("issues", "select risk from issues", &[])
        .await
        .unwrap();

    // A migration that drops a field works with views over the table, and the view follows.
    let m: rookery_store::collections::PluginMigration =
        serde_json::from_value(serde_json::json!({
            "version": 1, "steps": [{ "op": "dropField", "collection": "issues", "field": "risk" }]
        }))
        .unwrap();
    s.collections()
        .migrate("issues", &[m], &rookery_store::collections::NoTransforms)
        .await
        .unwrap();
    let e = sql
        .query("issues", "select risk from issues", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid, "{e}");
    sql.query("issues", "select title from issues", &[])
        .await
        .unwrap();

    // A removed collection's views are gone.
    let m: rookery_store::collections::PluginMigration =
        serde_json::from_value(serde_json::json!({
            "version": 2, "steps": [{ "op": "dropCollection", "collection": "comments" }]
        }))
        .unwrap();
    s.collections()
        .migrate("issues", &[m], &rookery_store::collections::NoTransforms)
        .await
        .unwrap();
    let gone: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_views WHERE schemaname = 'v_issues' AND viewname LIKE 'comments%'",
    )
    .fetch_one(s.pool())
    .await
    .unwrap();
    assert_eq!(gone, 0);
    let kept: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_views WHERE schemaname = 'v_issues'")
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(kept, 3 * 2);
    let _ = defs;
    w.db.close().await;
}

// ── what the role cannot do ──────────────────────────────────────────────────────────────────────

/// Runs `sql` through the kernel's pool; the error's code.
async fn via_kernel(db: &TestDb, plugin: &str, sql: &str) -> Result<(), Error> {
    db.store.sql().execute(plugin, sql, &[]).await.map(|_| ())
}

/// A statement the role must be refused, however it asks: through the kernel (`FORBIDDEN`, or `INVALID` when
/// the object does not exist for it) and on a direct connection.
async fn refused(db: &TestDb, plugin: &str, conn: &mut PgConnection, sql: &str) {
    let k = via_kernel(db, plugin, sql).await;
    let kernel = k
        .as_ref()
        .err()
        .unwrap_or_else(|| panic!("the kernel ran {sql:?}"));
    assert!(
        matches!(kernel.code(), ErrorCode::Forbidden | ErrorCode::Invalid),
        "{sql}: {kernel}"
    );
    // Whatever the error says, it names no schema of anyone else's.
    let text = kernel.to_string();
    for leak in ["rookery.", "p_other", "v_other", "x_other", "pg_authid"] {
        assert!(
            !text.contains(leak) || sql.contains(leak),
            "{sql}: the error leaks {leak}: {text}"
        );
    }
    let d = conn.execute(sqlx::AssertSqlSafe(sql.to_owned())).await;
    let e = d
        .err()
        .unwrap_or_else(|| panic!("the direct connection ran {sql:?}"));
    let c = code(&e);
    assert!(
        c.starts_with("42") || c == "0A000" || c.starts_with("55"),
        "{sql}: {c} {e}"
    );
}

#[tokio::test]
async fn the_role_cannot_reach_what_is_not_its_own() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    setup(&db, "other", true).await;
    db.store
        .sql()
        .execute("other", "create table secrets (v text)", &[])
        .await
        .unwrap();
    db.store
        .sql()
        .execute("other", "insert into secrets values ('hidden')", &[])
        .await
        .unwrap();
    let mut c = direct(&db, "alpha").await;
    // Positive controls: what the role may do.
    db.store
        .sql()
        .query("alpha", "select count(*) from notes", &[])
        .await
        .unwrap();
    db.store
        .sql()
        .execute("alpha", "create table mine (v int)", &[])
        .await
        .unwrap();
    c.execute("insert into mine values (1)").await.unwrap();
    let mine: i64 = sqlx::query_scalar("select count(*) from x_alpha.mine")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(mine, 1);

    let other = db.store.sql().names("other").unwrap();
    let cases = [
        // The kernel's schema: read and write.
        "select * from rookery.plugin_sql".to_string(),
        "select * from rookery.users".to_string(),
        "select count(*) from rookery.settings".to_string(),
        "insert into rookery.storage default values".to_string(),
        "update rookery.users set name = 'x'".to_string(),
        "delete from rookery.plugin_sql".to_string(),
        "create table rookery.evil (x int)".to_string(),
        "drop table rookery.plugin_sql".to_string(),
        "drop schema rookery cascade".to_string(),
        "alter table rookery.users add column x int".to_string(),
        "select * from rookery._sqlx_migrations".to_string(),
        "create function rookery.f() returns int language sql as 'select 1'".to_string(),
        // Another plugin: its views, its raw schema, its collection tables.
        format!("select * from {}.notes", other.view_schema),
        format!("select * from {}.secrets", other.raw_schema),
        format!("select * from {}.notes", other.data_schema),
        format!("create table {}.t (x int)", other.raw_schema),
        format!("drop table {}.secrets", other.raw_schema),
        format!("insert into {}.secrets values ('x')", other.raw_schema),
        format!("select * from {}._rookery_migrations", other.raw_schema),
        // Writing its own collection tables: the table, and the views over it.
        "insert into p_alpha.notes (id) values (gen_random_uuid())".to_string(),
        "update p_alpha.notes set title = 'x'".to_string(),
        "delete from p_alpha.notes".to_string(),
        "truncate p_alpha.notes".to_string(),
        "select * from p_alpha.notes".to_string(),
        "alter table p_alpha.notes add column x int".to_string(),
        "drop table p_alpha.notes".to_string(),
        "create index on p_alpha.notes (title)".to_string(),
        "insert into v_alpha.notes (title) values ('x')".to_string(),
        "update v_alpha.notes set title = 'x'".to_string(),
        "delete from v_alpha.notes__all".to_string(),
        "drop view v_alpha.notes".to_string(),
        "create view v_alpha.mine as select 1".to_string(),
        "create table v_alpha.mine (x int)".to_string(),
        "create trigger t before insert on p_alpha.notes for each row execute function pg_catalog.suppress_redundant_updates_trigger()".to_string(),
        "comment on table p_alpha.notes is 'x'".to_string(),
        "alter schema v_alpha rename to v_mine".to_string(),
        "alter schema x_alpha owner to rookery".to_string(),
        "drop schema v_alpha cascade".to_string(),
        // Secrets and the server.
        "select * from pg_authid".to_string(),
        "select rolpassword from pg_shadow".to_string(),
        "select pg_read_file('/etc/passwd')".to_string(),
        "select pg_read_binary_file('/etc/passwd')".to_string(),
        "select pg_ls_dir('/')".to_string(),
        "select pg_stat_file('/etc/passwd')".to_string(),
        "select lo_import('/etc/passwd')".to_string(),
        "select lo_export(1, '/tmp/rookery-sql-test')".to_string(),
        "copy mine from program 'id'".to_string(),
        "copy mine to program 'id'".to_string(),
        "copy mine from '/etc/passwd'".to_string(),
        "copy mine to '/tmp/rookery-sql-test'".to_string(),
        "copy (select 1) to program 'id'".to_string(),
        "select pg_reload_conf()".to_string(),
        "select pg_rotate_logfile()".to_string(),
        "alter system set work_mem = '1GB'".to_string(),
        "select pg_switch_wal()".to_string(),
        "set session_replication_role = replica".to_string(),
        "set log_statement = 'all'".to_string(),
        "set max_stack_depth = '7MB'".to_string(),
        // Creating roles, databases, extensions, languages, wrappers.
        "create role intruder superuser".to_string(),
        "create role intruder".to_string(),
        "create user intruder".to_string(),
        "alter role rookery superuser".to_string(),
        format!("alter role {} superuser", other.role),
        format!("alter role {} password 'x'", other.role),
        format!("alter role {} set search_path = public", other.role),
        format!("grant {} to {}", other.role, db.store.sql().names("alpha").unwrap().role),
        "create database intruder".to_string(),
        "alter database rookery set search_path = public".to_string(),
        "drop database rookery".to_string(),
        "create extension dblink".to_string(),
        "create extension postgres_fdw".to_string(),
        "create extension pgcrypto".to_string(),
        "create extension file_fdw".to_string(),
        "create language plpython3u".to_string(),
        "create foreign data wrapper w".to_string(),
        "create server s foreign data wrapper postgres_fdw".to_string(),
        "create schema intruder".to_string(),
        "create tablespace ts location '/tmp'".to_string(),
        "create event trigger et on ddl_command_end execute function pg_catalog.suppress_redundant_updates_trigger()".to_string(),
        "create publication p for all tables".to_string(),
        "create subscription s connection 'host=127.0.0.1' publication p".to_string(),
        "create access method am type table handler heap_tableam_handler".to_string(),
        "create operator class c for type int4 using btree as storage int4".to_string(),
        // Functions in languages that are not trusted, and ones that reach out.
        "create function f() returns int language c as '$libdir/nothing', 'f'".to_string(),
        "create function f() returns int language internal as 'pg_read_file'".to_string(),
        "create function f() returns int language plpython3u as 'return 1'".to_string(),
        "select dblink('host=127.0.0.1 dbname=rookery', 'select 1')".to_string(),
        "create table shadow as select * from pg_authid".to_string(),
    ];
    for sql in &cases {
        refused(&db, "alpha", &mut c, sql).await;
    }
    // The table in the other plugin is untouched, the kernel's schema is intact.
    let hidden: String = sqlx::query_scalar("select v from x_other.secrets")
        .fetch_one(db.store.pool())
        .await
        .unwrap();
    assert_eq!(hidden, "hidden");
    let rows: i64 = admin_scalar(&db, "select count(*) from rookery.plugin_sql").await;
    assert_eq!(rows, 2);
    db.close().await;
}

#[tokio::test]
async fn the_role_may_not_become_another_role_and_nothing_resets_it_into_one() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    setup(&db, "other", true).await;
    let a = db.store.sql().names("alpha").unwrap();
    let o = db.store.sql().names("other").unwrap();
    let kernel_user: String = admin_scalar(&db, "select current_user::text").await;
    let mut c = direct(&db, "alpha").await;

    // On a direct connection, one session throughout.
    let statements = [
        format!("set role {}", o.role),
        format!("set role {}", kernel_user),
        "set role postgres".to_string(),
        "set role none".to_string(), // allowed: it is the session's own role
        "reset role".to_string(),
        format!("set session authorization {}", o.role),
        format!("set session authorization {kernel_user}"),
        "reset session authorization".to_string(),
        "discard all".to_string(),
        "select set_config('role', 'postgres', false)".to_string(),
        format!("select set_config('session_authorization', '{kernel_user}', false)"),
        "do $$ begin execute 'reset role'; execute 'set session authorization default'; end $$".to_string(),
        "create function public.f() returns void language plpgsql security definer as $$ begin reset role; end $$".to_string(),
    ];
    for s in &statements {
        let _ = c.execute(sqlx::AssertSqlSafe(s.to_owned())).await;
        let who: (String, String) = sqlx::query_as("select current_user::text, session_user::text")
            .fetch_one(&mut c)
            .await
            .unwrap();
        assert_eq!(who, (a.role.clone(), a.role.clone()), "after {s}");
        let e = c
            .execute("select count(*) from rookery.plugin_sql")
            .await
            .unwrap_err();
        assert_eq!(code(&e), "42501", "after {s}");
        let e = c
            .execute(sqlx::AssertSqlSafe(format!(
                "select * from {}.notes",
                o.view_schema
            )))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "42501", "after {s}");
    }
    // `set role` to a role the session may not be is refused by name.
    for r in [&o.role, &kernel_user] {
        let e = c
            .execute(sqlx::AssertSqlSafe(format!("set role \"{r}\"")))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "42501", "{r}");
    }

    // Through the kernel, in a transaction (one connection): the same statements, the same answer.
    let sql = db.store.sql();
    let tx = sql.begin("alpha", None).await.unwrap();
    sql.rollback("alpha", &tx).await.unwrap();
    for s in &statements {
        let tx = sql.begin("alpha", None).await.unwrap();
        let _ = sql.tx_execute("alpha", &tx, s, &[]).await;
        // Whether or not the statement itself worked, the transaction is still the role's.
        let ok = sql
            .tx_query(
                "alpha",
                &tx,
                "select current_user::text as u, session_user::text as s",
                &[],
            )
            .await;
        if let Ok(r) = ok {
            assert_eq!(
                (
                    r.rows[0]["u"].as_str().unwrap(),
                    r.rows[0]["s"].as_str().unwrap()
                ),
                (a.role.as_str(), a.role.as_str()),
                "{s}"
            );
            let e = sql
                .tx_query("alpha", &tx, "select count(*) from rookery.plugin_sql", &[])
                .await
                .unwrap_err();
            assert_eq!(e.code(), ErrorCode::Forbidden, "{s}");
        }
        let _ = sql.rollback("alpha", &tx).await;
    }
    // Alone, each is refused or does nothing, and the next statement on the pool is the role's.
    for s in [
        "set role none",
        "reset role",
        "discard all",
        "reset session authorization",
    ] {
        let _ = sql.execute("alpha", s, &[]).await;
        let r = sql
            .query(
                "alpha",
                "select current_user::text as u, session_user::text as s",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(
            (
                r.rows[0]["u"].as_str().unwrap(),
                r.rows[0]["s"].as_str().unwrap()
            ),
            (a.role.as_str(), a.role.as_str())
        );
    }
    for r in [&o.role, &kernel_user] {
        let e = sql
            .execute("alpha", &format!("set role \"{r}\""), &[])
            .await
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Forbidden, "{r}");
    }
    db.close().await;
}

#[tokio::test]
async fn security_definer_functions_run_as_the_role_so_they_escape_nothing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    sql.execute("alpha", "create function peek() returns bigint language plpgsql security definer as $$ begin return (select count(*) from rookery.plugin_sql); end $$", &[]).await.unwrap();
    let e = sql.query("alpha", "select peek()", &[]).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Forbidden, "{e}");
    let owner: String = sqlx::query_scalar(
        "select pg_get_userbyid(proowner)::text from pg_proc where proname = 'peek'",
    )
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    assert_eq!(owner, sql.names("alpha").unwrap().role);
    // A definer function that switches roles does not get further either.
    sql.execute("alpha", "create function hop() returns void language plpgsql security definer as $$ begin set role postgres; end $$", &[]).await.unwrap();
    assert!(sql.execute("alpha", "select hop()", &[]).await.is_err());
    db.close().await;
}

#[tokio::test]
async fn the_password_and_the_secrets_are_not_readable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    let r = sql
        .query(
            "alpha",
            "select rolpassword as shown from pg_roles where rolname = current_user",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        r.rows[0]["shown"], "********",
        "pg_roles masks the verifier"
    );
    // The role sees roles' names (pg_roles is public) but no one's verifier, and no one else's sessions' queries.
    let r = sql.query("alpha", "select count(*) as n from pg_stat_activity where query <> '<insufficient privilege>' and usename <> current_user", &[]).await.unwrap();
    assert_eq!(r.rows[0]["n"], 0);
    db.close().await;
}

#[tokio::test]
async fn session_settings_and_the_search_path_cannot_be_used_to_get_around_it() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    let n = sql.names("alpha").unwrap();
    // The limits as the specification sets them, for the kernel's connections and for a direct one.
    let show = "select current_setting('search_path') as p, current_setting('statement_timeout') as s, \
                current_setting('idle_in_transaction_session_timeout') as i, current_setting('lock_timeout') as l, \
                current_setting('TimeZone') as tz";
    let k = sql.query("alpha", show, &[]).await.unwrap().rows.remove(0);
    assert_eq!(k["p"], format!("{},{},public", n.raw_schema, n.view_schema));
    assert_eq!(
        (
            k["s"].as_str().unwrap(),
            k["i"].as_str().unwrap(),
            k["l"].as_str().unwrap()
        ),
        ("30s", "15s", "10s")
    );
    assert_eq!(k["tz"], "UTC");
    let mut c = direct(&db, "alpha").await;
    let d = sqlx::query(sqlx::AssertSqlSafe(show.to_string()))
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(d.get::<String, _>("p"), "x_alpha, v_alpha, public");
    assert_eq!(d.get::<String, _>("s"), "30s");

    // Changing the search path to reach the kernel's schema reaches nothing; and the next statement of the
    // pool starts again from the role's own.
    sql.execute("alpha", "set search_path = crow, public", &[])
        .await
        .unwrap();
    assert!(
        sql.query("alpha", "select * from plugin_sql", &[])
            .await
            .is_err()
    );
    let p = sql.query("alpha", "show search_path", &[]).await.unwrap();
    assert_eq!(
        p.rows[0]["search_path"],
        format!("{},{},public", n.raw_schema, n.view_schema)
    );
    // Settings, temporary tables and session variables of one statement are not there for the next.
    sql.execute("alpha", "set statement_timeout = 0", &[])
        .await
        .unwrap();
    sql.execute("alpha", "create temp table t (x int)", &[])
        .await
        .unwrap();
    sql.execute("alpha", "select set_config('my.flag', 'on', false)", &[])
        .await
        .unwrap();
    sql.execute("alpha", "prepare p as select 1", &[])
        .await
        .unwrap();
    let after = sql
        .query("alpha", "select current_setting('statement_timeout') as s, current_setting('my.flag', true) as f, to_regclass('pg_temp.t')::text as t", &[])
        .await
        .unwrap();
    assert_eq!(
        (
            after.rows[0]["s"].as_str().unwrap(),
            after.rows[0]["f"].is_null() || after.rows[0]["f"] == ""
        ),
        ("30s", true)
    );
    assert!(after.rows[0]["t"].is_null());

    // `alter role ... set` on itself lasts across sessions, but loses to the kernel's own, and the next load
    // puts the role's settings back.
    sql.execute(
        "alpha",
        &format!("alter role \"{}\" set statement_timeout = 0", n.role),
        &[],
    )
    .await
    .unwrap();
    sql.execute(
        "alpha",
        &format!("alter role \"{}\" set search_path = public", n.role),
        &[],
    )
    .await
    .unwrap();
    let k = sql.query("alpha", show, &[]).await.unwrap().rows.remove(0);
    assert_eq!(
        (k["s"].as_str().unwrap(), k["p"].as_str().unwrap()),
        ("30s", "x_alpha,v_alpha,public")
    );
    sql.enable("alpha", true).await.unwrap();
    let mut c2 = direct(&db, "alpha").await;
    let d = sqlx::query(sqlx::AssertSqlSafe(show.to_string()))
        .fetch_one(&mut c2)
        .await
        .unwrap();
    assert_eq!(
        (d.get::<String, _>("s"), d.get::<String, _>("p")),
        ("30s".into(), "x_alpha, v_alpha, public".into())
    );
    db.close().await;
}

#[tokio::test]
async fn a_role_that_changes_its_own_password_is_put_right_by_the_kernel() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let sql = db.store.sql();
    let n = sql.names("alpha").unwrap();
    sql.execute(
        "alpha",
        &format!("alter role \"{}\" password 'mine'", n.role),
        &[],
    )
    .await
    .unwrap();
    // Existing sessions go on; once the pool has none, a new one is refused and the kernel sets the password again.
    sql.execute("alpha", "select 1", &[]).await.unwrap();
    sql.forget("alpha").await;
    let r = sql.query("alpha", "select 1 as one", &[]).await.unwrap();
    assert_eq!(r.rows[0]["one"], 1);
    db.close().await;
}

#[tokio::test]
async fn the_role_has_a_connection_limit_of_ten() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let mut held = Vec::new();
    for _ in 0..10 {
        held.push(direct(&db, "alpha").await);
    }
    let url = db
        .store
        .sql()
        .connection_url("alpha")
        .await
        .unwrap()
        .unwrap();
    let e = PgConnection::connect(&url)
        .await
        .expect_err("the eleventh connection is refused");
    assert_eq!(code(&e), "53300");
    drop(held);
    db.close().await;
}

#[tokio::test]
async fn the_row_cap_and_the_limits_hold() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let sql = db.store.sql();
    let ok = sql
        .query("alpha", "select g from generate_series(1, 10000) g", &[])
        .await
        .unwrap();
    assert_eq!(ok.rows.len(), 10_000);
    let e = sql
        .query("alpha", "select g from generate_series(1, 10001) g", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid, "{e}");
    assert!(e.to_string().contains("10000"));
    // A stream has no cap.
    use futures_util::StreamExt;
    let (cols, rows, written) = sql
        .query_stream("alpha", "select g from generate_series(1, 25000) g", &[])
        .await
        .unwrap();
    assert!(written.is_none(), "a select writes nothing");
    assert_eq!(cols[0].ty, "int4");
    assert_eq!(rows.count().await, 25_000);
    db.close().await;
}

#[tokio::test]
async fn disabling_takes_everything_away_and_keeps_the_data() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    setup(&db, "stays", false).await;
    let sql = db.store.sql();
    sql.execute("alpha", "create table mine (v int)", &[])
        .await
        .unwrap();
    sql.execute("alpha", "insert into mine values (42)", &[])
        .await
        .unwrap();
    let url = sql.connection_url("alpha").await.unwrap().unwrap();
    let mut c = PgConnection::connect(&url).await.unwrap();
    c.execute("select 1").await.unwrap();

    // A plugin that left: swept, as a kernel start does for roles of plugins it no longer finds.
    let keep: std::collections::BTreeSet<String> = ["stays".to_string()].into();
    assert_eq!(sql.sweep(&keep).await.unwrap(), ["alpha"]);
    assert!(c.execute("select 1").await.is_err(), "its session ended");
    assert!(
        PgConnection::connect(&url).await.is_err(),
        "it cannot log in"
    );
    let e = sql.query("alpha", "select 1", &[]).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::NotFound);
    let n = sql.names("alpha").unwrap();
    let r = sqlx::query(
        "select rolcanlogin, rolpassword is null as nopw from pg_authid where rolname = $1",
    )
    .bind(&n.role)
    .fetch_one(db.store.pool())
    .await
    .unwrap();
    assert_eq!(
        (r.get::<bool, _>("rolcanlogin"), r.get::<bool, _>("nopw")),
        (false, true)
    );
    let held = rookery_store::sql::provision::current_grants(
        &mut db.store.pool().acquire().await.unwrap(),
        &n.role,
    )
    .await
    .unwrap();
    assert!(held.is_empty(), "{held:?}");
    let views: i64 = admin_scalar(
        &db,
        "select count(*) from pg_namespace where nspname = 'v_alpha'",
    )
    .await;
    assert_eq!(views, 0);
    // The plugin that stays is untouched.
    sql.query("stays", "select count(*) from notes", &[])
        .await
        .unwrap();
    // The data is kept, and comes back with the plugin.
    let v: i32 = admin_scalar(&db, "select v from x_alpha.mine").await;
    assert_eq!(v, 42);
    sql.enable("alpha", false).await.unwrap();
    let back = sql.query("alpha", "select v from mine", &[]).await.unwrap();
    assert_eq!(back.rows[0]["v"], 42);
    sql.query("alpha", "select count(*) from notes", &[])
        .await
        .unwrap();

    // A manifest that drops `direct` takes the credential services held.
    let before = sql.connection_url("alpha").await.unwrap();
    assert!(before.is_none());
    sql.enable("alpha", true).await.unwrap();
    let url = sql.connection_url("alpha").await.unwrap().unwrap();
    sql.enable("alpha", false).await.unwrap();
    assert!(
        PgConnection::connect(&url).await.is_err(),
        "the old credential stopped working"
    );
    assert!(sql.connection_url("alpha").await.unwrap().is_none());
    db.close().await;
}

#[tokio::test]
async fn a_role_that_belongs_to_something_else_is_not_taken_over() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let n = db.store.sql().names("alpha").unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "create role \"{}\" login",
        n.role
    )))
    .execute(db.store.pool())
    .await
    .unwrap();
    db.store
        .collections()
        .apply("alpha", &super::fixtures::notes_defs())
        .await
        .unwrap();
    let e = db.store.sql().enable("alpha", false).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Conflict, "{e}");
    assert!(e.to_string().contains("ROOKERY_DATABASE_ROLE_TAG"));
    sqlx::query(sqlx::AssertSqlSafe(format!("drop role \"{}\"", n.role)))
        .execute(db.store.pool())
        .await
        .unwrap();
    db.close().await;
}

#[tokio::test]
async fn roles_of_databases_that_share_a_server_do_not_meet() {
    // Two databases, one plugin id: each has roles of its own, with passwords of its own.
    let Some(a) = TestDb::new().await else { return };
    let Some(b) = TestDb::new().await else { return };
    setup(&a, "same", true).await;
    setup(&b, "same", true).await;
    let (ra, rb) = (
        a.store.sql().names("same").unwrap().role,
        b.store.sql().names("same").unwrap().role,
    );
    assert_ne!(ra, rb);
    let (ua, ub) = (
        a.store.sql().connection_url("same").await.unwrap().unwrap(),
        b.store.sql().connection_url("same").await.unwrap().unwrap(),
    );
    assert_ne!(ua, ub);
    // Both work; one's removal leaves the other.
    PgConnection::connect(&ua)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    PgConnection::connect(&ub)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    a.store.sql().disable("same").await.unwrap();
    PgConnection::connect(&ub)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    a.close().await;
    let left: i64 = sqlx::query_scalar("select count(*) from pg_roles where rolname = $1")
        .bind(&ra)
        .fetch_one(b.store.pool())
        .await
        .unwrap();
    assert_eq!(left, 0, "closing the database dropped its roles");
    let other: i64 = sqlx::query_scalar("select count(*) from pg_roles where rolname = $1")
        .bind(&rb)
        .fetch_one(b.store.pool())
        .await
        .unwrap();
    assert_eq!(other, 1);
    b.close().await;
}

#[tokio::test]
async fn limits_hold_even_when_the_plugin_sets_its_own_timeouts_to_nothing() {
    use std::time::{Duration, Instant};
    let Some(db) = TestDb::new_with_limits(TestDb::limits(
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_millis(500),
        Duration::from_secs(1),
    ))
    .await
    else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    // The role's own timeout.
    let t = Instant::now();
    let e = sql
        .execute("alpha", "select pg_sleep(10)", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Timeout, "{e}");
    assert!(t.elapsed() < Duration::from_secs(5));
    // The plugin turns its timeout off in the session: the kernel's clock still ends the statement.
    let tx = sql.begin("alpha", None).await.unwrap();
    sql.tx_execute("alpha", &tx, "set local statement_timeout = 0", &[])
        .await
        .unwrap();
    let t = Instant::now();
    let e = sql
        .tx_execute("alpha", &tx, "select pg_sleep(10)", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Timeout, "{e}");
    assert!(t.elapsed() < Duration::from_secs(5));
    // A transaction idle too long is rolled back, and so is one that runs too long.
    let tx = sql.begin("alpha", None).await.unwrap();
    sql.tx_execute("alpha", &tx, "create table t (x int)", &[])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let e = sql
        .tx_query("alpha", &tx, "select 1", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::NotFound, "{e}");
    let gone: Option<String> = sqlx::query_scalar("select to_regclass('x_alpha.t')::text")
        .fetch_one(db.store.pool())
        .await
        .unwrap();
    assert!(gone.is_none(), "rolled back");

    // A direct connection that turns every timeout off: the watchdog ends what runs past the limits.
    let mut c = direct(&db, "alpha").await;
    c.execute("set statement_timeout = 0").await.unwrap();
    c.execute("set idle_in_transaction_session_timeout = 0")
        .await
        .unwrap();
    let long = tokio::spawn(async move {
        let r = c.execute("select pg_sleep(30)").await;
        (r.err().map(|e| code(&e)), c)
    });
    // Past the limit, and not yet so far past that the session is ended rather than its statement cancelled.
    tokio::time::sleep(Duration::from_millis(1350)).await;
    assert!(sql.enforce_limits().await.unwrap() >= 1);
    let (err, mut c) = tokio::time::timeout(Duration::from_secs(5), long)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(err.as_deref(), Some("57014"), "cancelled");
    c.execute("begin").await.unwrap();
    c.execute("select 1").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(sql.enforce_limits().await.unwrap() >= 1);
    assert!(
        c.execute("select 1").await.is_err(),
        "the idle transaction's session ended"
    );
    db.close().await;
}

#[tokio::test]
async fn pgvector_works_for_the_role_with_operators_and_parameters() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    sql.execute(
        "alpha",
        "create table items (id int primary key, embedding vector(3))",
        &[],
    )
    .await
    .unwrap();
    for (id, v) in [
        (1, [1.0, 0.0, 0.0]),
        (2, [0.0, 1.0, 0.0]),
        (3, [0.9, 0.1, 0.0]),
    ] {
        sql.execute(
            "alpha",
            "insert into items values ($1, $2)",
            &[serde_json::json!(id), serde_json::json!({ "$vector": v })],
        )
        .await
        .unwrap();
    }
    let q = sql
        .query(
            "alpha",
            "select id, embedding, embedding <=> $1 as d from items order by embedding <=> $1 limit 2",
            &[serde_json::json!({ "$vector": [1.0, 0.0, 0.0] })],
        )
        .await
        .unwrap();
    assert_eq!(q.columns[1].ty, "vector");
    assert_eq!(q.rows[0]["id"], 1);
    assert_eq!(q.rows[1]["id"], 3);
    assert_eq!(q.rows[0]["embedding"], serde_json::json!([1.0, 0.0, 0.0]));
    let mut c = direct(&db, "alpha").await;
    let d: f64 = sqlx::query_scalar(
        "select (embedding <-> '[0,1,0]'::vector)::float8 from items where id = 2",
    )
    .fetch_one(&mut c)
    .await
    .unwrap();
    assert_eq!(d, 0.0);
    db.close().await;
}

#[tokio::test]
async fn a_log_collection_has_one_view_of_the_columns_its_table_has() {
    let Some(w) = World::new().await else { return };
    let s = w.db.store.clone();
    let mut d = super::common::issues_defs();
    d.push(
        serde_json::from_value(serde_json::json!({
            "name": "pings", "kind": "log", "scope": "space",
            "fields": [{ "name": "target", "type": "text" }, { "name": "ms", "type": "integer" }]
        }))
        .unwrap(),
    );
    s.collections().apply("issues", &d).await.unwrap();
    s.sql().enable("issues", false).await.unwrap();
    let c = w.as_user(w.owner);
    for n in 0..3 {
        s.data("issues")
            .insert(
                &c,
                "pings",
                serde_json::json!({ "target": format!("t{n}"), "ms": n }),
            )
            .await
            .unwrap();
    }
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT attname::text FROM pg_attribute WHERE attrelid = 'v_issues.pings'::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum",
    )
    .fetch_all(s.pool())
    .await
    .unwrap();
    assert_eq!(
        cols,
        [
            "seq",
            "id",
            "space_id",
            "user_id",
            "created_at",
            "created_by",
            "target",
            "ms"
        ]
    );
    let r = s
        .sql()
        .query(
            "issues",
            "select seq, target, ms from pings order by seq",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert_eq!(r.rows[2]["target"], "t2");
    assert!(r.rows[0]["seq"].as_i64().unwrap() < r.rows[2]["seq"].as_i64().unwrap());
    // Appended only: the role cannot change a log through its view, and there is no `__all` view.
    for sql in ["delete from pings", "update pings set ms = 0"] {
        let e = s.sql().execute("issues", sql, &[]).await.unwrap_err();
        assert_eq!(e.code(), ErrorCode::Forbidden, "{sql}");
    }
    let e = s
        .sql()
        .query("issues", "select * from pings__all", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Invalid, "a log has no __all view");
}

/// An audit gap: a write run through `query` (`insert ... returning`) is audited by what
/// the statement did, which the kernel learns from the plan, not by parsing SQL. A read has no write; a
/// data-modifying CTE under a `SELECT` is still caught.
#[tokio::test]
async fn a_write_through_query_is_reported_by_its_plan_and_row_count() {
    use rookery_store::sql::exec::Written;
    use serde_json::json;
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    sql.execute("alpha", "create table w (n int)", &[])
        .await
        .unwrap();
    // A read writes nothing.
    let r = sql.query("alpha", "select n from w", &[]).await.unwrap();
    assert!(r.written.is_none());
    // `insert ... returning`: what the command tag would say, and the rows it counted.
    let r = sql
        .query(
            "alpha",
            "insert into w values ($1) returning n",
            &[json!(1)],
        )
        .await
        .unwrap();
    assert_eq!(
        r.written,
        Some(Written {
            command: "INSERT".into(),
            rows: 1
        })
    );
    let r = sql
        .query("alpha", "update w set n = 2 returning n", &[])
        .await
        .unwrap();
    assert_eq!(
        r.written,
        Some(Written {
            command: "UPDATE".into(),
            rows: 1
        })
    );
    let r = sql
        .query("alpha", "delete from w where n = 2 returning n", &[])
        .await
        .unwrap();
    assert_eq!(
        r.written,
        Some(Written {
            command: "DELETE".into(),
            rows: 1
        })
    );
    // A `SELECT` with a modifying CTE writes a table: the plan has the ModifyTable node under the select.
    let r = sql
        .query(
            "alpha",
            "with w2 as (insert into w values (3) returning n) select n from w2",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        r.written,
        Some(Written {
            command: "INSERT".into(),
            rows: 1
        })
    );
    // In a transaction too.
    let tx = sql.begin("alpha", None).await.unwrap();
    let r = sql
        .tx_query("alpha", &tx, "insert into w values (4) returning n", &[])
        .await
        .unwrap();
    assert_eq!(
        r.written,
        Some(Written {
            command: "INSERT".into(),
            rows: 1
        })
    );
    sql.rollback("alpha", &tx).await.unwrap();
    // A stream reports its command; its rows are counted by the reader.
    let (_, rows, written) = sql
        .query_stream("alpha", "insert into w values (5) returning n", &[])
        .await
        .unwrap();
    assert_eq!(written.as_deref(), Some("INSERT"));
    use futures_util::StreamExt;
    assert_eq!(rows.count().await, 1);
    db.close().await;
}

#[tokio::test]
async fn a_plugin_has_at_most_four_transactions_open_at_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    let mut open = Vec::new();
    for _ in 0..4 {
        open.push(sql.begin("alpha", None).await.unwrap());
    }
    let e = sql.begin("alpha", None).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Unavailable, "{e}");
    for t in open {
        sql.rollback("alpha", &t).await.unwrap();
    }
    sql.begin("alpha", None).await.map(|_| ()).unwrap();
    db.close().await;
}
