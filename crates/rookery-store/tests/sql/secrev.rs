//! Security review probes for the raw SQL branch (`origin/t3/raw-sql`). Each open finding is a test
//! written to pass once the gap is closed, and marked `#[ignore = "open finding N"]` so the suite stays
//! green. Run them with `cargo test --test store sql_secrev -- --ignored --nocapture`.

use std::time::Duration;

use rookery_store::sql::exec::Limits;
use rookery_store::testing::TestDb;
use sqlx::{Connection, Executor, PgConnection};

use super::common::World;
use super::fixtures::{code, direct, setup};

/// The kernel's global change-log ordering advisory lock (see `rookery_store::feed::SEQ_LOCK`).
const SEQ_LOCK: i64 = rookery_store::feed::SEQ_LOCK;

async fn admin_scalar<T>(db: &TestDb, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(db.store.pool())
        .await
        .unwrap()
}

// ── open findings ──────────────────────────────────────────────────────────────────────────────────

/// Every advisory lock function of the catalog as `(name(args), a call of it)`, found the way the kernel finds
/// them, so a function a later Postgres adds is covered too.
async fn advisory_calls(db: &TestDb) -> Vec<(String, String)> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.proname::text, pg_get_function_identity_arguments(p.oid) FROM pg_proc p \
         WHERE p.pronamespace = 'pg_catalog'::regnamespace AND p.proname LIKE '%advisory%' ORDER BY 1, 2",
    )
    .fetch_all(db.store.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(name, args)| {
            let call = match args.as_str() {
                "" => format!("select {name}()"),
                "bigint" => format!("select {name}(1::bigint)"),
                "integer, integer" => format!("select {name}(1, 2)"),
                other => panic!(
                    "an advisory function with arguments this test cannot call: {name}({other})"
                ),
            };
            (format!("{name}({args})"), call)
        })
        .collect()
}

/// The plugin role must not be able to hold the kernel's own advisory locks: the change-log ordering lock
/// (`SEQ_LOCK`, taken by every collection write), the per-plugin reconcile lock, the topic and blob locks.
/// Advisory locks are a global namespace, so nothing stops the role from taking them and stalling the kernel.
/// Closed by revoking `EXECUTE` on every `pg_advisory_*` and `pg_try_advisory_*` function from `PUBLIC`.
#[tokio::test]
async fn the_role_cannot_take_the_kernels_advisory_locks() {
    let Some(w) = World::new().await else { return };
    let s = w.db.store.clone();
    setup(&w.db, "alpha", true).await;
    setup(&w.db, "beta", true).await;
    let mut c = direct(&w.db, "alpha").await;

    let mut keys: Vec<(String, i64)> = vec![("SEQ_LOCK".into(), SEQ_LOCK)];
    for label in [
        "rookery.plugin:beta",
        "rookery.topicpart",
        "rookery.blobs.references",
        "rookery.sql.catalog",
    ] {
        let key: i64 = sqlx::query_scalar("select hashtext($1)::int8")
            .bind(label)
            .fetch_one(s.pool())
            .await
            .unwrap();
        keys.push((label.to_string(), key));
    }
    // The role must be refused every kernel advisory lock key, transaction-level (the kind the kernel takes).
    c.execute("begin").await.unwrap();
    for (name, key) in keys {
        let r = sqlx::query_scalar::<_, bool>("select pg_try_advisory_xact_lock($1)")
            .bind(key)
            .fetch_one(&mut c)
            .await;
        let e = r.expect_err(&format!(
            "the role CAN take the kernel's advisory lock {name} (key {key})"
        ));
        assert_eq!(code(&e), "42501", "{name}: {e}");
        c.execute("rollback").await.unwrap();
        c.execute("begin").await.unwrap();
    }
    c.execute("rollback").await.unwrap();
    w.db.close().await;
}

