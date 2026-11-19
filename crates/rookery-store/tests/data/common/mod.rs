#![allow(dead_code)]

use rookery_store::collections::CollectionDef;
use rookery_store::spaces::NewSpace;
use rookery_store::testing::TestDb;
use rookery_store::{Caller, Level};
use serde_json::{Value, json};
use uuid::Uuid;

pub fn defs(v: Value) -> Vec<CollectionDef> {
    serde_json::from_value(v).expect("collection definitions")
}

/// The issues plugin: projects and issues (the example in data.md), plus comments.
pub fn issues_defs() -> Vec<CollectionDef> {
    defs(json!([
        {
            "name": "projects",
            "scope": "space",
            "entity": { "scheme": "project", "title": "name", "private": "private" },
            "fields": [
                { "name": "name", "type": "text", "search": true, "required": true },
                { "name": "key", "type": "text", "unique": true, "caseInsensitive": true },
                { "name": "private", "type": "boolean" }
            ]
        },
        {
            "name": "issues",
            "scope": "space",
            "entity": { "scheme": "issue", "title": "title", "parent": "project" },
            "access": { "read": "view", "write": "use" },
            "history": true,
            "trash": true,
            "fields": [
                { "name": "number", "type": "sequence", "per": "space" },
                { "name": "title", "type": "text", "search": true },
                { "name": "state", "type": "enum", "values": ["open", "done"], "index": true, "default": "open" },
                { "name": "project", "type": "ref", "to": "project" },
                { "name": "assignee", "type": "ref", "to": "user" },
                { "name": "labels", "type": "refs" },
                { "name": "estimate", "type": "number" },
                { "name": "due", "type": "date" },
                { "name": "closedAt", "type": "datetime" },
                { "name": "body", "type": "richtext", "search": true },
                { "name": "meta", "type": "json" }
            ],
            "indexes": [["state", "assignee"]]
        },
        {
            "name": "comments",
            "scope": "space",
            "fields": [
                { "name": "issue", "type": "ref", "to": "issue" },
                { "name": "body", "type": "richtext" }
            ]
        },
        {
            "name": "drafts",
            "scope": "user",
            "fields": [
                { "name": "text", "type": "longtext", "search": true },
                { "name": "about", "type": "ref" }
            ]
        }
    ]))
}

/// The docs plugin: pages with rich text that mention issues and users.
pub fn docs_defs() -> Vec<CollectionDef> {
    defs(json!([
        {
            "name": "pages",
            "scope": "space",
            "entity": { "scheme": "doc", "title": "title" },
            "trash": true,
            "fields": [
                { "name": "title", "type": "text", "search": true },
                { "name": "body", "type": "richtext", "search": true },
                { "name": "related", "type": "refs" }
            ]
        }
    ]))
}

pub fn doc_with_mentions(text: &str, uris: &[&str]) -> Value {
    let mut content = vec![json!({ "type": "text", "text": text })];
    for u in uris {
        content.push(json!({ "type": "text", "text": " " }));
        content.push(json!({ "type": "mention", "attrs": { "uri": u, "label": format!("@{u}") } }));
    }
    json!({ "type": "doc", "content": [{ "type": "paragraph", "content": content }] })
}

/// A database with the issues plugin applied, a project space owned by `owner` and a member
/// `member` with `use` on it.
pub struct World {
    pub db: TestDb,
    pub owner: Uuid,
    pub member: Uuid,
    pub outsider: Uuid,
    pub space: Uuid,
}

impl World {
    /// `None` without a Postgres to test on.
    pub async fn new() -> Option<World> {
        install_statement_log();
        let db = TestDb::new().await?;
        db.store
            .collections()
            .apply("issues", &issues_defs())
            .await
            .expect("apply issues");
        let owner = db.user("owner").await;
        let member = db.user("member").await;
        let outsider = db.user("outsider").await;
        let space = db
            .store
            .spaces()
            .create(
                &Caller::user(owner),
                &NewSpace {
                    kind: "project".into(),
                    name: "Crow".into(),
                    ..Default::default()
                },
            )
            .await
            .expect("space")
            .id;
        db.store
            .spaces()
            .add_member(
                &Caller::user(owner),
                space,
                &format!("user:{}", member),
                Level::Use,
            )
            .await
            .expect("member");
        Some(World {
            db,
            owner,
            member,
            outsider,
            space,
        })
    }

    pub fn as_user(&self, u: Uuid) -> Caller {
        Caller::user(u).in_space(self.space)
    }

    pub fn data(&self) -> rookery_store::collections::Data<'_> {
        self.db.store.data("issues")
    }
}

thread_local! {
    static STATEMENTS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static RECORDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs the process-wide subscriber that records, per thread, every statement sqlx sends (its
/// `sqlx::query` events). It is global and not a `set_default` of the measuring thread on purpose: a scoped
/// dispatcher that is the only one registered makes tracing compute a callsite's interest from the thread that
/// first hits it, so a callsite first reached by another test's thread is silenced for this one (measured: an
/// event lost in about 1 of 60 runs; a global default loses none). `World::new` calls it before its first
/// statement, so the install does not happen in the middle of a measurement either.
pub fn install_statement_log() {
    use tracing_subscriber::layer::{Context, SubscriberExt};

    #[derive(Default)]
    struct Statement(String);
    impl tracing::field::Visit for Statement {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "db.statement" || field.name() == "summary" {
                self.0 = format!("{value:?}");
            }
        }
    }
    struct Log;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Log {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            if event.metadata().target() == "sqlx::query" && RECORDING.with(|r| r.get()) {
                let mut st = Statement::default();
                event.record(&mut st);
                let sql: String =
                    st.0.split_whitespace()
                        .take(12)
                        .collect::<Vec<_>>()
                        .join(" ");
                STATEMENTS.with(|log| log.borrow_mut().push(sql));
            }
        }
    }
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Log))
            .expect("no other global tracing subscriber in the store tests");
    });
}

/// The statements sent to Postgres while `f` runs, on this thread only (tests running beside it, on other
/// threads and databases, are not in it), each cut to a few words for a failure message. Use it in a test on
/// the default single-threaded runtime: everything `f` awaits then runs on this thread.
pub async fn queries_of<T>(f: impl std::future::Future<Output = T>) -> (Vec<String>, T) {
    install_statement_log();
    STATEMENTS.with(|log| log.borrow_mut().clear());
    RECORDING.with(|r| r.set(true));
    let out = f.await;
    RECORDING.with(|r| r.set(false));
    let statements = STATEMENTS.with(|log| std::mem::take(&mut *log.borrow_mut()));
    (statements, out)
}

/// Whether the Postgres of the tests has pgvector, which vector fields need. CI's has.
pub async fn has_pgvector(store: &rookery_store::Store) -> bool {
    let there: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'vector')",
    )
    .fetch_one(store.pool())
    .await
    .expect("asking for pgvector");
    if !there && std::env::var_os("CI").is_some() {
        panic!("CI's Postgres has pgvector");
    }
    there
}
