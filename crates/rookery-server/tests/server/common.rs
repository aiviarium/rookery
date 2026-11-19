//! A server on a test database, and a client for it.

use std::path::PathBuf;

use rookery_proto::auth::AuthMode;
use rookery_server::{Running, ServerConfig};
use rookery_store::testing::TestDb;
use serde_json::{Value, json};

pub struct Served {
    pub running: Running,
    pub url: String,
    pub socket: PathBuf,
    pub db: TestDb,
    _dir: tempfile::TempDir,
}

pub fn config(db: &TestDb, dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        database_url: db.url.clone(),
        lock_url: db.url.clone(),
        listen: "127.0.0.1:0".parse().unwrap(),
        public_url: String::new(),
        secret_key: Some(rookery_store::seal::Key::generate()),
        blobs: None,
        auth: AuthMode::Local,
        oidc: None,
        admins: Vec::new(),
        shell_dir: None,
        socket: Some(dir.join("server.sock")),
    }
}

/// A server that leads on a database of its own, or `None` with no Postgres to test on.
pub async fn serve_with(change: impl FnOnce(&mut ServerConfig)) -> Option<Served> {
    let db = TestDb::new().await?;
    let dir = tempfile::tempdir().unwrap();
    let mut config = config(&db, dir.path());
    change(&mut config);
    let socket = config.socket.clone().unwrap();
    let mut running = rookery_server::start(config)
        .await
        .expect("starting rookery serve");
    running.leading().await;
    let url = format!("http://{}", running.addr);
    Some(Served {
        running,
        url,
        socket,
        db,
        _dir: dir,
    })
}

#[macro_export]
macro_rules! served {
    () => {
        served!(|_| {})
    };
    ($change:expr) => {
        match $crate::common::serve_with($change).await {
            Some(served) => served,
            None => {
                eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
                return;
            }
        }
    };
}

impl Served {
    pub async fn stop(self) {
        self.running.stop().await.unwrap();
        self.db.close().await;
    }

    /// A call over the socket, as the server's own machine.
    pub async fn local(&self, method: &str, params: Value) -> Result<Value, String> {
        rookery_server::client::call(&self.socket, method, params).await
    }

    /// The first admin: a setup link from the socket, used up like a client does. Returns the session's
    /// token.
    pub async fn first_admin(&self, name: &str) -> String {
        let link = self
            .local("auth.createLink", json!({"kind": "setup"}))
            .await
            .unwrap();
        let url = link["url"].as_str().unwrap().to_owned();
        assert!(
            url.starts_with(&self.url),
            "{url} is on the server's public address"
        );
        let (status, body, _) = http(
            "POST",
            &url,
            &[
                ("accept", "application/json"),
                ("content-type", "application/json"),
            ],
            Some(json!({"name": name}).to_string()),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["user"]["admin"], true);
        body["token"].as_str().unwrap().to_owned()
    }

    /// A member made by an invite of `admin`'s, signed in: their session's token.
    pub async fn member(&self, admin: &str, name: &str) -> String {
        let link = self
            .rpc(
                admin,
                "auth.createLink",
                json!({"kind": "invite", "name": name}),
            )
            .await
            .unwrap();
        let (status, body, _) = http(
            "POST",
            link["url"].as_str().unwrap(),
            &[("accept", "application/json")],
            None,
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (
                body["user"]["name"].as_str(),
                body["user"]["admin"].as_bool()
            ),
            (Some(name), Some(false))
        );
        body["token"].as_str().unwrap().to_owned()
    }

    /// A call on /rpc with a token.
    pub async fn rpc(&self, token: &str, method: &str, params: Value) -> Result<Value, Value> {
        let body =
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let auth = format!("Bearer {token}");
        let (status, text, _) = http(
            "POST",
            &format!("{}/rpc", self.url),
            &[("authorization", &auth)],
            Some(body),
        )
        .await;
        let response: Value =
            serde_json::from_str(&text).unwrap_or_else(|_| panic!("{status}: {text}"));
        match response.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(response["result"].clone()),
        }
    }
}

/// The name of an error's code.
pub fn code(error: &Value) -> &str {
    error["data"]["code"].as_str().unwrap_or("")
}

/// One HTTP request, no redirects followed: the status, the body and the headers.
pub async fn http(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<String>,
) -> (u16, String, Vec<(String, String)>) {
    let method = method.to_owned();
    let url = url.to_owned();
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into();
        let mut request = ureq::http::Request::builder()
            .method(method.as_str())
            .uri(&url);
        for (k, v) in &headers {
            request = request.header(k, v);
        }
        let mut response = match body {
            Some(body) => agent.run(request.body(body).unwrap()),
            None => agent.run(request.body(()).unwrap()),
        }
        .unwrap_or_else(|e| panic!("{method} {url}: {e}"));
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        (response.status().as_u16(), text, headers)
    })
    .await
    .unwrap()
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
        .collect()
}

/// The `name=value` of a cookie a response set.
pub fn set_cookie(headers: &[(String, String)], name: &str) -> Option<String> {
    header(headers, "set-cookie")
        .into_iter()
        .filter_map(|c| c.split(';').next())
        .find(|pair| pair.starts_with(&format!("{name}=")))
        .map(str::to_owned)
}
