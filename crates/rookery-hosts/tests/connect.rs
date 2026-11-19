//! Terminals, files, ports and the port proxy of a host, with grants on it: against a rookeryd in
//! single-user mode on this machine and a server of their own.

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rookery_kernel::methods::Caller;
use rookery_kernel::testing::Inbox;
use rookery_kernel::{Call, Config, Kernel, Methods, RpcError};
use rookery_proto::ErrorCode;
use rookery_store::testing::TestDb;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

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
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut methods = Methods::default();
        rookery_kernel::streams::register(&mut methods);
        rookery_facilities::register(&mut methods);
        let hosts = rookery_hosts::Registration::new();
        rookery_jobs::register(&mut methods, hosts.handlers(), Default::default());
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

    async fn person(&self, name: &str, admin: bool) -> (Caller, String) {
        let login = rookery_store::users::Login {
            issuer: "https://id.example.test".into(),
            subject: name.into(),
            name: name.into(),
            email: Some(format!("{}@example.test", name.to_lowercase())),
            admin,
        };
        let (user, _) = self.db.store.users().upsert_login(&login).await.unwrap();
        let id = user.id.to_string();
        (
            Caller::User {
                user: rookery_kernel::auth::wire_user(&user),
                session: None,
                token: None,
            },
            id,
        )
    }

    async fn call(&self, caller: &Caller, method: &str, params: Value) -> Result<Value, RpcError> {
        let call = Call::new(self.kernel.clone(), caller.clone(), None);
        self.kernel.methods.dispatch(call, method, params).await
    }

    async fn call_on(&self, inbox: &Inbox, method: &str, params: Value) -> Result<Value, RpcError> {
        let call = Call::new(
            self.kernel.clone(),
            inbox.conn.caller().clone(),
            Some(inbox.conn.clone()),
        );
        self.kernel.methods.dispatch(call, method, params).await
    }

    fn connect(&self, caller: &Caller) -> Inbox {
        let (conn, rx) = rookery_kernel::conn::ConnHandle::detached(&self.kernel, caller.clone());
        Inbox { conn, rx }
    }

    async fn grant(&self, admin: &Caller, user: &str, level: &str) {
        self.call(
            admin,
            "access.grant",
            json!({"resource": "env://studio", "principal": format!("user:{user}"), "level": level}),
        )
        .await
        .unwrap();
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

/// Frames of a terminal's stream until one of `kind` carries `want`, with the bytes of type 0 seen.
async fn until(inbox: &mut Inbox, stream: u32, kind: u8, want: &str) -> (Vec<Vec<u8>>, u64) {
    let mut seen = vec![];
    let mut output = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let frame = tokio::time::timeout_at(deadline, inbox.frame(stream))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "no frame {kind} with {want:?}; saw {:?}",
                    seen.iter()
                        .map(|f: &Vec<u8>| String::from_utf8_lossy(f).into_owned())
                        .collect::<Vec<_>>()
                )
            });
        if frame[0] == 0 {
            output += frame.len() as u64 - 1;
        }
        let found = frame[0] == kind && String::from_utf8_lossy(&frame[1..]).contains(want);
        seen.push(frame);
        if found {
            return (seen, output);
        }
    }
}

fn send(inbox: &Inbox, stream: u32, kind: u8, body: &[u8]) {
    let mut payload = vec![kind];
    payload.extend_from_slice(body);
    inbox
        .conn
        .streams()
        .deliver(&rookery_proto::stream::encode_frame(stream, &payload));
}

