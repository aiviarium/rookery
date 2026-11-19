//! The store on Postgres: migrations, people, sessions, tokens and links.

use rookery_proto::auth::LinkKind;
use rookery_store::test_db;
use rookery_store::users::Login;

fn login(subject: &str, admin: bool) -> Login {
    Login {
        issuer: "https://id.example.test".into(),
        subject: subject.into(),
        name: format!("Person {subject}"),
        email: Some(format!("{subject}@example.test")),
        admin,
    }
}

#[test]
fn test_urls_name_another_database() {
    use rookery_store::testing::url_for;
    assert_eq!(
        url_for("postgres://a:b@db:5432/rookery", "x"),
        "postgres://a:b@db:5432/x"
    );
    assert_eq!(
        url_for("postgres://a@db/rookery?sslmode=disable", "x"),
        "postgres://a@db/x?sslmode=disable"
    );
    assert_eq!(url_for("postgres://db", "x"), "postgres://db/x");
}

#[tokio::test]
async fn migrations_run_once() {
    let db = test_db!();
    let ran: Vec<i32> =
        sqlx::query_scalar("SELECT version FROM rookery.migrations ORDER BY version")
            .fetch_all(db.store.pool())
            .await
            .unwrap();
    let known: Vec<i32> = rookery_store::migrations()
        .iter()
        .map(|(v, _)| *v)
        .collect();
    assert_eq!(ran, known);
    // Opening it again runs nothing and fails on nothing.
    let again = rookery_store::Store::open(rookery_store::StoreOptions::new(&db.url))
        .await
        .unwrap();
    again.close().await;
    db.close().await;
}

#[tokio::test]
async fn a_changed_migration_stops_the_start() {
    let db = test_db!();
    sqlx::query("UPDATE rookery.migrations SET checksum = '\\x00' WHERE version = 1")
        .execute(db.store.pool())
        .await
        .unwrap();
    let refused = rookery_store::Store::open(rookery_store::StoreOptions::new(&db.url))
        .await
        .err()
        .unwrap();
    assert!(
        refused.to_string().contains("changed after it ran"),
        "{refused}"
    );
    db.close().await;
}