/// Every advisory function variant (lock, shared, xact, try, unlock, unlock_all, one and two keys), through
/// `rookery.sql` and through a direct login, is refused; the kernel's own user still has them all.
#[tokio::test]
async fn no_advisory_function_is_open_to_the_role_on_any_path() {
    let Some(w) = World::new().await else { return };
    setup(&w.db, "alpha", true).await;
    let calls = advisory_calls(&w.db).await;
    assert!(
        calls.len() >= 20,
        "the catalog lists {} advisory functions; expected the lock, unlock and try families",
        calls.len()
    );
    let sql = w.db.store.sql();
    let mut c = direct(&w.db, "alpha").await;
    for (sig, call) in &calls {
        let e = sql.query("alpha", call, &[], false).await.unwrap_err();
        assert_eq!(
            e.code(),
            rookery_proto::ErrorCode::Forbidden,
            "through rookery.sql: {sig}: {e}"
        );
        let e = c
            .execute(sqlx::AssertSqlSafe(call.to_owned()))
            .await
            .unwrap_err();
        assert_eq!(code(&e), "42501", "on a direct login: {sig}: {e}");
    }
    // The catalog agrees, for the role and for PUBLIC; the kernel's user has every one of them.
    assert_eq!(
        sql.open_advisory_functions().await.unwrap(),
        Vec::<String>::new()
    );
    let role = sql.names("alpha").unwrap().role;
    let can: i64 = sqlx::query_scalar(
        "select count(*) from pg_proc p where p.pronamespace = 'pg_catalog'::regnamespace and p.proname like '%advisory%' \
         and (has_function_privilege($1, p.oid, 'EXECUTE') or has_function_privilege('public', p.oid, 'EXECUTE'))",
    )
    .bind(&role)
    .fetch_one(w.db.store.pool())
    .await
    .unwrap();
    assert_eq!(can, 0);
    let kernel_has: bool = admin_scalar(
        &w.db,
        "select bool_and(has_function_privilege(current_user, p.oid, 'EXECUTE')) from pg_proc p \
         where p.pronamespace = 'pg_catalog'::regnamespace and p.proname like '%advisory%'",
    )
    .await;
    assert!(kernel_has);
    for (sig, call) in &calls {
        // The kernel's own pool runs them (the one-key and two-key forms need no other lock to be held).
        if sig.starts_with("pg_try_advisory_xact_lock(") {
            sqlx::query(sqlx::AssertSqlSafe(call.to_string()))
                .execute(w.db.store.pool())
                .await
                .unwrap_or_else(|e| panic!("{sig}: {e}"));
        }
    }
    w.db.close().await;
}

