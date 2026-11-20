//! Integration tests for app-service authenticated callbacks (`POST /services/rpc` + `rookery-sdk`),
//! plugin-owned connection types (`connections.test` routing to app services and host halves),
//! and per-scheme `entities.text` providers (app services and host halves).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Methods, RpcError, test_kernel};
use rookery_plugins::{HostOptions, Plugins, Setup};
use rookery_proto::ErrorCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_facilities::register(&mut methods);
    rookery_jobs::register(&mut methods, vec![], rookery_jobs::Options::default());
    rookery_data::register(&mut methods);
    rookery_plugins::register(&mut methods);
    methods
}

struct TestData;

impl rookery_plugins::data::PluginData for TestData {
    fn apply<'a>(
        &'a self,
        kernel: &'a rookery_kernel::Kernel,
        plugin: &'a rookery_plugins::loaded::Loaded,
        declared: &'a rookery_plugins::data::Declared,
        _transforms: rookery_plugins::data::Transforms,
    ) -> futures_util::future::BoxFuture<'a, Result<(), RpcError>> {
        Box::pin(async move {
            rookery_data::plugins::apply(
                kernel,
                &rookery_data::plugins::PluginData {
                    plugin: plugin.id.clone(),
                    collections: Value::Array(declared.collections.clone()),
                    ..Default::default()
                },
                &rookery_data::plugins::NoTransforms,
            )
            .await?;
            Ok(())
        })
    }
}

async fn start_plugins(k: &TestKernel, scratch: &Path) -> Arc<Plugins> {
    let paths = rookery_core::paths::Paths {
        config_dir: scratch.join("config"),
        data_dir: scratch.join("data"),
    };
    fs::create_dir_all(&paths.config_dir).unwrap();
    fs::write(
        paths.settings_file(),
        json!({"source": checkout()}).to_string(),
    )
    .unwrap();
    k.kernel
        .set_facility(rookery_plugins::DataLayer(Arc::new(TestData)));
    k.kernel.set_facility(Plugins::new(Setup {
        paths,
        host: HostOptions {
            bun: PathBuf::from("bun"),
            entry: Some(checkout().join("host/src/main.ts")),
            memory_mb: HostOptions::default().memory_mb,
        },
    }));
    let plugins = k.kernel.get_facility::<Plugins>().unwrap();
    plugins.start(&k.kernel).await.unwrap();
    plugins
}

