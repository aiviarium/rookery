//! A kernel with the data layer and documents, the plugin `jot` applied, and connections that keep
//! what the server sends them, as the documents' tests expect of a socket.

use std::time::Duration;

use rookery_kernel::conn::{ConnHandle, Outbound};
use rookery_kernel::testing::TestKernel;
use rookery_kernel::{Call, Caller, Methods};
use rookery_proto::Message;
use serde_json::{Value, json};
use tokio::sync::mpsc;

pub fn methods() -> Methods {
    let mut methods = Methods::default();
    rookery_kernel::streams::register(&mut methods);
    rookery_auth::register(&mut methods);
    rookery_facilities::register(&mut methods);
    rookery_jobs::notify::register(&mut methods);
    rookery_data::register(&mut methods);
    rookery_documents::register(&mut methods);
    methods
}

/// A kernel on a fresh database with the plugin `jot`: a space kind, an entity collection and a block.
/// `None` without a Postgres to test on.
pub async fn start() -> Option<TestKernel> {
    let k = TestKernel::new(methods()).await?;
    rookery_data::plugins::apply(
        &k.kernel,
        &rookery_data::plugins::PluginData {
            plugin: "jot".into(),
            collections: json!([{
                "name": "jots",
                "entity": { "scheme": "jot", "title": "title" },
                "trash": true,
                "fields": [{ "name": "title", "type": "text", "search": true }]
            }]),
            ..Default::default()
        },
        &rookery_data::plugins::NoTransforms,
    )
    .await
    .expect("applying jot's collections");
    rookery_data::spaces::set_kinds(
        &k.kernel,
        "jot",
        vec![rookery_proto::data::SpaceKindInfo {
            id: "jot.team".into(),
            plugin: "jot".into(),
            title: "Team".into(),
            icon: None,
            description: None,
        }],
    );
    rookery_documents::live::set_blocks(
        &k.kernel,
        "jot",
        vec![rookery_documents::live::BlockTemplate {
            id: "jot.counter".into(),
            text: Some("{label}: {count}".into()),
            markdown: None,
        }],
    );
    Some(k)
}

/// A person signed in to the test kernel.
pub async fn sign_in(k: &TestKernel, name: &str) -> Caller {
    k.person(name, false).await
}

/// A connection as a socket of the tests sees it: answers to calls, and the notifications and binary
/// frames the server sent, in order.
pub struct Ws {
    kernel: rookery_kernel::Kernel,
    conn: ConnHandle,
    rx: mpsc::Receiver<Outbound>,
    pub notifications: Vec<Value>,
    pub frames: Vec<Vec<u8>>,
}

impl Ws {
    pub async fn connect(k: &TestKernel, caller: &Caller) -> Ws {
        let (conn, rx) = ConnHandle::detached(&k.kernel, caller.clone());
        Ws {
            kernel: k.kernel.clone(),
            conn,
            rx,
            notifications: Vec::new(),
            frames: Vec::new(),
        }
    }

    /// Calls a method: `{ result }` or `{ error }`, as a response object has them.
    pub async fn call_raw(&mut self, method: &str, params: Value) -> Value {
        let call = Call::new(
            self.kernel.clone(),
            self.conn.caller().clone(),
            Some(self.conn.clone()),
        );
        match self.kernel.methods.dispatch(call, method, params).await {
            Ok(result) => json!({ "result": result }),
            Err(e) => json!({ "error": e }),
        }
    }

    /// Calls a method and returns its result, panicking on an error.
    pub async fn call(&mut self, method: &str, params: Value) -> Value {
        let r = self.call_raw(method, params).await;
        if let Some(e) = r.get("error") {
            panic!("{method} failed: {e}");
        }
        r["result"].clone()
    }

    /// Sends a notification, as a socket would: `streams.credit` and `streams.close`.
    pub async fn notify(&mut self, method: &str, params: Value) {
        let _ = self.call_raw(method, params).await;
    }

    /// Sends a binary frame of a stream, as a socket would.
    pub async fn send_binary(&mut self, frame: Vec<u8>) {
        self.conn.streams().deliver(&frame);
    }

    /// Takes one thing the server sent; `false` when the connection is gone.
    async fn recv(&mut self) -> bool {
        match self.rx.recv().await {
            Some(Outbound::Binary(frame)) => {
                self.frames.push(frame);
                true
            }
            Some(Outbound::Text(text)) => {
                if let Ok(Message::Notification(n)) = Message::parse(&text) {
                    self.notifications
                        .push(json!({ "method": n.method, "params": n.params }));
                }
                true
            }
            None => false,
        }
    }

    /// Takes what the server sends until `done` holds or `limit` passes. Returns whether it held.
    pub async fn pump_until(&mut self, limit: Duration, mut done: impl FnMut(&Ws) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        while !done(self) {
            match tokio::time::timeout_at(deadline, self.recv()).await {
                Ok(true) => {}
                _ => return done(self),
            }
        }
        true
    }
}
