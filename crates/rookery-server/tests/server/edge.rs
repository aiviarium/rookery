//! The edge: the leader lock, links, sessions and tokens, /ws with streams, levels and the shell.

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as Ws;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::common::{code, http, set_cookie};
use crate::served;

#[tokio::test]
async fn a_standby_answers_503_until_the_leader_stops() {
    let first = served!();
    let (status, body, _) = http("GET", &format!("{}/healthz", first.url), &[], None).await;
    assert_eq!((status, body.as_str()), (200, "leader\n"));

    // A second server on the same database waits on the lock and answers 503 meanwhile.
    let dir = tempfile::tempdir().unwrap();
    let mut second = rookery_server::start(crate::common::config(&first.db, dir.path()))
        .await
        .unwrap();
    let url = format!("http://{}", second.addr);
    assert_eq!(second.role(), rookery_server::Role::Standby);
    let (status, body, _) = http("GET", &format!("{url}/healthz"), &[], None).await;
    assert_eq!((status, body.as_str()), (503, "standby\n"));
    let (status, body, _) = http("POST", &format!("{url}/rpc"), &[], Some("{}".into())).await;
    assert_eq!(status, 503);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        "UNAVAILABLE"
    );
    let refused =
        rookery_server::client::call(&dir.path().join("server.sock"), "auth.me", json!({}))
            .await
            .unwrap_err();
    assert!(refused.contains("standby"), "{refused}");

    // When the leader stops, the standby takes over.
    let crate::common::Served { running, db, .. } = first;
    running.stop().await.unwrap();
    second.leading().await;
    let (status, _, _) = http("GET", &format!("{url}/healthz"), &[], None).await;
    assert_eq!(status, 200);
    second.stop().await.unwrap();
    db.close().await;
}

#[tokio::test]
async fn the_first_admin_signs_in_with_a_setup_link_from_the_socket() {
    let served = served!();
    // The socket is the server's own machine: an admin with no user.
    let me = served.local("auth.me", json!({})).await.unwrap();
    assert_eq!(
        (
            me["principal"]["kind"].as_str(),
            me["admin"].as_bool(),
            me["user"].is_null()
        ),
        (Some("local"), Some(true), true)
    );

    let link = served
        .local("auth.createLink", json!({"kind": "setup"}))
        .await
        .unwrap();
    let url = link["url"].as_str().unwrap();
    // Looking at the link does not use it.
    let (status, page, _) = http("GET", url, &[], None).await;
    assert_eq!(status, 200);
    assert!(page.contains("first admin"), "{page}");
    // A browser posts the form, gets the cookie and goes to the shell.
    let (status, _, headers) = http(
        "POST",
        url,
        &[("content-type", "application/x-www-form-urlencoded")],
        Some("name=Maria".into()),
    )
    .await;
    assert_eq!(status, 303);
    let cookie = set_cookie(&headers, "rookery_session").expect("a session cookie");
    let (status, session, _) = http(
        "GET",
        &format!("{}/auth/session", served.url),
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(status, 200);
    let session: Value = serde_json::from_str(&session).unwrap();
    assert_eq!(
        (
            session["user"]["name"].as_str(),
            session["user"]["admin"].as_bool()
        ),
        (Some("Maria"), Some(true))
    );
    // Once.
    let (status, _, _) = http("POST", url, &[], None).await;
    assert_eq!(status, 410);
    let (status, _, _) = http("GET", &format!("{}/auth/session", served.url), &[], None).await;
    assert_eq!(status, 401);

    // A setup link is the socket's to make, not an admin's over the network.
    let token = cookie.trim_start_matches("rookery_session=");
    let refused = served
        .rpc(token, "auth.createLink", json!({"kind": "setup"}))
        .await
        .unwrap_err();
    assert_eq!(code(&refused), "FORBIDDEN");

    // Signing out ends the session.
    let (status, _, _) = http(
        "POST",
        &format!("{}/auth/logout", served.url),
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(status, 204);
    let (status, _, _) = http(
        "GET",
        &format!("{}/auth/session", served.url),
        &[("cookie", &cookie)],
        None,
    )
    .await;
    assert_eq!(status, 401);
    served.stop().await;
}

#[tokio::test]
async fn access_tokens_work_on_rpc_until_revoked() {
    let served = served!();
    let session = served.first_admin("Maria").await;
    let made = served
        .rpc(&session, "auth.createToken", json!({"name": "nightly"}))
        .await
        .unwrap();
    let token = made["token"].as_str().unwrap();
    assert!(token.starts_with("rookery_pat_"));
    let me = served.rpc(token, "auth.me", json!({})).await.unwrap();
    assert_eq!(me["user"]["name"], "Maria");
    let listed = served
        .rpc(&session, "auth.listTokens", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["tokens"][0]["hint"], made["info"]["hint"]);

    served
        .rpc(
            &session,
            "auth.revokeToken",
            json!({"id": made["info"]["id"]}),
        )
        .await
        .unwrap();
    let (status, _, _) = http(
        "POST",
        &format!("{}/rpc", served.url),
        &[("authorization", &format!("Bearer {token}"))],
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "auth.me"}).to_string()),
    )
    .await;
    assert_eq!(status, 401);
    // /rpc reads no cookie, so a page elsewhere cannot call it with a browser's session.
    let (status, _, _) = http(
        "POST",
        &format!("{}/rpc", served.url),
        &[("cookie", &format!("rookery_session={session}"))],
        Some(json!({"jsonrpc": "2.0", "id": 1, "method": "auth.me"}).to_string()),
    )
    .await;
    assert_eq!(status, 401);
    served.stop().await;
}

