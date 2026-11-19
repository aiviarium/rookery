//! `rookery server import --from FILE|postgres://…`: an existing Rookery store into the server's
//! database. It writes the new tables in one transaction on an empty database, and a second run
//! refuses. Sealed values are opened with the old key and sealed again under the server's key, which is
//! the same key when the store's key file is the server's. The output of the jobs within the retention
//! goes into their logs. The old store is only read.

use crate::cli::fail::{Fail, Result};
use crate::cli::out;
use rookery_core::seal;
use rookery_core::store::{self, StateRow, kind};
use rookery_server::rookery_store;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A row of the old `messages`.
struct OldMessage {
    seq: i64,
    host: String,
    id: i64,
    topic: String,
    body: String,
    keep: String,
    from_level: String,
    from_uid: i64,
    session: Option<String>,
    time: String,
    done: bool,
}

/// A row of the old `jobs`.
struct OldJob {
    id: String,
    host: String,
    what: String,
    started: String,
    ended: Option<String>,
    exit_code: Option<i32>,
    output_path: String,
    subject: String,
    action: String,
    stopped_by: Option<String>,
    arguments: Option<String>,
}

/// What a store of before holds.
#[derive(Default)]
struct Old {
    state: Vec<StateRow>,
    messages: Vec<OldMessage>,
    cursors: Vec<(String, i64)>,
    kv: Vec<(String, String, String, i64)>,
    jobs: Vec<OldJob>,
    /// Where the job's output files are, beside a SQLite file.
    jobs_dir: Option<PathBuf>,
}

fn sqlite_rows(path: &Path) -> Result<Old> {
    use rusqlite::{Connection, OpenFlags};
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Fail::failed(format!("{}: {e}", path.display())))?;
    let fail = |e: rusqlite::Error| Fail::failed(format!("reading {}: {e}", path.display()));
    // Every table as of one moment, while a Rookery that still runs may write it.
    let snapshot = db.unchecked_transaction().map_err(fail)?;
    let db = &*snapshot;
    let schema: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(fail)?;
    if schema < 7 {
        return Err(Fail::refused(format!(
            "{} is a store of an older Rookery (schema {schema}): open it once with Rookery 0.6 first, which brings it up to date",
            path.display()
        )));
    }
    let mut old = Old {
        jobs_dir: path.parent().map(|dir| dir.join("jobs")),
        ..Old::default()
    };
    let mut q = db
        .prepare("SELECT kind, scope, name, value, sealed, updated, label FROM state ORDER BY kind, scope, name")
        .map_err(fail)?;
    old.state = q
        .query_map([], |r| {
            Ok(StateRow {
                kind: r.get(0)?,
                scope: r.get(1)?,
                name: r.get(2)?,
                value: r.get(3)?,
                sealed: r.get(4)?,
                updated: r.get(5)?,
                label: r.get(6)?,
            })
        })
        .map_err(fail)?
        .collect::<rusqlite::Result<_>>()
        .map_err(fail)?;
    let mut q = db
        .prepare("SELECT seq, host, id, topic, body, keep, from_level, from_uid, session, time, done FROM messages ORDER BY seq")
        .map_err(fail)?;
    old.messages = q
        .query_map([], |r| {
            Ok(OldMessage {
                seq: r.get(0)?,
                host: r.get(1)?,
                id: r.get(2)?,
                topic: r.get(3)?,
                body: r.get(4)?,
                keep: r.get(5)?,
                from_level: r.get(6)?,
                from_uid: r.get(7)?,
                session: r.get(8)?,
                time: r.get(9)?,
                done: r.get(10)?,
            })
        })
        .map_err(fail)?
        .collect::<rusqlite::Result<_>>()
        .map_err(fail)?;
    let mut q = db
        .prepare("SELECT host, last_id FROM cursors")
        .map_err(fail)?;
    old.cursors = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(fail)?
        .collect::<rusqlite::Result<_>>()
        .map_err(fail)?;
    let mut q = db
        .prepare("SELECT namespace, key, value, revision FROM kv")
        .map_err(fail)?;
    old.kv = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(fail)?
        .collect::<rusqlite::Result<_>>()
        .map_err(fail)?;
    let mut q = db
        .prepare("SELECT id, host, what, started, ended, exit_code, output_path, subject, action, stopped_by, arguments FROM jobs ORDER BY started")
        .map_err(fail)?;
    old.jobs = q
        .query_map([], |r| {
            Ok(OldJob {
                id: r.get(0)?,
                host: r.get(1)?,
                what: r.get(2)?,
                started: r.get(3)?,
                ended: r.get(4)?,
                exit_code: r.get(5)?,
                output_path: r.get(6)?,
                subject: r.get(7)?,
                action: r.get(8)?,
                stopped_by: r.get(9)?,
                arguments: r.get(10)?,
            })
        })
        .map_err(fail)?
        .collect::<rusqlite::Result<_>>()
        .map_err(fail)?;
    Ok(old)
}