/// A plugin that tries the kernel's locks (through `rookery.sql` and directly, again and again) delays no
/// collection write: the writes do not wait, and every attempt is refused.
#[tokio::test]
async fn a_collection_write_is_not_delayed_while_a_plugin_role_tries_the_ordering_lock() {
    let Some(w) = World::new().await else { return };
    let s = w.db.store.clone();
    setup(&w.db, "alpha", true).await;
    let mut c = direct(&w.db, "alpha").await;
    let s2 = s.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let attacker = tokio::spawn(async move {
        let mut refused = 0u32;
        while !stop2.load(std::sync::atomic::Ordering::SeqCst) {
            c.execute("begin").await.unwrap();
            let r = sqlx::query("select pg_advisory_xact_lock($1)")
                .bind(SEQ_LOCK)
                .execute(&mut c)
                .await;
            if r.is_err() {
                refused += 1;
            }
            let _ = c.execute("rollback").await;
            let r = s2
                .sql()
                .execute(
                    "alpha",
                    &format!("select pg_advisory_lock({SEQ_LOCK})"),
                    &[],
                )
                .await;
            if r.is_err() {
                refused += 1;
            }
        }
        refused
    });
    let caller = w.as_user(w.owner);
    let mut slowest = Duration::ZERO;
    for i in 0..10 {
        let t = std::time::Instant::now();
        s.data("issues")
            .insert(
                &caller,
                "issues",
                serde_json::json!({ "title": format!("t{i}") }),
            )
            .await
            .unwrap();
        slowest = slowest.max(t.elapsed());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let refused = attacker.await.unwrap();
    assert!(
        refused >= 4,
        "the plugin's attempts were not all refused ({refused} were)"
    );
    assert!(
        slowest < Duration::from_secs(1),
        "a collection write took {slowest:?} while a plugin role tried the ordering lock"
    );
    w.db.close().await;
}

/// The seal is a database-level setting a superuser made, and the kernel looks at it every time it enables a
/// plugin: when it was undone (a restored dump, a new volume, a hand), a kernel that may redo it does, and the
/// plugin's role gets the lock functions no more.
#[tokio::test]
async fn a_kernel_that_may_seal_the_advisory_locks_does_when_they_are_open() {
    let Some(w) = World::new().await else { return };
    let s = w.db.store.clone();
    setup(&w.db, "alpha", true).await;
    assert!(s.sql().open_advisory_functions().await.unwrap().is_empty());
    // A hand (or a restore) gives them back to everyone.
    let mut admin = w.db.admin_connection().await;
    admin.execute(open_all()).await.unwrap();
    let open = s.sql().open_advisory_functions().await.unwrap();
    assert!(
        open.len() >= 20 && open.contains(&"pg_advisory_lock(bigint)".to_string()),
        "{open:?}"
    );
    let mut c = direct(&w.db, "alpha").await;
    c.execute("select pg_advisory_lock(7)")
        .await
        .expect("open: the role takes locks");
    drop(c);
    // The next load of a plugin with raw SQL closes them again.
    s.sql().enable("alpha", true).await.unwrap();
    assert!(s.sql().open_advisory_functions().await.unwrap().is_empty());
    let mut c = direct(&w.db, "alpha").await;
    let e = c.execute("select pg_advisory_lock(7)").await.unwrap_err();
    assert_eq!(code(&e), "42501");
    // Other databases of the server are not touched: the template this one was copied from is sealed on its own,
    // and the server's maintenance database keeps Postgres's defaults.
    let mut other = PgConnection::connect(&format!(
        "{}/postgres",
        rookery_store::testing::server_url()
            .unwrap()
            .split('?')
            .next()
            .unwrap()
            .rsplit_once('/')
            .unwrap()
            .0
    ))
    .await
    .unwrap();
    let public_can: bool = sqlx::query_scalar(
        "select has_function_privilege('public', 'pg_advisory_lock(bigint)', 'EXECUTE')",
    )
    .fetch_one(&mut other)
    .await
    .unwrap();
    assert!(public_can, "sealing one database changed another");
    w.db.close().await;
}

/// A kernel whose database user is not a superuser cannot seal the advisory locks (the functions belong to the
/// bootstrap superuser): raw SQL stays unavailable, naming the step, no role is made, a role from before stops
/// logging in, and once a superuser does the step, raw SQL works. Nothing else is affected.
#[tokio::test]
async fn a_kernel_that_cannot_seal_the_advisory_locks_keeps_raw_sql_off() {
    let Some(db) = TestDb::new_owned_by_app_role(true).await else {
        return;
    };
    let s = db.store.sql();
    let app: String = sqlx::query_scalar("select current_user::text")
        .fetch_one(db.store.pool())
        .await
        .unwrap();
    // Plain collections keep working on this deployment.
    db.store
        .collections()
        .apply("alpha", &crate::fixtures::notes_defs())
        .await
        .expect("collections need no raw SQL");
    let e = s.seal_advisory_locks().await.unwrap_err();
    assert_eq!(e.code(), rookery_proto::ErrorCode::Unavailable, "{e}");
    assert!(
        e.to_string().contains("advisory") && e.to_string().contains("superuser"),
        "{e}"
    );
    let e = s.enable("alpha", false).await.unwrap_err();
    assert_eq!(e.code(), rookery_proto::ErrorCode::Unavailable, "{e}");
    let role = s.names("alpha").unwrap().role;
    let roles = |db: &TestDb, role: String| {
        let pool = db.store.pool().clone();
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from pg_roles where rolname = $1")
                .bind(role)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        roles(&db, role.clone()).await,
        0,
        "a role was made on a database that is not sealed"
    );
    assert!(!s.is_enabled("alpha").await.unwrap());

    // The operator's step, as a superuser, with the statement of the docs for the kernel's own user.
    let mut admin = db.admin_connection().await;
    admin
        .execute(sqlx::AssertSqlSafe(
            rookery_store::sql::locks::seal_statement(&app),
        ))
        .await
        .unwrap();
    s.enable("alpha", true)
        .await
        .expect("sealed: raw SQL works");
    let mut c = direct(&db, "alpha").await;
    let e = c.execute("select pg_advisory_lock(1)").await.unwrap_err();
    assert_eq!(code(&e), "42501");
    // The kernel's own user kept the locks it needs.
    sqlx::query("select pg_advisory_lock(1), pg_advisory_unlock(1)")
        .execute(db.store.pool())
        .await
        .unwrap();
    drop(c);

    // The step is undone (a restored dump, a hand): the next load fails and the role cannot log in any more.
    admin.execute(open_all()).await.unwrap();
    let e = s.enable("alpha", true).await.unwrap_err();
    assert_eq!(e.code(), rookery_proto::ErrorCode::Unavailable, "{e}");
    let can_login: bool = sqlx::query_scalar("select rolcanlogin from pg_roles where rolname = $1")
        .bind(&role)
        .fetch_one(db.store.pool())
        .await
        .unwrap();
    assert!(
        !can_login,
        "the plugin's role can still log in on a database that is not sealed"
    );
    assert!(!s.is_enabled("alpha").await.unwrap());
    // Sealed again: the role comes back, from a kernel that is not a superuser (`ALTER ROLE ... NOSUPERUSER` is
    // refused to such a user, so it must not be asked), with its data.
    admin
        .execute(sqlx::AssertSqlSafe(
            rookery_store::sql::locks::seal_statement(&app),
        ))
        .await
        .unwrap();
    s.enable("alpha", true)
        .await
        .expect("an existing role is altered by a non-superuser kernel");
    s.enable("alpha", false)
        .await
        .expect("and again, with another credential");
    s.rotate("alpha").await.unwrap();
    assert!(s.is_enabled("alpha").await.unwrap());
    drop(admin);
    db.close().await;
}

/// SQL that gives the advisory lock functions back to `PUBLIC`, as Postgres ships them.
fn open_all() -> &'static str {
    "do $$ declare f record; begin \
       for f in select format('%I.%I(%s)', n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)) as sig \
                from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
                where n.nspname = 'pg_catalog' and p.proname like '%advisory%' \
       loop execute format('grant execute on function %s to public', f.sig); end loop; end $$"
}