#[tokio::test(flavor = "multi_thread")]
async fn terminals_files_and_ports_follow_the_levels_on_a_host() {
    let Some(host) = Rookeryd::start().await else {
        return;
    };
    let Some(server) = Server::start().await else {
        return;
    };
    let (admin, _) = server.person("Ada", true).await;
    let (ann, ann_id) = server.person("Ann", false).await;
    let (bob, bob_id) = server.person("Bob", false).await;
    let (vic, vic_id) = server.person("Vic", false).await;
    let (mia, mia_id) = server.person("Mia", false).await;
    let (eve, _) = server.person("Eve", false).await;
    server
        .call(
            &Caller::Local,
            "hosts.save",
            json!({"host": "studio", "profile": host.profile()}),
        )
        .await
        .unwrap();
    server.grant(&admin, &ann_id, "use").await;
    server.grant(&admin, &bob_id, "use").await;
    server.grant(&admin, &vic_id, "view").await;
    server.grant(&admin, &mia_id, "manage").await;

    // The hosts as environments, with the caller's level, for those who may see them.
    let listed = server.call(&vic, "env.list", json!({})).await.unwrap();
    let studio = &listed["environments"][0];
    assert_eq!(studio["uri"], "env://studio", "{listed}");
    assert_eq!(studio["level"], "view");
    assert_eq!(studio["kind"], "managed");
    assert_eq!(studio["supports"]["terminal"], true);
    assert_eq!(studio["supports"]["desktop"], false);
    assert!(
        server.call(&eve, "env.list", json!({})).await.unwrap()["environments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        code(
            server
                .call(&eve, "env.get", json!({"env": "env://studio"}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    assert_eq!(
        server
            .call(&ann, "env.get", json!({"env": "studio"}))
            .await
            .unwrap()["level"],
        "use"
    );

    // A terminal is opened with use, and is its owner's.
    assert_eq!(
        code(
            server
                .call(&vic, "terminals.open", json!({"env": "env://studio"}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    assert_eq!(
        code(
            server
                .call(
                    &ann,
                    "terminals.open",
                    json!({"env": "env://studio", "user": "root"})
                )
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    let opened = server
        .call(
            &ann,
            "terminals.open",
            json!({"env": "env://studio", "command": ["sh", "-c", "echo ready; exec cat"], "size": {"cols": 100, "rows": 30}}),
        )
        .await
        .unwrap();
    let id = opened["id"].as_str().unwrap().to_owned();
    let session = opened["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(opened["owner"], ann_id.as_str());
    assert_eq!(opened["level"], "use");
    assert_eq!(opened["env"], "env://studio");
    assert_eq!(opened["size"], json!({"cols": 100, "rows": 30}));

    // Nobody else with use, and nobody with view, reaches it; a manager does.
    for (who, method, params) in [
        (&bob, "terminals.get", json!({"id": id})),
        (&bob, "terminals.write", json!({"id": id, "text": "x"})),
        (&bob, "terminals.close", json!({"id": id})),
        (&bob, "terminals.screen", json!({"id": id})),
        (&vic, "terminals.get", json!({"id": id})),
        (
            &vic,
            "terminals.attach",
            json!({"id": id, "readonly": true}),
        ),
        (&eve, "terminals.rename", json!({"id": id, "title": "mine"})),
        (
            &bob,
            "hosts.sessions.input",
            json!({"host": "studio", "terminal": session, "body": {"text": "x"}}),
        ),
        (
            &bob,
            "hosts.sessions.screen",
            json!({"host": "studio", "terminal": session}),
        ),
        (
            &bob,
            "hosts.sessions.attach",
            json!({"host": "studio", "terminal": session}),
        ),
        (
            &vic,
            "terminals.adopt",
            json!({"env": "studio", "session": session}),
        ),
        (
            &bob,
            "terminals.adopt",
            json!({"env": "studio", "session": session}),
        ),
    ] {
        assert_eq!(
            code(server.call(who, method, params).await),
            Some(ErrorCode::Forbidden),
            "{method}"
        );
    }
    assert_eq!(
        code(
            server
                .call(&ann, "terminals.get", json!({"id": "term_none"}))
                .await
        ),
        Some(ErrorCode::NotFound)
    );
    assert_eq!(
        server
            .call(&mia, "terminals.get", json!({"id": id}))
            .await
            .unwrap()["level"],
        "manage"
    );
    assert_eq!(
        server
            .call(&ann, "terminals.list", json!({}))
            .await
            .unwrap()["terminals"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        server
            .call(&bob, "terminals.list", json!({}))
            .await
            .unwrap()["terminals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        server
            .call(&mia, "terminals.list", json!({}))
            .await
            .unwrap()["terminals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        server
            .call(&mia, "terminals.list", json!({"all": true}))
            .await
            .unwrap()["terminals"][0]["id"],
        id.as_str()
    );
    // Someone with view learns who works there, and nothing of what they do.
    let presence = server
        .call(&vic, "env.presence", json!({"env": "studio"}))
        .await
        .unwrap();
    assert_eq!(
        presence["users"],
        json!([{"id": ann_id, "name": "Ann", "terminals": 1, "since": presence["users"][0]["since"]}])
    );
    let sessions = server
        .call(
            &vic,
            "terminals.sessions",
            json!({"env": "studio", "preview": true}),
        )
        .await
        .unwrap();
    let listed = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == session.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["adopted"], true);
    assert!(
        listed.get("terminal").is_none() && listed.get("preview").is_none(),
        "{listed}"
    );
    let sessions = server
        .call(&ann, "terminals.sessions", json!({"env": "studio"}))
        .await
        .unwrap();
    assert!(
        sessions["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["terminal"] == id.as_str())
    );
    // A session that is a terminal already is not adopted again.
    assert_eq!(
        code(
            server
                .call(
                    &mia,
                    "terminals.adopt",
                    json!({"env": "studio", "session": session})
                )
                .await
        ),
        Some(ErrorCode::Conflict)
    );

    // Attached, it starts with the screen, then takes input and gives output.
    let mut inbox = server.connect(&ann);
    let attached = server
        .call_on(
            &inbox,
            "terminals.attach",
            json!({"id": id, "size": {"cols": 90, "rows": 20}}),
        )
        .await
        .unwrap();
    assert_eq!(attached["mode"], "snapshot", "{attached}");
    assert_eq!(attached["readonly"], false);
    let stream = attached["stream"].as_u64().unwrap() as u32;
    let snapshot = attached["snapshot"].as_u64().unwrap();
    assert!(snapshot > 0);
    let (frames, _) = until(&mut inbox, stream, 1, "ready").await;
    let shown: Vec<u8> = frames
        .iter()
        .filter(|f| f[0] == 2)
        .flat_map(|f| f[1..].to_vec())
        .collect();
    assert_eq!(shown.len() as u64, snapshot);
    assert!(String::from_utf8_lossy(&shown).contains("ready"));
    // The viewer's size is the terminal's.
    until(&mut inbox, stream, 1, "\"cols\":90").await;
    send(&inbox, stream, 0, b"hello there\n");
    let (_, output) = until(&mut inbox, stream, 0, "hello there").await;
    let offset = attached["offset"].as_u64().unwrap() + output;

    // A viewer that only watches cannot type.
    let mut watcher = server.connect(&ann);
    let watching = server
        .call_on(
            &watcher,
            "terminals.attach",
            json!({"id": id, "readonly": true}),
        )
        .await
        .unwrap();
    assert_eq!(watching["readonly"], true);
    let watched = watching["stream"].as_u64().unwrap() as u32;
    until(&mut watcher, watched, 1, "ready").await;
    send(&watcher, watched, 0, b"from the watcher\n");
    send(&inbox, stream, 0, b"from the owner\n");
    let (seen, _) = until(&mut watcher, watched, 0, "from the owner").await;
    assert!(
        !seen
            .iter()
            .any(|f| String::from_utf8_lossy(f).contains("from the watcher"))
    );

    // Attached again after an offset, it goes on from there.
    let mut again = server.connect(&ann);
    let resumed = server
        .call_on(
            &again,
            "terminals.attach",
            json!({"id": id, "after": offset}),
        )
        .await
        .unwrap();
    assert_eq!(resumed["mode"], "resume", "{resumed}");
    assert_eq!(resumed["offset"].as_u64().unwrap(), offset);
    assert_eq!(resumed["snapshot"], 0);
    let again_stream = resumed["stream"].as_u64().unwrap() as u32;
    until(&mut again, again_stream, 0, "from the owner").await;
    assert!(
        server
            .call(&ann, "terminals.get", json!({"id": id}))
            .await
            .unwrap()["viewers"]
            .as_u64()
            .unwrap()
            >= 2
    );

    // Automation, the screen, a title.
    server
        .call(
            &ann,
            "terminals.write",
            json!({"id": id, "text": "typed by a script\n"}),
        )
        .await
        .unwrap();
    until(&mut inbox, stream, 0, "typed by a script").await;
    let screen = server
        .call(&ann, "terminals.screen", json!({"id": id}))
        .await
        .unwrap();
    assert!(
        screen["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("typed by a script")),
        "{screen}"
    );
    server
        .call(&ann, "terminals.rename", json!({"id": id, "title": "cat"}))
        .await
        .unwrap();
    until(&mut inbox, stream, 1, "\"title\":\"cat\"").await;
    let info = server
        .call(&ann, "terminals.get", json!({"id": id}))
        .await
        .unwrap();
    assert_eq!(info["title"], "cat");
    assert_eq!(info["lastInput"]["principal"], format!("user:{ann_id}"));

    // A manager drives another account's terminal.
    server
        .call(
            &mia,
            "terminals.write",
            json!({"id": id, "text": "the manager\n"}),
        )
        .await
        .unwrap();
    until(&mut inbox, stream, 0, "the manager").await;

    // Files, inside the host's roots and at use.
    let path = host.file("notes/a.txt");
    assert_eq!(
        code(
            server
                .call(&vic, "env.readFile", json!({"env": "studio", "path": path}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    assert_eq!(
        code(
            server
                .call(
                    &vic,
                    "env.writeFile",
                    json!({"env": "studio", "path": path, "base64": "eA=="})
                )
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    let written = server
        .call(
            &ann,
            "env.writeFile",
            json!({"env": "studio", "path": path, "base64": rookery_api::b64(b"one")}),
        )
        .await
        .unwrap();
    let read = server
        .call(
            &ann,
            "env.readFile",
            json!({"env": "env://studio", "path": path}),
        )
        .await
        .unwrap();
    assert_eq!(read["base64"], rookery_api::b64(b"one"));
    assert_eq!(read["sha256"], written["sha256"]);
    assert_eq!(read["size"], 3);
    // One that is there is not replaced by accident, nor when it changed since it was read.
    assert_eq!(
        code(
            server
                .call(
                    &ann,
                    "env.writeFile",
                    json!({"env": "studio", "path": path, "base64": "eA=="})
                )
                .await
        ),
        Some(ErrorCode::Conflict)
    );
    assert_eq!(
        code(server.call(&ann, "env.writeFile", json!({"env": "studio", "path": path, "base64": "eA==", "ifMatch": "0".repeat(64)})).await),
        Some(ErrorCode::Conflict)
    );
    server
        .call(&ann, "env.writeFile", json!({"env": "studio", "path": path, "base64": rookery_api::b64(b"two"), "ifMatch": read["sha256"]}))
        .await
        .unwrap();
    let listing = server
        .call(
            &ann,
            "env.listDir",
            json!({"env": "studio", "path": host.file("notes")}),
        )
        .await
        .unwrap();
    assert_eq!(listing["entries"][0]["name"], "a.txt");
    let stat = server
        .call(&ann, "env.stat", json!({"env": "studio", "path": path}))
        .await
        .unwrap();
    assert_eq!(stat["entry"]["size"], 3);
    assert_eq!(
        code(
            server
                .call(
                    &ann,
                    "env.readFile",
                    json!({"env": "studio", "path": "/etc/passwd"})
                )
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    server
        .call(
            &ann,
            "env.deleteFile",
            json!({"env": "studio", "path": path}),
        )
        .await
        .unwrap();
    assert_eq!(
        code(
            server
                .call(&ann, "env.readFile", json!({"env": "studio", "path": path}))
                .await
        ),
        Some(ErrorCode::NotFound)
    );

    // A port on the host's loopback, as a stream and in a browser.
    let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = app.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = app.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut asked = Vec::new();
                let mut buf = [0u8; 4096];
                while !asked.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => asked.extend_from_slice(&buf[..n]),
                    }
                }
                let asked = String::from_utf8_lossy(&asked).into_owned();
                let body = asked.replace("\r\n", "\n");
                let answer = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nset-cookie: app=1; Domain=localhost; Path=/\r\nset-cookie: rookery_session=forged\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(answer.as_bytes()).await;
            });
        }
    });
    assert_eq!(
        code(
            server
                .call_on(
                    &server.connect(&vic),
                    "env.connect",
                    json!({"env": "studio", "port": port})
                )
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    let mut piped = server.connect(&ann);
    let connected = server
        .call_on(
            &piped,
            "env.connect",
            json!({"env": "studio", "port": port}),
        )
        .await
        .unwrap();
    let pipe = connected["stream"].as_u64().unwrap() as u32;
    piped
        .conn
        .streams()
        .deliver(&rookery_proto::stream::encode_frame(
            pipe,
            b"GET /through HTTP/1.1\r\nhost: x\r\n\r\n",
        ));
    let answer = piped.frame(pipe).await;
    assert!(String::from_utf8_lossy(&answer).contains("GET /through"));

    let routes = rookery_hosts::proxy::routes().with_state(server.kernel.clone());
    let ports = rookery_hosts::proxy::port_routes().with_state(server.kernel.clone());
    let token = |user: &str| {
        let store = server.db.store.clone();
        let user = uuid::Uuid::parse_str(user).unwrap();
        async move {
            store
                .auth()
                .create_token(user, "test", None)
                .await
                .unwrap()
                .0
        }
    };
    let vic_token = token(&vic_id).await;
    let refused = routes
        .clone()
        .oneshot(
            Request::get(format!("/env/studio/port/{port}/x"))
                .header(header::AUTHORIZATION, format!("Bearer {vic_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let unsigned = routes
        .clone()
        .oneshot(
            Request::get(format!("/env/studio/port/{port}/x"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
    let ann_token = token(&ann_id).await;
    let sent = routes
        .clone()
        .oneshot(
            Request::get(format!("/env/studio/port/{port}/page?q=1"))
                .header(header::AUTHORIZATION, format!("Bearer {ann_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(sent.status().is_redirection(), "{}", sent.status());
    let to = sent.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let (origin, claim) = to.split_once("/.rookery/").unwrap();
    let port_host = origin.strip_prefix("http://").unwrap().to_owned();
    assert!(
        port_host.starts_with(&format!("{port}-")) && port_host.ends_with(".localhost:7777"),
        "{to}"
    );
    // Without the port's cookie, its origin refuses; the ticket works once.
    let bare = ports
        .clone()
        .oneshot(
            Request::get("/page")
                .header(header::HOST, &port_host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bare.status(), StatusCode::UNAUTHORIZED);
    let claimed = ports
        .clone()
        .oneshot(
            Request::get(format!("/.rookery/{claim}"))
                .header(header::HOST, &port_host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(claimed.status().is_redirection());
    assert_eq!(claimed.headers()[header::LOCATION], "/page?q=1");
    let cookie = claimed.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert!(cookie.starts_with("rookery_port="));
    let twice = ports
        .clone()
        .oneshot(
            Request::get(format!("/.rookery/{claim}"))
                .header(header::HOST, &port_host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(twice.status(), StatusCode::FORBIDDEN);
    // The cookie is for this port alone.
    let other = ports
        .clone()
        .oneshot(
            Request::get("/page")
                .header(
                    header::HOST,
                    port_host.replacen(&format!("{port}-"), &format!("{}-", port + 1), 1),
                )
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::UNAUTHORIZED);
    let page = ports
        .clone()
        .oneshot(
            Request::get("/page?q=1")
                .header(header::HOST, &port_host)
                .header(
                    header::COOKIE,
                    format!("{cookie}; rookery_session=rookery_ses_secret; theme=dark"),
                )
                .header(header::AUTHORIZATION, format!("Bearer {ann_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let set: Vec<String> = page
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    assert_eq!(set, vec!["app=1; Path=/".to_owned()]);
    let asked = String::from_utf8(
        axum::body::to_bytes(page.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(asked.starts_with("GET /page?q=1 HTTP/1.1"), "{asked}");
    assert!(asked.contains("theme=dark"), "{asked}");
    assert!(
        !asked.contains("rookery_session")
            && !asked.contains("rookery_port")
            && !asked.contains(&ann_token),
        "{asked}"
    );
    assert!(
        asked
            .to_ascii_lowercase()
            .contains(&format!("host: localhost:{port}")),
        "{asked}"
    );
    // Each request is checked again: without use on the host, the cookie opens nothing.
    server
        .call(
            &admin,
            "access.revoke",
            json!({"resource": "env://studio", "principal": format!("user:{ann_id}")}),
        )
        .await
        .unwrap();
    let revoked = ports
        .clone()
        .oneshot(
            Request::get("/page")
                .header(header::HOST, &port_host)
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::FORBIDDEN);
    let closed_err = wait_stream_closed(&mut inbox, stream).await;
    assert_eq!(closed_err["code"], "FORBIDDEN", "{closed_err}");
    // And without use, the owner's own terminal is out of reach too.
    assert_eq!(
        code(server.call(&ann, "terminals.get", json!({"id": id})).await),
        Some(ErrorCode::Forbidden)
    );
    server.grant(&admin, &ann_id, "use").await;
    let mut inbox = server.connect(&ann);
    let reattached = server
        .call_on(&inbox, "terminals.attach", json!({"id": id}))
        .await
        .unwrap();
    let stream = reattached["stream"].as_u64().unwrap() as u32;
    until(&mut inbox, stream, 1, "ready").await;

    // Closing it ends the session and forgets it; its viewers hear.
    server
        .call(&ann, "terminals.close", json!({"id": id}))
        .await
        .unwrap();
    until(&mut inbox, stream, 1, "\"state\"").await;
    assert_eq!(
        code(server.call(&ann, "terminals.get", json!({"id": id})).await),
        Some(ErrorCode::NotFound)
    );
    assert!(
        server
            .call(&vic, "env.presence", json!({"env": "studio"}))
            .await
            .unwrap()["users"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // A terminal that exits by itself is recorded so.
    let short = server
        .call(
            &bob,
            "terminals.open",
            json!({"env": "studio", "command": ["sh", "-c", "exit 3"]}),
        )
        .await
        .unwrap();
    let short = short["id"].as_str().unwrap().to_owned();
    let mut ended = Value::Null;
    for _ in 0..100 {
        ended = server
            .call(&bob, "terminals.get", json!({"id": short}))
            .await
            .unwrap();
        if ended["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(ended["state"], "exited", "{ended}");
    assert_eq!(ended["exitCode"], 3);
    assert!(
        server
            .call(&bob, "terminals.list", json!({}))
            .await
            .unwrap()["terminals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        server
            .call(&bob, "terminals.list", json!({"ended": true}))
            .await
            .unwrap()["terminals"][0]["id"],
        short.as_str()
    );
    drop((watcher, again, piped));
    server.close().await;
}

/// Waits until `streams.close` is sent for `stream`, returning its error JSON (or Null).
async fn wait_stream_closed(inbox: &mut Inbox, stream: u32) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, inbox.rx.recv()).await {
            Ok(Some(rookery_kernel::conn::Outbound::Text(text))) => {
                if let Ok(rookery_proto::Message::Notification(n)) =
                    rookery_proto::Message::parse(&text)
                    && n.method == "streams.close"
                    && n.params["stream"] == stream
                {
                    return n.params["error"].clone();
                }
            }
            Ok(Some(rookery_kernel::conn::Outbound::Binary(_))) => {}
            Ok(None) => panic!("connection dropped before stream {stream} closed"),
            Err(_) => panic!("timed out waiting for stream {stream} to close"),
        }
    }
}

/// Calls a JSON endpoint directly over a host's Unix control socket (`control.sock`).
async fn control_json(
    sock: &std::path::Path,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let mut stream = tokio::net::UnixStream::connect(sock).await.unwrap();
    let payload = body.map(|v| v.to_string()).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.unwrap();
    let text = String::from_utf8(resp).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(
        head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.1 201"),
        "unexpected HTTP response: {head}\n{body}"
    );
    serde_json::from_str(body).unwrap()
}

/// Performs a raw HTTP/1.1 WebSocket upgrade over TCP and returns `(head, stream)`.
async fn ws_upgrade(
    addr: std::net::SocketAddr,
    host: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> (String, tokio::net::TcpStream) {
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    tcp.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut byte))
            .await
            .expect("WS handshake timeout")
            .unwrap();
        assert!(
            n > 0,
            "EOF before WS headers: {}",
            String::from_utf8_lossy(&buf)
        );
        buf.push(byte[0]);
    }
    (String::from_utf8_lossy(&buf).into_owned(), tcp)
}

fn ws_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() < 126);
    let mut frame = vec![0x80 | opcode, 0x80 | (payload.len() as u8), 0, 0, 0, 0];
    frame.extend_from_slice(payload);
    frame
}

async fn read_ws_frame(tcp: &mut tokio::net::TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 2];
    if tcp.read_exact(&mut hdr).await.is_err() {
        return None;
    }
    let opcode = hdr[0] & 0x0f;
    let masked = (hdr[1] & 0x80) != 0;
    let len_byte = hdr[1] & 0x7f;
    let len = match len_byte {
        126 => {
            let mut ext = [0u8; 2];
            tcp.read_exact(&mut ext).await.ok()?;
            u16::from_be_bytes(ext) as usize
        }
        127 => {
            let mut ext = [0u8; 8];
            tcp.read_exact(&mut ext).await.ok()?;
            u64::from_be_bytes(ext) as usize
        }
        n => n as usize,
    };
    let mask = if masked {
        let mut m = [0u8; 4];
        tcp.read_exact(&mut m).await.ok()?;
        Some(m)
    } else {
        None
    };
    let mut payload = vec![0u8; len];
    tcp.read_exact(&mut payload).await.ok()?;
    if let Some(m) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    Some((opcode, payload))
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_or_disabling_cuts_open_streams() {
    let Some(host) = Rookeryd::start().await else {
        return;
    };
    let Some(server) = Server::start().await else {
        return;
    };
    let (admin, _) = server.person("Ada", true).await;
    let (ann, ann_id) = server.person("Ann", false).await;
    let (bob, bob_id) = server.person("Bob", false).await;

    // Save as workstation so both session streams/ports AND desktop can be tested.
    let mut profile = host.profile();
    profile["type"] = json!("workstation");
    profile["builder"] = json!({});
    server
        .call(
            &Caller::Local,
            "hosts.save",
            json!({"host": "studio", "profile": profile}),
        )
        .await
        .unwrap();
    server.grant(&admin, &ann_id, "use").await;
    server.grant(&admin, &bob_id, "use").await;

    // 1. terminals.attach closes promptly when Ann loses use on env://studio.
    let term = server
        .call(
            &ann,
            "terminals.open",
            json!({"env": "studio", "command": ["sh", "-c", "echo term-up; exec cat"]}),
        )
        .await
        .unwrap();
    let term_id = term["id"].as_str().unwrap().to_owned();
    let mut term_inbox = server.connect(&ann);
    let attached = server
        .call_on(&term_inbox, "terminals.attach", json!({"id": term_id}))
        .await
        .unwrap();
    let term_stream = attached["stream"].as_u64().unwrap() as u32;
    until(&mut term_inbox, term_stream, 1, "ready").await;

    server
        .call(
            &admin,
            "access.revoke",
            json!({"resource": "env://studio", "principal": format!("user:{ann_id}")}),
        )
        .await
        .unwrap();
    let err = wait_stream_closed(&mut term_inbox, term_stream).await;
    assert_eq!(err["code"], "FORBIDDEN", "{err}");

    // 2. hosts.sessions.attach closes promptly when Ann loses use on env://studio.
    server.grant(&admin, &ann_id, "use").await;
    let proc_sess = server
        .call(
            &ann,
            "hosts.sessions.open",
            json!({"host": "studio", "kind": "process", "command": ["cat"]}),
        )
        .await
        .unwrap();
    let proc_id = proc_sess["id"].as_str().unwrap().to_owned();
    let mut sess_inbox = server.connect(&ann);
    let sess_attached = server
        .call_on(
            &sess_inbox,
            "hosts.sessions.attach",
            json!({"host": "studio", "terminal": proc_id}),
        )
        .await
        .unwrap();
    let sess_stream = sess_attached["stream"].as_u64().unwrap() as u32;
    sess_inbox
        .conn
        .streams()
        .deliver(&rookery_proto::stream::encode_frame(
            sess_stream,
            b"b\x00ping-proc\n",
        ));
    let got = tokio::time::timeout(Duration::from_secs(5), sess_inbox.frame(sess_stream))
        .await
        .expect("process attach echo timeout");
    assert!(String::from_utf8_lossy(&got).contains("ping-proc"));

    server
        .call(
            &admin,
            "access.revoke",
            json!({"resource": "env://studio", "principal": format!("user:{ann_id}")}),
        )
        .await
        .unwrap();
    let err = wait_stream_closed(&mut sess_inbox, sess_stream).await;
    assert_eq!(err["code"], "FORBIDDEN", "{err}");

    // 3. env.connect closes promptly when its user is disabled.
    let echo_tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo_tcp.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = echo_tcp.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    let mut bob_inbox = server.connect(&bob);
    let conn_open = server
        .call_on(
            &bob_inbox,
            "env.connect",
            json!({"env": "studio", "port": echo_port}),
        )
        .await
        .unwrap();
    let conn_stream = conn_open["stream"].as_u64().unwrap() as u32;
    bob_inbox
        .conn
        .streams()
        .deliver(&rookery_proto::stream::encode_frame(
            conn_stream,
            b"hello-tcp",
        ));
    let tcp_echo = tokio::time::timeout(Duration::from_secs(5), bob_inbox.frame(conn_stream))
        .await
        .expect("env.connect echo timeout");
    assert_eq!(tcp_echo, b"hello-tcp");

    server
        .call(
            &admin,
            "admin.setDisabled",
            json!({"id": bob_id, "disabled": true}),
        )
        .await
        .unwrap();
    let err = wait_stream_closed(&mut bob_inbox, conn_stream).await;
    assert_eq!(err["code"], "FORBIDDEN", "{err}");

    // 4. Port WebSocket: 101 Set-Cookie is filtered, Bearer stripped, and revoke closes open WS.
    server.grant(&admin, &ann_id, "use").await;
    let ws_app = axum::Router::new().route(
        "/ws",
        axum::routing::get(
            |headers: axum::http::HeaderMap, ws: axum::extract::WebSocketUpgrade| async move {
                let auth_saw = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("none")
                    .to_owned();
                let mut resp = ws.on_upgrade(move |mut sock| async move {
                    use futures_util::StreamExt;
                    let _ = sock
                        .send(axum::extract::ws::Message::Text(auth_saw.into()))
                        .await;
                    while let Some(Ok(msg)) = sock.next().await {
                        if sock.send(msg).await.is_err() {
                            break;
                        }
                    }
                });
                resp.headers_mut().append(
                    header::SET_COOKIE,
                    "rookery_session=forged; Path=/".parse().unwrap(),
                );
                resp.headers_mut().append(
                    header::SET_COOKIE,
                    "ws_cookie=1; Domain=localhost; Path=/".parse().unwrap(),
                );
                resp
            },
        ),
    );
    let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(ws_listener, ws_app).await;
    });

    let ann_token = server
        .db
        .store
        .auth()
        .create_token(uuid::Uuid::parse_str(&ann_id).unwrap(), "ws-test", None)
        .await
        .unwrap()
        .0;
    let routes = rookery_hosts::proxy::routes().with_state(server.kernel.clone());
    let ports = rookery_hosts::proxy::port_routes().with_state(server.kernel.clone());
    let claim_resp = routes
        .clone()
        .oneshot(
            Request::get(format!("/env/studio/port/{ws_port}/ws"))
                .header(header::AUTHORIZATION, format!("Bearer {ann_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let to = claim_resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let (origin, claim_path) = to.split_once("/.rookery/").unwrap();
    let port_host = origin.strip_prefix("http://").unwrap().to_owned();
    let claimed = ports
        .clone()
        .oneshot(
            Request::get(format!("/.rookery/{claim_path}"))
                .header(header::HOST, &port_host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let port_cookie = claimed.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(proxy_listener, ports).await;
    });

    let cookie_header = format!("{port_cookie}; rookery_session=rookery_ses_secret");
    let bearer_header = format!("bearer {ann_token}");
    let (head_101, mut ws_tcp) = ws_upgrade(
        proxy_addr,
        &port_host,
        "/ws",
        &[
            ("Cookie", &cookie_header),
            ("Authorization", &bearer_header),
        ],
    )
    .await;
    assert!(head_101.starts_with("HTTP/1.1 101"), "{head_101}");
    let head_lower = head_101.to_ascii_lowercase();
    assert!(!head_lower.contains("rookery_session="), "{head_101}");
    assert!(
        head_lower.contains("set-cookie: ws_cookie=1; path=/"),
        "{head_101}"
    );
    let (_, first_msg) = read_ws_frame(&mut ws_tcp).await.unwrap();
    // Rookery Bearer token must be stripped on the WS upgrade request too.
    assert_eq!(String::from_utf8_lossy(&first_msg), "none");

    server
        .call(
            &admin,
            "access.revoke",
            json!({"resource": "env://studio", "principal": format!("user:{ann_id}")}),
        )
        .await
        .unwrap();
    let mut eof_buf = [0u8; 64];
    let closed_cleanly = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws_tcp.read(&mut eof_buf).await {
                Ok(0) | Err(_) => break true,
                Ok(_) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        closed_cleanly,
        "port WebSocket stayed open after access revoke"
    );

    // 5. Desktop relay closes promptly on revoke (when loopback VNC port 5901 is free).
    if let Ok(vnc_listener) = tokio::net::TcpListener::bind("127.0.0.1:5901").await {
        tokio::spawn(async move {
            let Ok((mut vnc, _)) = vnc_listener.accept().await else {
                return;
            };
            let _ = vnc.write_all(b"RFB 003.008\n").await;
            let mut ver = [0u8; 12];
            if vnc.read_exact(&mut ver).await.is_err() {
                return;
            }
            let _ = vnc.write_all(&[1, 1]).await; // 1 security type: None
            let mut chosen = [0u8; 1];
            if vnc.read_exact(&mut chosen).await.is_err() {
                return;
            }
            let _ = vnc.write_all(&[0, 0, 0, 0]).await; // SecurityResult OK
            let mut init = [0u8; 1];
            if vnc.read_exact(&mut init).await.is_err() {
                return;
            }
            let _ = vnc.write_all(b"VNC-READY").await;
            let mut sink = [0u8; 64];
            while let Ok(n) = vnc.read(&mut sink).await {
                if n == 0 {
                    break;
                }
            }
        });
        server.grant(&admin, &ann_id, "use").await;
        let desktop_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let desktop_addr = desktop_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(desktop_listener, routes).await;
        });
        let auth_hdr = format!("Bearer {ann_token}");
        let (head, mut d_ws) = ws_upgrade(
            desktop_addr,
            "127.0.0.1:7777",
            "/env/studio/desktop",
            &[
                ("Origin", "http://127.0.0.1:7777"),
                ("Authorization", &auth_hdr),
            ],
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        let (_, rfb_ver) = read_ws_frame(&mut d_ws).await.unwrap();
        assert_eq!(rfb_ver, b"RFB 003.008\n");
        d_ws.write_all(&ws_client_frame(0x02, b"RFB 003.008\n"))
            .await
            .unwrap();
        let (_, sec_types) = read_ws_frame(&mut d_ws).await.unwrap();
        assert_eq!(sec_types, vec![1, 1]);
        d_ws.write_all(&ws_client_frame(0x02, &[1])).await.unwrap();
        let (_, sec_result) = read_ws_frame(&mut d_ws).await.unwrap();
        assert_eq!(sec_result, vec![0, 0, 0, 0]);
        d_ws.write_all(&ws_client_frame(0x02, &[1])).await.unwrap();
        let (_, server_init) = read_ws_frame(&mut d_ws).await.unwrap();
        assert_eq!(server_init, b"VNC-READY");

        server
            .call(
                &admin,
                "access.revoke",
                json!({"resource": "env://studio", "principal": format!("user:{ann_id}")}),
            )
            .await
            .unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some((op, _)) = read_ws_frame(&mut d_ws).await {
                if op == 0x08 {
                    return true;
                }
            }
            true
        })
        .await
        .unwrap_or(false);
        assert!(closed, "desktop WS stayed open after access revoke");
    }

    drop((term_inbox, sess_inbox, bob_inbox));
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_ownership_unrecorded_cleanup_and_run_as() {
    let Some(host) = Rookeryd::start().await else {
        return;
    };
    let Some(server) = Server::start().await else {
        return;
    };
    let (admin, _) = server.person("Ada", true).await;
    let (ann, ann_id) = server.person("Ann", false).await;
    let (mia, mia_id) = server.person("Mia", false).await;
    server
        .call(
            &Caller::Local,
            "hosts.save",
            json!({"host": "studio", "profile": host.profile()}),
        )
        .await
        .unwrap();
    server.grant(&admin, &ann_id, "use").await;
    server.grant(&admin, &mia_id, "manage").await;

    // 1. Unrecorded terminal on the host needs manage on hosts.sessions.*; unrecorded process uses use.
    let sock = host.dir.path().join("control.sock");
    let unrec_term = control_json(
        &sock,
        "POST",
        "/v1/sessions",
        Some(json!({"kind": "terminal", "user": "agent", "command": ["sh", "-c", "echo unrec; exec cat"]})),
    )
    .await;
    let unrec_term_id = unrec_term["id"].as_str().unwrap().to_owned();
    let unrec_proc = control_json(
        &sock,
        "POST",
        "/v1/sessions",
        Some(json!({"kind": "process", "user": "agent", "command": ["cat"]})),
    )
    .await;
    let unrec_proc_id = unrec_proc["id"].as_str().unwrap().to_owned();

    for (method, params) in [
        (
            "hosts.sessions.screen",
            json!({"host": "studio", "terminal": unrec_term_id}),
        ),
        (
            "hosts.sessions.output",
            json!({"host": "studio", "terminal": unrec_term_id}),
        ),
        (
            "hosts.sessions.input",
            json!({"host": "studio", "terminal": unrec_term_id, "body": {"text": "x"}}),
        ),
        (
            "hosts.sessions.resize",
            json!({"host": "studio", "terminal": unrec_term_id, "body": {"rows": 24, "cols": 80}}),
        ),
        (
            "hosts.sessions.signal",
            json!({"host": "studio", "terminal": unrec_term_id, "body": {"signal": "INT"}}),
        ),
        (
            "hosts.sessions.attach",
            json!({"host": "studio", "terminal": unrec_term_id}),
        ),
        (
            "hosts.sessions.close",
            json!({"host": "studio", "terminal": unrec_term_id}),
        ),
    ] {
        assert_eq!(
            code(server.call(&ann, method, params).await),
            Some(ErrorCode::Forbidden),
            "unrecorded terminal allowed use on {method}"
        );
    }
    server
        .call(
            &mia,
            "hosts.sessions.screen",
            json!({"host": "studio", "terminal": unrec_term_id}),
        )
        .await
        .unwrap();
    server
        .call(
            &ann,
            "hosts.sessions.output",
            json!({"host": "studio", "terminal": unrec_proc_id}),
        )
        .await
        .unwrap();
    server
        .call(
            &ann,
            "hosts.sessions.close",
            json!({"host": "studio", "terminal": unrec_proc_id}),
        )
        .await
        .unwrap();

    // 2. If saving a terminal's record fails, both terminals.open and hosts.sessions.open fail and
    // delete the session on the host so nothing unrecorded is left behind.
    let pool = sqlx::PgPool::connect(&server.db.url).await.unwrap();
    sqlx::raw_sql(
        "CREATE OR REPLACE FUNCTION rookery.fail_terminal_insert() RETURNS trigger AS $$
         BEGIN RAISE EXCEPTION 'forced terminal insert failure'; END;
         $$ LANGUAGE plpgsql;
         CREATE TRIGGER fail_terminal_insert_tr BEFORE INSERT ON rookery.terminals
         FOR EACH ROW EXECUTE FUNCTION rookery.fail_terminal_insert();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let before_sessions = server
        .call(&mia, "hosts.sessions.list", json!({"host": "studio"}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .len();
    assert!(
        server
            .call(
                &ann,
                "terminals.open",
                json!({"env": "studio", "command": ["cat"]}),
            )
            .await
            .is_err()
    );
    assert!(
        server
            .call(
                &ann,
                "hosts.sessions.open",
                json!({"host": "studio", "kind": "terminal", "command": ["cat"]}),
            )
            .await
            .is_err()
    );
    let after_sessions = server
        .call(&mia, "hosts.sessions.list", json!({"host": "studio"}))
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .len();
    assert_eq!(after_sessions, before_sessions);
    sqlx::raw_sql(
        "DROP TRIGGER fail_terminal_insert_tr ON rookery.terminals;
         DROP FUNCTION rookery.fail_terminal_insert();",
    )
    .execute(&pool)
    .await
    .unwrap();

    // 3. Owner downgraded to use loses access to a run-as root terminal, including an open attach.
    let root_term = server
        .call(
            &mia,
            "terminals.open",
            json!({"env": "studio", "command": ["sh", "-c", "echo root-term; exec cat"]}),
        )
        .await
        .unwrap();
    let root_id = root_term["id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE rookery.terminals SET run_as = 'root' WHERE id = $1")
        .bind(&root_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        server
            .call(&mia, "terminals.get", json!({"id": root_id}))
            .await
            .unwrap()["level"],
        "manage"
    );
    let mut mia_inbox = server.connect(&mia);
    let attached = server
        .call_on(&mia_inbox, "terminals.attach", json!({"id": root_id}))
        .await
        .unwrap();
    let root_stream = attached["stream"].as_u64().unwrap() as u32;
    until(&mut mia_inbox, root_stream, 1, "ready").await;

    // Downgrade Mia from manage to use -> Mia is still owner and holds use on env://studio,
    // yet loses the run-as root terminal immediately.
    server.grant(&admin, &mia_id, "use").await;
    let err = wait_stream_closed(&mut mia_inbox, root_stream).await;
    assert_eq!(err["code"], "FORBIDDEN", "{err}");
    assert_eq!(
        code(
            server
                .call(&mia, "terminals.get", json!({"id": root_id}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );
    assert_eq!(
        code(
            server
                .call(&mia, "terminals.write", json!({"id": root_id, "text": "x"}))
                .await
        ),
        Some(ErrorCode::Forbidden)
    );

    pool.close().await;
    drop(mia_inbox);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn file_cas_locks_canonical_path_across_methods() {
    let Some(host) = Rookeryd::start().await else {
        return;
    };
    let Some(server) = Server::start().await else {
        return;
    };
    let (admin, _) = server.person("Ada", true).await;
    let (ann, ann_id) = server.person("Ann", false).await;
    server
        .call(
            &Caller::Local,
            "hosts.save",
            json!({"host": "studio", "profile": host.profile()}),
        )
        .await
        .unwrap();
    server.grant(&admin, &ann_id, "use").await;

    let target = host.file("cas/target.txt");
    let alias = host.file("cas/alias.txt");
    let v0 = server
        .call(
            &ann,
            "env.writeFile",
            json!({"env": "studio", "path": target, "base64": rookery_api::b64(b"v0")}),
        )
        .await
        .unwrap();
    let v0_sha = v0["sha256"].as_str().unwrap().to_owned();
    std::os::unix::fs::symlink("target.txt", &alias).unwrap();

    // Concurrent CAS writes to the target and its symlink alias race on the same canonical lock:
    // exactly one wins and the other observes Conflict.
    let (r1, r2) = tokio::join!(
        server.call(
            &ann,
            "env.writeFile",
            json!({"env": "studio", "path": target, "base64": rookery_api::b64(b"v1"), "ifMatch": v0_sha}),
        ),
        server.call(
            &ann,
            "env.writeFile",
            json!({"env": "studio", "path": alias, "base64": rookery_api::b64(b"v2"), "ifMatch": v0_sha}),
        ),
    );
    let results = [r1, r2];
    let ok_count = results.iter().filter(|r| r.is_ok()).count();
    let conflict_count = results
        .iter()
        .filter(|r| matches!(r, Err(e) if e.error_code() == ErrorCode::Conflict))
        .count();
    assert_eq!(
        (ok_count, conflict_count),
        (1, 1),
        "unexpected CAS race: {results:?}"
    );

    // hosts.files.write (no etag => overwrite) replaces target and updates what alias sees,
    // and env.deleteFile removes it through the same lock.
    server
        .call(
            &ann,
            "hosts.files.write",
            json!({"host": "studio", "path": target, "base64": rookery_api::b64(b"v3")}),
        )
        .await
        .unwrap();
    let read_alias = server
        .call(
            &ann,
            "env.readFile",
            json!({"env": "studio", "path": alias}),
        )
        .await
        .unwrap();
    assert_eq!(read_alias["base64"], rookery_api::b64(b"v3"));
    server
        .call(
            &ann,
            "env.deleteFile",
            json!({"env": "studio", "path": target}),
        )
        .await
        .unwrap();
    assert_eq!(
        code(
            server
                .call(
                    &ann,
                    "env.readFile",
                    json!({"env": "studio", "path": target})
                )
                .await
        ),
        Some(ErrorCode::NotFound)
    );

    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_resume_ready_slow_viewer_and_silent_controller() {
    let Some(host) = Rookeryd::start().await else {
        return;
    };
    let Some(server) = Server::start().await else {
        return;
    };
    let (admin, _) = server.person("Ada", true).await;
    let (ann, ann_id) = server.person("Ann", false).await;
    server
        .call(
            &Caller::Local,
            "hosts.save",
            json!({"host": "studio", "profile": host.profile()}),
        )
        .await
        .unwrap();
    server.grant(&admin, &ann_id, "use").await;

    // 1. Resume mode delivers advertised replay frames BEFORE TerminalEvent::Ready.
    let t = server
        .call(
            &ann,
            "terminals.open",
            json!({"env": "studio", "command": ["sh", "-c", "printf 'before-mark\\n'; exec cat"]}),
        )
        .await
        .unwrap();
    let id = t["id"].as_str().unwrap().to_owned();
    let mut inbox1 = server.connect(&ann);
    let a1 = server
        .call_on(&inbox1, "terminals.attach", json!({"id": id}))
        .await
        .unwrap();
    let s1 = a1["stream"].as_u64().unwrap() as u32;
    until(&mut inbox1, s1, 1, "ready").await;
    send(&inbox1, s1, 0, b"sync1\n");
    let (_, out_len) = until(&mut inbox1, s1, 0, "sync1").await;
    let after = a1["offset"].as_u64().unwrap() + out_len;
    drop(inbox1);

    server
        .call(
            &ann,
            "terminals.write",
            json!({"id": id, "text": "after-mark\n"}),
        )
        .await
        .unwrap();
    for _ in 0..50 {
        let screen = server
            .call(&ann, "terminals.screen", json!({"id": id}))
            .await
            .unwrap();
        if screen["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("after-mark"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let mut inbox2 = server.connect(&ann);
    let resumed = server
        .call_on(
            &inbox2,
            "terminals.attach",
            json!({"id": id, "after": after}),
        )
        .await
        .unwrap();
    assert_eq!(resumed["mode"], "resume", "{resumed}");
    let s2 = resumed["stream"].as_u64().unwrap() as u32;
    let (frames_before_ready, _) = until(&mut inbox2, s2, 1, "ready").await;
    let replayed: String = frames_before_ready
        .iter()
        .filter(|f| f[0] == 0)
        .map(|f| String::from_utf8_lossy(&f[1..]).into_owned())
        .collect();
    assert!(
        replayed.contains("after-mark"),
        "expected after-mark before Ready frame, saw: {frames_before_ready:?}"
    );
    drop(inbox2);

    // 2. Slow viewer with 0 credit does not block pump input or upstream exit for a normal viewer.
    let flood = server
        .call(
            &ann,
            "terminals.open",
            json!({"env": "studio", "command": ["sh"]}),
        )
        .await
        .unwrap();
    let flood_id = flood["id"].as_str().unwrap().to_owned();
    let slow_inbox = server.connect(&ann);
    let _slow = server
        .call_on(
            &slow_inbox,
            "terminals.attach",
            json!({"id": flood_id, "readonly": true}),
        )
        .await
        .unwrap();
    let mut fast_inbox = server.connect(&ann);
    let fast = server
        .call_on(&fast_inbox, "terminals.attach", json!({"id": flood_id}))
        .await
        .unwrap();
    let fast_stream = fast["stream"].as_u64().unwrap() as u32;
    until(&mut fast_inbox, fast_stream, 1, "ready").await;
    send(
        &fast_inbox,
        fast_stream,
        0,
        b"i=0; while [ $i -lt 360 ]; do printf '%01000d\\n' $i; i=$((i+1)); done; echo FLOOD_DONE\n",
    );
    until(&mut fast_inbox, fast_stream, 0, "FLOOD_DONE").await;
    send(&fast_inbox, fast_stream, 0, b"echo STILL_ALIVE\n");
    until(&mut fast_inbox, fast_stream, 0, "STILL_ALIVE").await;
    drop(slow_inbox);
    drop(fast_inbox);

    // 3. An older controller that sends no initial `attached` header falls back after probe (~1s)
    // instead of timing out (15s), and forwards subsequent live output.
    let mock_dir = tempfile::tempdir().unwrap();
    let mock_sock = mock_dir.path().join("control.sock");
    let listener = tokio::net::UnixListener::bind(&mock_sock).unwrap();
    let router = axum::Router::new()
        .route(
            "/v1/version",
            axum::routing::get(|| async {
                axum::Json(json!({
                    "version": rookery_api::VERSION,
                    "api": rookery_api::API,
                    "mode": "single"
                }))
            }),
        )
        .route(
            "/v1/sessions",
            axum::routing::post(|| async {
                axum::Json(json!({
                    "id": "soldctrl00001",
                    "kind": "terminal",
                    "title": "old",
                    "command": ["sh"],
                    "user": "agent",
                    "cols": 80,
                    "rows": 24,
                    "state": "running"
                }))
            })
            .get(|| async { axum::Json(json!([])) }),
        )
        .route(
            "/v1/sessions/{id}",
            axum::routing::get(
                |ws: Result<
                    axum::extract::WebSocketUpgrade,
                    axum::extract::ws::rejection::WebSocketUpgradeRejection,
                >| async move {
                    use axum::response::IntoResponse;
                    if let Ok(ws) = ws {
                        ws.on_upgrade(|mut sock| async move {
                            // Stay silent past the 1s probe, then send raw live output without header.
                            tokio::time::sleep(Duration::from_millis(1300)).await;
                            let _ = sock
                                .send(axum::extract::ws::Message::Binary(
                                    b"late-hello\n".to_vec().into(),
                                ))
                                .await;
                            tokio::time::sleep(Duration::from_secs(10)).await;
                        })
                        .into_response()
                    } else {
                        axum::Json(json!({
                            "id": "soldctrl00001",
                            "kind": "terminal",
                            "title": "old",
                            "command": ["sh"],
                            "user": "agent",
                            "cols": 80,
                            "rows": 24,
                            "state": "running"
                        }))
                        .into_response()
                    }
                },
            ),
        );
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let service = hyper_util::service::TowerToHyperService::new(router.clone());
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    server
        .call(
            &Caller::Local,
            "hosts.save",
            json!({
                "host": "legacy",
                "profile": {
                    "type": "managed",
                    "access": {"link": [host.program.to_string_lossy(), "connect", "--dir", mock_dir.path().to_string_lossy()]}
                }
            }),
        )
        .await
        .unwrap();
    server
        .call(
            &admin,
            "access.grant",
            json!({"resource": "env://legacy", "principal": format!("user:{ann_id}"), "level": "use"}),
        )
        .await
        .unwrap();
    let old_term = server
        .call(
            &ann,
            "terminals.open",
            json!({"env": "legacy", "command": ["sh"]}),
        )
        .await
        .unwrap();
    let old_id = old_term["id"].as_str().unwrap().to_owned();
    let mut old_inbox = server.connect(&ann);
    let started_at = std::time::Instant::now();
    let old_attached = tokio::time::timeout(
        Duration::from_secs(5),
        server.call_on(&old_inbox, "terminals.attach", json!({"id": old_id})),
    )
    .await
    .expect("silent older controller should fall back after ~1s probe, not time out")
    .unwrap();
    assert!(started_at.elapsed() < Duration::from_secs(5));
    let old_stream = old_attached["stream"].as_u64().unwrap() as u32;
    until(&mut old_inbox, old_stream, 1, "ready").await;
    until(&mut old_inbox, old_stream, 0, "late-hello").await;

    drop(old_inbox);
    server.close().await;
}