fn postgres_rows(url: &str) -> Result<Old> {
    // The old store's own client: a runtime of its own, as it was read before.
    let url = url.to_owned();
    std::thread::spawn(move || postgres_read(&url))
        .join()
        .map_err(|_| Fail::failed("reading the store stopped"))?
}

fn postgres_read(url: &str) -> Result<Old> {
    let mut config: postgres::Config = url
        .parse()
        .map_err(|_| Fail::usage(format!("the store's URL {} is not one", redacted(url))))?;
    // The password stays out of the URL that a command line or a log shows.
    if config.get_password().is_none()
        && let Ok(password) = std::env::var("PGPASSWORD")
    {
        config.password(password);
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| Fail::failed(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let mut client = config
        .connect(tokio_postgres_rustls::MakeRustlsConnect::new(tls))
        .map_err(|e| {
            Fail::unreachable(format!("the store's database at {}: {e}", redacted(url)))
        })?;
    let fail = |e: postgres::Error| {
        let said = e
            .as_db_error()
            .map(|db| db.message().to_owned())
            .unwrap_or_else(|| e.to_string());
        Fail::failed(format!("reading the store: {said}"))
    };
    // Nothing of this transaction is ever written.
    let mut t = client
        .build_transaction()
        .read_only(true)
        .start()
        .map_err(fail)?;
    // The tables are where Rookery made them, in public, before a schema named as the user such as
    // the server's own `rookery`.
    t.batch_execute("SET LOCAL search_path TO public, \"$user\"")
        .map_err(fail)?;
    let mut old = Old::default();
    for r in t
        .query("SELECT kind, scope, name, value, sealed, updated, label FROM state ORDER BY kind, scope, name", &[])
        .map_err(fail)?
    {
        old.state.push(StateRow {
            kind: r.get(0),
            scope: r.get(1),
            name: r.get(2),
            value: r.get(3),
            sealed: r.get(4),
            updated: r.get(5),
            label: r.get(6),
        });
    }
    for r in t
        .query("SELECT seq, host, id, topic, body, keep, from_level, from_uid, session, time, done FROM messages ORDER BY seq", &[])
        .map_err(fail)?
    {
        old.messages.push(OldMessage {
            seq: r.get(0),
            host: r.get(1),
            id: r.get(2),
            topic: r.get(3),
            body: r.get(4),
            keep: r.get(5),
            from_level: r.get(6),
            from_uid: r.get(7),
            session: r.get(8),
            time: r.get(9),
            done: r.get(10),
        });
    }
    for r in t
        .query("SELECT host, last_id FROM cursors", &[])
        .map_err(fail)?
    {
        old.cursors.push((r.get(0), r.get(1)));
    }
    for r in t
        .query("SELECT namespace, key, value, revision FROM kv", &[])
        .map_err(fail)?
    {
        old.kv.push((r.get(0), r.get(1), r.get(2), r.get(3)));
    }
    for r in t
        .query("SELECT id, host, what, started, ended, exit_code, output_path, subject, action, stopped_by, arguments FROM jobs ORDER BY started", &[])
        .map_err(fail)?
    {
        old.jobs.push(OldJob {
            id: r.get(0),
            host: r.get(1),
            what: r.get(2),
            started: r.get(3),
            ended: r.get(4),
            exit_code: r.get(5),
            output_path: r.get(6),
            subject: r.get(7),
            action: r.get(8),
            stopped_by: r.get(9),
            arguments: r.get(10),
        });
    }
    let _ = t.rollback();
    Ok(old)
}

/// The kinds the server does not keep, and why.
fn left_out(kind: &str) -> Option<&'static str> {
    match kind {
        kind::TOKEN => {
            Some("the server token; make access tokens with rookery server token --user NAME")
        }
        kind::MIRROR | kind::META => Some("what clients cached of a server"),
        _ => None,
    }
}

/// What the import did.
#[derive(Default)]
pub struct Report {
    pub tables: BTreeMap<String, u64>,
    pub skipped: BTreeMap<String, u64>,
}

impl Report {
    fn wrote(&mut self, table: &str) {
        *self.tables.entry(format!("rookery.{table}")).or_default() += 1;
    }

    fn skipped(&mut self, why: &str) {
        *self.skipped.entry(why.to_owned()).or_default() += 1;
    }
}

fn rfc3339(text: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// The job's output, from where its row says or beside the store.
fn output_of(job: &OldJob, jobs_dir: Option<&Path>) -> Option<String> {
    let given = Path::new(&job.output_path);
    let read = std::fs::read(given).ok().or_else(|| {
        let name = given.file_name()?;
        std::fs::read(jobs_dir?.join(name)).ok()
    })?;
    Some(String::from_utf8_lossy(&read).into_owned())
}

/// The old key: its file, or the server's own.
fn old_keys(
    key_file: Option<&Path>,
    server: Option<&rookery_store::seal::Key>,
) -> Result<Option<seal::Keys>> {
    let key = match key_file {
        Some(file) => Some(
            seal::Key::read(file)
                .map_err(|e| Fail::failed(format!("{}: {e}", file.display())))?
                .ok_or_else(|| Fail::failed(format!("{} holds no key", file.display())))?,
        ),
        None => match server {
            Some(server) => Some(
                seal::Key::from_hex(&server.to_hex()).map_err(|e| Fail::failed(e.to_string()))?,
            ),
            None => None,
        },
    };
    Ok(key.map(|key| seal::Keys::new(key, vec![])))
}

/// Imports into the database of `ROOKERY_DATABASE_URL`.
/// The store a person names: a file, a URL without a password (which goes in `PGPASSWORD`), or the
/// first line of `--from-file`, which may hold one.
pub fn store_of(from: Option<&str>, from_file: Option<&Path>) -> Result<String> {
    match (from, from_file) {
        (Some(from), None) => {
            if is_url(from) && password_of(from).is_some() {
                return Err(Fail::usage(
                    "a password in --from shows in the process list: give it in PGPASSWORD, or the URL in a file with --from-file",
                ));
            }
            Ok(from.to_owned())
        }
        (None, Some(file)) => {
            let text = std::fs::read_to_string(file)
                .map_err(|e| Fail::usage(format!("--from-file {}: {e}", file.display())))?;
            text.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| Fail::usage(format!("{} names no store", file.display())))
        }
        _ => Err(Fail::usage("name the store with --from or --from-file")),
    }
}

fn is_url(from: &str) -> bool {
    from.starts_with("postgres://") || from.starts_with("postgresql://")
}

fn password_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let user = rest.split(['/', '?']).next()?.rsplit_once('@')?.0;
    user.split_once(':').map(|(_, password)| password)
}

