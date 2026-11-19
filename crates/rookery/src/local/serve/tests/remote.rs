//! The command line against a rookery serve of its own over the network: a
//! server of an older version is refused, and its version still read.

use crate::cli::settings::Settings;
use serde_json::{Value, json};

/// A server of rookery 0.5.2 as its portal answers: every call with its version.
async fn older_server(token: &'static str) -> String {
    use axum::extract::ws::{Message, WebSocketUpgrade};
    let portal = axum::routing::get(move |ws: WebSocketUpgrade| async move {
        ws.protocols([token]).on_upgrade(|mut socket| async move {
            while let Some(Ok(Message::Text(text))) = socket.recv().await {
                let asked: Value = serde_json::from_str(text.as_str()).unwrap();
                let said = json!({"id": asked["id"], "result": {"version": "0.5.2", "api": 1, "mode": "serve"}});
                if socket.send(Message::Text(said.to_string().into())).await.is_err() {
                    break;
                }
            }
        })
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new().route(rookery_api::ops::PORTAL, portal);
    tokio::spawn(async move { axum::serve(listener, router).await });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_refuses_an_older_server_and_still_reads_its_version() {
    use crate::cli::{args::Global, client::Client, fail};
    let url = older_server("s3cret-token").await;
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token");
    std::fs::write(&token, "s3cret-token").unwrap();
    let global = Global {
        server: Some(url.clone()),
        token_file: Some(token),
        ..Global::default()
    };
    let kept = Settings::new(Some(dir.path().join("rookery")));
    let (opened, version) = tokio::task::spawn_blocking(move || {
        let opened = Client::open_in(&global, &kept).map(|_| ());
        let version = Client::open_any(&global, &kept).and_then(|client| client.server_version());
        (opened, version)
    })
    .await
    .unwrap();
    let refused = opened.unwrap_err();
    assert_eq!(refused.status, fail::REFUSED);
    assert!(
        refused
            .message
            .contains("is rookery 0.5.2 (API 1), older than this rookery")
            && refused.message.contains("Update the server first"),
        "{}",
        refused.message
    );
    assert_eq!(version.unwrap(), ("0.5.2".to_owned(), 1));
}