/// Starts a real HTTP server serving `rookery_plugins::routes()` so `rookery-sdk` can call
/// `POST /services/rpc` over HTTP just like in production.
async fn start_rookery_http(k: &TestKernel) -> String {
    let router = rookery_plugins::routes().with_state(k.kernel.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    url
}

fn package(scratch: &Path, name: &str, version: &str, files: &[(&str, &str)]) -> PathBuf {
    let (group, id) = name.split_once('/').unwrap_or(("example", name));
    let dir = scratch.join(format!("packages/{group}-{id}-{version}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("rookery")).unwrap();
    fs::write(
        dir.join("package.kdl"),
        format!(
            "format 1\npackage {{\n  group {group}\n  id {id}\n  version \"{version}\"\n}}\ntargets {{\n  rookery {{\n    requires {{\n      rookery \">=0.6\"\n    }}\n  }}\n}}\n"
        ),
    )
    .unwrap();
    for (path, text) in files {
        let file = dir.join("rookery").join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    dir
}

async fn install(k: &TestKernel, plugins: &Arc<Plugins>, dir: &Path) {
    let egg = hatchery::Egg::pack(dir, &[&rookery_api::plugin_contract::RookeryTarget]).unwrap();
    let bytes = egg.bytes().to_vec();
    plugins
        .change(&k.kernel, move |manager| manager.upload(&bytes))
        .await
        .unwrap();
}

async fn raw_callback_status(rookery_url: &str, token: Option<&str>, body: Value) -> (u16, Value) {
    let url = format!("{rookery_url}/services/rpc");
    let token = token.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        let mut req = agent.post(&url).header("content-type", "application/json");
        if let Some(tok) = token {
            req = req.header("authorization", &format!("Bearer {tok}"));
        }
        let mut resp = req.send(body.to_string().as_bytes()).unwrap();
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, parsed)
    })
    .await
    .unwrap()
}

fn bun_ready() -> bool {
    let ok = std::process::Command::new("bun")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok && checkout().join("node_modules").exists()
}

#[tokio::test]
async fn app_service_callbacks_enforce_auth_person_scope_and_plugin_level() {
    let k = test_kernel!(methods());
    let scratch = tempfile::tempdir().unwrap();
    let plugins = start_plugins(&k, scratch.path()).await;
    let rookery_url = start_rookery_http(&k).await;

    let admin = k.person("Ada", true).await;
    let alice = k.person("Alice", false).await;
    let bob = k.person("Bob", false).await;

    // Install plugin "alpha" with:
    // - personal-space collection "notes"
    // - global collection "counters"
    // - plugin-owned connection type "alpha.api" with testHandler
    // - entity type "alpha-note" with textHandler
    // - two app services ("worker" and "peer")
    let alpha_manifest = json!({
        "apiVersion": "1",
        "name": "Alpha",
        "contributes": {
            "collections": [
                {
                    "name": "notes",
                    "scope": "space",
                    "fields": [
                        {"name": "title", "type": "text", "required": true},
                        {"name": "body", "type": "text"}
                    ],
                    "entity": {"scheme": "alpha-note", "title": "title"}
                },
                {
                    "name": "counters",
                    "scope": "global",
                    "fields": [
                        {"name": "name", "type": "text", "required": true},
                        {"name": "n", "type": "integer", "required": true}
                    ]
                }
            ],
            "connectionTypes": [
                {
                    "id": "alpha.api",
                    "title": "Alpha API",
                    "testHandler": {"service": "worker", "method": "testConnection"},
                    "fields": {
                        "type": "object",
                        "properties": {
                            "endpoint": {"type": "string"},
                            "apiKey": {"type": "string"}
                        },
                        "required": ["endpoint", "apiKey"]
                    },
                    "secret": ["apiKey"]
                }
            ],
            "entityTypes": [
                {
                    "scheme": "alpha-note",
                    "title": "Alpha Note",
                    "textHandler": {"service": "worker", "method": "entityText"}
                }
            ],
            "spaceKinds": [
                {"id": "alpha.space", "title": "Alpha Space"}
            ]
        },
        "services": [
            {"id": "worker", "title": "Alpha Worker", "provides": "api"},
            {"id": "peer", "title": "Alpha Peer", "provides": "peer"}
        ],
        "provides": {
            "services": ["api", "peer"]
        }
    });
    let alpha_dir = package(
        scratch.path(),
        "example/alpha",
        "0.1.0",
        &[("plugin.json", &alpha_manifest.to_string())],
    );
    install(&k, &plugins, &alpha_dir).await;

    // Install plugin "beta" owning its own connection type so we can prove cross-plugin isolation.
    let beta_manifest = json!({
        "apiVersion": "1",
        "name": "Beta",
        "contributes": {
            "connectionTypes": [
                {
                    "id": "beta.secret",
                    "title": "Beta Secret",
                    "fields": {
                        "type": "object",
                        "properties": {
                            "token": {"type": "string"}
                        },
                        "required": ["token"]
                    },
                    "secret": ["token"]
                }
            ]
        }
    });
    let beta_dir = package(
        scratch.path(),
        "example/beta",
        "0.1.0",
        &[("plugin.json", &beta_manifest.to_string())],
    );
    install(&k, &plugins, &beta_dir).await;

    // Create private spaces for Alice and Bob, and insert a private note in each.
    let alice_space = k
        .call(
            &alice,
            "spaces.create",
            json!({"kind": "alpha.space", "name": "Alice Space"}),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bob_space = k
        .call(
            &bob,
            "spaces.create",
            json!({"kind": "alpha.space", "name": "Bob Space"}),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let alice_note = k
        .call(
            &alice,
            "data.insert",
            json!({
                "plugin": "alpha",
                "collection": "notes",
                "space": alice_space,
                "values": {"title": "Alice Secret", "body": "only for alice"}
            }),
        )
        .await
        .unwrap()["row"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bob_note = k
        .call(
            &bob,
            "data.insert",
            json!({
                "plugin": "alpha",
                "collection": "notes",
                "space": bob_space,
                "values": {"title": "Bob Secret", "body": "only for bob"}
            }),
        )
        .await
        .unwrap()["row"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Create one alpha.api connection (global, owned by alpha) and one beta.secret connection (owned by beta).
    let alpha_conn = k
        .call(
            &admin,
            "connections.create",
            json!({
                "type": "alpha.api",
                "name": "Alpha Prod",
                "config": {"endpoint": "https://alpha.example.test"},
                "credential": "alpha-secret-key-999"
            }),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let beta_conn = k
        .call(
            &admin,
            "connections.create",
            json!({
                "type": "beta.secret",
                "name": "Beta Prod",
                "config": {},
                "credential": "beta-secret-key-888"
            }),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Build real rookery-sdk app service for alpha/worker!
    let inbound_secret = "alpha-worker-inbound-secret".to_owned();
    let callback_secret = "alpha-worker-callback-secret".to_owned();
    let last_call_id: Arc<Mutex<Option<String>>> = Arc::default();
    let captured_call_id = last_call_id.clone();

    let sdk_cfg = rookery_sdk::Config {
        rookery_url: rookery_url.clone(),
        listen: "127.0.0.1:0".to_owned(),
        plugin_id: "alpha".to_owned(),
        service_id: "worker".to_owned(),
        inbound_secret: inbound_secret.clone(),
        callback_secret: callback_secret.clone(),
    };

    // Rookery::new refuses identical inbound and callback secrets.
    assert!(
        rookery_sdk::Rookery::new(rookery_sdk::Config {
            rookery_url: rookery_url.clone(),
            listen: "127.0.0.1:0".to_owned(),
            plugin_id: "alpha".to_owned(),
            service_id: "worker".to_owned(),
            inbound_secret: "same-secret".to_owned(),
            callback_secret: "same-secret".to_owned(),
        })
        .is_err()
    );

    let active_call_notify = Arc::new(tokio::sync::Notify::new());
    let release_call_notify = Arc::new(tokio::sync::Notify::new());
    let active_call_id: Arc<Mutex<Option<String>>> = Arc::default();

    let svc = rookery_sdk::Rookery::new(sdk_cfg).unwrap();
    {
        let captured = captured_call_id.clone();
        svc.method("readSpace", move |ctx, params: Value| {
            let captured = captured.clone();
            async move {
                *captured.lock().unwrap() = Some(ctx.call_id().to_owned());
                let space = params["space"].as_str().unwrap();
                let rows = ctx.data().space(space).find("notes", None, None).await?;
                Ok(json!({ "rows": rows }))
            }
        });
    }
    svc.method("writeSpace", |ctx, params: Value| async move {
        let space = params["space"].as_str().unwrap();
        let title = params["title"].as_str().unwrap();
        ctx.data()
            .space(space)
            .insert("notes", json!({"title": title}))
            .await
    });
    svc.method("tryAdmin", |ctx, _params: Value| async move {
        ctx.call("users.list", json!({})).await
    });
    svc.method("resolveConn", |ctx, params: Value| async move {
        let id = params["id"].as_str().unwrap();
        let resolved = ctx.connections().resolve(id).await?;
        Ok(serde_json::to_value(resolved).unwrap())
    });
    svc.method("bumpCounter", |ctx, params: Value| async move {
        let name = params["name"].as_str().unwrap();
        let row = ctx
            .client()
            .data()
            .insert("counters", json!({"name": name, "n": 1}))
            .await?;
        Ok(json!({ "row": row }))
    });
    svc.method("testConnection", |_ctx, params: Value| async move {
        let conn = &params["connection"];
        let endpoint = conn["values"]["endpoint"]
            .as_str()
            .or_else(|| conn["config"]["endpoint"].as_str())
            .unwrap_or("");
        let has_cred = conn["credential"].as_str().is_some_and(|c| !c.is_empty());
        Ok(json!({
            "ok": endpoint == "https://alpha.example.test" && has_cred,
            "message": format!("tested {endpoint}"),
            "details": {"endpoint": endpoint}
        }))
    });
    svc.method("entityText", |ctx, params: Value| async move {
        let uri = params["uri"].as_str().unwrap();
        let id = params["id"].as_str().unwrap();
        let resolved = ctx
            .entities()
            .resolve(uri)
            .await?
            .ok_or_else(|| rookery_sdk::RpcError::not_found("no entity"))?;
        let mut data = ctx.data();
        if let Some(s) = resolved.space_id.as_deref() {
            data = data.space(s);
        }
        let rows = data.find("notes", Some(json!({"id": id})), None).await?;
        let row = rows
            .first()
            .ok_or_else(|| rookery_sdk::RpcError::not_found("no note"))?;
        Ok(json!({
            "title": row["title"],
            "text": format!("{}: {}", row["title"].as_str().unwrap_or(""), row["body"].as_str().unwrap_or(""))
        }))
    });
    {
        let active_notify = active_call_notify.clone();
        let release_notify = release_call_notify.clone();
        let active_id = active_call_id.clone();
        svc.method("holdCall", move |ctx, _params: Value| {
            let active_notify = active_notify.clone();
            let release_notify = release_notify.clone();
            let active_id = active_id.clone();
            async move {
                *active_id.lock().unwrap() = Some(ctx.call_id().to_owned());
                active_notify.notify_one();
                release_notify.notified().await;
                Ok(json!({"released": true}))
            }
        });
    }

    let worker_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let worker_url = format!("http://{}", worker_listener.local_addr().unwrap());
    let worker_client = svc.client().clone();
    tokio::spawn(async move {
        svc.serve_on(worker_listener, std::future::pending())
            .await
            .unwrap();
    });

    // services.setExternal rejects matching secret and callbackSecret.
    let same_err = k
        .call(
            &admin,
            "services.setExternal",
            json!({
                "plugin": "alpha",
                "service": "worker",
                "url": worker_url,
                "secret": "identical",
                "callbackSecret": "identical"
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(same_err.error_code(), ErrorCode::Invalid);

    // Place alpha/worker with distinct inbound and callback secrets.
    let placed = k
        .call(
            &admin,
            "services.setExternal",
            json!({
                "plugin": "alpha",
                "service": "worker",
                "url": worker_url,
                "secret": inbound_secret,
                "callbackSecret": callback_secret
            }),
        )
        .await
        .unwrap();
    assert_eq!(placed["state"], "ready");

    // Place alpha/peer with its own distinct callback secret (to test cross-service callId theft).
    let peer_callback_secret = "alpha-peer-callback-secret";
    k.call(
        &admin,
        "services.setExternal",
        json!({
            "plugin": "alpha",
            "service": "peer",
            "url": worker_url,
            "secret": "alpha-peer-inbound-secret",
            "callbackSecret": peer_callback_secret
        }),
    )
    .await
    .unwrap();

    // 1. Auth checks on POST /services/rpc:
    // Missing auth -> 401
    let (status, _) = raw_callback_status(
        &rookery_url,
        None,
        json!({"jsonrpc": "2.0", "id": 1, "method": "data.find", "params": {"plugin": "alpha", "collection": "counters"}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED.as_u16());

    // Inbound secret MUST NOT be accepted on /services/rpc -> 401
    let (status, _) = raw_callback_status(
        &rookery_url,
        Some(&inbound_secret),
        json!({"jsonrpc": "2.0", "id": 1, "method": "data.find", "params": {"plugin": "alpha", "collection": "counters"}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED.as_u16());

    // 2. Plugin-level callbacks (no callId) via rookery_sdk::Client:
    // Can write & read own global collection
    let inserted = worker_client
        .data()
        .insert("counters", json!({"name": "boot", "n": 42}))
        .await
        .unwrap();
    assert_eq!(inserted["n"], 42);
    // Can resolve own plugin's connection
    let resolved = worker_client
        .connections()
        .resolve(&alpha_conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.credential.as_deref(), Some("alpha-secret-key-999"));
    // CANNOT resolve beta's connection
    let cross_conn_err = worker_client
        .connections()
        .resolve(&beta_conn)
        .await
        .unwrap_err();
    assert_eq!(cross_conn_err.error_code(), ErrorCode::Forbidden);
    // CANNOT call admin methods at plugin level
    let admin_err = worker_client
        .call("users.list", json!({}))
        .await
        .unwrap_err();
    assert_eq!(admin_err.error_code(), ErrorCode::Forbidden);

    // 3. Invocation-bound callbacks (serving a person):
    // Alice can read her own space through the app service.
    let alice_rows = k
        .call(
            &alice,
            "services.call",
            json!({
                "plugin": "alpha",
                "service": "api",
                "method": "readSpace",
                "params": {"space": alice_space}
            }),
        )
        .await
        .unwrap();
    assert_eq!(alice_rows["rows"].as_array().unwrap().len(), 1);
    assert_eq!(alice_rows["rows"][0]["title"], "Alice Secret");

    // Alice CANNOT read or write Bob's private space through the app service!
    let bob_rows = k
        .call(
            &alice,
            "services.call",
            json!({
                "plugin": "alpha",
                "service": "api",
                "method": "readSpace",
                "params": {"space": bob_space}
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        bob_rows["rows"].as_array().unwrap().len(),
        0,
        "Alice must not read Bob's rows: {bob_rows}"
    );
    let write_escape = k
        .call(
            &alice,
            "services.call",
            json!({
                "plugin": "alpha",
                "service": "api",
                "method": "writeSpace",
                "params": {"space": bob_space, "title": "intrusion"}
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(write_escape.error_code(), ErrorCode::Forbidden);

    // Even when serving an admin (Ada), the app service CANNOT invoke admin methods on /services/rpc!
    let admin_escape = k
        .call(
            &admin,
            "services.call",
            json!({
                "plugin": "alpha",
                "service": "api",
                "method": "tryAdmin",
                "params": {}
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(admin_escape.error_code(), ErrorCode::Forbidden);

    // Serving a person still allows plugin-level writes via ctx.client() (e.g., usage counters).
    let bumped = k
        .call(
            &alice,
            "services.call",
            json!({
                "plugin": "alpha",
                "service": "api",
                "method": "bumpCounter",
                "params": {"name": "alice-req"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(bumped["row"]["name"], "alice-req");

    // Serving a person can resolve alpha's connection, but CANNOT resolve beta's connection.
    let ok_conn = k
        .call(
            &alice,
            "services.call",
            json!({
                "plugin": "alpha",
                "service": "api",
                "method": "resolveConn",
                "params": {"id": alpha_conn}
            }),
        )
        .await
        .unwrap();
    assert_eq!(ok_conn["credential"], "alpha-secret-key-999");

    let denied_conn = k
        .call(
            &alice,
            "services.call",
            json!({
                "plugin": "alpha",
                "service": "api",
                "method": "resolveConn",
                "params": {"id": beta_conn}
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(denied_conn.error_code(), ErrorCode::Forbidden);

    // 4. Expired callId & forged callId & cross-service callId theft:
    let expired_id = last_call_id.lock().unwrap().clone().unwrap();
    let (_, expired_resp) = raw_callback_status(
        &rookery_url,
        Some(&callback_secret),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "callId": expired_id,
            "method": "data.find",
            "params": {"plugin": "alpha", "collection": "notes", "space": alice_space}
        }),
    )
    .await;
    assert_eq!(
        expired_resp["error"]["data"]["code"], "NOT_FOUND",
        "{expired_resp}"
    );

    // Start an in-flight call on alpha/worker as Alice, then try to hijack its callId using alpha/peer's token!
    let k_call = k.kernel.clone();
    let alice_clone = alice.clone();
    let inflight = tokio::spawn(async move {
        k_call
            .methods
            .dispatch(
                rookery_kernel::Call::new(k_call.clone(), alice_clone, None),
                "services.call",
                json!({
                    "plugin": "alpha",
                    "service": "api",
                    "method": "holdCall",
                    "params": {}
                }),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), active_call_notify.notified())
        .await
        .unwrap();
    let live_call_id = active_call_id.lock().unwrap().clone().unwrap();
    let (_, hijack_resp) = raw_callback_status(
        &rookery_url,
        Some(peer_callback_secret),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "callId": live_call_id,
            "method": "data.find",
            "params": {"plugin": "alpha", "collection": "notes", "space": alice_space}
        }),
    )
    .await;
    assert!(
        matches!(
            hijack_resp["error"]["data"]["code"].as_str(),
            Some("FORBIDDEN" | "NOT_FOUND")
        ),
        "{hijack_resp}"
    );
    release_call_notify.notify_one();
    assert_eq!(inflight.await.unwrap().unwrap()["released"], true);

    // 5. Plugin-owned connection type `connections.test` routed to app service testHandler:
    let tested = k
        .call(&admin, "connections.test", json!({"id": alpha_conn}))
        .await
        .unwrap();
    assert_eq!(tested["ok"], true);
    assert_eq!(tested["message"], "tested https://alpha.example.test");
    assert_eq!(tested["details"]["endpoint"], "https://alpha.example.test");

    // 6. Server-side per-scheme `entities.text` routed to app service textHandler:
    // Alice gets her note's text from the provider, and CANNOT get Bob's private note!
    let alice_uri = format!("alpha-note://{alice_note}");
    let bob_uri = format!("alpha-note://{bob_note}");
    let text_res = k
        .call(
            &alice,
            "entities.text",
            json!({"uris": [alice_uri, bob_uri], "maxChars": 500}),
        )
        .await
        .unwrap();
    let items = text_res["texts"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "Bob's private entity must not be returned: {text_res}"
    );
    assert_eq!(items[0]["uri"], alice_uri);
    assert_eq!(items[0]["text"], "Alice Secret: only for alice");
    assert_eq!(text_res["missing"], json!([bob_uri]));
}

#[tokio::test]
async fn host_half_connection_test_and_entity_text_handlers() {
    if !bun_ready() {
        eprintln!("skipped: the extension host needs bun and `bun install`");
        return;
    }
    let k = test_kernel!(methods());
    let scratch = tempfile::tempdir().unwrap();
    let plugins = start_plugins(&k, scratch.path()).await;
    let admin = k.person("Ada", true).await;
    let alice = k.person("Alice", false).await;

    let manifest = json!({
        "apiVersion": "1",
        "name": "Gamma",
        "host": "host.ts",
        "contributes": {
            "connectionTypes": [
                {
                    "id": "gamma.token",
                    "title": "Gamma Token",
                    "test": true,
                    "fields": {
                        "type": "object",
                        "properties": {
                            "baseUrl": {"type": "string"},
                            "token": {"type": "string"}
                        },
                        "required": ["baseUrl", "token"]
                    },
                    "secret": ["token"]
                }
            ],
            "entityTypes": [
                {
                    "scheme": "gamma-item",
                    "title": "Gamma Item"
                }
            ]
        }
    });
    let host_ts = r#"
import { definePlugin } from "@rookery/sdk"

export default definePlugin(async (rookery) => {
  rookery.connections.test("gamma.token", async (_ctx, conn) => {
    const ok = conn.values?.baseUrl === "https://gamma.example.test" && conn.credential === "gamma-tok-123"
    return {
      ok,
      message: ok ? "gamma ok" : "gamma bad",
      details: { hasCred: Boolean(conn.credential) },
    }
  })
  rookery.entities.text("gamma-item", async (ctx, entity) => {
    return {
      title: `Gamma ${entity.id}`,
      text: `Rendered ${entity.uri} for ${ctx.principal?.id ?? "none"}`,
    }
  })
  await rookery.entities.register({
    uri: "gamma-item://42",
    title: "Gamma 42",
    subtitle: "test item",
  })
})
"#;
    let dir = package(
        scratch.path(),
        "example/gamma",
        "0.1.0",
        &[("plugin.json", &manifest.to_string()), ("host.ts", host_ts)],
    );
    install(&k, &plugins, &dir).await;

    let conn_id = k
        .call(
            &admin,
            "connections.create",
            json!({
                "type": "gamma.token",
                "name": "Gamma",
                "config": {"baseUrl": "https://gamma.example.test"},
                "credential": "gamma-tok-123"
            }),
        )
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let tested = k
        .call(&admin, "connections.test", json!({"id": conn_id}))
        .await
        .unwrap();
    assert_eq!(tested["ok"], true, "{tested}");
    assert_eq!(tested["message"], "gamma ok");

    // Grant Alice view access on gamma-item://42; Bob has no grant and must not see its text.
    let bob = k.person("Bob", false).await;
    let alice_id = alice.user().unwrap().id.clone();
    k.call(
        &admin,
        "access.grant",
        json!({
            "resource": "gamma-item://42",
            "principal": format!("user:{alice_id}"),
            "level": "view"
        }),
    )
    .await
    .unwrap();

    let text = k
        .call(
            &alice,
            "entities.text",
            json!({"uris": ["gamma-item://42"]}),
        )
        .await
        .unwrap();
    let items = text["texts"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{text}");
    assert_eq!(items[0]["title"], "Gamma 42");
    assert!(
        items[0]["text"]
            .as_str()
            .unwrap()
            .contains("Rendered gamma-item://42 for"),
        "{text}"
    );

    let denied_text = k
        .call(&bob, "entities.text", json!({"uris": ["gamma-item://42"]}))
        .await
        .unwrap();
    assert_eq!(denied_text["texts"], json!([]));
    assert_eq!(denied_text["missing"], json!(["gamma-item://42"]));
}
