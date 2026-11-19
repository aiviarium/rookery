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
    plugins_with(k, scratch, HostOptions::default().memory_mb).await
}

async fn plugins_with(k: &TestKernel, scratch: &Path, memory_mb: u32) -> Arc<Plugins> {
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
            memory_mb,
        },
    }));
    let plugins = k.kernel.get_facility::<Plugins>().unwrap();
    plugins.start(&k.kernel).await.unwrap();
    plugins
}

/// A package folder of a plugin `id`, or `group/id`, at `version`, with these files in its target.
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

/// Installs a package folder as an egg that was sent.
async fn install(k: &TestKernel, plugins: &Arc<Plugins>, dir: &Path) {
    try_install(k, plugins, dir).await.unwrap();
}

async fn try_install(
    k: &TestKernel,
    plugins: &Arc<Plugins>,
    dir: &Path,
) -> Result<Vec<String>, rookery_kernel::RpcError> {
    let egg = hatchery::Egg::pack(dir, &[&rookery_api::plugin_contract::RookeryTarget]).unwrap();
    let bytes = egg.bytes().to_vec();
    plugins
        .change(&k.kernel, move |manager| manager.upload(&bytes))
        .await
        .map(|(lines, _)| lines)
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

    // What is served is what was hashed: a file changed in place is not served under the old address.
    let script = panel["client"]["script"].as_str().unwrap().to_owned();
    let installed = plugins.get("panel").unwrap().dir.join("client/index.js");
    fs::write(&installed, "export default {changed: true}").unwrap();
    assert_eq!(get(&k, &script).await.2, b"export default {two: 2}");

    // An id another package holds is refused, and the holder stays.
    let other = package(
        scratch.path(),
        "elsewhere/panel",
        "9.0.0",
        &[(
            "plugin.json",
            r#"{"apiVersion": "1", "name": "Not the panel"}"#,
        )],
    );
    let refused = try_install(&k, &plugins, &other).await.unwrap_err();
    assert!(
        refused
            .message
            .contains("the plugin id panel is held by the package example/panel"),
        "{refused}"
    );
    let list = k.call(&member, "plugins.list", json!({})).await.unwrap();
    assert_eq!(info(&list, "panel").unwrap()["package"], "example/panel");
    assert_eq!(info(&list, "panel").unwrap()["version"], "1.1.0");

    // A record of a plugin the folder lacks is kept and reported when the server reads the folder, as
    // a new leader does, and a change of other plugins leaves it.
    sqlx::query(
        "INSERT INTO rookery.plugins (id, package, version, origin, digest) VALUES ('gone', 'example/gone', '1.0.0', 'file', 'x')",
    )
    .execute(k.kernel.store.pool())
    .await
    .unwrap();
    plugins.reload(&k.kernel).await.unwrap();
    let list = k.call(&member, "plugins.list", json!({})).await.unwrap();
    assert_eq!(list["missing"], json!(["gone"]));
    assert!(row(&k, "gone").await.is_some());

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
    assert!(row(&k, "gone").await.is_some());
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

fn echo_files(host: &str, manifest: &str) -> Vec<(&'static str, String)> {
    vec![
        ("plugin.json", manifest.to_owned()),
        ("host.ts", host.to_owned()),
    ]
}

const ECHO: &str = r#"{"apiVersion": "1", "name": "Echo", "host": "host.ts"}"#;

async fn echo_like(k: &TestKernel, plugins: &Arc<Plugins>, scratch: &Path, id: &str) {
    echo_with(k, plugins, scratch, id, ECHO).await
}

async fn echo_with(
    k: &TestKernel,
    plugins: &Arc<Plugins>,
    scratch: &Path,
    id: &str,
    manifest: &str,
) {
    let host = fs::read_to_string(checkout().join("host/test/fixtures/echo/host.ts")).unwrap();
    let files = echo_files(&host, manifest);
    let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
    install(k, plugins, &package(scratch, id, "1.0.0", &files)).await;
}

fn rpc(plugin: &str, method: &str, params: Value) -> Value {
    json!({"plugin": plugin, "method": method, "params": params})
}

#[tokio::test]
async fn host_halves_run_alone_call_the_kernel_as_themselves_and_start_again_when_they_end() {
    if !bun_ready() {
        eprintln!("skipped: the extension host needs bun and `bun install`");
        return;
    }
    let k = test_kernel!(methods(), |config| config.call_timeout =
        Duration::from_secs(3));
    let scratch = tempfile::tempdir().unwrap();
    let plugins = plugins(&k, scratch.path()).await;
    let admin = k.person("Ada", true).await;
    let member = k.person("Bob", false).await;
    echo_like(&k, &plugins, scratch.path(), "echo").await;
    echo_like(&k, &plugins, scratch.path(), "other").await;
    echo_with(
        &k,
        &plugins,
        scratch.path(),
        "dep",
        r#"{"apiVersion": "1", "name": "Dep", "host": "host.ts", "dependencies": {"echo": "^1.0"}}"#,
    )
    .await;
    // A host half starts when it is first used.
    assert_eq!(state(&k, &member, "echo").await, "inactive");
    assert_eq!(plugins.host_pid("echo"), None);
    let greeted = k
        .call(
            &member,
            "plugins.rpc",
            rpc("echo", "greet", json!({"name": "Ada"})),
        )
        .await
        .unwrap();
    let bob = member.user().unwrap().id.clone();
    // A handler runs for the caller; the calls it makes go as the plugin.
    assert_eq!(
        greeted,
        json!({"greeting": "Hello, Ada", "as": bob, "from": "echo"})
    );
    assert_eq!(state(&k, &member, "echo").await, "active");
    let remembered = k
        .call(
            &member,
            "plugins.rpc",
            rpc("echo", "remember", json!({"value": "x"})),
        )
        .await
        .unwrap();
    assert_eq!(remembered, json!({"stored": "x"}));
    let stored = |plugin: &'static str, key: &'static str| {
        let (k, admin) = (&k, &admin);
        async move {
            k.call(admin, "storage.get", json!({"plugin": plugin, "key": key}))
                .await
                .unwrap()["value"]
                .clone()
        }
    };
    assert_eq!(stored("echo", "last").await, "x");
    let error = |e: rookery_kernel::RpcError| e.error_code();
    use rookery_proto::ErrorCode;
    let refused = k
        .call(&member, "plugins.rpc", rpc("echo", "greet", json!({})))
        .await
        .unwrap_err();
    assert_eq!(error(refused), ErrorCode::Invalid);
    let missing = k
        .call(&member, "plugins.rpc", rpc("echo", "none", json!({})))
        .await
        .unwrap_err();
    assert_eq!(error(missing), ErrorCode::NotFound);
    // A handler says who may call it.
    let forbidden = k
        .call(&member, "plugins.rpc", rpc("echo", "reset", json!({})))
        .await
        .unwrap_err();
    assert_eq!(error(forbidden), ErrorCode::Forbidden);
    let reset = k
        .call(&admin, "plugins.rpc", rpc("echo", "reset", json!({})))
        .await
        .unwrap();
    assert_eq!(reset, json!({"reset": true}));

    // It gets none of the server's environment beyond what it needs.
    let env = k
        .call(&member, "plugins.rpc", rpc("echo", "env", json!({})))
        .await
        .unwrap();
    assert_eq!(env["keys"], json!(["ROOKERY_EXTHOST_PROTOCOL"]), "{env}");

    // Each plugin has a process of its own, and what one writes to its pipe is its own, whichever
    // plugin it names.
    k.call(
        &member,
        "plugins.rpc",
        rpc("other", "greet", json!({"name": "x"})),
    )
    .await
    .unwrap();
    let (echo_pid, other_pid) = (
        plugins.host_pid("echo").unwrap(),
        plugins.host_pid("other").unwrap(),
    );
    assert_ne!(echo_pid, other_pid);
    k.call(
        &member,
        "plugins.rpc",
        rpc("echo", "forge", json!({"as": "other"})),
    )
    .await
    .unwrap();
    until("the forged call to land", || async {
        stored("echo", "forged").await == json!(true)
    })
    .await;
    assert_eq!(stored("other", "forged").await, Value::Null);
    // The forged unregistering took echo's own greet away, and left other's.
    let gone = k
        .call(
            &member,
            "plugins.rpc",
            rpc("echo", "greet", json!({"name": "x"})),
        )
        .await
        .unwrap_err();
    assert_eq!(error(gone), ErrorCode::NotFound);
    let still = k
        .call(
            &member,
            "plugins.rpc",
            rpc("other", "greet", json!({"name": "x"})),
        )
        .await
        .unwrap();
    assert_eq!(still["from"], "other");
    // Nor can it open another host's pipes or read its environment, or the server's: they are not
    // dumpable, so their /proc files are root's.
    for pid in [other_pid, std::process::id()] {
        let peeked = k
            .call(
                &member,
                "plugins.rpc",
                rpc("echo", "peek", json!({"pid": pid})),
            )
            .await
            .unwrap();
        assert_eq!(
            peeked,
            json!({"fd": "EACCES", "environ": "EACCES"}),
            "{pid}"
        );
    }

    // A worker that ends is started again, in a process of its own, as is one that keeps running a
    // call past its deadline; neither touches the other plugin.
    let crashed = k
        .call(&member, "plugins.rpc", rpc("echo", "crash", json!({})))
        .await
        .unwrap_err();
    assert_eq!(crashed.error_code(), ErrorCode::Unavailable, "{crashed:?}");
    until("echo starting again", || async {
        state(&k, &member, "echo").await == "active"
            && plugins.host_pid("echo").is_some_and(|pid| pid != echo_pid)
    })
    .await;
    let echo_pid = plugins.host_pid("echo").unwrap();
    let late = k
        .call(&member, "plugins.rpc", rpc("echo", "stuck", json!({})))
        .await
        .unwrap_err();
    assert_eq!(error(late), ErrorCode::Timeout);
    until(
        "echo ended and started again after a call past its deadline",
        || async {
            state(&k, &member, "echo").await == "active"
                && plugins.host_pid("echo").is_some_and(|pid| pid != echo_pid)
        },
    )
    .await;
    assert_eq!(plugins.host_pid("other"), Some(other_pid));
    assert_eq!(state(&k, &member, "other").await, "active");

    // A process that is killed is started again too.
    let echo_pid = plugins.host_pid("echo").unwrap();
    unsafe { libc::kill(echo_pid as i32, libc::SIGKILL) };
    until("a new process for echo", || async {
        state(&k, &member, "echo").await == "active"
            && plugins.host_pid("echo").is_some_and(|pid| pid != echo_pid)
    })
    .await;
    k.call(
        &member,
        "plugins.rpc",
        rpc("echo", "greet", json!({"name": "x"})),
    )
    .await
    .unwrap();

    // One that keeps ending is failed, and only an admin starts it again.
    until("echo failing", || async {
        if state(&k, &member, "echo").await == "failed" {
            return true;
        }
        let _ = k
            .call(&member, "plugins.rpc", rpc("echo", "crash", json!({})))
            .await;
        false
    })
    .await;
    let refused = k
        .call(&member, "plugins.activate", json!({"id": "echo"}))
        .await
        .unwrap_err();
    assert_eq!(error(refused), ErrorCode::Forbidden);
    let refused = k
        .call(
            &member,
            "plugins.rpc",
            rpc("echo", "greet", json!({"name": "x"})),
        )
        .await
        .unwrap_err();
    assert_eq!(error(refused), ErrorCode::Unavailable);
    // Nor does a plugin that depends on it start it for one who is not an admin.
    let refused = k
        .call(
            &member,
            "plugins.rpc",
            rpc("dep", "greet", json!({"name": "x"})),
        )
        .await
        .unwrap_err();
    assert_eq!(error(refused), ErrorCode::Unavailable);
    assert_eq!(state(&k, &member, "echo").await, "failed");
    let again = k
        .call(&admin, "plugins.activate", json!({"id": "echo"}))
        .await
        .unwrap();
    assert_eq!(again["state"], "active");

    // A removed plugin's host half runs its cleanups and stops.
    k.call(&admin, "plugins.remove", json!({"id": "echo"}))
        .await
        .unwrap();
    assert_eq!(stored("echo", "disposed").await, json!(true));
    assert_eq!(plugins.host_pid("echo"), None);
    let gone = k
        .call(
            &member,
            "plugins.rpc",
            rpc("echo", "greet", json!({"name": "x"})),
        )
        .await
        .unwrap_err();
    assert_eq!(error(gone), ErrorCode::NotFound);
    k.close().await;
}

/// A datastore that records what it is given, and runs each transform step of the migrations.
struct FakeData(std::sync::Mutex<Vec<(String, usize, Vec<i64>)>>);

impl rookery_plugins::PluginData for FakeData {
    fn apply<'a>(
        &'a self,
        _: &'a rookery_kernel::Kernel,
        plugin: &'a rookery_plugins::Loaded,
        declared: &'a rookery_plugins::Declared,
        transforms: rookery_plugins::Transforms,
    ) -> futures_util::future::BoxFuture<'a, Result<(), rookery_kernel::RpcError>> {
        Box::pin(async move {
            self.0.lock().unwrap().push((
                plugin.id.clone(),
                declared.collections.len(),
                declared.migrations.iter().map(|m| m.version).collect(),
            ));
            for migration in &declared.migrations {
                for step in migration.body["steps"].as_array().into_iter().flatten() {
                    if step["op"] == "transform" {
                        let name = step["name"].as_str().unwrap();
                        transforms
                            .run(name, &format!("m-{}", migration.version))
                            .await?;
                    }
                }
            }
            Ok(())
        })
    }
}