/// The watchdog used to skip any session whose `application_name` was `rookery-sql-migration`, but
/// `application_name` is a session-settable GUC, so a plugin with a direct connection could opt its session out
/// of the watchdog's statement-cancel and idle-in-transaction kill. The exemption now belongs to the sessions
/// the kernel registered itself, by backend PID and start time.
#[tokio::test]
async fn the_watchdog_does_not_exempt_sessions_by_application_name() {
    let Some(db) = TestDb::new_with_limits(Limits {
        statement: Duration::from_secs(2),
        tx_total: Duration::from_secs(2),
        tx_idle: Duration::from_millis(500),
        idle_in_transaction: Duration::from_millis(300),
    })
    .await
    else {
        return;
    };
    setup(&db, "alpha", true).await;
    let sql = db.store.sql();
    let mut c = direct(&db, "alpha").await;
    c.execute("set application_name = 'rookery-sql-migration'")
        .await
        .unwrap();
    c.execute("set statement_timeout = 0").await.unwrap();
    c.execute("set idle_in_transaction_session_timeout = 0")
        .await
        .unwrap();
    // Idle in a transaction past the (shortened) limit: the watchdog ends it, whatever it calls itself.
    c.execute("begin").await.unwrap();
    c.execute("select 1").await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let ended = sql.enforce_limits().await.unwrap();
    let still_there = c.execute("select 1").await.is_ok();
    assert!(
        !still_there,
        "open finding 2: a session named rookery-sql-migration was not ended by the watchdog (it ended {ended} others)"
    );
    // A statement running past its limit under that name is cancelled too.
    let mut c = direct(&db, "alpha").await;
    c.execute("set application_name = 'rookery-sql-migration'")
        .await
        .unwrap();
    c.execute("set statement_timeout = 0").await.unwrap();
    let slow = tokio::spawn(async move { c.execute("select pg_sleep(30)").await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(4500)).await;
    sql.enforce_limits().await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), slow)
        .await
        .unwrap()
        .unwrap();
    assert!(
        r.is_err(),
        "a long statement of a session called rookery-sql-migration ran on"
    );
    db.close().await;
}

