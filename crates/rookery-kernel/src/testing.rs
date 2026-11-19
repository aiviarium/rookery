//! A kernel on a fresh test database, for the tests of facilities: people to call as, calls, and
//! connections with no socket.

use rookery_proto::{Message, RpcError, User};
use rookery_store::testing::TestDb;
use rookery_store::users::Login;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::conn::{ConnHandle, Outbound};
use crate::{Call, Caller, Config, Kernel, Methods};

pub struct TestKernel {
    pub kernel: Kernel,
    pub db: TestDb,
}

/// What a detached connection was sent.
pub struct Inbox {
    pub conn: ConnHandle,
    pub rx: mpsc::UnboundedReceiver<Outbound>,
}

impl Inbox {
    /// The next notification named `method`, skipping the others and binary frames.
    pub async fn notification(&mut self, method: &str) -> Value {
        loop {
            match self.rx.recv().await.expect("the connection is open") {
                Outbound::Text(text) => {
                    if let Ok(Message::Notification(n)) = Message::parse(&text)
                        && n.method == method
                    {
                        return n.params;
                    }
                }
                Outbound::Binary(_) => {}
            }
        }
    }

    /// The next binary frame of a stream, without its id. Grants the credit back.
    pub async fn frame(&mut self, stream: u32) -> Vec<u8> {
        loop {
            if let Outbound::Binary(frame) = self.rx.recv().await.expect("the connection is open")
                && let Some((id, payload)) = rookery_proto::stream::decode_frame(&frame)
                && id == stream
            {
                self.conn.streams().credit(stream, payload.len() as u32);
                return payload.to_vec();
            }
        }
    }

    /// The JSON lines of a stream until it closes.
    pub async fn lines(&mut self, stream: u32) -> Vec<Value> {
        let mut bytes = Vec::new();
        loop {
            match self.rx.recv().await {
                Some(Outbound::Binary(frame)) => {
                    if let Some((id, payload)) = rookery_proto::stream::decode_frame(&frame)
                        && id == stream
                    {
                        self.conn.streams().credit(stream, payload.len() as u32);
                        bytes.extend_from_slice(payload);
                    }
                }
                Some(Outbound::Text(text)) => {
                    if let Ok(Message::Notification(n)) = Message::parse(&text)
                        && n.method == "streams.close"
                        && n.params["stream"] == stream
                    {
                        break;
                    }
                }
                None => break,
            }
        }
        bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).expect("a JSON line"))
            .collect()
    }
}

impl TestKernel {
    /// A kernel with `methods`, started, or `None` with no Postgres to test on.
    pub async fn new(methods: Methods) -> Option<TestKernel> {
        let db = TestDb::new().await?;
        let kernel = Kernel::new(
            Config::new("http://127.0.0.1:7777"),
            db.store.clone(),
            methods,
        );
        kernel.methods.started(&kernel);
        Some(TestKernel { kernel, db })
    }

    /// A person to call as.
    pub async fn person(&self, name: &str, admin: bool) -> Caller {
        let login = Login {
            issuer: "https://id.example.test".into(),
            subject: name.into(),
            name: name.into(),
            email: Some(format!("{}@example.test", name.to_lowercase())),
            admin,
        };
        let (user, _) = self
            .db
            .store
            .users()
            .upsert_login(&login)
            .await
            .expect("making a person");
        let user: User = crate::auth::wire_user(&user);
        Caller::User {
            user,
            session: None,
            token: None,
        }
    }

    pub async fn call(
        &self,
        caller: &Caller,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let call = Call::new(self.kernel.clone(), caller.clone(), None);
        self.kernel.methods.dispatch(call, method, params).await
    }

    /// A connection for `caller`, for methods that stream or notify.
    pub fn connect(&self, caller: &Caller) -> Inbox {
        let (conn, rx) = ConnHandle::detached(&self.kernel, caller.clone());
        Inbox { conn, rx }
    }

    pub async fn call_on(
        &self,
        inbox: &Inbox,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let call = Call::new(
            self.kernel.clone(),
            inbox.conn.caller().clone(),
            Some(inbox.conn.clone()),
        );
        self.kernel.methods.dispatch(call, method, params).await
    }

    pub async fn close(self) {
        self.kernel.shutdown.cancel();
        self.db.close().await;
    }
}

/// `let k = test_kernel!(methods);`: a kernel on a fresh database, or the test returns early when there
/// is no Postgres.
#[macro_export]
macro_rules! test_kernel {
    ($methods:expr) => {
        match $crate::testing::TestKernel::new($methods).await {
            Some(kernel) => kernel,
            None => {
                eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
                return;
            }
        }
    };
}