static TX: std::sync::Mutex<Vec<(String, Value)>> = std::sync::Mutex::new(vec![]);

fn methods_with_tx() -> Methods {
    let mut methods = methods();
    methods
        .add(
            "data.txRows",
            |call: rookery_kernel::Call, p: Value| async move {
                TX.lock()
                    .unwrap()
                    .push((format!("rows as {}", call.principal().id), p.clone()));
                let rows = if p.get("after").is_some_and(|a| !a.is_null()) {
                    json!([])
                } else {
                    json!([{"id": "n1", "text": "one"}])
                };
                Ok(json!({ "rows": rows }))
            },
        )
        .level(rookery_kernel::Level::Plugin);
    methods
        .add(
            "data.txInsert",
            |call: rookery_kernel::Call, p: Value| async move {
                TX.lock()
                    .unwrap()
                    .push((format!("insert as {}", call.principal().id), p.clone()));
                Ok(json!({"row": p["values"]}))
            },
        )
        .level(rookery_kernel::Level::Plugin);
    methods
}

const NOTES: &str = r#"{"apiVersion": "1", "name": "Notes",
  "contributes": {"collections": [
    {"name": "notes", "fields": [{"name": "text", "type": "text"}]},
    {"name": "copies", "fields": [{"name": "of", "type": "text"}, {"name": "text", "type": "text"}]}]}}"#;

