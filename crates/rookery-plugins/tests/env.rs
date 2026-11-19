//! What a host half reaches on a host: `env.*` methods run for the person whose call it serves, and
//! as the plugin alone when it serves no person, with a refusal for anyone without a level on
//! `env://NAME`. Against a rookeryd in single-user mode on this machine and a server of its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Caller, Config, Kernel, Methods};
use rookery_plugins::{HostOptions, Plugins, Setup};
use rookery_proto::ErrorCode;
use serde_json::{Value, json};

const HOST: &str = r#"
import { parentPort } from "node:worker_threads"
import { definePlugin, RookeryError } from "@rookery/sdk"

// A call on the pipe itself, for what the SDK does not offer: asking for an account a plugin
// never gets.
let next = -1
function rawCall(method, params) {
  const id = next--
  return new Promise((resolve, reject) => {
    const listener = (message) => {
      if (message.type !== "result" || message.id !== id) return
      parentPort?.off("message", listener)
      if (message.error)
        reject(new RookeryError(message.error.code, message.error.message, message.error.details))
      else resolve(message.result)
    }
    parentPort?.on("message", listener)
    parentPort?.postMessage({ type: "call", id, method, params })
  })
}

export default definePlugin((rookery) => {
  rookery.services.provide("files", {
    read: async (ctx, { path }) => await rookery.env.readFile(ctx, "studio", path),
    write: async (ctx, { path, content }) =>
      await rookery.env.writeFile(ctx, "studio", path, content),
    stat: async (ctx, { path }) => await rookery.env.stat(ctx, "studio", path),
    run: async (ctx, { command }) => await rookery.env.exec(ctx, "studio", command),
    // Raw requests on the pipe, for what the SDK does not make.
    id: async (ctx) => ctx.callId,
    env: async (ctx, { callId, method, params }) =>
      await rawCall("host.env", { callId: callId ?? ctx.callId, method, params }),
    pipe: async (_ctx, { method, params }) => await rawCall(method, params),
    checkouts: async (ctx) => await rookery.env.checkouts(ctx, "studio"),
    clone: async (ctx, { repository }) => await rookery.env.clone(ctx, "studio", repository),
    worktree: async (ctx, { checkout, branch }) =>
      await rookery.env.worktree(ctx, "studio", checkout, branch),
    remove: async (ctx, { path }) => {
      await rookery.env.removeCheckout(ctx, "studio", path)
    },
  })

  rookery.jobs.handle("sample.check", {
    async run(job, { path }) {
      const data = await rookery.env.readFile(job.ctx, "studio", path)
      await job.log(`read ${data.size}`)
      return { size: data.size }
    },
  })
})
"#;

const MANIFEST: &str = r#"{
  "apiVersion": "1", "name": "Sample", "host": "host.ts",
  "provides": {"services": ["files"]},
  "contributes": {"jobs": [{"id": "sample.check", "title": "Check", "enqueue": "admin", "maxAttempts": 1}]}
}"#;

fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
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

struct Rookeryd {
    child: std::process::Child,
    dir: tempfile::TempDir,
    program: PathBuf,
}

