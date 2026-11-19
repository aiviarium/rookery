//! rookery-core's store on the server's tables.

use rookery_api::{From, Keep, Level, Message};
use rookery_core::store::{self, Batch, Store, When, kind};
use rookery_hosts::{backend::ServerBackend, db::Db};
use rookery_store::testing::TestDb;
use serde_json::json;

const HOST: &str = "0190f0a0b1c27d3e8f9a0b1c2d3e4f50";
const REGISTRY: &str = "0190f0a0b1c27d3e8f9a0b1c2d3e4f51";
const SECRET: &str = "0190f0a0b1c27d3e8f9a0b1c2d3e4f52";

fn on(db: &TestDb) -> Store {
    let threads = Db::start(&db.url).unwrap();
    Store::from_backend(Box::new(ServerBackend::new(
        threads,
        Some(db.store.key().unwrap().clone()),
        None,
        Some(db.store.clone()),
    )))
}

#[tokio::test(flavor = "multi_thread")]
async fn state_lands_in_its_tables() {
    let Some(db) = TestDb::new().await else {
        eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
        return;
    };
    let url = db.url.clone();
    let s = on(&db);
    tokio::task::spawn_blocking(move || {
        let own = store::OWN;
        // A host with its access and tailnet, written in one batch.
        let mut batch = Batch::default();
        batch.write_labelled(
            kind::PROFILE,
            own,
            HOST,
            "studio",
            json!({"type": "node", "access": {"address": "192.0.2.10"}})
                .to_string()
                .into_bytes(),
        );
        batch.write(
            kind::ACCESS,
            own,
            HOST,
            json!({"keys": ["ssh-ed25519 AAAA"]})
                .to_string()
                .into_bytes(),
        );
        batch.write(
            kind::TAILNET,
            own,
            HOST,
            json!({"rename": "forge"}).to_string().into_bytes(),
        );
        s.apply(&batch).unwrap();
        assert_eq!(
            s.find(kind::PROFILE, own, "studio").unwrap().unwrap().id,
            HOST
        );
        assert_eq!(
            s.label_of(kind::PROFILE, own, HOST).unwrap().as_deref(),
            Some("studio")
        );
        assert_eq!(
            s.read_json(kind::TAILNET, own, HOST).unwrap().unwrap()["rename"],
            "forge"
        );
        // A second host may not take the name.
        let other = "0190f0a0b1c27d3e8f9a0b1c2d3e4f5f";
        assert!(matches!(
            s.write_labelled(kind::PROFILE, own, other, "studio", b"{}"),
            Err(store::Error::Taken { .. })
        ));
        assert!(s.relabel(kind::PROFILE, own, HOST, "forge").unwrap());
        // A registry, its login owned by it, and a secret of its own name.
        s.write_labelled(
            kind::REGISTRY,
            own,
            REGISTRY,
            "ghcr",
            json!({"url": "ghcr.io"}).to_string().as_bytes(),
        )
        .unwrap();
        s.write(kind::SECRET, own, &format!("{REGISTRY}/login"), b"hunter2")
            .unwrap();
        s.write_labelled(kind::SECRET, own, SECRET, "sudo/forge", b"pw")
            .unwrap();
        assert_eq!(
            s.read(kind::SECRET, own, &format!("{REGISTRY}/login"))
                .unwrap()
                .unwrap(),
            b"hunter2"
        );
        assert_eq!(s.read(kind::SECRET, own, SECRET).unwrap().unwrap(), b"pw");
        s.write(kind::SECRET, own, SECRET, b"pw2").unwrap();
        assert_eq!(s.labels(kind::SECRET, own).unwrap()[0].name, "sudo/forge");
        assert_eq!(s.read(kind::SECRET, own, SECRET).unwrap().unwrap(), b"pw2");
        // Removing the registry with what it owns.
        let mut batch = Batch::default();
        batch
            .remove(kind::REGISTRY, own, REGISTRY)
            .forget_owned(own, REGISTRY);
        s.apply(&batch).unwrap();
        assert!(
            s.read(kind::SECRET, own, &format!("{REGISTRY}/login"))
                .unwrap()
                .is_none()
        );
        assert!(s.labels(kind::REGISTRY, own).unwrap().is_empty());
        // Credentials, keys, settings, resources, packages.
        s.write(kind::CREDENTIALS, own, HOST, b"{\"password\":\"x\"}")
            .unwrap();
        assert!(s.read(kind::CREDENTIALS, own, HOST).unwrap().is_some());
        s.write_labelled(kind::KEY, own, SECRET, "ring", b"-----BEGIN-----")
            .unwrap();
        assert_eq!(
            s.read(kind::KEY, own, SECRET).unwrap().unwrap(),
            b"-----BEGIN-----"
        );
        s.write(kind::SETTING, own, "retention_days", b"30")
            .unwrap();
        assert_eq!(
            s.entries(kind::SETTING, own).unwrap(),
            vec![("retention_days".to_owned(), b"30".to_vec())]
        );
        s.write(kind::SPEC, own, "r1", b"{\"size\":1}").unwrap();
        // A specification is text, such as YAML, and comes back as it went in.
        let compose = b"services:\n  web:\n    image: busybox:1.37\n";
        s.write(kind::SPEC, own, "r2", compose).unwrap();
        assert_eq!(s.read(kind::SPEC, own, "r2").unwrap().unwrap(), compose);
        assert_eq!(
            s.read(kind::SPEC, own, "r1").unwrap().unwrap(),
            b"{\"size\":1}"
        );
        assert!(s.forget(kind::SPEC, own, "r2").unwrap());
        s.write(kind::RESOURCE, own, "r1", b"{\"id\":\"r1\"}")
            .unwrap();
        assert!(s.forget(kind::SPEC, own, "r1").unwrap());
        assert!(s.read(kind::RESOURCE, own, "r1").unwrap().is_some());
        assert!(s.forget(kind::RESOURCE, own, "r1").unwrap());
        assert!(s.names(kind::RESOURCE, own).unwrap().is_empty());
        s.replace(
            own,
            &[kind::PLUGIN],
            &[(kind::PLUGIN, "hatchery.kdl", b"x".to_vec())],
        )
        .unwrap();
        assert_eq!(s.names(kind::PLUGIN, own).unwrap(), ["hatchery.kdl"]);
        s.write(kind::PROVISIONER, own, "incus/vm", b"{\"a\":1}")
            .unwrap();
        assert_eq!(s.names(kind::PROVISIONER, own).unwrap(), ["incus/vm"]);
        // Key-value, with revisions.
        let first = s.put("box", "k", &json!(1), When::New).unwrap();
        assert!(matches!(
            s.put("box", "k", &json!(2), When::New),
            Err(store::Error::Conflict(Some(1)))
        ));
        assert_eq!(s.put("box", "k", &json!(2), When::At(first)).unwrap(), 2);
        assert_eq!(s.list("box").unwrap()[0].value, json!(2));
        assert!(matches!(
            s.remove("box", "k", When::At(first)),
            Err(store::Error::Conflict(Some(2)))
        ));
        // Messages and the cursor.
        let message = |id, keep| Message {
            id,
            topic: "build/done".into(),
            body: json!({"ok": true}),
            keep,
            from: From {
                level: Level::Host,
                uid: 0,
                session: None,
            },
            time: rookery_api::now(),
        };
        assert!(s.take_in(HOST, &message(1, Keep::Last)).unwrap().is_some());
        assert!(s.take_in(HOST, &message(2, Keep::Last)).unwrap().is_some());
        assert!(s.take_in(HOST, &message(2, Keep::Last)).unwrap().is_none());
        assert_eq!(s.cursor(HOST).unwrap(), 2);
        let (kept, _) = s
            .inbox(&store::Filter {
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert!(s.set_done(HOST, 2, true).unwrap());
    })
    .await
    .unwrap();
    // Each kind is in its own table.
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let count = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(count("SELECT count(*) FROM rookery.hosts WHERE name = 'forge' AND type = 'node' AND tailnet IS NOT NULL").await, 1);
    assert_eq!(count("SELECT count(*) FROM rookery.host_access").await, 1);
    assert_eq!(count("SELECT count(*) FROM rookery.host_keys").await, 1);
    assert_eq!(
        count(
            "SELECT count(*) FROM rookery.secrets WHERE plugin = 'rookery' AND name = 'sudo/forge'"
        )
        .await,
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM rookery.settings WHERE plugin = 'rookery'").await,
        1
    );
    assert_eq!(count("SELECT count(*) FROM rookery.host_messages").await, 1);
    // Key-value goes through Rookery's collection of storage, so each set is a change another reader
    // sees: a live query on `rookery/storage` follows what a host action keeps.
    assert_eq!(
        count("SELECT count(*) FROM rookery.\"storage\" WHERE plugin = 'box' AND key = 'k'").await,
        1
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM rookery.changes WHERE plugin = 'rookery' AND collection = 'storage'"
        )
        .await,
        2
    );
    pool.close().await;
    db.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_does_not_lead_writes_nothing() {
    let Some(db) = TestDb::new().await else {
        eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
        return;
    };
    // No server leads with this epoch: every write of its backend is refused.
    let threads = Db::start(&db.url).unwrap();
    let stale = Store::from_backend(Box::new(ServerBackend::new(
        threads,
        Some(db.store.key().unwrap().clone()),
        Some(41),
        Some(db.store.clone()),
    )));
    let unfenced = on(&db);
    tokio::task::spawn_blocking(move || {
        let mut batch = Batch::default();
        batch.write(
            kind::ACCESS,
            store::OWN,
            HOST,
            json!({"keys": []}).to_string().into_bytes(),
        );
        let refused = stale.apply(&batch).unwrap_err();
        assert!(refused.to_string().contains("does not lead"), "{refused}");
        assert!(stale.put("ui", "layout", &json!(1), When::Always).is_err());
        assert!(
            unfenced
                .read(kind::ACCESS, store::OWN, HOST)
                .unwrap()
                .is_none()
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_is_known_before_it_is_saved_and_forgotten_with_it() {
    let Some(db) = TestDb::new().await else {
        eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
        return;
    };
    let s = on(&db);
    tokio::task::spawn_blocking(move || {
        let own = store::OWN;
        // A first contact records the host's keys before its profile is saved, as adopting does.
        s.write(
            kind::ACCESS,
            own,
            HOST,
            b"{\"host_keys\":[\"ssh-ed25519 AAAA\"]}",
        )
        .unwrap();
        s.write_labelled(kind::PROFILE, own, HOST, "studio", b"{\"type\":\"ssh\"}")
            .unwrap();
        assert!(s.read(kind::ACCESS, own, HOST).unwrap().is_some());
        // Removed, nothing of it is left.
        s.forget(kind::PROFILE, own, HOST).unwrap();
        assert!(s.read(kind::ACCESS, own, HOST).unwrap().is_none());
    })
    .await
    .unwrap();
}

/// The tables of hosts are Rookery's own collections: `data.*` reads them as each caller may, and the
/// sealed columns of the key ring are no fields.
#[tokio::test(flavor = "multi_thread")]
async fn the_tables_of_hosts_are_read_as_collections() {
    let Some(db) = TestDb::new().await else {
        eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
        return;
    };
    let ops = db.user("ops").await;
    let dev = db.user("dev").await;
    // The store of a server registers the tables of hosts, as `rookery serve` does.
    let mut options = db.options();
    options.system_tables = rookery_hosts::backend::system_tables();
    let store = rookery_store::Store::open(options).await.unwrap();
    sqlx::query(
        "INSERT INTO rookery.hosts (id, name, profile, owner) VALUES ($1, 'studio', '{}', $2)",
    )
    .bind(HOST)
    .bind(ops)
    .execute(store.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO rookery.terminals (id, host, session, owner, title) VALUES ('t1', $1, 's1', $2, 'build')",
    )
    .bind(HOST)
    .bind(dev)
    .execute(store.pool())
    .await
    .unwrap();
    store
        .access()
        .grant("env://studio", &format!("user:{dev}"), 1, "system:rookery")
        .await
        .unwrap();
    let names = |caller: rookery_store::Caller, collection: &'static str| {
        let data = store.data("rookery");
        async move {
            data.find(
                &caller,
                collection,
                &rookery_store::collections::Query::default(),
            )
            .await
            .unwrap()
            .rows
        }
    };
    // The host's owner and those with view on it see it; anyone else sees nothing.
    let seen = names(rookery_store::Caller::user(dev), "hosts").await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["name"], "studio");
    assert_eq!(seen[0]["owner"], ops.to_string());
    assert!(names(rookery_store::Caller::system(), "hosts").await.len() == 1);
    assert!(names(rookery_store::Caller::user(ops), "hosts").await.len() == 1);
    // A terminal is on its host, and on the resource of the host it is seen.
    let terminals = names(rookery_store::Caller::user(dev), "terminals").await;
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0]["session"], "s1");
    assert_eq!(terminals[0]["runAs"], "agent");
    assert!(terminals[0].get("lastInput").is_none());
    let stranger = db.user("eve").await;
    assert!(
        names(rookery_store::Caller::user(stranger), "hosts")
            .await
            .is_empty()
    );
    assert!(
        names(rookery_store::Caller::user(stranger), "terminals")
            .await
            .is_empty()
    );
    // The key ring keeps its key out of what anyone reads, admin or not.
    let keys = store.collections().get("rookery", "hostKeys").unwrap();
    for name in ["keyId", "nonce", "ciphertext"] {
        assert!(keys.def.field(name).is_none(), "{name}");
    }
    store.close().await;
    db.close().await;
}