/// What the kernel registered is exempt, and across kernels: one kernel's watchdog does not end another kernel's
/// pooled connections or its migration, even on a plugin without `database.direct`, and even when the migration
/// runs longer than a statement may.
#[tokio::test]
async fn one_kernels_watchdog_leaves_another_kernels_sessions_and_migration_alone() {
    let Some(db) = TestDb::new_with_limits(Limits {
        statement: Duration::from_millis(500),
        tx_total: Duration::from_secs(2),
        tx_idle: Duration::from_millis(500),
        idle_in_transaction: Duration::from_millis(300),
    })
    .await
    else {
        return;
    };
    setup(&db, "alpha", false).await;
    let a = db.store.sql();
    let other = db.second_store().await.unwrap();
    // Another kernel (its own pool, its own memory) opens a pooled connection as the role, and runs a migration
    // whose statement outlasts the first kernel's statement limit by far.
    other.sql().execute("alpha", "select 1", &[]).await.unwrap();
    let files = rookery_store::sql::migrations::order(vec![(
        "0001_slow.sql".to_string(),
        "select pg_sleep(5); create table t (n int)".to_string(),
    )])
    .unwrap();
    let o2 = other.clone();
    let migrating = tokio::spawn(async move { o2.sql().migrate("alpha", &files).await });
    // This kernel's watchdog looks over and over, past the grace period for unregistered sessions.
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        a.enforce_limits().await.unwrap();
    }
    let ran = migrating
        .await
        .unwrap()
        .expect("the migration of the other kernel was not killed");
    assert_eq!(ran, [1]);
    // Its pooled connection is still its own: the statement works and was not ended as a foreign session.
    other.sql().execute("alpha", "select 1", &[]).await.unwrap();
    let registered: i64 = admin_scalar(
        &db,
        "select count(*) from rookery.plugin_sql_sessions where purpose = 'pool'",
    )
    .await;
    assert!(
        registered >= 1,
        "the other kernel's pooled connection is registered"
    );
    other.close().await;
    db.close().await;
}

/// The role must not read another plugin's data on any path, including the planner's column statistics.
/// This holds: `pg_stats` filters by `has_column_privilege`, so a role sees only columns it may SELECT.
#[tokio::test]
async fn the_role_cannot_read_another_plugins_column_statistics() {
    let Some(w) = World::new().await else { return };
    let s = w.db.store.clone();
    setup(&w.db, "alpha", true).await;
    setup(&w.db, "beta", true).await;
    // Seed `beta`'s collection with distinctive values and gather statistics as the kernel would.
    let caller = w.as_user(w.owner);
    for (title, n) in [("zebra-secret", 1), ("zebra-secret", 2), ("apple", 3)] {
        s.data("beta")
            .insert(
                &caller,
                "notes",
                serde_json::json!({ "title": title, "n": n }),
            )
            .await
            .unwrap();
    }
    sqlx::query("analyze p_beta.notes")
        .execute(s.pool())
        .await
        .unwrap();
    // The admin (who owns the table) sees the statistics; the other plugin's role does not.
    let admin_sees: i64 = admin_scalar(
        &w.db,
        "select count(*) from pg_stats where schemaname = 'p_beta'",
    )
    .await;
    assert!(admin_sees > 0, "ANALYZE populated no statistics for p_beta");

    let mut c = direct(&w.db, "alpha").await;
    let leak: i64 = sqlx::query_scalar("select count(*) from pg_stats where schemaname = 'p_beta'")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(
        leak, 0,
        "the role sees {leak} row(s) of another plugin's statistics"
    );
    w.db.close().await;
}