#[tokio::test]
async fn declared_data_goes_to_the_datastore_and_transform_steps_run_in_the_host() {
    let k = test_kernel!(methods_with_tx());
    let scratch = tempfile::tempdir().unwrap();
    // Without a datastore, a plugin with collections is not used.
    let plugins = plugins(&k, scratch.path()).await;
    let admin = k.person("Ada", true).await;
    let notes = package(
        scratch.path(),
        "notes",
        "1.0.0",
        &[
            ("plugin.json", NOTES),
            (
                "migrations/0001.json",
                r#"{"onInstall": true, "steps": [{"op": "transform", "name": "copy"}]}"#,
            ),
            (
                "migrations/copy.ts",
                &fs::read_to_string(
                    checkout().join("host/test/fixtures/migrating/migrations/copy.ts"),
                )
                .unwrap(),
            ),
        ],
    );
    install(&k, &plugins, &notes).await;
    let list = k.call(&admin, "plugins.list", json!({})).await.unwrap();
    let found = info(&list, "notes").unwrap();
    assert_eq!(found["state"], "invalid");
    assert!(
        found["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no datastore"),
        "{found}"
    );
    if !bun_ready() {
        eprintln!("skipped the transform step: the extension host needs bun and `bun install`");
        k.close().await;
        return;
    }
    // With one, it is applied, and its transform step reads and writes as the plugin.
    let data = Arc::new(FakeData(Default::default()));
    k.kernel
        .set_facility(rookery_plugins::DataLayer(data.clone()));
    plugins.reload(&k.kernel).await.unwrap();
    let list = k.call(&admin, "plugins.list", json!({})).await.unwrap();
    assert_eq!(info(&list, "notes").unwrap()["state"], "active", "{list}");
    assert_eq!(*data.0.lock().unwrap(), [("notes".to_owned(), 2, vec![1])]);
    let tx = TX.lock().unwrap().clone();
    let kinds: Vec<&str> = tx.iter().map(|(what, _)| what.as_str()).collect();
    assert_eq!(kinds, ["rows as notes", "insert as notes", "rows as notes"]);
    assert_eq!(tx[1].1["migration"], "m-1");
    assert_eq!(tx[1].1["values"], json!({"of": "n1", "text": "ONE"}));
    // Read again without a change, it is not applied again.
    plugins.reload(&k.kernel).await.unwrap();
    assert_eq!(data.0.lock().unwrap().len(), 1);
    k.close().await;
}

