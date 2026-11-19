//! The command line as a script sees it: the exit statuses of the documented
//! table, and `-o json` printing the API's own documents. It runs the rookery
//! that was built, against a `rookery serve` on a fresh database of the
//! Postgres of `ROOKERY_TEST_POSTGRES`, on its socket, in scratch directories,
//! with made-up hosts.
#![cfg(target_os = "linux")]

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A scratch installation: its directories, its database, and its `rookery serve`.
struct Scratch {
    dir: tempfile::TempDir,
    serve: Option<Child>,
    db: Option<rookery_store::testing::TestDb>,
    runtime: tokio::runtime::Runtime,
}

impl Scratch {
    /// A server on a database of its own, or nothing without a Postgres to test on.
    fn new() -> Option<Scratch> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let db = runtime.block_on(rookery_store::testing::TestDb::new())?;
        let dir = tempfile::Builder::new().prefix("bc").tempdir().unwrap();
        for sub in ["r", "c", "d", "home"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        let mut scratch = Scratch {
            dir,
            serve: None,
            db: Some(db),
            runtime,
        };
        scratch.serve();
        Some(scratch)
    }

    fn command(&self) -> Command {
        let path = self.dir.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_rookery"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", path.join("home"))
            .env("XDG_RUNTIME_DIR", path.join("r"))
            .env("XDG_CONFIG_HOME", path.join("c"))
            .env("XDG_DATA_HOME", path.join("d"))
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Run rookery with `args`: its exit status, standard output and error.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let done = self.command().args(args).output().unwrap();
        (
            done.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&done.stdout).into_owned(),
            String::from_utf8_lossy(&done.stderr).into_owned(),
        )
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("r/rookery/server.sock")
    }

    /// Start `rookery serve` and wait until it leads.
    fn serve(&mut self) {
        let db = self.db.as_ref().unwrap();
        let child = self
            .command()
            .args(["serve"])
            .env("ROOKERY_DATABASE_URL", &db.url)
            .env("ROOKERY_SECRET_KEY", db.store.key().unwrap().to_hex())
            .env("ROOKERY_LISTEN", "127.0.0.1:0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.serve = Some(child);
        let start = Instant::now();
        while std::os::unix::net::UnixStream::connect(self.socket()).is_err()
            || call(&self.socket(), "hosts.list", json!({})).is_err()
        {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "rookery serve did not start"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// What the server answers to a method, as JSON.
    fn call(&self, method: &str, params: Value) -> Value {
        call(&self.socket(), method, params).unwrap_or_else(|error| panic!("{method}: {error}"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(mut serve) = self.serve.take() {
            let _ = serve.kill();
            let _ = serve.wait();
        }
        if let Some(db) = self.db.take() {
            self.runtime.block_on(db.close());
        }
    }
}

/// One call over the socket's `POST /rpc`: its result, or its error.
fn call(socket: &Path, method: &str, params: Value) -> Result<Value, String> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(|e| e.to_string())?;
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    write!(
        stream,
        "POST /rpc HTTP/1.0\r\nHost: rookery\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| e.to_string())?;
    let mut answer = String::new();
    stream
        .read_to_string(&mut answer)
        .map_err(|e| e.to_string())?;
    let (_, body) = answer.split_once("\r\n\r\n").ok_or("no answer")?;
    let said: Value = serde_json::from_str(body).map_err(|e| format!("{e}: {body}"))?;
    match said.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(said["error"].to_string()),
    }
}

#[test]
fn exit_statuses_follow_the_documented_table() {
    let Some(scratch) = Scratch::new() else {
        eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
        return;
    };
    let (status, _, said) = scratch.run(&[
        "host",
        "add",
        "studio",
        "192.0.2.10",
        "--type",
        "ssh",
        "--no-contact",
    ]);
    assert_eq!(status, 0, "{said}");
    let table: &[(&[&str], i32)] = &[
        // 0: done.
        (&["--version"], 0),
        (&[], 0),
        (&["help"], 0),
        (&["host", "list"], 0),
        (&["-o", "json", "job", "list"], 0),
        // 1: the work failed.
        (&["host", "show", "forge"], 1),
        (&["host", "add", "studio", "192.0.2.11", "--no-contact"], 1),
        // 2: the command line was wrong.
        (&["aply", "studio"], 2),
        (&["host", "show"], 2),
        (&["host", "list", "--colour"], 2),
        (&["-o", "yaml", "host", "list"], 2),
        (&["host", "create", "--no-input"], 2),
        // 3: refused.
        (&["host", "apply", "studio", "--no-input"], 3),
        (&["host", "remove", "studio", "--no-input"], 3),
        // 4: the server could not be reached.
        (&["--server", "http://127.0.0.1:9", "host", "list"], 4),
        (
            &["--server", "unix:/nonexistent/serve.sock", "job", "list"],
            4,
        ),
    ];
    for (args, expected) in table {
        let (status, _, said) = scratch.run(args);
        assert_eq!(status, *expected, "rookery {}: {said}", args.join(" "));
    }
    // With -o json, a failure is an object on standard error with the same status.
    let (status, out, said) = scratch.run(&["-o", "json", "aply"]);
    assert_eq!((status, out.as_str()), (2, ""));
    let error: Value = serde_json::from_str(said.trim()).unwrap();
    assert_eq!(error["error"]["exit"], 2);
    let (status, _, said) = scratch.run(&["-o", "json", "host", "apply", "studio", "--no-input"]);
    assert_eq!(status, 3);
    let error: Value = serde_json::from_str(said.trim()).unwrap();
    assert_eq!(error["error"]["exit"], 3, "{error}");
    // A confirmation answered with -y goes on.
    let (status, _, said) = scratch.run(&["host", "remove", "studio", "-y"]);
    assert_eq!(status, 0, "{said}");
}

#[test]
fn json_is_the_documents_of_the_api() {
    let Some(scratch) = Scratch::new() else {
        eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
        return;
    };
    let (status, _, said) = scratch.run(&[
        "host",
        "add",
        "studio",
        "192.0.2.10",
        "--type",
        "ssh",
        "--no-contact",
    ]);
    assert_eq!(status, 0, "{said}");
    let (status, _, said) = scratch.run(&["key", "create", "spare"]);
    assert_eq!(status, 0, "{said}");
    let json = |args: &[&str]| -> Value {
        let mut all = vec!["-o", "json"];
        all.extend(args);
        let (status, out, said) = scratch.run(&all);
        assert_eq!(status, 0, "rookery {}: {said}", all.join(" "));
        serde_json::from_str(&out)
            .unwrap_or_else(|error| panic!("rookery {}: {error}: {out}", all.join(" ")))
    };
    // What does not change between two reads is the same document.
    let same: &[(&[&str], &str, Value, &str)] = &[
        (&["key", "list"], "keys.list", json!({}), ""),
        (
            &["secret", "list"],
            "installation.secrets.list",
            json!({}),
            "",
        ),
        (&["registry", "list"], "registries.list", json!({}), ""),
        (&["provisioner", "list"], "provisioners.list", json!({}), ""),
        (
            &["provisioner", "types"],
            "provisioners.types",
            json!({}),
            "",
        ),
        (&["resource", "list"], "resources.list", json!({}), ""),
        (&["plugin", "list"], "plugins.installed", json!({}), ""),
        (&["job", "list"], "jobs.list", json!({"limit": 20}), "jobs"),
    ];
    for (args, method, params, under) in same {
        let served = scratch.call(method, params.clone());
        let served = match *under {
            "" => served,
            key => served[key].clone(),
        };
        assert_eq!(json(args), served, "rookery {}", args.join(" "));
    }
    // A host's live state may move between two reads; what it is does not.
    let keys = |value: &Value| -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    let listed = json(&["host", "list"]);
    let served = scratch.call("hosts.list", json!({}))["hosts"].clone();
    assert_eq!(
        listed.as_array().unwrap().len(),
        served.as_array().unwrap().len()
    );
    assert_eq!(keys(&listed[0]), keys(&served[0]));
    assert_eq!(listed[0]["name"], "studio");
    let shown = json(&["host", "show", "studio"]);
    let got = scratch.call("hosts.get", json!({"host": "studio"}));
    assert_eq!(keys(&shown), keys(&got));
    assert_eq!(
        (&shown["name"], &shown["type"]),
        (&got["name"], &got["type"])
    );
}
