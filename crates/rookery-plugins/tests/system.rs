//! The plugin system on a kernel of its own, on Postgres: installs through Hatchery recorded in
//! `rookery.plugins`, client halves served by their hash, and host halves in the extension host, which
//! call the kernel's facilities and are started again when they end.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Methods, test_kernel};
use rookery_plugins::{HostOptions, Plugins, Setup};
use serde_json::{Value, json};
use tower::ServiceExt;

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
    rookery_plugins::register(&mut methods);
    methods
}

/// A plugin system with its folders in `scratch`, whose registries are those of the checkout.
async fn plugins(k: &TestKernel, scratch: &Path) -> Arc<Plugins> {
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
    k.kernel.set_facility(Plugins::new(Setup {
        paths,
        host: HostOptions {
            bun: PathBuf::from("bun"),
            entry: Some(checkout().join("host/src/main.ts")),
        },
    }));
    let plugins = k.kernel.get_facility::<Plugins>().unwrap();
    plugins.start(&k.kernel).await.unwrap();
    plugins
}

/// A package folder of a plugin `id` at `version`, with these files in its target.
fn package(scratch: &Path, id: &str, version: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = scratch.join(format!("packages/{id}-{version}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("rookery")).unwrap();
    fs::write(
        dir.join("package.kdl"),
        format!(
            "format 1\npackage {{\n  group example\n  id {id}\n  version \"{version}\"\n}}\ntargets {{\n  rookery {{\n    requires {{\n      rookery \">=0.6\"\n    }}\n  }}\n}}\n"
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

/// Installs a package folder as an egg that was sent.
async fn install(k: &TestKernel, plugins: &Arc<Plugins>, dir: &Path) {
    let egg = hatchery::Egg::pack(dir, &[&rookery_api::plugin_contract::RookeryTarget]).unwrap();
    let bytes = egg.bytes().to_vec();
    plugins
        .change(&k.kernel, move |manager| manager.upload(&bytes))
        .await
        .unwrap();
}

fn info<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
}

async fn get(k: &TestKernel, path: &str) -> (StatusCode, Option<String>, Vec<u8>) {
    let router = rookery_plugins::routes().with_state(k.kernel.clone());
    let response = router
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get("cache-control")
        .map(|v| v.to_str().unwrap().to_owned());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, cache, body.to_vec())
}

async fn row(k: &TestKernel, id: &str) -> Option<(String, chrono::DateTime<chrono::Utc>)> {
    sqlx::query_as("SELECT version, updated_at FROM rookery.plugins WHERE id = $1")
        .bind(id)
        .fetch_optional(k.kernel.store.pool())
        .await
        .unwrap()
}

const PANEL: &str = r#"{"apiVersion": "1", "name": "Panel", "client": "client/index.js", "styles": "panel.css",
  "contributes": {"views": [{"id": "panel.main", "title": "Panel"}]}}"#;

#[tokio::test]
async fn installs_are_recorded_and_client_halves_served_by_their_hash() {
    let k = test_kernel!(methods());
    let scratch = tempfile::tempdir().unwrap();
    let plugins = plugins(&k, scratch.path()).await;
    let admin = k.person("Ada", true).await;
    let member = k.person("Bob", false).await;

    let list = k.call(&member, "plugins.list", json!({})).await.unwrap();
    assert_eq!(list["plugins"], json!([]));

    // One that comes with Rookery, from the registry of the provisioning source.
    let refused = k
        .call(&member, "plugins.add", json!({"spec": "github"}))
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), rookery_proto::ErrorCode::Forbidden);
    let added = k
        .call(
            &admin,
            "plugins.add",
            json!({"spec": "github", "allowYoung": true}),
        )
        .await
        .unwrap();
    assert!(
        added["lines"][0].as_str().unwrap().starts_with('✓'),
        "{added}"
    );
    let github = info(&added, "github").unwrap();
    assert_eq!(
        (&github["version"], &github["origin"], &github["state"]),
        (&json!("0.2.0"), &json!("shipped"), &json!("active"))
    );
    assert_eq!(
        github["manifest"]["contributes"]["hostFeatures"][0]["id"],
        "cli"
    );
    assert_eq!(row(&k, "github").await.unwrap().0, "0.2.0");

    // One with a client half, sent as an egg.
    let panel = package(
        scratch.path(),
        "panel",
        "1.0.0",
        &[
            ("plugin.json", PANEL),
            ("client/index.js", "import './chunk.js'; export default {}"),
            ("client/chunk.js", "export const one = 1"),
            ("panel.css", ".panel {}"),
        ],
    );
    install(&k, &plugins, &panel).await;
    let list = k.call(&member, "plugins.list", json!({})).await.unwrap();
    let client = info(&list, "panel").unwrap()["client"].clone();
    let hash = client["hash"].as_str().unwrap().to_owned();
    assert_eq!(
        client["script"],
        format!("/plugins/panel/{hash}/client/index.js")
    );
    assert_eq!(client["styles"], format!("/plugins/panel/{hash}/panel.css"));
    let (status, cache, body) = get(&k, client["script"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(cache.unwrap().contains("immutable"));
    assert_eq!(body, b"import './chunk.js'; export default {}");
    assert_eq!(
        get(&k, &format!("/plugins/panel/{hash}/client/chunk.js"))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        get(&k, client["styles"].as_str().unwrap()).await.0,
        StatusCode::OK
    );
    // Only the client half is served, and only at its hash.
    for path in [
        format!("/plugins/panel/{hash}/plugin.json"),
        format!("/plugins/panel/{hash}/../package.kdl"),
        format!("/plugins/panel/{}/client/index.js", "0".repeat(64)),
        format!("/plugins/github/{hash}/client/index.js"),
    ] {
        assert_eq!(get(&k, &path).await.0, StatusCode::NOT_FOUND, "{path}");
    }

    // A new version: a new hash, the old address gone, the record updated.
    let (_, before) = row(&k, "panel").await.unwrap();
    let newer = package(
        scratch.path(),
        "panel",
        "1.1.0",
        &[
            ("plugin.json", PANEL),
            ("client/index.js", "export default {two: 2}"),
            ("panel.css", ".panel {}"),
        ],
    );
    install(&k, &plugins, &newer).await;
    let list = k.call(&member, "plugins.list", json!({})).await.unwrap();
    let panel = info(&list, "panel").unwrap();
    assert_eq!(panel["version"], "1.1.0");
    assert_ne!(panel["client"]["hash"], json!(hash));
    assert_eq!(
        get(&k, &format!("/plugins/panel/{hash}/client/index.js"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (version, after) = row(&k, "panel").await.unwrap();
    assert_eq!(version, "1.1.0");
    assert!(after > before);

    // What updates there are, and a removal.
    let checked = k.call(&admin, "plugins.check", json!({})).await.unwrap();
    assert!(
        checked["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "github"),
        "{checked}"
    );
    let removed = k
        .call(&admin, "plugins.remove", json!({"id": "panel"}))
        .await
        .unwrap();
    assert!(info(&removed, "panel").is_none());
    assert!(row(&k, "panel").await.is_none());
    assert!(row(&k, "github").await.is_some());
    k.close().await;
}

/// Whether Bun and the checkout's packages are there to run the extension host. CI has both.
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

async fn until<F: std::future::Future<Output = bool>>(what: &str, mut check: impl FnMut() -> F) {
    for _ in 0..300 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what} did not happen in 15 seconds");
}

async fn state(k: &TestKernel, caller: &Caller, id: &str) -> Value {
    let list = k.call(caller, "plugins.list", json!({})).await.unwrap();
    info(&list, id).unwrap()["state"].clone()
}

#[tokio::test]
async fn host_halves_run_call_the_kernel_and_start_again_when_they_end() {
    if !bun_ready() {
        eprintln!("skipped: the extension host needs bun and `bun install`");
        return;
    }
    let k = test_kernel!(methods());
    let scratch = tempfile::tempdir().unwrap();
    let plugins = plugins(&k, scratch.path()).await;
    let admin = k.person("Ada", true).await;
    let member = k.person("Bob", false).await;
    let host = fs::read_to_string(checkout().join("host/test/fixtures/echo/host.ts")).unwrap();
    let echo = package(
        scratch.path(),
        "echo",
        "1.0.0",
        &[
            (
                "plugin.json",
                r#"{"apiVersion": "1", "name": "Echo", "host": "host.ts"}"#,
            ),
            ("host.ts", &host),
        ],
    );
    install(&k, &plugins, &echo).await;
    // It starts when it is first used.
    assert_eq!(state(&k, &member, "echo").await, "inactive");
    assert_eq!(plugins.host_pid(), None);
    let rpc =
        |method: &str, params: Value| json!({"plugin": "echo", "method": method, "params": params});
    let greeted = k
        .call(&member, "plugins.rpc", rpc("greet", json!({"name": "Ada"})))
        .await
        .unwrap();
    let bob = member.user().unwrap().id.clone();
    // A handler runs for the caller; the calls it makes go as the plugin.
    assert_eq!(greeted, json!({"greeting": "Hello, Ada", "as": bob}));
    assert_eq!(state(&k, &member, "echo").await, "active");
    let remembered = k
        .call(
            &member,
            "plugins.rpc",
            rpc("remember", json!({"value": "x"})),
        )
        .await
        .unwrap();
    assert_eq!(remembered, json!({"stored": "x"}));
    let stored = k
        .call(
            &admin,
            "storage.get",
            json!({"plugin": "echo", "key": "last"}),
        )
        .await
        .unwrap();
    assert_eq!(stored["value"], "x");
    let invalid = k
        .call(&member, "plugins.rpc", rpc("greet", json!({})))
        .await
        .unwrap_err();
    assert_eq!(invalid.error_code(), rookery_proto::ErrorCode::Invalid);
    let missing = k
        .call(&member, "plugins.rpc", rpc("none", json!({})))
        .await
        .unwrap_err();
    assert_eq!(missing.error_code(), rookery_proto::ErrorCode::NotFound);

    // A worker that ends is started again.
    let crashed = k
        .call(&member, "plugins.rpc", rpc("crash", json!({})))
        .await
        .unwrap_err();
    assert_eq!(
        crashed.error_code(),
        rookery_proto::ErrorCode::Unavailable,
        "{crashed}"
    );
    let greet = || async {
        k.call(
            &member,
            "plugins.rpc",
            rpc("greet", json!({"name": "again"})),
        )
        .await
        .is_ok()
    };
    until("the host half starting again", || async {
        state(&k, &member, "echo").await == "active"
    })
    .await;
    assert!(greet().await);

    // So is a host that ends, with the plugins that ran in it.
    let pid = plugins.host_pid().unwrap();
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    until("a new extension host", || async {
        plugins.host_pid().is_some_and(|now| now != pid)
            && state(&k, &member, "echo").await == "active"
    })
    .await;
    assert!(greet().await);

    // A removed plugin's host half stops.
    k.call(&admin, "plugins.remove", json!({"id": "echo"}))
        .await
        .unwrap();
    let gone = k
        .call(&member, "plugins.rpc", rpc("greet", json!({"name": "x"})))
        .await
        .unwrap_err();
    assert_eq!(gone.error_code(), rookery_proto::ErrorCode::NotFound);
    k.close().await;
}