#[tokio::test]
async fn levels_say_who_may_call_what() {
    let served = served!();
    let admin = served.first_admin("Maria").await;
    let member = served.member(&admin, "Ops").await;

    let refused = served
        .rpc(
            &member,
            "auth.createLink",
            json!({"kind": "invite", "name": "Eve"}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&refused), "FORBIDDEN");
    // Anyone makes a link to sign in on another device, for themselves.
    let own = served
        .rpc(&member, "auth.createLink", json!({"kind": "device"}))
        .await
        .unwrap();
    assert_eq!(own["info"]["kind"], "device");
    // A member sees their own links, an admin all of them.
    assert_eq!(
        served
            .rpc(&member, "auth.listLinks", json!({}))
            .await
            .unwrap()["links"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let all = served
        .rpc(&admin, "auth.listLinks", json!({}))
        .await
        .unwrap();
    assert_eq!(
        all["links"][0]["createdByName"], "Ops",
        "an admin sees the links of others"
    );
    // The member cannot end the admin's links, and gets told there is none.
    let theirs = served
        .rpc(
            &admin,
            "auth.createLink",
            json!({"kind": "invite", "name": "Eve"}),
        )
        .await
        .unwrap();
    let refused = served
        .rpc(
            &member,
            "auth.revokeLink",
            json!({"id": theirs["info"]["id"]}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&refused), "NOT_FOUND");

    let refused = served
        .rpc(&member, "nobody.knows", json!({}))
        .await
        .unwrap_err();
    assert_eq!(
        (refused["code"].as_i64(), code(&refused)),
        (Some(-32601), "NOT_FOUND")
    );
    let refused = served
        .rpc(
            &member,
            "auth.createToken",
            json!({"name": "x", "extra": 1}),
        )
        .await
        .unwrap_err();
    assert_eq!(code(&refused), "INVALID");
    // A stream needs a connection on /ws.
    let refused = served
        .rpc(&member, "streams.echo", json!({}))
        .await
        .unwrap_err();
    assert_eq!(code(&refused), "UNAVAILABLE");
    served.stop().await;
}

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

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

/// The next text frame, as JSON.
async fn next_json(socket: &mut Socket) -> Value {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Ws::Text(text) => return serde_json::from_str(&text).unwrap(),
            Ws::Ping(_) | Ws::Pong(_) => {}
            other => panic!("a text frame was expected, not {other:?}"),
        }
    }
}

async fn call(socket: &mut Socket, id: i64, method: &str, params: Value) -> Value {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Ws::Text(request.to_string().into()))
        .await
        .unwrap();
    loop {
        let message = next_json(socket).await;
        if message["id"] == id {
            return message;
        }
    }
}

#[tokio::test]
async fn ws_carries_calls_and_streams_with_credit() {
    let served = served!();
    let token = served.first_admin("Maria").await;
    let mut socket = connect(&served, &token).await;
    let me = call(&mut socket, 1, "auth.me", json!({})).await;
    assert_eq!(me["result"]["user"]["name"], "Maria");
    let devices = call(&mut socket, 2, "auth.listDevices", json!({})).await;
    assert_eq!(devices["result"]["devices"][0]["current"], true);

    let opened = call(&mut socket, 3, "streams.echo", json!({})).await;
    let stream = opened["result"]["stream"].as_u64().unwrap() as u32;
    assert_eq!(
        stream % 2,
        0,
        "a stream the client asked for has an even id"
    );
    socket
        .send(Ws::Binary(
            rookery_proto::stream::encode_frame(stream, b"hello").into(),
        ))
        .await
        .unwrap();
    // The server takes the bytes, grants them back as credit and echoes them.
    let (mut credit, mut echoed) = (false, false);
    while !(credit && echoed) {
        match socket.next().await.unwrap().unwrap() {
            Ws::Text(text) => {
                let message: Value = serde_json::from_str(&text).unwrap();
                if message["method"] == "streams.credit" {
                    assert_eq!(message["params"], json!({"stream": stream, "bytes": 5}));
                    credit = true;
                }
            }
            Ws::Binary(frame) => {
                assert_eq!(
                    rookery_proto::stream::decode_frame(&frame),
                    Some((stream, &b"hello"[..]))
                );
                echoed = true;
            }
            _ => {}
        }
    }
    let eof = json!({"jsonrpc": "2.0", "method": "streams.eof", "params": {"stream": stream}});
    socket.send(Ws::Text(eof.to_string().into())).await.unwrap();
    loop {
        let message = next_json(&mut socket).await;
        if message["method"] == "streams.close" {
            assert_eq!(message["params"]["stream"], stream);
            break;
        }
    }

    served.stop().await;
}