impl Drop for Rookeryd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Rookeryd {
    async fn start() -> Option<Rookeryd> {
        let program = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/rookeryd");
        if !program.is_file() {
            eprintln!("skipped: {} is not built", program.display());
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        std::fs::write(dir.path().join("etc/actions.json"), "[]").unwrap();
        let roots = json!({"roots": [dir.path().join("files").to_string_lossy()]});
        std::fs::write(dir.path().join("etc/rookeryd.json"), roots.to_string()).unwrap();
        let child = std::process::Command::new(&program)
            .arg("--single")
            .arg(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let host = Rookeryd {
            child,
            dir,
            program,
        };
        for _ in 0..100 {
            if host.dir.path().join("control.sock").exists() {
                return Some(host);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }

    fn profile(&self) -> Value {
        json!({
            "type": "managed",
            "access": {"link": [self.program.to_string_lossy(), "connect", "--dir", self.dir.path().to_string_lossy()]},
        })
    }

    fn file(&self, name: &str) -> String {
        self.dir
            .path()
            .join("files")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }
}

async fn server(scratch: &Path) -> Option<(TestKernel, Arc<Plugins>)> {
    let db = rookery_store::testing::TestDb::new().await?;
    let paths = rookery_core::paths::Paths {
        config_dir: scratch.join("config"),
        data_dir: scratch.join("data"),
    };
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(
        paths.settings_file(),
        json!({"source": checkout()}).to_string(),
    )
    .unwrap();
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_facilities::register(&mut methods);
    let hosts = rookery_hosts::Registration::new();
    rookery_jobs::register(&mut methods, hosts.handlers(), Default::default());
    hosts.register(&mut methods);
    rookery_plugins::register(&mut methods);
    let kernel = Kernel::new(
        Config::new("http://127.0.0.1:7777"),
        db.store.clone(),
        methods,
    );
    kernel.set_facility(rookery_hosts::Setup {
        database_url: db.url.clone(),
        secret_key: Some(db.store.key().unwrap().clone()),
        paths: paths.clone(),
        provision: Default::default,
    });
    kernel.set_facility(Plugins::new(Setup {
        paths,
        host: HostOptions {
            bun: PathBuf::from("bun"),
            entry: Some(checkout().join("host/src/main.ts")),
            memory_mb: HostOptions::default().memory_mb,
        },
    }));
    kernel.methods.started(&kernel);
    let plugins = kernel.get_facility::<Plugins>().unwrap();
    plugins.start(&kernel).await.unwrap();
    let serve = rookery_hosts::hosts(&kernel).ready().await.unwrap();
    let source = checkout();
    tokio::task::spawn_blocking(move || {
        serve.paths.set("source", source.to_str().unwrap()).unwrap()
    })
    .await
    .unwrap();
    let k = TestKernel { kernel, db };
    Some((k, plugins))
}

/// Installs a plugin whose host half serves `files` on a host.
async fn install(k: &TestKernel, plugins: &Arc<Plugins>, scratch: &Path) {
    let dir = scratch.join("packages/example-sample-1.0.0");
    std::fs::create_dir_all(dir.join("rookery")).unwrap();
    std::fs::write(
        dir.join("package.kdl"),
        "format 1\npackage {\n  group example\n  id sample\n  version \"1.0.0\"\n}\ntargets {\n  rookery {\n    requires {\n      rookery \">=0.6\"\n    }\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("rookery/plugin.json"), MANIFEST).unwrap();
    std::fs::write(dir.join("rookery/host.ts"), HOST).unwrap();
    let egg = hatchery::Egg::pack(&dir, &[&rookery_api::plugin_contract::RookeryTarget]).unwrap();
    let bytes = egg.bytes().to_vec();
    plugins
        .change(&k.kernel, move |manager| manager.upload(&bytes))
        .await
        .unwrap();
}

fn service(method: &str, params: Value) -> Value {
    json!({"plugin": "sample", "service": "files", "method": method, "params": params})
}

/// The account this test runs as, which a host's agent account is too.
fn whoami() -> String {
    let out = std::process::Command::new("id")
        .arg("-un")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A git repository the test can clone, with one commit on `main`.
fn repository(dir: &Path) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.test"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::create_dir_all(dir).unwrap();
    git(&["init", "-q", "-b", "main", dir.to_str().unwrap()]);
    std::fs::write(dir.join("README.md"), "a repository\n").unwrap();
    git(&["-C", dir.to_str().unwrap(), "add", "-A"]);
    git(&["-C", dir.to_str().unwrap(), "commit", "-q", "-m", "one"]);
}

/// Enqueues `sample.check` and gives how it ended.
async fn check_job(k: &TestKernel, admin: &Caller, path: &str) -> Value {
    let queued = k
        .call(
            admin,
            "jobs.enqueue",
            json!({"kind": "sample/check", "params": {"path": path}}),
        )
        .await
        .unwrap();
    let id = queued["id"].as_str().unwrap().to_owned();
    for _ in 0..300 {
        let job = k.call(admin, "jobs.get", json!({"id": id})).await.unwrap();
        match job["state"].as_str() {
            Some("succeeded" | "failed" | "cancelled") => return job,
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("the job {id} did not end");
}

#[tokio::test(flavor = "multi_thread")]
async fn env_calls_serve_the_person_and_never_reach_beyond_them() {
    if !bun_ready() {
        return;
    }
    let Some(host) = Rookeryd::start().await else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let Some((k, plugins)) = server(scratch.path()).await else {
        return;
    };
    let admin = k.person("Ada", true).await;
    let member = k.person("Bob", false).await;
    let member_id = member
        .user()
        .map(|user| user.id.clone())
        .expect("a person's caller");
    let stranger = k.person("Eve", false).await;
    install(&k, &plugins, scratch.path()).await;
    k.call(
        &Caller::Local,
        "hosts.save",
        json!({"host": "studio", "profile": host.profile()}),
    )
    .await
    .unwrap();
    k.call(
        &admin,
        "access.grant",
        json!({"resource": "env://studio", "principal": format!("user:{member_id}"), "level": "use"}),
    )
    .await
    .unwrap();
    let file = host.file("notes.txt");
    std::fs::write(&file, "one\n").unwrap();

    // A service call a person made runs on their level on the host: it reads and writes as them.
    let read = k
        .call(
            &member,
            "services.call",
            service("read", json!({"path": file})),
        )
        .await
        .unwrap();
    assert_eq!(read["size"], 4, "{read}");
    k.call(
        &member,
        "services.call",
        service(
            "write",
            json!({"path": host.file("written.txt"), "content": "two\n"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(host.file("written.txt")).unwrap(),
        "two\n"
    );
    assert_eq!(
        k.call(
            &member,
            "services.call",
            service("stat", json!({"path": file}))
        )
        .await
        .unwrap()["entry"]["name"],
        "notes.txt"
    );

    // Without a level on the host, the same call is refused, and the host is not reached.
    let refused = k
        .call(
            &stranger,
            "services.call",
            service("read", json!({"path": file})),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), ErrorCode::Forbidden, "{refused}");

    // A command runs on the host as the person, inside the handler's call.
    let me = whoami();
    let ran = k
        .call(
            &member,
            "services.call",
            service("run", json!({"command": ["id", "-un"]})),
        )
        .await
        .unwrap();
    assert_eq!(ran["exitCode"], 0, "{ran}");
    if me != "root" {
        // A suite that runs as root cannot tell the agent account from root by its name.
        assert_eq!(
            ran["stdout"].as_str().unwrap_or_default().trim(),
            me,
            "{ran}"
        );
    }

    // Checkouts are the host's own folder, cloned, worked on and removed through the pipe.
    repository(Path::new(&host.file("source-repo")));
    let empty = k
        .call(&member, "services.call", service("checkouts", json!({})))
        .await
        .unwrap();
    assert_eq!(empty.as_array().map(Vec::len), Some(0), "{empty}");
    let cloned = k
        .call(
            &member,
            "services.call",
            service("clone", json!({"repository": host.file("source-repo")})),
        )
        .await
        .unwrap();
    assert_eq!(cloned["name"], "source-repo", "{cloned}");
    assert_eq!(cloned["branch"], "main", "{cloned}");
    let tree = k
        .call(
            &member,
            "services.call",
            service(
                "worktree",
                json!({"checkout": cloned["path"], "branch": "feature/x"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(tree["branch"], "feature/x", "{tree}");
    assert_eq!(tree["worktreeOf"], cloned["path"], "{tree}");
    let listed = k
        .call(&member, "services.call", service("checkouts", json!({})))
        .await
        .unwrap();
    assert_eq!(listed.as_array().map(Vec::len), Some(2), "{listed}");
    k.call(
        &member,
        "services.call",
        service("remove", json!({"path": tree["path"], "force": true})),
    )
    .await
    .unwrap();
    let left = k
        .call(&member, "services.call", service("checkouts", json!({})))
        .await
        .unwrap();
    assert_eq!(left.as_array().map(Vec::len), Some(1), "{left}");

    // A raw host.env for a call that never was, and for one that has ended.
    let ended = k
        .call(&member, "services.call", service("id", json!({})))
        .await
        .unwrap();
    for call_id in [json!("call-nowhere"), ended] {
        let refused = k
            .call(
                &member,
                "services.call",
                service(
                    "env",
                    json!({
                        "callId": call_id,
                        "method": "env.readFile",
                        "params": {"env": "studio", "path": file},
                    }),
                ),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.error_code(), ErrorCode::NotFound, "{refused}");
    }

    // What a host half has no path to, through host.env and straight on the pipe.
    for (method, params) in [
        ("env.connect", json!({"env": "studio", "port": 80})),
        ("env.portUrl", json!({"env": "studio", "port": 80})),
        (
            "env.exec",
            json!({"env": "studio", "command": ["id", "-un"], "user": "root"}),
        ),
    ] {
        let through = k
            .call(
                &member,
                "services.call",
                service("env", json!({"method": method, "params": params})),
            )
            .await
            .unwrap_err();
        assert_eq!(
            through.error_code(),
            ErrorCode::Forbidden,
            "{method} through host.env: {through}"
        );
        let direct = k
            .call(
                &member,
                "services.call",
                service("pipe", json!({"method": method, "params": params})),
            )
            .await
            .unwrap_err();
        assert_eq!(
            direct.error_code(),
            ErrorCode::Forbidden,
            "{method} directly: {direct}"
        );
    }
    // A server method a host half has no business calling is refused as well.
    let outside = k
        .call(
            &member,
            "services.call",
            service(
                "pipe",
                json!({"method": "plugins.remove", "params": {"id": "sample"}}),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(outside.error_code(), ErrorCode::Forbidden, "{outside}");

    // A plugin's own env call, made straight at the kernel, is refused too.
    let sample = Caller::Plugin("sample".into());
    let alone = k
        .call(
            &sample,
            "env.readFile",
            json!({"env": "studio", "path": file}),
        )
        .await
        .unwrap_err();
    assert_eq!(alone.error_code(), ErrorCode::Forbidden, "{alone}");

    // An `everyone` grant does not reach a plugin: its job still has no level on the host.
    k.call(
        &admin,
        "access.grant",
        json!({"resource": "env://studio", "principal": "everyone", "level": "use"}),
    )
    .await
    .unwrap();
    let failed = check_job(&k, &admin, &file).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    assert!(
        failed["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("use on env://studio")),
        "{failed}"
    );

    // With a grant of its own, the job reads on the plugin's level.
    k.call(
        &admin,
        "access.grant",
        json!({"resource": "env://studio", "principal": "plugin:sample", "level": "use"}),
    )
    .await
    .unwrap();
    let done = check_job(&k, &admin, &file).await;
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(done["result"]["size"], 4, "{done}");

    // A file the person wrote through the plugin is audited for the person and the plugin.
    let audit = k
        .call(
            &admin,
            "audit.list",
            json!({"action": "env.writeFile", "resource": "env://studio"}),
        )
        .await
        .unwrap();
    let entry = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["principal"] == format!("user:{member_id} via plugin:sample"))
        .unwrap_or_else(|| panic!("no such audit entry: {audit}"));
    assert_eq!(entry["action"], "env.writeFile", "{entry}");

    // Removing the plugin takes its grants, so a package that takes its id again starts with none.
    k.call(&admin, "plugins.remove", json!({"id": "sample"}))
        .await
        .unwrap();
    let grants = k
        .call(&admin, "access.grants", json!({"resource": "env://studio"}))
        .await
        .unwrap();
    assert!(
        !grants["grants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|grant| grant["principal"] == "plugin:sample"),
        "{grants}"
    );
    install(&k, &plugins, scratch.path()).await;
    let again = check_job(&k, &admin, &file).await;
    assert_eq!(again["state"], "failed", "{again}");
    k.close().await;
}