#[tokio::test]
async fn a_host_past_its_memory_is_ended_as_a_crash() {
    if !bun_ready() {
        eprintln!("skipped: the extension host needs bun and `bun install`");
        return;
    }
    let k = test_kernel!(methods(), |config| config.call_timeout =
        Duration::from_secs(20));
    let scratch = tempfile::tempdir().unwrap();
    let plugins = plugins_with(&k, scratch.path(), 200).await;
    let member = k.person("Bob", false).await;
    echo_like(&k, &plugins, scratch.path(), "echo").await;
    k.call(
        &member,
        "plugins.rpc",
        rpc("echo", "greet", json!({"name": "x"})),
    )
    .await
    .unwrap();
    let pid = plugins.host_pid("echo").unwrap();
    let ended = k
        .call(
            &member,
            "plugins.rpc",
            rpc("echo", "hog", json!({"mb": 400})),
        )
        .await
        .unwrap_err();
    assert_eq!(
        ended.error_code(),
        rookery_proto::ErrorCode::Unavailable,
        "{ended:?}"
    );
    until("echo starting again after its memory ran over", || async {
        state(&k, &member, "echo").await == "active"
            && plugins.host_pid("echo").is_some_and(|now| now != pid)
    })
    .await;
    k.close().await;
}

const JOBS: &str = r#"{"apiVersion": "1", "name": "Echo", "host": "host.ts",
  "contributes": {"jobs": [
    {"id": "echo.build", "title": "Build"},
    {"id": "echo.wait", "title": "Wait", "maxAttempts": 1},
    {"id": "echo.wipe", "title": "Wipe", "enqueue": "admin"}]}}"#;

