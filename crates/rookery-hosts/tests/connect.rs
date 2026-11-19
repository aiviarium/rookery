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
    // And without use, the owner's own terminal is out of reach too.
    assert_eq!(
        code(server.call(&ann, "terminals.get", json!({"id": id})).await),
        Some(ErrorCode::Forbidden)
    );
    server.grant(&admin, &ann_id, "use").await;

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
