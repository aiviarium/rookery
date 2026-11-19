//! A plugin through the whole server: installed as an egg over `/ws`, its host half started by a call of
//! its method over `/ws` in the extension host that a release carries (`host/dist`), its client half
//! and its shell served, and its cleanups run when the server stops.

use std::fs;
use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as Ws;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::common::http;
use crate::served;

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

async fn connect(served: &crate::common::Served, token: &str) -> Socket {
    let mut request = format!("ws://{}/ws", served.running.addr)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let tcp = tokio::net::TcpStream::connect(served.running.addr)
        .await
        .unwrap();
    tokio_tungstenite::client_async(request, tcp)
        .await
        .unwrap()
        .0
}

async fn call(socket: &mut Socket, id: i64, method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Ws::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        let Ws::Text(text) = socket.next().await.unwrap().unwrap() else {
            continue;
        };
        let message: Value = serde_json::from_str(&text).unwrap();
        if message["id"] == id {
            return message;
        }
    }
}

/// The egg of a package with a host half, a client half and a shell.
fn egg(scratch: &Path) -> Vec<u8> {
    let dir = scratch.join("echo");
    let target = dir.join("rookery");
    fs::create_dir_all(target.join("client")).unwrap();
    fs::create_dir_all(target.join("shell")).unwrap();
    fs::write(
        dir.join("package.kdl"),
        "format 1\npackage {\n  group example\n  id echo\n  version \"1.0.0\"\n}\ntargets {\n  rookery {\n    requires {\n      rookery \">=0.6\"\n    }\n  }\n}\n",
    )
    .unwrap();
    fs::write(
        target.join("plugin.json"),
        r#"{"apiVersion": "1", "name": "Echo", "host": "host.ts", "client": "client/index.js",
            "contributes": {"shell": {"entry": "shell/index.html"}}}"#,
    )
    .unwrap();
    fs::copy(
        checkout().join("host/test/fixtures/echo/host.ts"),
        target.join("host.ts"),
    )
    .unwrap();
    fs::write(target.join("client/index.js"), "export default {}").unwrap();
    fs::write(
        target.join("shell/index.html"),
        "<!doctype html><title>Echo shell</title>",
    )
    .unwrap();
    hatchery::Egg::pack(&dir, &[&rookery_api::plugin_contract::RookeryTarget])
        .unwrap()
        .bytes()
        .to_vec()
}

fn bun_ready() -> bool {
    let ready = std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
        && checkout().join("node_modules/@rookery/sdk").exists();
    if !ready && std::env::var_os("CI").is_some() {
        panic!("CI runs the extension host: it needs bun and `bun install` in the checkout");
    }
    ready
}

#[tokio::test]
async fn a_plugin_installed_over_ws_runs_its_host_half_and_serves_its_halves() {
    if !bun_ready() {
        eprintln!("skipped: the extension host needs bun and `bun install`");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().to_owned();
    let home_for = home.clone();
    let served = served!(|config| {
        let home = home_for;
        let paths = rookery_core::paths::Paths {
            config_dir: home.join("config"),
            data_dir: home.join("data"),
        };
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(
            paths.settings_file(),
            json!({"source": checkout()}).to_string(),
        )
        .unwrap();
        config.paths = Some(paths);
        config.exthost.entry = Some(checkout().join("host/dist/main.js"));
    });
    let admin = served.first_admin("Ada").await;
    let member = served.member(&admin, "Bob").await;

    let mut ws = connect(&served, &admin).await;
    let upload = json!({"base64": rookery_api::b64(&egg(scratch.path()))});
    let added = call(&mut ws, 1, "plugins.upload", upload).await;
    assert!(
        added["result"]["lines"][0]
            .as_str()
            .is_some_and(|l| l.starts_with('✓')),
        "{added}"
    );

    // A member calls a method of its host half, which starts for it.
    let mut ws = connect(&served, &member).await;
    let listed = call(&mut ws, 2, "plugins.list", json!({})).await;
    let echo = listed["result"]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "echo")
        .unwrap_or_else(|| panic!("{added}\n{listed}"))
        .clone();
    assert_eq!(echo["state"], "inactive", "{echo}");
    let greeted = call(
        &mut ws,
        3,
        "plugins.rpc",
        json!({"plugin": "echo", "method": "greet", "params": {"name": "Ada"}}),
    )
    .await;
    assert_eq!(greeted["result"]["greeting"], "Hello, Ada", "{greeted}");
    assert_eq!(greeted["result"]["from"], "echo");
    let refused = call(
        &mut ws,
        4,
        "plugins.rpc",
        json!({"plugin": "echo", "method": "reset", "params": {}}),
    )
    .await;
    assert_eq!(refused["error"]["data"]["code"], "FORBIDDEN", "{refused}");

    // Its client half at its hash, and its shell at `/`, without a sign-in.
    let script = format!(
        "{}{}",
        served.url,
        echo["client"]["script"].as_str().unwrap()
    );
    let (status, body, _) = http("GET", &script, &[], None).await;
    assert_eq!((status, body.as_str()), (200, "export default {}"));
    let (status, body, _) = http("GET", &format!("{}/hosts/studio", served.url), &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("Echo shell"), "{body}");

    // Stopping the server runs the host half's cleanups while the store is open.
    let db = served.db;
    served.running.stop().await.unwrap();
    let disposed = db.store.kv().get("echo", None, "disposed").await.unwrap();
    assert_eq!(disposed.map(|entry| entry.value), Some(json!(true)));
    db.close().await;
}