/// `database.direct: false` cannot mean the role cannot log in: the kernel's own pool authenticates as it, and a
/// role may always change its own password in Postgres. What the kernel guarantees instead: a session of that
/// role that the kernel did not open lives at most one watchdog interval (and the registration grace), the
/// password goes back to the kernel's, and the first time is in the audit log. With `direct: true` nothing of
/// this applies.
#[tokio::test]
async fn a_plugin_without_direct_cannot_keep_a_session_it_opened_after_resetting_its_own_password()
{
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    setup(&db, "beta", true).await;
    let sql = db.store.sql();
    let n = sql.names("alpha").unwrap();
    let own = db.store.pool().connect_options();
    let url = |role: &str, pw: &str| {
        format!(
            "postgres://{role}:{pw}@{}:{}/{}",
            own.get_host(),
            own.get_port(),
            own.get_database().unwrap_or("rookery"),
        )
    };
    for round in 0..2 {
        // A role may change its own password, through the kernel's own pool.
        sql.execute(
            "alpha",
            &format!("alter role \"{}\" password 'mine'", n.role),
            &[],
        )
        .await
        .unwrap();
        // So the plugin can log in directly, for the moment.
        let mut c = PgConnection::connect(&url(&n.role, "mine"))
            .await
            .expect("a role can always change its own password and log in with it");
        c.execute("select 1").await.unwrap();
        // Past the registration grace the watchdog ends it: the kernel did not open this session.
        tokio::time::sleep(Duration::from_millis(2300)).await;
        let done = sql.enforce().await.unwrap();
        assert_eq!(done.foreign.len(), 1, "round {round}: {done:?}");
        assert_eq!(done.foreign[0].plugin, "alpha");
        assert_eq!(
            done.foreign[0].first,
            round == 0,
            "the audit entry is for the first time only: {done:?}"
        );
        assert!(
            c.execute("select 1").await.is_err(),
            "round {round}: the session of the plugin's own making lives on"
        );
        // The password is the kernel's again: the one the plugin chose no longer logs in.
        assert!(
            PgConnection::connect(&url(&n.role, "mine")).await.is_err(),
            "round {round}"
        );
        // The kernel's own connections work throughout.
        sql.execute("alpha", "select 1", &[]).await.unwrap();
    }
    let entries = db
        .store
        .audit()
        .list(&rookery_store::audit::Filter {
            action: Some("sql.directSessionEnded".into()),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].resource.as_deref(), Some("plugin://alpha"));
    assert_eq!(
        entries[0].principal,
        rookery_store::Caller::system().actor()
    );
    // `direct: true`: the plugin's services connect with the credential, and nothing is ended or written.
    let _c = direct(&db, "beta").await;
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(sql.enforce().await.unwrap().foreign.is_empty());
    let mut c = direct(&db, "beta").await;
    sql.enforce().await.unwrap();
    c.execute("select 1")
        .await
        .expect("a direct connection of a plugin with database.direct stays");
    db.close().await;
}

// ── demonstrated behaviour (these pass today and document the gap) ───────────────────────────────

/// Holding the kernel's ordering lock stalls collection writes across plugins: that is why no plugin's role
/// may hold it (see `the_role_cannot_take_the_kernels_advisory_locks`). Here a session of the kernel's own
/// user holds it.
#[tokio::test]
async fn holding_the_ordering_lock_blocks_collection_writes() {
    let Some(w) = World::new().await else { return };
    let s = w.db.store.clone();
    setup(&w.db, "alpha", true).await;
    let mut c = w.db.admin_connection().await;
    c.execute("begin").await.unwrap();
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(SEQ_LOCK)
        .execute(&mut c)
        .await
        .unwrap();

    let s2 = s.clone();
    let caller = w.as_user(w.owner);
    let insert = tokio::spawn(async move {
        let t = std::time::Instant::now();
        let r = s2
            .data("issues")
            .insert(&caller, "issues", serde_json::json!({ "title": "blocked" }))
            .await;
        (t.elapsed(), r.is_ok())
    });
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        !insert.is_finished(),
        "a write must be blocked while the ordering lock is held"
    );
    c.execute("rollback").await.unwrap();
    let (elapsed, ok) = tokio::time::timeout(Duration::from_secs(5), insert)
        .await
        .unwrap()
        .unwrap();
    assert!(ok, "the write completes once the lock is released");
    assert!(
        elapsed >= Duration::from_millis(1200),
        "the write waited for the lock"
    );
    w.db.close().await;
}