async fn job_ends(k: &TestKernel, caller: &Caller, id: &Value) -> Value {
    for _ in 0..300 {
        let job = k.call(caller, "jobs.get", json!({"id": id})).await.unwrap();
        if ["succeeded", "failed", "cancelled"].contains(&job["state"].as_str().unwrap_or("")) {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the job {id} did not end in 15 seconds");
}

#[tokio::test]
async fn declared_jobs_run_in_the_host_half_for_whom_the_manifest_allows() {
    if !bun_ready() {
        eprintln!("skipped: the extension host needs bun and `bun install`");
        return;
    }
    let k = test_kernel!(methods());
    let scratch = tempfile::tempdir().unwrap();
    let plugins = plugins(&k, scratch.path()).await;
    let admin = k.person("Ada", true).await;
    let member = k.person("Bob", false).await;
    echo_with(&k, &plugins, scratch.path(), "echo", JOBS).await;
    use rookery_proto::ErrorCode;

    // A member enqueues a job of a kind the plugin declares; its host half starts and runs it.
    let job = k
        .call(
            &member,
            "jobs.enqueue",
            json!({"kind": "echo/build", "params": {"n": 2}}),
        )
        .await
        .unwrap();
    assert_eq!(
        job["enqueuedBy"],
        format!("user:{}", member.user().unwrap().id)
    );
    let done = job_ends(&k, &member, &job["id"]).await;
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(
        done["result"],
        json!({"built": 4, "attempt": 1, "kind": "echo/build"})
    );
    let output = k
        .call(&member, "jobs.output", json!({"id": job["id"]}))
        .await
        .unwrap();
    assert!(output.to_string().contains("building 2"), "{output}");

    // One the manifest keeps for admins, and a kind nobody declares.
    let refused = k
        .call(&member, "jobs.enqueue", json!({"kind": "echo/wipe"}))
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), ErrorCode::Forbidden);
    let wiped = k
        .call(&admin, "jobs.enqueue", json!({"kind": "echo/wipe"}))
        .await
        .unwrap();
    assert_eq!(
        job_ends(&k, &admin, &wiped["id"]).await["state"],
        "succeeded"
    );
    let unknown = k
        .call(&member, "jobs.enqueue", json!({"kind": "echo/nothing"}))
        .await
        .unwrap_err();
    assert_eq!(unknown.error_code(), ErrorCode::Forbidden);
    let subject = k
        .call(
            &member,
            "jobs.enqueue",
            json!({"kind": "echo/build", "params": {"n": 1}, "subject": "env://studio"}),
        )
        .await
        .unwrap_err();
    assert_eq!(subject.error_code(), ErrorCode::Forbidden);

    // A job that is cancelled ends its handler.
    let waiting = k
        .call(&member, "jobs.enqueue", json!({"kind": "echo/wait"}))
        .await
        .unwrap();
    until("the job running", || async {
        k.call(&member, "jobs.get", json!({"id": waiting["id"]}))
            .await
            .unwrap()["state"]
            == "running"
    })
    .await;
    k.call(&member, "jobs.cancel", json!({"id": waiting["id"]}))
        .await
        .unwrap();
    let ended = job_ends(&k, &member, &waiting["id"]).await;
    assert_ne!(ended["state"], "succeeded", "{ended}");

    // A host half hears its own events by a name without a `/`.
    let rpc = |method: &str| json!({"plugin": "echo", "method": method, "params": {}});
    k.call(&member, "plugins.rpc", rpc("listen")).await.unwrap();
    k.call(&member, "plugins.rpc", rpc("ping")).await.unwrap();
    until("the event reaching the host half", || async {
        k.call(&member, "plugins.rpc", rpc("heard")).await.unwrap()["heard"]
            == json!(["echo/pinged"])
    })
    .await;

    // A removed plugin's kinds have no handler: their jobs wait.
    k.call(&admin, "plugins.remove", json!({"id": "echo"}))
        .await
        .unwrap();
    let after = k
        .call(
            &member,
            "jobs.enqueue",
            json!({"kind": "echo/build", "params": {"n": 1}}),
        )
        .await
        .unwrap_err();
    assert_eq!(after.error_code(), ErrorCode::Forbidden);
    k.close().await;
}