/// A store as it may be shown: a URL's password replaced.
pub fn redacted(from: &str) -> String {
    match password_of(from) {
        Some(password) if is_url(from) => from.replacen(&format!(":{password}@"), ":…@", 1),
        _ => from.to_owned(),
    }
}

/// What an import that ran before refuses with.
const IMPORTED: &str = "has an import already";

/// `rookery server import`. With `once`, a database that had its import already is no failure, for
/// a container that imports on every start until it has.
pub fn import(from: &str, key_file: Option<&Path>, owner: Option<&str>, once: bool) -> Result<()> {
    let config = rookery_server::ServerConfig::from_vars(&|name| std::env::var(name).ok())
        .map_err(|e| Fail::usage(e.to_string()))?;
    let report = match import_into(
        &config.database_url,
        config.secret_key.clone(),
        from,
        key_file,
        owner,
    ) {
        Err(fail) if once && fail.message.contains(IMPORTED) => {
            out::note(fail.message);
            return Ok(());
        }
        done => done?,
    };
    for (table, count) in &report.tables {
        out::line(format!("{table}: {count}"));
    }
    for (why, count) in &report.skipped {
        out::line(format!("skipped {count}: {why}"));
    }
    out::done(format!(
        "Imported {}. The server token is not carried over: make an access token with rookery server token --user NAME",
        redacted(from)
    ));
    Ok(())
}

/// Imports the store at `from` into the database at `database_url`, sealing with `key`. With `owner`,
/// a person by name or email, each imported host is theirs.
pub fn import_into(
    database_url: &str,
    key: Option<rookery_store::seal::Key>,
    from: &str,
    key_file: Option<&Path>,
    owner: Option<&str>,
) -> Result<Report> {
    let old = match is_url(from) {
        true => postgres_rows(from)?,
        false => sqlite_rows(Path::new(from))?,
    };
    let keys = old_keys(key_file, key.as_ref())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Fail::failed(e.to_string()))?;
    runtime.block_on(write(database_url, key, keys, old, owner))
}