/// A single `sql.*` call takes one statement: the extended protocol refuses a string with several statements,
/// so a plugin cannot smuggle a `SET` ahead of a query in one call.
#[tokio::test]
async fn one_call_takes_one_statement() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let sql = db.store.sql();
    let e = sql
        .execute("alpha", "set statement_timeout = 0; select 1", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), rookery_proto::ErrorCode::Invalid, "{e}");
    db.close().await;
}

// ── checks that should hold (these are run in CI; a regression fails them) ────────────────────────

/// `DISCARD ALL` on pool return clears what a plugin's session holds, so nothing leaks into the next statement
/// on that pooled connection. The role cannot take advisory locks at all (see
/// `no_advisory_function_is_open_to_the_role_on_any_path`); a temporary table and a changed setting stand for
/// session state here.
#[tokio::test]
async fn discard_all_clears_session_state_on_pool_return() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    let sql = db.store.sql();
    let e = sql
        .execute("alpha", "select pg_advisory_lock(901254)", &[])
        .await
        .unwrap_err();
    assert_eq!(e.code(), rookery_proto::ErrorCode::Forbidden, "{e}");
    // One pooled connection is reused; a temp table and `work_mem` set on it must be gone on its next use.
    sql.execute("alpha", "create temp table leak (x int)", &[])
        .await
        .unwrap();
    sql.execute("alpha", "set work_mem = '64MB'", &[])
        .await
        .unwrap();
    for _ in 0..5 {
        let t = sql
            .query(
                "alpha",
                "select to_regclass('pg_temp.leak')::text as t, current_setting('work_mem') as w",
                &[],
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            t.rows[0]["t"],
            serde_json::Value::Null,
            "a temporary table survived DISCARD ALL"
        );
        assert_ne!(
            t.rows[0]["w"],
            serde_json::json!("64MB"),
            "a setting survived DISCARD ALL"
        );
    }
    db.close().await;
}

/// `LOCK TABLE` does not apply to a view, and `SELECT ... FOR UPDATE` through a view is refused (the role
/// has SELECT only, and FOR UPDATE needs UPDATE): the role cannot take row locks on collection tables at all.
#[tokio::test]
async fn lock_table_on_a_view_is_refused_and_row_locks_are_the_plugins_own() {
    let Some(w) = World::new().await else { return };
    setup(&w.db, "alpha", true).await;
    setup(&w.db, "beta", true).await;
    let mut c = direct(&w.db, "alpha").await;
    c.execute("begin").await.unwrap();
    let e = c
        .execute("lock table v_alpha.notes in access exclusive mode")
        .await
        .unwrap_err();
    assert!(
        matches!(code(&e).as_str(), "42501" | "42809"),
        "LOCK TABLE on a view: {e}"
    );
    c.execute("rollback").await.unwrap();
    // FOR UPDATE on another plugin's view is refused outright.
    let e = c
        .execute("select * from v_beta.notes for update")
        .await
        .unwrap_err();
    assert_eq!(code(&e), "42501", "{e}");
    // Its own view too: SELECT-only on the view is not enough to take row locks (FOR UPDATE needs UPDATE).
    let e = c
        .execute("select * from v_alpha.notes for update")
        .await
        .unwrap_err();
    assert_eq!(code(&e), "42501", "FOR UPDATE on its own view: {e}");
    c.execute("rollback").await.unwrap();
    w.db.close().await;
}

// ── the same class: what else a role can do that blocks the kernel ───────────────────────────────────

