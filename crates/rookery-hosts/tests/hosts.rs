//! The methods of hosts on a server of their own, against a rookeryd in single-user mode that a
//! saved host's profile reaches with a command of its own.

use std::path::PathBuf;
use std::time::Duration;

use rookery_kernel::methods::Caller;
use rookery_kernel::testing::Inbox;
use rookery_kernel::{Call, Config, Kernel, Methods, RpcError};
use rookery_proto::ErrorCode;
use rookery_store::testing::TestDb;
use serde_json::{Value, json};

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
    /// A rookeryd in single-user mode with these actions, or nothing when it is not built.
    async fn start(actions: Value) -> Option<Rookeryd> {
        let program = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/rookeryd");
        if !program.is_file() {
            eprintln!("skipped: {} is not built", program.display());
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        std::fs::write(dir.path().join("etc/actions.json"), actions.to_string()).unwrap();
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

    /// The profile of a managed host reached by `rookeryd connect`.
    fn profile(&self) -> Value {
        json!({
            "type": "managed",
            "access": {"link": [self.program.to_string_lossy(), "connect", "--dir", self.dir.path().to_string_lossy()]},
        })
    }
}

struct Server {
    kernel: Kernel,
    db: TestDb,
    _dir: tempfile::TempDir,
}

impl Server {
    async fn start() -> Option<Server> {
        let db = TestDb::new().await?;
        let dir = tempfile::tempdir().unwrap();
        let paths = rookery_core::paths::Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let checkout = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // The checkout is the provisioning source, so that a test does not unpack the one built in.
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut methods = Methods::default();
        rookery_kernel::streams::register(&mut methods);
        rookery_facilities::register(&mut methods);
        let hosts = rookery_hosts::Registration::new();
        let options = rookery_jobs::Options {
            backoff: Duration::from_millis(50),
            ..Default::default()
        };
        rookery_jobs::register(&mut methods, hosts.handlers(), options);
        hosts.register(&mut methods);
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
        kernel.methods.started(&kernel);
        let serve = rookery_hosts::hosts(&kernel).ready().await.unwrap();
        let source = checkout.canonicalize().unwrap();
        tokio::task::spawn_blocking(move || {
            serve.paths.set("source", source.to_str().unwrap()).unwrap()
        })
        .await
        .unwrap();
        Some(Server {
            kernel,
            db,
            _dir: dir,
        })
    }

    async fn person(&self, name: &str, admin: bool) -> Caller {
        let login = rookery_store::users::Login {
            issuer: "https://id.example.test".into(),
            subject: name.into(),
            name: name.into(),
            email: Some(format!("{}@example.test", name.to_lowercase())),
            admin,
        };
        let (user, _) = self.db.store.users().upsert_login(&login).await.unwrap();
        Caller::User {
            user: rookery_kernel::auth::wire_user(&user),
            session: None,
            token: None,
        }
    }

    async fn call(&self, caller: &Caller, method: &str, params: Value) -> Result<Value, RpcError> {
        let call = Call::new(self.kernel.clone(), caller.clone(), None);
        self.kernel.methods.dispatch(call, method, params).await
    }

    fn connect(&self, caller: &Caller) -> Inbox {
        let (conn, rx) = rookery_kernel::conn::ConnHandle::detached(&self.kernel, caller.clone());
        Inbox { conn, rx }
    }

    /// The job once it has ended, and its log.
    async fn ended(&self, caller: &Caller, id: &str) -> (Value, Vec<String>) {
        for _ in 0..400 {
            let job = self
                .call(caller, "jobs.get", json!({"id": id}))
                .await
                .unwrap();
            if matches!(
                job["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) {
                let log = self
                    .call(caller, "jobs.output", json!({"id": id, "after": 0}))
                    .await
                    .unwrap();
                let lines = log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["text"].as_str().unwrap().to_owned())
                    .collect();
                return (job, lines);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the job {id} did not end");
    }

    async fn close(self) {
        rookery_core::store::set_shared(None);
        rookery_core::runner::set_locks(None);
        self.kernel.shutdown.cancel();
        self.db.close().await;
    }
}

fn code(result: Result<Value, RpcError>) -> Option<ErrorCode> {
    result.err().map(|e| e.error_code())
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_are_saved_shared_acted_on_and_renamed() {
    let actions = json!([
        {"name": "greet", "label": "Greet", "run": "echo hello from the host"},
        {"name": "nap", "label": "Nap", "run": "sleep 30"},
    ]);
    let Some(host) = Rookeryd::start(actions).await else {
        return;
    };
    let Some(server) = Server::start().await else {
        eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
        return;
    };
    let admin = server.person("Ada", true).await;
    let member = server.person("Ben", false).await;

    // Saving a new host needs the role admin.
    let save = json!({"host": "studio", "profile": host.profile()});
    assert_eq!(
        code(server.call(&member, "hosts.save", save.clone()).await),
        Some(ErrorCode::Forbidden)
    );
    // A link that is a command is the server's own machine's to give.
    assert_eq!(
        code(server.call(&admin, "hosts.save", save.clone()).await),
        Some(ErrorCode::Invalid)
    );
    let saved = server
        .call(&Caller::Local, "hosts.save", save)
        .await
        .unwrap();
    assert_eq!(saved["host"]["name"], "studio");
    assert_eq!(saved["host"]["type"], "managed");
    let got = server
        .call(&admin, "hosts.get", json!({"host": "env://studio"}))
        .await
        .unwrap();
    assert!(got["reachable"].as_bool().unwrap(), "{got}");
    let profile = server
        .call(&admin, "hosts.profile", json!({"host": "studio"}))
        .await
        .unwrap();
    assert_eq!(profile["profile"]["type"], "managed");

    let broker_path = host.dir.path().join(rookery_api::PARROT_SOCKET);
    let broker = tokio::net::UnixListener::bind(&broker_path).unwrap();
    std::fs::set_permissions(
        &broker_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let broker_task = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let (stream, _) = broker.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = tokio::io::BufReader::new(read).lines();
        let request: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "initialize");
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
            .await
            .unwrap();
    });

    // A managed host cannot be applied: the refusal names what would let it.
    let checked = server
        .call(
            &admin,
            "hosts.check",
            json!({"host": "studio", "capability": "apply"}),
        )
        .await
        .unwrap();
    assert_eq!(checked["allowed"], false);
    assert!(
        checked["refusal"]["why"]
            .as_str()
            .unwrap()
            .contains("studio")
    );
    let refused = server
        .call(
            &admin,
            "hosts.desktop.screenshot",
            json!({"host": "studio"}),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), ErrorCode::Unavailable);
    assert!(
        refused.details().unwrap()["refusal"]["raise"].is_string(),
        "{refused:?}"
    );

    let denied = Call::new(server.kernel.clone(), member.clone(), None);
    assert_eq!(
        code(
            server
                .kernel
                .methods
                .dispatch(denied, "agents.connect", json!({"env": "env://studio"}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );

    // Someone without a grant sees nothing of it; with view they see it and its state.
    assert!(
        server.call(&member, "hosts.list", json!({})).await.unwrap()["hosts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        code(
            server
                .call(&member, "hosts.get", json!({"host": "studio"}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    let ben = match &member {
        Caller::User { user, .. } => format!("user:{}", user.id),
        _ => unreachable!(),
    };
    server
        .call(
            &admin,
            "access.grant",
            json!({"resource": "env://studio", "principal": ben, "level": "view"}),
        )
        .await
        .unwrap();
    assert_eq!(
        server.call(&member, "hosts.list", json!({})).await.unwrap()["hosts"][0]["name"],
        "studio"
    );
    let mut agents_inbox = server.connect(&member);
    let params = json!({"env": "env://studio"});
    let call_agents = |inbox: &Inbox| {
        let call = Call::new(
            server.kernel.clone(),
            member.clone(),
            Some(inbox.conn.clone()),
        );
        server
            .kernel
            .methods
            .dispatch(call, "agents.connect", params.clone())
    };
    assert_eq!(
        code(call_agents(&agents_inbox).await),
        Some(ErrorCode::Forbidden)
    );
    server
        .call(
            &admin,
            "access.grant",
            json!({"resource": "env://studio", "principal": ben, "level": "use"}),
        )
        .await
        .unwrap();
    let opened = call_agents(&agents_inbox).await.unwrap();
    let stream = opened["stream"].as_u64().unwrap() as u32;
    let connect = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n";
    agents_inbox
        .conn
        .streams()
        .deliver(&rookery_proto::stream::encode_frame(stream, connect));
    let frames = tokio::time::timeout(Duration::from_secs(5), agents_inbox.lines(stream))
        .await
        .unwrap();
    assert_eq!(
        frames,
        vec![json!({"jsonrpc": "2.0", "id": 1, "result": {}})]
    );
    broker_task.await.unwrap();
    server
        .call(
            &admin,
            "access.grant",
            json!({"resource": "env://studio", "principal": ben, "level": "view"}),
        )
        .await
        .unwrap();

    assert!(
        server
            .call(&member, "hosts.stats", json!({"host": "studio"}))
            .await
            .is_ok()
    );
    assert_eq!(
        code(
            server
                .call(&member, "hosts.remove", json!({"host": "studio"}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );

    // What names files of the server's machine is for its socket alone, an admin elsewhere refused.
    for (method, params) in [
        ("hosts.inventory", json!({"hosts": ["studio"]})),
        ("hosts.ssh", json!({"host": "studio"})),
        ("hosts.tailnetKey", json!({"host": "studio"})),
    ] {
        assert_eq!(
            code(server.call(&admin, method, params).await),
            Some(ErrorCode::Forbidden),
            "{method}"
        );
    }
    assert!(
        server
            .call(&Caller::Local, "hosts.inventory", json!({}))
            .await
            .is_ok()
    );

    // The profile is manage's to read. Who manages a host changes how it is reached and set
    // up, but not which of the installation's secrets, registries and features it gets.
    assert_eq!(
        code(
            server
                .call(&member, "hosts.profile", json!({"host": "studio"}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    let manager = server.person("Cy", false).await;
    let cy = match &manager {
        Caller::User { user, .. } => format!("user:{}", user.id),
        _ => unreachable!(),
    };
    server
        .call(
            &admin,
            "access.grant",
            json!({"resource": "env://studio", "principal": cy, "level": "manage"}),
        )
        .await
        .unwrap();
    let kept = server
        .call(&manager, "hosts.profile", json!({"host": "studio"}))
        .await
        .unwrap()["profile"]
        .clone();
    for (key, value) in [
        (
            "secrets",
            json!([{"name": "deploy-token", "env": "DEPLOY_TOKEN"}]),
        ),
        ("registries", json!(["images"])),
    ] {
        let mut changed = kept.clone();
        changed[key] = value;
        let refused = server
            .call(
                &manager,
                "hosts.save",
                json!({"host": "studio", "profile": changed}),
            )
            .await
            .unwrap_err();
        assert_eq!(refused.error_code(), ErrorCode::Forbidden, "{key}");
    }
    let mut featured = kept.clone();
    featured["ansible_vars"] = json!({"rookery_plugin_features": ["example/tool"]});
    assert_eq!(
        code(
            server
                .call(
                    &manager,
                    "hosts.save",
                    json!({"host": "studio", "profile": featured})
                )
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    let mut renamed = kept.clone();
    renamed["description"] = json!("the studio, described by its manager");
    server
        .call(
            &manager,
            "hosts.save",
            json!({"host": "studio", "profile": renamed}),
        )
        .await
        .unwrap();

    // A host's own action runs as a session there, which a job follows.
    let listed = server
        .call(&admin, "hosts.actions", json!({"host": "studio"}))
        .await
        .unwrap();
    assert!(
        listed["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == "greet"),
        "{listed}"
    );
    let acted = server
        .call(
            &admin,
            "hosts.act",
            json!({"host": "studio", "action": "greet", "token": "t-1"}),
        )
        .await
        .unwrap();
    let job = acted["job"].as_str().unwrap().to_owned();
    // The same token again finds the same job.
    let again = server
        .call(
            &admin,
            "hosts.act",
            json!({"host": "studio", "action": "greet", "token": "t-1"}),
        )
        .await
        .unwrap();
    assert_eq!(again["job"], job.as_str());
    let (ended, lines) = server.ended(&admin, &job).await;
    assert_eq!(ended["state"], "succeeded", "{ended} {lines:?}");
    assert_eq!(ended["kind"], "hosts.session");
    assert_eq!(ended["subject"], "env://studio");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("hello from the host")),
        "{lines:?}"
    );
    // A session an action runs is the host's managers' to read and drive: use reaches the
    // agent's own sessions alone, whichever id it names.
    let session = ended["params"]["session"].as_str().unwrap().to_owned();
    let user = server.person("Di", false).await;
    let di = match &user {
        Caller::User { user, .. } => format!("user:{}", user.id),
        _ => unreachable!(),
    };
    server
        .call(
            &admin,
            "access.grant",
            json!({"resource": "env://studio", "principal": di, "level": "use"}),
        )
        .await
        .unwrap();
    for method in [
        "hosts.sessions.output",
        "hosts.sessions.screen",
        "hosts.sessions.close",
    ] {
        assert_eq!(
            code(
                server
                    .call(
                        &user,
                        method,
                        json!({"host": "studio", "terminal": session})
                    )
                    .await
            ),
            Some(ErrorCode::Forbidden),
            "{method}"
        );
    }
    assert_eq!(
        code(
            server
                .call(
                    &user,
                    "hosts.sessions.input",
                    json!({"host": "studio", "terminal": session, "body": {"data_b64": "eA=="}}),
                )
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    assert!(
        server
            .call(
                &admin,
                "hosts.sessions.output",
                json!({"host": "studio", "terminal": session})
            )
            .await
            .is_ok()
    );

    // Its viewer sees the job and its log on a stream, and cannot cancel it.
    let inbox = server.connect(&member);
    let followed = server_call_on(
        &server,
        &inbox,
        "jobs.follow",
        json!({"id": job, "after": 0}),
    )
    .await;
    let stream = followed["stream"].as_u64().unwrap() as u32;
    let mut inbox = inbox;
    let streamed = inbox.lines(stream).await;
    assert_eq!(streamed.last().unwrap()["state"], "succeeded");
    assert_eq!(
        code(
            server
                .call(&member, "jobs.cancel", json!({"id": job}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );

    // Cancelling a job ends its session on the host.
    let nap = server
        .call(
            &admin,
            "hosts.act",
            json!({"host": "studio", "action": "nap"}),
        )
        .await
        .unwrap();
    let nap = nap["job"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(500)).await;
    server
        .call(&admin, "jobs.cancel", json!({"id": nap}))
        .await
        .unwrap();
    let (ended, _) = server.ended(&admin, &nap).await;
    assert_eq!(ended["state"], "cancelled");

    // A rename is a job of Rookery's; the grants and the jobs go with the name.
    let renamed = server
        .call(
            &admin,
            "hosts.rename",
            json!({"host": "studio", "to": "forge"}),
        )
        .await
        .unwrap();
    let (ended, lines) = server.ended(&admin, renamed["job"].as_str().unwrap()).await;
    assert_eq!(ended["state"], "succeeded", "{lines:?}");
    assert_eq!(
        code(
            server
                .call(&admin, "hosts.get", json!({"host": "studio"}))
                .await
        ),
        Some(ErrorCode::NotFound)
    );
    assert_eq!(
        server
            .call(&member, "hosts.get", json!({"host": "forge"}))
            .await
            .unwrap()["name"],
        "forge"
    );
    let jobs = server
        .call(&member, "jobs.list", json!({"subject": "env://forge"}))
        .await
        .unwrap();
    assert!(jobs["jobs"].as_array().unwrap().len() >= 3, "{jobs}");

    // The key ring and removing the host.
    assert!(server.call(&admin, "keys.list", json!({})).await.is_ok());
    assert_eq!(
        code(server.call(&member, "keys.list", json!({})).await),
        Some(ErrorCode::Forbidden)
    );
    server
        .call(&admin, "hosts.remove", json!({"host": "forge"}))
        .await
        .unwrap();
    assert!(
        server.call(&admin, "hosts.list", json!({})).await.unwrap()["hosts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let grants = server
        .call(&admin, "access.grants", json!({"resource": "env://forge"}))
        .await
        .unwrap();
    assert!(grants["grants"].as_array().unwrap().is_empty(), "{grants}");
    server.close().await;
}

async fn server_call_on(server: &Server, inbox: &Inbox, method: &str, params: Value) -> Value {
    let call = Call::new(
        server.kernel.clone(),
        inbox.conn.caller().clone(),
        Some(inbox.conn.clone()),
    );
    server
        .kernel
        .methods
        .dispatch(call, method, params)
        .await
        .unwrap()
}
