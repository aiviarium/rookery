//! The sealing key of a server: secrets under it, and moving them to a new one.

use serde_json::json;

#[tokio::test]
async fn a_rekeyed_store_opens_with_the_new_key_only() {
    let Some(db) = rookery_store::testing::TestDb::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::common::config(&db, dir.path());
    let old = config.secret_key.clone().unwrap();
    let mut running = rookery_server::start(config.clone()).await.unwrap();
    running.leading().await;
    let socket = dir.path().join("server.sock");
    let call = |method: &'static str, params| {
        let socket = socket.clone();
        async move { rookery_server::client::call(&socket, method, params).await }
    };
    call(
        "secrets.set",
        json!({"plugin": "fleet", "name": "api", "value": "made-up-secret"}),
    )
    .await
    .unwrap();

    let new = rookery_store::seal::Key::generate();
    let refused = rookery_server::rekey(&config, &new).await.unwrap_err();
    assert!(refused.contains("stop it first"), "{refused}");
    running.stop().await.unwrap();
    let moved = rookery_server::rekey(&config, &new).await.unwrap();
    assert_eq!((moved.secrets, moved.connections), (1, 0));

    // With the old key the server runs and cannot open it; with the new one it can.
    let mut running = rookery_server::start(config.clone()).await.unwrap();
    running.leading().await;
    let refused = call("secrets.get", json!({"plugin": "fleet", "name": "api"}))
        .await
        .unwrap_err();
    assert!(refused.contains("UNAVAILABLE"), "{refused}");
    running.stop().await.unwrap();
    config.secret_key = Some(new.clone());
    let mut running = rookery_server::start(config).await.unwrap();
    running.leading().await;
    let opened = call("secrets.get", json!({"plugin": "fleet", "name": "api"}))
        .await
        .unwrap();
    assert_eq!(opened, json!({"value": "made-up-secret"}));
    let listed = call("secrets.list", json!({})).await.unwrap();
    assert_eq!(listed["secrets"][0]["keyId"], new.id());
    assert_ne!(old.id(), new.id());
    running.stop().await.unwrap();
    db.close().await;
}