const SERVICES: &str = r#"{"apiVersion": "1", "name": "Echo", "host": "host.ts",
  "provides": {"services": ["greeter", "remote"]},
  "extensionPoints": [{"id": "echo.links", "schema": {"type": "object", "required": ["url"]}}],
  "services": [{"id": "backend", "title": "Backend", "provides": "remote"}],
  "contributes": {
    "extensions": {"echo.links": [{"url": "https://example.test", "service": "greeter"}, {"nourl": true}]},
    "rpc": [{"method": "fromBackend", "handler": {"service": "backend", "method": "hi"}}]}}"#;

/// What an app service of a test was sent: the authorization header and the request.
type Seen = Arc<std::sync::Mutex<Vec<(Option<String>, Value)>>>;

/// An app service: JSON-RPC 2.0 on `POST /rpc`, answering with what it was sent.
async fn backend() -> (String, Seen) {
    use axum::routing::post;
    let seen: Seen = Arc::default();
    let kept = seen.clone();
    let app = axum::Router::new().route(
        "/rpc",
        post(move |headers: axum::http::HeaderMap, body: String| {
            let kept = kept.clone();
            async move {
                let request: Value = serde_json::from_str(&body).unwrap();
                let auth = headers
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned());
                kept.lock().unwrap().push((auth, request.clone()));
                axum::Json(json!({"jsonrpc": "2.0", "id": request["id"],
                    "result": {"said": request["method"], "params": request["params"], "as": request["ctx"]["principal"]["id"]}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

#[tokio::test]
async fn extension_points_and_services_of_host_halves_and_app_services() {
    if !bun_ready() {
        eprintln!("skipped: the extension host needs bun and `bun install`");
        return;
    }
    let k = test_kernel!(methods());
    let scratch = tempfile::tempdir().unwrap();
    let plugins = plugins(&k, scratch.path()).await;
    let admin = k.person("Ada", true).await;
    let member = k.person("Bob", false).await;
    echo_with(&k, &plugins, scratch.path(), "echo", SERVICES).await;
    use rookery_proto::ErrorCode;

    // The contributions that match the point's schema, with the service each names.
    let listed = k
        .call(&member, "extensions.list", json!({"point": "echo.links"}))
        .await
        .unwrap();
    assert_eq!(
        listed["contributions"],
        json!([{"plugin": "echo", "value": {"url": "https://example.test", "service": "greeter"}, "service": "greeter"}])
    );
    let none = k
        .call(
            &member,
            "extensions.list",
            json!({"point": "nobody.declares"}),
        )
        .await
        .unwrap();
    assert_eq!(none["contributions"], json!([]));

    // A service the host half provides, called by a person and by a plugin.
    let hello = k
        .call(
            &member,
            "services.call",
            json!({"plugin": "echo", "service": "greeter", "method": "hello", "params": {"name": "Bob"}}),
        )
        .await
        .unwrap();
    assert_eq!(
        hello,
        json!({"hello": "Bob", "from": "echo", "as": member.user().unwrap().id})
    );
    let asked = k
        .call(
            &member,
            "plugins.rpc",
            json!({"plugin": "echo", "method": "ask", "params": {"plugin": "echo", "service": "greeter"}}),
        )
        .await
        .unwrap();
    assert_eq!(asked["as"], "echo", "{asked}");
    let from_host = k
        .call(
            &member,
            "plugins.rpc",
            json!({"plugin": "echo", "method": "contributions", "params": {"point": "echo.links"}}),
        )
        .await
        .unwrap();
    assert_eq!(from_host[0]["service"], "greeter");
    let undeclared = k
        .call(
            &member,
            "services.call",
            json!({"plugin": "echo", "service": "nothing", "method": "x"}),
        )
        .await
        .unwrap_err();
    assert_eq!(undeclared.error_code(), ErrorCode::NotFound);

    // An app service is nowhere until an admin places it.
    let status = k
        .call(
            &member,
            "services.status",
            json!({"plugin": "echo", "service": "backend"}),
        )
        .await
        .unwrap();
    assert_eq!(status["state"], "absent");
    let absent = k
        .call(
            &member,
            "services.call",
            json!({"plugin": "echo", "service": "remote", "method": "hi"}),
        )
        .await
        .unwrap_err();
    assert_eq!(absent.error_code(), ErrorCode::Unavailable);
    let (url, seen) = backend().await;
    let place = json!({"plugin": "echo", "service": "backend", "url": url, "secret": "s3cret"});
    let refused = k
        .call(&member, "services.setExternal", place.clone())
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), ErrorCode::Forbidden);
    k.call(&admin, "services.setExternal", place).await.unwrap();
    let listed = k
        .call(&member, "services.list", json!({"plugin": "echo"}))
        .await
        .unwrap();
    assert_eq!(listed["services"][0]["status"]["state"], "ready");
    assert!(listed["services"][0].get("url").is_none(), "{listed}");

    // services.call of the service it provides, and a method contributes.rpc binds to it.
    let remote = k
        .call(
            &member,
            "services.call",
            json!({"plugin": "echo", "service": "remote", "method": "hi", "params": {"n": 1}}),
        )
        .await
        .unwrap();
    let bob = member.user().unwrap().id.clone();
    assert_eq!(remote, json!({"said": "hi", "params": {"n": 1}, "as": bob}));
    let bound = k
        .call(
            &member,
            "plugins.rpc",
            json!({"plugin": "echo", "method": "fromBackend", "params": {"n": 2}}),
        )
        .await
        .unwrap();
    assert_eq!(bound, json!({"said": "hi", "params": {"n": 2}, "as": bob}));
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(
        seen.iter()
            .all(|(auth, _)| auth.as_deref() == Some("Bearer s3cret"))
    );
    k.close().await;
}