/// A plugin's session may write only a bounded amount of temporary files (sorts and hashes that spill, temporary
/// tables are separate), set by the kernel as a role default that the role cannot change or remove: the
/// parameter is superuser-only, and so is undoing a superuser's setting of it.
#[tokio::test]
async fn the_role_has_a_temp_file_limit_it_cannot_lift() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", true).await;
    let role = db.store.sql().names("alpha").unwrap().role;
    let set: Vec<String> = sqlx::query_scalar(
        "select unnest(setconfig) from pg_db_role_setting where setrole = $1::regrole and setdatabase <> 0",
    )
    .bind(&role)
    .fetch_all(db.store.pool())
    .await
    .unwrap();
    assert!(set.contains(&"temp_file_limit=2GB".to_string()), "{set:?}");
    let mut c = direct(&db, "alpha").await;
    let limit: String = sqlx::query_scalar("show temp_file_limit")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(limit, "2GB");
    for sql in [
        "set temp_file_limit = -1".to_string(),
        "set temp_file_limit = '100GB'".to_string(),
        format!("alter role \"{role}\" set temp_file_limit = -1"),
        format!(
            "alter role \"{role}\" in database \"{}\" reset temp_file_limit",
            db.name()
        ),
        format!(
            "alter role \"{role}\" in database \"{}\" set temp_file_limit = -1",
            db.name()
        ),
    ] {
        let e = c
            .execute(sqlx::AssertSqlSafe(sql.to_owned()))
            .await
            .expect_err(&format!("the role may run: {sql}"));
        assert_eq!(code(&e), "42501", "{sql}: {e}");
    }
    // Resetting everything the role may reset leaves the one it may not (these run, and change nothing).
    c.execute(sqlx::AssertSqlSafe(format!(
        "alter role \"{role}\" reset temp_file_limit"
    )))
    .await
    .unwrap();
    c.execute(sqlx::AssertSqlSafe(format!(
        "alter role \"{role}\" in database \"{}\" reset all",
        db.name()
    )))
    .await
    .unwrap();
    let limit: String = admin_scalar(
        &db,
        &format!("select setconfig::text from pg_db_role_setting where setrole = '{role}'::regrole and setdatabase <> 0"),
    )
    .await;
    assert!(limit.contains("temp_file_limit=2GB"), "{limit}");
    db.close().await;
}

/// The headroom check: plugin roles hold 10 connections each, and only a reserve for the kernel's own user
/// keeps them from taking the connections the kernel's pool needs.
#[tokio::test]
async fn the_kernel_knows_whether_plugin_roles_can_starve_its_pool() {
    use rookery_store::sql::headroom::Headroom;
    let h = |plugins, reserved, uses, su| Headroom {
        max_connections: 100,
        superuser_reserved: 3,
        reserved,
        kernel_is_superuser: su,
        kernel_uses_reserve: uses,
        plugin_roles: plugins,
    };
    // No plugin with raw SQL: nothing to say.
    assert!(h(0, 0, false, false).warnings(17).is_empty());
    // Ten plugins, no reserve: the kernel's pool can be starved.
    let w = h(10, 0, false, false).warnings(17);
    assert_eq!(w.len(), 1);
    assert!(
        w[0].contains("reserved_connections") && w[0].contains("pg_use_reserved_connections"),
        "{w:?}"
    );
    // A reserve the kernel's user may not use is none.
    assert_eq!(h(10, 30, false, false).warnings(17).len(), 1);
    // A reserve of 30 for the kernel: fine, until the plugins hold more than the ordinary slots.
    assert!(h(5, 30, true, false).warnings(17).is_empty());
    let w = h(10, 30, true, false).warnings(17);
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("raise max_connections"), "{w:?}");
    // A superuser kernel has the superuser slots (3) and the reserve.
    assert_eq!(h(1, 20, false, true).kernel_reserve(), 23);
    // The numbers come from the server.
    let Some(db) = TestDb::new().await else {
        return;
    };
    setup(&db, "alpha", false).await;
    setup(&db, "beta", true).await;
    let real = db.store.sql().connection_headroom().await.unwrap();
    assert_eq!(real.plugin_roles, 2);
    assert!(real.max_connections >= 100 && real.kernel_is_superuser);
    db.close().await;
}