fn opened(keys: &Option<seal::Keys>, row: &StateRow) -> Result<Vec<u8>> {
    if !row.sealed {
        return Ok(row.value.clone());
    }
    let keys = keys.as_ref().ok_or_else(|| {
        Fail::refused(format!(
            "{} {:?} is sealed, and no key was given to open it: --key-file FILE, or the server's ROOKERY_SECRET_KEY",
            row.kind, row.name
        ))
    })?;
    let context = [row.kind.as_str(), row.scope.as_str(), row.name.as_str()].join("\0");
    keys.open(context.as_bytes(), &row.value).map_err(|e| {
        Fail::refused(format!(
            "{} {:?} does not open with the key given: {e}",
            row.kind, row.name
        ))
    })
}

async fn write(
    database_url: &str,
    key: Option<rookery_store::seal::Key>,
    keys: Option<seal::Keys>,
    old: Old,
    owner: Option<&str>,
) -> Result<Report> {
    let fail = |e: sqlx::Error| Fail::failed(format!("writing the database: {e}"));
    let store = rookery_store::Store::open(rookery_store::StoreOptions::new(database_url))
        .await
        .map_err(|e| Fail::failed(format!("opening the database: {e}")))?;
    // A person named to own the hosts who has not signed in yet is made, as an admin of a local
    // account, as rookery server token makes one.
    if let Some(wanted) = owner {
        let users = store
            .users()
            .list()
            .await
            .map_err(|e| Fail::failed(e.to_string()))?;
        let known = users.iter().any(|u| {
            u.name == wanted
                || u.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(wanted))
        });
        if !known {
            let deployment = store
                .state()
                .deployment_id()
                .await
                .map_err(|e| Fail::failed(e.to_string()))?;
            let login = rookery_store::users::Login {
                issuer: format!("rookery-local:{deployment}"),
                subject: rookery_store::util::random_token(""),
                name: wanted.to_owned(),
                email: wanted.contains('@').then(|| wanted.to_owned()),
                admin: true,
            };
            store
                .users()
                .upsert_login(&login)
                .await
                .map_err(|e| Fail::failed(e.to_string()))?;
        }
    }
    let mut tx = store.pool().begin().await.map_err(fail)?;
    // A server that leads would take what it does not know of the hosts as gone.
    let free: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(rookery_server::leader::LOCK_KEY)
        .fetch_one(&mut *tx)
        .await
        .map_err(fail)?;
    if !free {
        return Err(Fail::refused(
            "a rookery serve runs on this database: stop it, import, and start it again",
        ));
    }
    // One import, into a database that has no hosts and never had one.
    sqlx::query("LOCK TABLE rookery.hosts IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .map_err(fail)?;
    let imported: Option<String> =
        sqlx::query_scalar("SELECT value FROM rookery.server_state WHERE key = 'imported'")
            .fetch_optional(&mut *tx)
            .await
            .map_err(fail)?;
    if let Some(when) = imported {
        return Err(Fail::refused(format!(
            "This database {IMPORTED}, of {when}: the import runs once"
        )));
    }
    let hosts: i64 = sqlx::query_scalar("SELECT count(*) FROM rookery.hosts")
        .fetch_one(&mut *tx)
        .await
        .map_err(fail)?;
    if hosts > 0 {
        return Err(Fail::refused(
            "This database has hosts already: an import goes into an empty one",
        ));
    }
    let mut report = Report::default();
    // Profiles first: the rows of a host, such as its tailnet device, go beside its own.
    let mut rows: Vec<&StateRow> = old.state.iter().collect();
    rows.sort_by_key(|row| {
        (
            row.kind != kind::PROFILE,
            row.kind.clone(),
            row.name.clone(),
        )
    });
    let mut names = BTreeMap::new();
    for row in rows {
        if row.scope != store::OWN {
            report.skipped("rows of a scope of a server's clients");
            continue;
        }
        if let Some(why) = left_out(&row.kind) {
            report.skipped(why);
            continue;
        }
        let Some(table) = rookery_hosts::backend::table_of(&row.kind) else {
            report.skipped(&format!("state of the kind {}", row.kind));
            continue;
        };
        let plain = StateRow {
            value: opened(&keys, row)?,
            sealed: false,
            ..row.clone()
        };
        if row.kind == kind::PROFILE
            && let Some(label) = &row.label
        {
            names.insert(row.name.clone(), label.clone());
        }
        rookery_hosts::backend::import_state(&mut tx, key.clone(), &plain)
            .await
            .map_err(|e| Fail::failed(format!("{} {:?}: {e}", row.kind, row.name)))?;
        report.wrote(table);
    }
    for m in &old.messages {
        sqlx::query(
            "INSERT INTO rookery.host_messages (seq, host, id, topic, body, keep, from_level, from_uid, session, time, done)
             VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9, $10::timestamptz, $11)",
        )
        .bind(m.seq)
        .bind(&m.host)
        .bind(m.id)
        .bind(&m.topic)
        .bind(serde_json::from_str::<Value>(&m.body).unwrap_or(Value::Null).to_string())
        .bind(if m.keep == "last" { "last" } else { "log" })
        .bind(&m.from_level)
        .bind(m.from_uid)
        .bind(&m.session)
        .bind(&m.time)
        .bind(m.done)
        .execute(&mut *tx)
        .await
        .map_err(fail)?;
        report.wrote("host_messages");
    }
    if !old.messages.is_empty() {
        sqlx::query("SELECT setval(pg_get_serial_sequence('rookery.host_messages', 'seq'), (SELECT max(seq) FROM rookery.host_messages))")
            .execute(&mut *tx)
            .await
            .map_err(fail)?;
    }
    for (host, last) in &old.cursors {
        sqlx::query("INSERT INTO rookery.host_cursors (host, last_id) VALUES ($1, $2)")
            .bind(host)
            .bind(last)
            .execute(&mut *tx)
            .await
            .map_err(fail)?;
        report.wrote("host_cursors");
    }
    for (namespace, key, value, revision) in &old.kv {
        let value: Value = serde_json::from_str(value).unwrap_or(Value::String(value.clone()));
        sqlx::query(
            "INSERT INTO rookery.kv (plugin, scope, user_id, key, value, revision) VALUES ($1, 'global', NULL, $2, $3, $4)",
        )
        .bind(namespace)
        .bind(key)
        .bind(&value)
        .bind(revision)
        .execute(&mut *tx)
        .await
        .map_err(fail)?;
        report.wrote("kv");
    }
    let days = std::env::var("ROOKERY_RETENTION_DAYS")
        .ok()
        .and_then(|d| d.trim().parse::<i64>().ok())
        .unwrap_or(store::RETENTION_DAYS as i64);
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    for job in &old.jobs {
        let Some(started) = rfc3339(&job.started) else {
            report.skipped("jobs whose start cannot be read");
            continue;
        };
        if started < cutoff {
            report.skipped(&format!("jobs older than the retention of {days} days"));
            continue;
        }
        let Some(ended) = job.ended.as_deref().and_then(rfc3339) else {
            report.skipped("jobs that had not ended");
            continue;
        };
        let action = match job.action.is_empty() {
            true => job.what.clone(),
            false => job.action.clone(),
        };
        let (kind, subject) = match job.subject.split_once(':') {
            Some(("host", id)) => (
                "hosts.act",
                format!(
                    "env://{}",
                    names.get(id).cloned().unwrap_or_else(|| id.to_owned())
                ),
            ),
            Some(("provisioner", id)) => ("provisioners.act", format!("provisioner://{id}")),
            Some(("resource", id)) => ("resources.act", format!("resource://{id}")),
            _ => ("installation.act", "rookery://installation".to_owned()),
        };
        let host = job
            .host
            .split(", ")
            .map(|id| names.get(id).cloned().unwrap_or_else(|| id.to_owned()))
            .collect::<Vec<_>>()
            .join(", ");
        let arguments: Vec<String> = job
            .arguments
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default();
        let params = json!({"action": action, "subject": job.subject, "host": host, "arguments": arguments, "imported": job.id});
        let (state, result, error) = match (job.exit_code, &job.stopped_by) {
            (_, Some(by)) => (
                "cancelled",
                None,
                Some(json!({"code": "UNAVAILABLE", "message": format!("stopped by {by}")})),
            ),
            (Some(0), None) => ("succeeded", Some(json!({"exit": 0})), None),
            (code, None) => (
                "failed",
                None,
                Some(
                    json!({"code": "INTERNAL", "message": format!("{action} ended with exit code {}", code.unwrap_or(1)), "details": {"exit": code}}),
                ),
            ),
        };
        let id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO rookery.jobs (id, kind, params, state, attempts, max_attempts, subject, enqueued_by, result, error, created_at, started_at, finished_at)
             VALUES ($1, $2, $3, $4, 1, 1, $5, 'system:import', $6, $7, $8, $8, $9)",
        )
        .bind(id)
        .bind(kind)
        .bind(&params)
        .bind(state)
        .bind(&subject)
        .bind(&result)
        .bind(&error)
        .bind(started)
        .bind(ended)
        .execute(&mut *tx)
        .await
        .map_err(fail)?;
        report.wrote("jobs");
        match output_of(job, old.jobs_dir.as_deref()) {
            Some(output) => {
                for (seq, line) in output.lines().enumerate() {
                    sqlx::query(
                        "INSERT INTO rookery.log_items (job_id, seq, at, attempt, level, text) VALUES ($1, $2, $3, 1, $4, $5)",
                    )
                    .bind(id)
                    .bind(seq as i64 + 1)
                    .bind(ended)
                    .bind(if line.starts_with('✗') { "error" } else { "info" })
                    .bind(line)
                    .execute(&mut *tx)
                    .await
                    .map_err(fail)?;
                    report.wrote("log_items");
                }
            }
            None => report.skipped("job output files that are gone"),
        }
    }
    // Every host has an owner: the one named, or the server's one admin.
    let user: Option<uuid::Uuid> = match owner {
        Some(owner) => sqlx::query_scalar(
            "SELECT id FROM rookery.users WHERE name = $1 OR lower(email) = lower($1) LIMIT 1",
        )
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await
        .map_err(fail)?,
        None => {
            let admins: Vec<uuid::Uuid> = sqlx::query_scalar(
                "SELECT id FROM rookery.users WHERE role = 'admin' AND disabled_at IS NULL ORDER BY created_at LIMIT 2",
            )
            .fetch_all(&mut *tx)
            .await
            .map_err(fail)?;
            match admins.as_slice() {
                [one] => Some(*one),
                [] => {
                    return Err(Fail::usage(
                        "The server has no admin to own the hosts yet: sign the first one in with rookery server setup-key, or name the owner with --owner",
                    ));
                }
                _ => {
                    return Err(Fail::usage(
                        "The server has several admins: name the one who owns the imported hosts with --owner",
                    ));
                }
            }
        }
    };
    if let Some(user) = user {
        for name in names.values() {
            sqlx::query("INSERT INTO rookery.owners (resource, user_id) VALUES ($1, $2) ON CONFLICT (resource) DO UPDATE SET user_id = EXCLUDED.user_id")
                .bind(format!("env://{name}"))
                .bind(user)
                .execute(&mut *tx)
                .await
                .map_err(fail)?;
            report.wrote("owners");
        }
        sqlx::query("UPDATE rookery.hosts SET owner = $1")
            .bind(user)
            .execute(&mut *tx)
            .await
            .map_err(fail)?;
    } else {
        return Err(Fail::usage(format!(
            "No person {} on the server to own the hosts: sign them in first",
            owner.unwrap_or_default()
        )));
    }
    // The roles of the old server's access tokens, from an issuer's claim, become groups bound to
    // those claims, with the level each role had on every host.
    for (claim, level) in [
        ("rookery:view", 1i16),
        ("rookery:operate", 3),
        ("rookery:admin", 3),
    ] {
        let group: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO rookery.groups (id, name, claim) VALUES ($1, $2, $2)
             ON CONFLICT (claim) DO UPDATE SET claim = EXCLUDED.claim RETURNING id",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(claim)
        .fetch_one(&mut *tx)
        .await
        .map_err(fail)?;
        report.wrote("groups");
        for name in names.values() {
            sqlx::query(
                "INSERT INTO rookery.grants (resource, principal, level, created_by) VALUES ($1, $2, $3, 'import')
                 ON CONFLICT (resource, principal) DO NOTHING",
            )
            .bind(format!("env://{name}"))
            .bind(format!("group:{group}"))
            .bind(level)
            .execute(&mut *tx)
            .await
            .map_err(fail)?;
            report.wrote("grants");
        }
    }
    sqlx::query("INSERT INTO rookery.server_state (key, value) VALUES ('imported', $1)")
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(fail)?;
    tx.commit().await.map_err(fail)?;
    store.close().await;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rookery_store::testing::TestDb;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/store-0.6");

    /// A copy of the store a Rookery 0.6 wrote, with the jobs that are not the old one started now.
    fn copied() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("jobs")).unwrap();
        for entry in std::fs::read_dir(Path::new(FIXTURE).join("jobs")).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(
                entry.path(),
                dir.path().join("jobs").join(entry.file_name()),
            )
            .unwrap();
        }
        let file = dir.path().join("rookery.db");
        std::fs::copy(Path::new(FIXTURE).join("rookery.db"), &file).unwrap();
        let db = rusqlite::Connection::open(&file).unwrap();
        db.execute(
            "UPDATE jobs SET started = ?1, ended = ?1 WHERE id <> 'jancient00000001'",
            [rookery_api::now()],
        )
        .unwrap();
        (dir, file)
    }

    fn key() -> rookery_store::seal::Key {
        let text = std::fs::read_to_string(Path::new(FIXTURE).join("store.key")).unwrap();
        rookery_store::seal::Key::parse(&text).unwrap()
    }

    /// What the server reads of the import, through the same store its hosts use.
    fn read(url: &str) -> std::sync::Arc<rookery_core::store::Store> {
        rookery_hosts::attach(url, Some(key())).unwrap();
        rookery_core::store::shared(&rookery_core::paths::Paths {
            config_dir: "/nowhere/config".into(),
            data_dir: "/nowhere/data".into(),
        })
        .unwrap()
    }

    /// The server's first admin, who owns what comes in.
    async fn admin(db: &TestDb) -> uuid::Uuid {
        let id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO rookery.users (id, issuer, subject, name, role) VALUES ($1, 'rookery-local:test', 'ada', 'Ada', 'admin')",
        )
        .bind(id)
        .execute(db.store.pool())
        .await
        .unwrap();
        id
    }

    async fn check(db: &TestDb, report: &Report) {
        assert_eq!(report.tables["rookery.hosts"], 3, "{:?}", report.tables);
        assert_eq!(report.tables["rookery.jobs"], 2);
        assert_eq!(report.tables["rookery.log_items"], 6);
        assert!(
            report
                .skipped
                .keys()
                .any(|why| why.contains("server token"))
        );
        assert!(
            report
                .skipped
                .keys()
                .any(|why| why.contains("older than the retention"))
        );
        let pool = db.store.pool();
        let count = |sql: &'static str| async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        assert_eq!(
            count(
                "SELECT count(*) FROM rookery.hosts WHERE tailnet IS NOT NULL AND name = 'studio'"
            )
            .await,
            1
        );
        assert_eq!(count("SELECT count(*) FROM rookery.jobs WHERE subject = 'env://studio' AND state = 'succeeded'").await, 1);
        assert_eq!(
            count("SELECT count(*) FROM rookery.jobs WHERE state = 'failed'").await,
            1
        );
        assert_eq!(count("SELECT count(*) FROM rookery.host_messages").await, 2);
        // Every host is the admin's, and the old roles are groups of their claims.
        assert_eq!(
            count("SELECT count(*) FROM rookery.hosts WHERE owner IS NULL").await,
            0
        );
        assert_eq!(
            count("SELECT count(*) FROM rookery.owners WHERE starts_with(resource, 'env://')")
                .await,
            count("SELECT count(*) FROM rookery.hosts").await
        );
        assert_eq!(
            count("SELECT count(*) FROM rookery.groups WHERE claim IN ('rookery:view', 'rookery:operate', 'rookery:admin')").await,
            3
        );
        assert_eq!(
            count("SELECT count(*) FROM rookery.grants WHERE resource = 'env://studio' AND starts_with(principal, 'group:')").await,
            3
        );
        assert_eq!(
            count("SELECT count(*) FROM rookery.kv WHERE plugin = 'box' AND revision = 2").await,
            1
        );
        let url = db.url.clone();
        tokio::task::spawn_blocking(move || {
            use rookery_core::store::{OWN, kind};
            let s = read(&url);
            let secret = s
                .find(kind::SECRET, OWN, "tailnet_api_key")
                .unwrap()
                .unwrap();
            assert_eq!(
                s.read(kind::SECRET, OWN, &secret.id).unwrap().unwrap(),
                b"tskey-made-up"
            );
            let registry = s.find(kind::REGISTRY, OWN, "ghcr").unwrap().unwrap();
            let login = format!("{}/login", registry.id);
            assert_eq!(
                s.read(kind::SECRET, OWN, &login).unwrap().unwrap(),
                b"made-up-token"
            );
            let ring = s.find(kind::KEY, OWN, "rookery").unwrap().unwrap();
            assert!(
                s.read(kind::KEY, OWN, &ring.id)
                    .unwrap()
                    .unwrap()
                    .starts_with(b"-----BEGIN")
            );
            let host = s.find(kind::PROFILE, OWN, "studio").unwrap().unwrap();
            assert!(s.read(kind::CREDENTIALS, OWN, &host.id).unwrap().is_some());
            assert_eq!(
                s.read(kind::SETTING, OWN, "tailnet_user").unwrap().unwrap(),
                b"agents"
            );
            assert_eq!(
                s.labels(kind::PROVISIONER_ENTRY, OWN).unwrap()[0].name,
                "lab"
            );
            rookery_core::store::set_shared(None);
            rookery_core::runner::set_locks(None);
        })
        .await
        .unwrap();
    }

    #[test]
    fn a_password_never_shows() {
        let url = "postgres://rookery:hunter-made-up@db.example.org/rookery?sslmode=require";
        assert_eq!(
            redacted(url),
            "postgres://rookery:…@db.example.org/rookery?sslmode=require"
        );
        assert!(store_of(Some(url), None).is_err());
        assert_eq!(
            store_of(Some("postgres://rookery@db.example.org/rookery"), None).unwrap(),
            "postgres://rookery@db.example.org/rookery"
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("store-url");
        std::fs::write(&file, format!("\n{url}\n")).unwrap();
        assert_eq!(store_of(None, Some(&file)).unwrap(), url);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_store_of_before_comes_in_once_and_is_left_as_it_was() {
        let Some(db) = TestDb::new().await else {
            eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
            return;
        };
        let (_dir, file) = copied();
        let before = std::fs::read(&file).unwrap();
        admin(&db).await;
        let (url, path) = (db.url.clone(), file.to_string_lossy().into_owned());
        let report =
            tokio::task::spawn_blocking(move || import_into(&url, Some(key()), &path, None, None))
                .await
                .unwrap()
                .unwrap();
        check(&db, &report).await;
        assert_eq!(
            std::fs::read(&file).unwrap(),
            before,
            "the old store was written"
        );
        // A second run refuses.
        let (url, path) = (db.url.clone(), file.to_string_lossy().into_owned());
        let again =
            tokio::task::spawn_blocking(move || import_into(&url, Some(key()), &path, None, None))
                .await
                .unwrap();
        assert!(again.err().unwrap().message.contains("import already"));
        db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_store_on_postgres_comes_in_too() {
        let Some(old) = TestDb::new().await else {
            eprintln!("skipped: ROOKERY_TEST_POSTGRES is not set");
            return;
        };
        let db = TestDb::new().await.unwrap();
        admin(&db).await;
        let (dir, file) = copied();
        // The old tables, filled with the rows of the SQLite store.
        let mut conn = old.store.pool().acquire().await.unwrap();
        let pool = &mut *conn;
        sqlx::raw_sql("SET search_path TO public")
            .execute(&mut *pool)
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            std::fs::read_to_string(Path::new(FIXTURE).join("postgres.sql")).unwrap(),
        ))
        .execute(&mut *pool)
        .await
        .unwrap();
        let rows = sqlite_rows(&file).unwrap();
        for r in &rows.state {
            sqlx::query("INSERT INTO state (kind, scope, name, value, sealed, updated, label) VALUES ($1, $2, $3, $4, $5, $6, $7)")
                .bind(&r.kind).bind(&r.scope).bind(&r.name).bind(&r.value).bind(r.sealed).bind(&r.updated).bind(&r.label)
                .execute(&mut *pool).await.unwrap();
        }
        for m in &rows.messages {
            sqlx::query("INSERT INTO messages (seq, host, id, topic, body, keep, from_level, from_uid, session, time, done) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)")
                .bind(m.seq).bind(&m.host).bind(m.id).bind(&m.topic).bind(&m.body).bind(&m.keep).bind(&m.from_level).bind(m.from_uid).bind(&m.session).bind(&m.time).bind(m.done)
                .execute(&mut *pool).await.unwrap();
        }
        for (host, last) in &rows.cursors {
            sqlx::query("INSERT INTO cursors (host, last_id) VALUES ($1, $2)")
                .bind(host)
                .bind(last)
                .execute(&mut *pool)
                .await
                .unwrap();
        }
        for (namespace, key, value, revision) in &rows.kv {
            sqlx::query("INSERT INTO kv (namespace, key, value, revision, updated) VALUES ($1, $2, $3, $4, '')")
                .bind(namespace).bind(key).bind(value).bind(revision).execute(&mut *pool).await.unwrap();
        }
        for j in &rows.jobs {
            // The output files are where the old server's machine kept them.
            let output = dir
                .path()
                .join("jobs")
                .join(Path::new(&j.output_path).file_name().unwrap());
            sqlx::query("INSERT INTO jobs (id, host, what, started, ended, exit_code, output_path, subject, action, stopped_by, arguments) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)")
                .bind(&j.id).bind(&j.host).bind(&j.what).bind(&j.started).bind(&j.ended).bind(j.exit_code)
                .bind(output.to_string_lossy().into_owned()).bind(&j.subject).bind(&j.action).bind(&j.stopped_by).bind(&j.arguments)
                .execute(&mut *pool).await.unwrap();
        }
        drop(conn);
        let (url, from) = (db.url.clone(), old.url.clone());
        let report =
            tokio::task::spawn_blocking(move || import_into(&url, Some(key()), &from, None, None))
                .await
                .unwrap()
                .unwrap();
        check(&db, &report).await;
        db.close().await;
        old.close().await;
    }
}