#[tokio::test]
async fn a_revoked_device_ends_its_connections() {
    let served = served!();
    let token = served.first_admin("Maria").await;
    let mut socket = connect(&served, &token).await;
    let devices = call(&mut socket, 1, "auth.listDevices", json!({})).await;
    let id = devices["result"]["devices"][0]["id"].clone();
    // The answer may or may not get out before the connection closes.
    let request =
        json!({"jsonrpc": "2.0", "id": 2, "method": "auth.revokeDevice", "params": {"id": id}});
    socket
        .send(Ws::Text(request.to_string().into()))
        .await
        .unwrap();
    let close = loop {
        match socket.next().await {
            Some(Ok(Ws::Close(frame))) => break frame,
            Some(Ok(_)) => {}
            other => panic!("the connection was to close with a frame, not {other:?}"),
        }
    };
    assert_eq!(
        close.map(|f| u16::from(f.code)),
        Some(rookery_kernel::conn::CLOSE_REVOKED)
    );
    let (status, _, _) = http(
        "GET",
        &format!("{}/auth/session", served.url),
        &[("authorization", &format!("Bearer {token}"))],
        None,
    )
    .await;
    assert_eq!(status, 401);
    served.stop().await;
}

#[tokio::test]
async fn ws_refuses_without_a_session_and_from_other_sites() {
    let served = served!();
    let token = served.first_admin("Maria").await;
    let (status, _, _) = http(
        "GET",
        &format!("{}/ws", served.url),
        &[
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
        None,
    )
    .await;
    assert_eq!(status, 401);
    // A page of another site gets no use of the browser's cookie.
    let cookie = format!("rookery_session={token}");
    let upgrade = [
        ("connection", "upgrade"),
        ("upgrade", "websocket"),
        ("sec-websocket-version", "13"),
        ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ("cookie", cookie.as_str()),
    ];
    let mut elsewhere = upgrade.to_vec();
    elsewhere.push(("origin", "https://elsewhere.example.test"));
    let (status, _, _) = http("GET", &format!("{}/ws", served.url), &elsewhere, None).await;
    assert_eq!(status, 401);
    let mut own = upgrade.to_vec();
    let origin = served.url.clone();
    own.push(("origin", origin.as_str()));
    let (status, _, _) = http("GET", &format!("{}/ws", served.url), &own, None).await;
    assert_eq!(status, 101);
    served.stop().await;
}

#[tokio::test]
async fn the_shell_is_served_from_its_folder() {
    let shell = tempfile::tempdir().unwrap();
    std::fs::create_dir(shell.path().join("assets")).unwrap();
    std::fs::write(
        shell.path().join("index.html"),
        "<!doctype html><title>Crow</title>",
    )
    .unwrap();
    std::fs::write(shell.path().join("assets/app.js"), "export {}").unwrap();
    let dir = shell.path().to_owned();
    let served =
        served!(move |config: &mut rookery_server::ServerConfig| config.shell_dir = Some(dir));
    let get = |path: &str| {
        let url = format!("{}{path}", served.url);
        async move { http("GET", &url, &[], None).await }
    };
    let (status, body, headers) = get("/").await;
    assert_eq!(
        (status, body.as_str()),
        (200, "<!doctype html><title>Crow</title>")
    );
    assert_eq!(
        crate::common::header(&headers, "cache-control"),
        ["no-cache"]
    );
    let (status, body, headers) = get("/assets/app.js").await;
    assert_eq!((status, body.as_str()), (200, "export {}"));
    assert!(crate::common::header(&headers, "content-type")[0].starts_with("text/javascript"));
    // The shell's own routes load the shell; a missing asset is missing.
    assert_eq!(
        get("/hosts/studio").await.1,
        "<!doctype html><title>Crow</title>"
    );
    assert_eq!(get("/assets/gone.js").await.0, 404);
    assert_eq!(get("/../../etc/passwd").await.0, 404);
    served.stop().await;
}

#[tokio::test]
async fn without_a_shell_the_root_says_so() {
    let served = served!();
    let (status, body, _) = http("GET", &format!("{}/", served.url), &[], None).await;
    assert_eq!(status, 200);
    assert!(body.contains("no shell is installed"), "{body}");
    assert_eq!(
        http("GET", &format!("{}/hosts", served.url), &[], None)
            .await
            .0,
        404
    );
    served.stop().await;
}