#[tokio::test]
async fn sign_ins_make_people_once_and_never_take_admin_away() {
    let db = test_db!();
    let users = db.store.users();
    let (maria, new) = users.upsert_login(&login("maria", true)).await.unwrap();
    assert!(new && maria.is_admin());
    let (again, new) = users.upsert_login(&login("maria", false)).await.unwrap();
    assert!(!new && again.is_admin() && again.id == maria.id);
    let (ops, _) = users.upsert_login(&login("ops", false)).await.unwrap();
    assert!(!ops.is_admin());
    assert_eq!(
        users
            .by_email("MARIA@example.test")
            .await
            .unwrap()
            .unwrap()
            .id,
        maria.id
    );

    // The last admin stays one.
    assert!(users.set_role(maria.id, false).await.is_err());
    assert!(users.set_disabled(maria.id, true).await.is_err());
    users.set_role(ops.id, true).await.unwrap();
    users.set_role(maria.id, false).await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn sessions_and_tokens_work_until_revoked_or_disabled() {
    let db = test_db!();
    let auth = db.store.auth();
    let (admin, _) = db
        .store
        .users()
        .upsert_login(&login("admin", true))
        .await
        .unwrap();
    let (ops, _) = db
        .store
        .users()
        .upsert_login(&login("ops", false))
        .await
        .unwrap();

    let (token, session) = auth
        .create_session(ops.id, Some("Firefox on Linux"), None, None)
        .await
        .unwrap();
    assert!(token.starts_with("rookery_ses_"));
    let (found, user) = auth.resolve_session(&token).await.unwrap().unwrap();
    assert_eq!((found.id, user.id), (session.id, ops.id));
    assert!(
        auth.resolve_session("rookery_ses_wrong")
            .await
            .unwrap()
            .is_none()
    );
    // Someone else's session is not theirs to end.
    assert!(auth.revoke_session(admin.id, session.id).await.is_err());
    auth.revoke_session(ops.id, session.id).await.unwrap();
    assert!(auth.resolve_session(&token).await.unwrap().is_none());

    let (pat, record) = auth.create_token(ops.id, "nightly", None).await.unwrap();
    assert!(pat.starts_with("rookery_pat_") && pat.ends_with(&record.hint));
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM rookery.access_tokens WHERE id = $1")
            .bind(record.id)
            .fetch_one(db.store.pool())
            .await
            .unwrap();
    assert_ne!(stored, pat.as_bytes(), "the token is stored hashed");
    assert_eq!(
        auth.resolve_token(&pat).await.unwrap().unwrap().1.id,
        ops.id
    );
    db.store.users().set_disabled(ops.id, true).await.unwrap();
    assert!(auth.resolve_token(&pat).await.unwrap().is_none());
    db.store.users().set_disabled(ops.id, false).await.unwrap();
    auth.revoke_token(ops.id, record.id).await.unwrap();
    assert!(auth.resolve_token(&pat).await.unwrap().is_none());
    assert!(auth.list_tokens(ops.id).await.unwrap().is_empty());
    db.close().await;
}

#[tokio::test]
async fn links_work_once() {
    let db = test_db!();
    let links = db.store.links();
    let (code, link) = links
        .create(LinkKind::Setup, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(links.peek(&code).await.unwrap().unwrap().id, link.id);
    assert_eq!(links.pending(None).await.unwrap().len(), 1);
    let (first, second) = tokio::join!(links.consume(&code), links.consume(&code));
    assert_eq!(
        [first.unwrap().is_some(), second.unwrap().is_some()]
            .iter()
            .filter(|used| **used)
            .count(),
        1,
        "only one of two at once gets the link"
    );
    assert!(links.peek(&code).await.unwrap().is_none());
    assert!(
        links
            .create(LinkKind::Device, None, None, None, None)
            .await
            .is_err()
    );
    let (code, link) = links
        .create(LinkKind::Invite, None, Some("Maria"), None, None)
        .await
        .unwrap();
    links.revoke(link.id).await.unwrap();
    assert!(links.consume(&code).await.unwrap().is_none());
    db.close().await;
}

#[tokio::test]
async fn writes_of_a_deposed_leader_are_refused() {
    use std::time::Duration;
    let db = test_db!();
    let mut conn = db.store.pool().acquire().await.unwrap();
    let first = rookery_store::leader::take(&mut conn).await.unwrap();
    let old = db.store.fenced(first);
    let params = serde_json::json!({});
    let new = || rookery_store::jobs::NewJob {
        kind: "test.echo",
        params: &params,
        max_attempts: 3,
        run_after: None,
        subject: None,
        enqueued_by: "user:x",
        token: None,
    };
    let kinds = vec!["test.echo".to_owned()];
    let job = old.jobs().enqueue(new()).await.unwrap();
    old.jobs()
        .claim(&kinds, "server:old", Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    let lease = old
        .leases()
        .acquire("fleet/sync", "plugin:fleet/w1", Duration::from_secs(30))
        .await
        .unwrap();
    sqlx::query("INSERT INTO rookery.hosts (id, name, profile) VALUES ('host1', 'studio', '{}')")
        .execute(db.store.pool())
        .await
        .unwrap();
    let cmd = serde_json::json!([]);
    let term = old
        .terminals()
        .insert(rookery_store::terminals::New {
            id: "term_1",
            host: "studio",
            session: "s1",
            owner: None,
            run_as: "agent",
            title: "sh",
            command: &cmd,
            cwd: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();

    // Another server takes the lock: the old leader's writes fail, whatever they are.
    let second = rookery_store::leader::take(&mut conn).await.unwrap();
    assert_eq!(second, first + 1);
    assert_eq!(
        rookery_store::leader::epoch(&mut conn).await.unwrap(),
        Some(second)
    );
    let deposed = |e: rookery_store::Error| {
        assert!(e.is_deposed(), "{e}");
        assert_eq!(
            rookery_proto::RpcError::from(e).error_code().as_str(),
            "UNAVAILABLE"
        );
    };
    deposed(
        old.jobs()
            .heartbeat(job.id, "server:old", Duration::from_secs(30))
            .await
            .unwrap_err(),
    );
    deposed(
        old.jobs()
            .finish(
                job.id,
                "server:old",
                rookery_store::jobs::Outcome::Succeeded(serde_json::json!(1)),
            )
            .await
            .unwrap_err(),
    );
    deposed(
        old.jobs()
            .log_claimed(job.id, "server:old", "info", "late", None)
            .await
            .unwrap_err(),
    );
    deposed(old.jobs().enqueue(new()).await.unwrap_err());
    deposed(
        old.leases()
            .renew(
                "fleet/sync",
                "plugin:fleet/w1",
                lease.fence,
                Duration::from_secs(30),
            )
            .await
            .unwrap_err(),
    );
    deposed(
        old.leases()
            .acquire("fleet/other", "plugin:fleet/w1", Duration::from_secs(30))
            .await
            .unwrap_err(),
    );
    deposed(
        old.terminals()
            .insert(rookery_store::terminals::New {
                id: "term_2",
                host: "studio",
                session: "s2",
                owner: None,
                run_as: "agent",
                title: "sh",
                command: &cmd,
                cwd: None,
                cols: 80,
                rows: 24,
            })
            .await
            .unwrap_err(),
    );
    deposed(old.terminals().set_title(&term.id, "x").await.unwrap_err());
    deposed(
        old.terminals()
            .set_size(&term.id, 100, 40)
            .await
            .unwrap_err(),
    );
    deposed(
        old.terminals()
            .set_last_input(&term.id, &serde_json::json!({"via": "test"}))
            .await
            .unwrap_err(),
    );
    deposed(old.terminals().set_offset(&term.id, 42).await.unwrap_err());
    deposed(
        old.terminals()
            .end(&term.id, "exited", Some(0))
            .await
            .unwrap_err(),
    );
    deposed(old.terminals().delete(&term.id).await.unwrap_err());
    assert_eq!(
        db.store
            .terminals()
            .get(&term.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "running",
        "the refused terminal writes changed nothing"
    );
    assert_eq!(
        db.store.jobs().get(job.id).await.unwrap().unwrap().state,
        "running",
        "the refused writes changed nothing"
    );

    // The new leader's writes go through, and it ends what the old one left running.
    let new_leader = db.store.fenced(second);
    let recovered = new_leader
        .jobs()
        .recover(&kinds, &[], "server:new")
        .await
        .unwrap();
    assert_eq!(recovered[0].state, "queued");
    new_leader
        .leases()
        .renew(
            "fleet/sync",
            "plugin:fleet/w1",
            lease.fence,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    new_leader
        .terminals()
        .set_title(&term.id, "new")
        .await
        .unwrap();
    assert!(new_leader.terminals().delete(&term.id).await.unwrap());
    drop(conn);
    db.close().await;
}
