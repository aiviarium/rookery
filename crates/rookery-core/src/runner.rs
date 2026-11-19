//! What a job of Rookery's is made of, wherever it runs: its subject and
//! the lock it holds while an action that changes something runs, so two of
//! those do not overlap; the work, which is a function of rookery (applying
//! a host's profile, a roll-out, a provisioning), a program (ansible, or a
//! plugin's provisioner), or an action of a host's controller, which runs as
//! a session there; how it is stopped; and its output with secrets masked.
//! rookery-hosts runs it as a job of the server.

use crate::{Result, paths::Paths, profiles::Place};
use rookery_api::ops::Role;
use serde_json::Value;
use std::{
    cell::RefCell,
    fs,
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

// ── Stopping a job ──────────────────────────────────────────────────────────

/// How long a stopped job's programs have to end after SIGTERM before
/// they are killed.
const GRACE: Duration = Duration::from_secs(10);

/// What stops a running job: the process groups its programs run in, and
/// who asked it to stop.
#[derive(Default)]
pub struct Stop {
    by: Mutex<Option<String>>,
    groups: Mutex<Vec<u32>>,
}

impl Stop {
    /// Who asked the job to stop, once someone has.
    pub fn asked(&self) -> Option<String> {
        self.by.lock().unwrap().clone()
    }

    fn signal(&self, signal: i32) {
        for group in self.groups.lock().unwrap().iter() {
            // SAFETY: a signal to a process group of this job's, by number.
            unsafe {
                libc::kill(-(*group as i32), signal);
            }
        }
    }

    /// Ask for the stop: its programs get SIGTERM, and SIGKILL when they
    /// outlast the grace. Says whether this was the first to ask.
    fn request(self: &Arc<Self>, by: &str) -> bool {
        {
            let mut asked = self.by.lock().unwrap();
            if asked.is_some() {
                return false;
            }
            *asked = Some(by.to_owned());
        }
        self.signal(libc::SIGTERM);
        let stop = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(GRACE);
            stop.signal(libc::SIGKILL);
        });
        true
    }
}

thread_local! {
    /// The stop of the job this thread runs, when it runs one.
    static CURRENT: RefCell<Option<Arc<Stop>>> = const { RefCell::new(None) };
}

/// Run `work` on this thread as the work of a job that `stop` stops: its
/// programs get process groups of their own and end when the stop is asked.
pub fn with_stop<T>(stop: Arc<Stop>, work: impl FnOnce() -> T) -> T {
    let before = CURRENT.with(|current| current.borrow_mut().replace(stop));
    let done = work();
    CURRENT.with(|current| *current.borrow_mut() = before);
    done
}

impl Stop {
    /// Ask the job to stop, as `by`: its programs get SIGTERM, and SIGKILL
    /// when they outlast the grace. Says whether this was the first to ask.
    pub fn ask(self: &Arc<Self>, by: &str) -> bool {
        self.request(by)
    }
}

/// Who asked the job this thread runs to stop, for work that looks
/// between its steps.
pub fn stopped() -> Option<String> {
    CURRENT.with(|current| current.borrow().as_ref().and_then(|stop| stop.asked()))
}

/// Give a program a process group of its own when this thread runs a job,
/// so that stopping the job ends it with what it started. A program of a
/// rookery that keeps no jobs stays in the terminal's group.
pub fn own_group(command: &mut Command) {
    if CURRENT.with(|current| current.borrow().is_some()) {
        command.process_group(0);
    }
}

/// A program of a job, ended with the job when it is stopped, for as long
/// as this is held.
pub struct Group(Option<(Arc<Stop>, u32)>);

impl Drop for Group {
    fn drop(&mut self) {
        if let Some((stop, pid)) = self.0.take() {
            stop.groups.lock().unwrap().retain(|known| *known != pid);
        }
    }
}

/// Have a program started with [`own_group`] stopped with the job this
/// thread runs. One started after the stop was asked for is ended at once.
pub fn watch(child: &Child) -> Group {
    let Some(stop) = CURRENT.with(|current| current.borrow().clone()) else {
        return Group(None);
    };
    let pid = child.id();
    stop.groups.lock().unwrap().push(pid);
    if stop.asked().is_some() {
        // SAFETY: a signal to the process group of the program just started.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    Group(Some((stop, pid)))
}

/// What came of asking a job to stop.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopping {
    /// It is being stopped now.
    Asked,
    /// Someone asked before; it is ending.
    AskedBefore(String),
    /// It has ended already.
    Ended,
    /// No job of that id runs in this process: it ran elsewhere or never was.
    NotHere,
}

/// What a job is done to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Host(String),
    /// A provisioner by its name.
    Provisioner(String),
    Resource(String),
    /// The installation itself, as for a roll-out over its hosts.
    Installation,
}

impl Subject {
    /// `host:NAME`, `provisioner:NAME`, `resource:ID` or `installation`.
    pub fn key(&self) -> String {
        match self {
            Subject::Host(name) => format!("host:{name}"),
            Subject::Provisioner(id) => format!("provisioner:{id}"),
            Subject::Resource(id) => format!("resource:{id}"),
            Subject::Installation => "installation".into(),
        }
    }

    pub fn parse(key: &str) -> Option<Subject> {
        if key == "installation" {
            return Some(Subject::Installation);
        }
        let (kind, name) = key.split_once(':')?;
        if name.is_empty() {
            return None;
        }
        match kind {
            "host" => Some(Subject::Host(name.into())),
            "provisioner" => Some(Subject::Provisioner(name.into())),
            "resource" => Some(Subject::Resource(name.into())),
            _ => None,
        }
    }

    /// The subject with the id of the host, provisioner or resource it
    /// names by name or id. A host in a file stays its path, and a
    /// provisioner of a type no provisioner is registered for its type.
    pub fn by_id(&self, paths: &Paths) -> Subject {
        match self {
            Subject::Host(given) => Subject::Host(host_key(paths, given)),
            Subject::Provisioner(given) => Subject::Provisioner(
                crate::provisioner::reference(paths, given).unwrap_or_else(|_| given.clone()),
            ),
            Subject::Resource(given) => Subject::Resource(
                rookery_api::ids::id_in(rookery_api::ids::Kind::Resource, given)
                    .unwrap_or_else(|| given.clone()),
            ),
            Subject::Installation => Subject::Installation,
        }
    }

    /// The subject as people call it: the name of its host or provisioner
    /// now, or its id when it has none any more.
    pub fn named(&self, paths: &Paths) -> Subject {
        let store = crate::store::shared(paths).ok();
        let label = |kind: &str, id: &str| {
            store
                .as_ref()
                .and_then(|store| store.label_of(kind, crate::store::OWN, id).ok().flatten())
                .unwrap_or_else(|| id.to_owned())
        };
        match self {
            Subject::Host(id) => Subject::Host(label(crate::store::kind::PROFILE, id)),
            Subject::Provisioner(id) => {
                Subject::Provisioner(label(crate::store::kind::PROVISIONER_ENTRY, id))
            }
            other => other.clone(),
        }
    }

    /// The file whose lock is held while something changes it. A host's is
    /// the one its machine's provisioning holds, so that the two exclude
    /// each other too.
    fn lock_file(&self, paths: &Paths) -> Result<PathBuf> {
        let file = |kind: &str, name: &str| {
            let plain = name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(ch))
                && !name.starts_with('.');
            let name = match plain {
                true => name.to_owned(),
                false => {
                    use sha2::{Digest, Sha256};
                    let hash = Sha256::digest(name.as_bytes());
                    hash.iter()
                        .take(8)
                        .map(|byte| format!("{byte:02x}"))
                        .collect()
                }
            };
            paths
                .data_dir
                .join("locks")
                .join(format!("{kind}.{name}.lock"))
        };
        Ok(match self {
            Subject::Host(name) => Place::resolve(paths, name)?.lock_file(paths),
            Subject::Provisioner(id) => file("provisioner", &id.replace('/', ".")),
            Subject::Resource(id) => file("resource", id),
            Subject::Installation => paths.data_dir.join("locks").join("installation.lock"),
        })
    }
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Subject::Host(name) => write!(f, "{name}"),
            Subject::Provisioner(id) => write!(f, "the provisioner {id}"),
            Subject::Resource(id) => write!(f, "the resource {id}"),
            Subject::Installation => write!(f, "this installation"),
        }
    }
}

/// A job's hosts, as `studio, lab`, by their ids.
pub fn hosts_by_id(paths: &Paths, hosts: &str) -> String {
    each_host(hosts, |host| host_key(paths, host))
}

/// The id of a saved host a name or id gives, or what was given: the path
/// of a profile in a file, or a host that is not saved.
fn host_key(paths: &Paths, given: &str) -> String {
    if given.contains('/') || given.ends_with(".json") {
        return Place::resolve(paths, given).map_or_else(|_| given.to_owned(), |place| place.key());
    }
    Place::find(paths, &crate::profiles::scope(paths), given)
        .ok()
        .flatten()
        .map_or_else(|| given.to_owned(), |place| place.key())
}

/// A job's hosts, kept by their ids, by their names now.
pub fn hosts_named(paths: &Paths, hosts: &str) -> String {
    let store = crate::store::shared(paths).ok();
    each_host(hosts, |host| {
        store
            .as_ref()
            .and_then(|store| {
                store
                    .label_of(crate::store::kind::PROFILE, crate::store::OWN, host)
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| host.to_owned())
    })
}

fn each_host(hosts: &str, map: impl Fn(&str) -> String) -> String {
    if hosts.trim().is_empty() {
        return String::new();
    }
    hosts
        .split(", ")
        .map(|host| map(host.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

// ── Locks ───────────────────────────────────────────────────────────────────

/// Held while something changes a subject. A second caller is refused rather
/// than kept waiting.
pub struct Lock(Held, Option<Arc<Lapse>>);

/// How a lock that lapses while it is held says so: the work that holds it stops, as another may
/// take the lock and change the subject meanwhile.
#[derive(Default)]
pub struct Lapse {
    stop: Mutex<Option<Arc<Stop>>>,
    lost: Mutex<Option<String>>,
}

impl Lapse {
    /// A lapse that stops the job this thread runs, when it runs one.
    fn here() -> Arc<Lapse> {
        let lapse = Lapse::default();
        *lapse.stop.lock().unwrap() = CURRENT.with(|current| current.borrow().clone());
        Arc::new(lapse)
    }

    /// The lock is gone, for `why`: the work that holds it is asked to stop.
    pub fn lost(&self, why: &str) {
        *self.lost.lock().unwrap() = Some(why.to_owned());
        if let Some(stop) = self.stop.lock().unwrap().as_ref() {
            stop.ask(why);
        }
    }
}

impl Lock {
    /// Stop `stop`'s job when this lock lapses, as for one taken before the job's work began.
    pub fn stops(&self, stop: Arc<Stop>) {
        if let Some(lapse) = &self.1 {
            if let Some(why) = lapse.lost.lock().unwrap().clone() {
                stop.ask(&why);
            }
            *lapse.stop.lock().unwrap() = Some(stop);
        }
    }
}

enum Held {
    File(fs::File),
    /// One a [`Locks`] gave, let go when it is dropped.
    Other(#[allow(dead_code)] Box<dyn Send>),
}

impl Drop for Lock {
    /// Unlocked rather than only closed: a process started meanwhile may hold
    /// a copy of the descriptor until it runs its program, and with it the lock.
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        if let Held::File(file) = &self.0 {
            // SAFETY: the descriptor is the open file's own.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

/// What takes the locks of subjects in place of files, such as the leases of `rookery serve` on
/// Postgres, which hold across the servers of one database.
pub trait Locks: Send + Sync {
    /// Take the lock named `name` for what `doing` names, or say who holds it. When the lock is
    /// lost while it is held, the one that took it tells `lapse`.
    fn take(
        &self,
        name: &str,
        doing: &str,
        lapse: Arc<Lapse>,
    ) -> std::result::Result<Box<dyn Send>, Holder>;
}

/// Why a lock was refused.
pub enum Holder {
    /// What the one who holds it is doing, which may be unknown.
    Held(String),
    Failed(String),
}

static LOCKS: std::sync::RwLock<Option<Arc<dyn Locks>>> = std::sync::RwLock::new(None);

/// Take every lock through `locks` from now on, or through files again.
pub fn set_locks(locks: Option<Arc<dyn Locks>>) {
    *LOCKS.write().unwrap_or_else(|e| e.into_inner()) = locks;
}

/// The name a lock file has among [`Locks`]: its path below the data directory's `locks`, or the
/// whole path.
fn lock_name(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.rfind("/locks/") {
        Some(at) => text[at + 1..].trim_end_matches(".lock").to_owned(),
        None => text.into_owned(),
    }
}

/// Take the lock at `path` for what `doing` names, which is written into the
/// file so that a caller who is refused can be told who holds it. `refused`
/// makes the refusal from that, with ` (…)` around it or empty.
pub fn lock_at(path: &Path, doing: &str, refused: impl FnOnce(&str) -> String) -> Result<Lock> {
    let locks = LOCKS.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(locks) = locks {
        let lapse = Lapse::here();
        return match locks.take(&lock_name(path), doing, lapse.clone()) {
            Ok(held) => Ok(Lock(Held::Other(held), Some(lapse))),
            Err(Holder::Held(holder)) => {
                let said = match holder.trim().is_empty() {
                    true => String::new(),
                    false => format!(" ({})", holder.trim()),
                };
                Err(refused(&said).into())
            }
            Err(Holder::Failed(error)) => Err(error.into()),
        };
    }
    if let Some(dir) = path.parent() {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    use std::os::fd::AsRawFd;
    // SAFETY: the descriptor is the open file's own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let mut holder = String::new();
        let _ = file.read_to_string(&mut holder);
        let holder = holder.trim();
        let said = match holder.is_empty() {
            true => String::new(),
            false => format!(" ({holder})"),
        };
        return Err(refused(&said).into());
    }
    // Only the holder writes, so what is there is the one holding it.
    file.set_len(0)?;
    let _ = writeln!(file, "{doing}, process {}", std::process::id());
    Ok(Lock(Held::File(file), None))
}

/// Take the subject's lock for what `doing` names, or say what else changes
/// it now.
pub fn lock(paths: &Paths, subject: &Subject, doing: &str) -> Result<Lock> {
    // By id, so that a rename does not give the subject a second lock.
    lock_at(&subject.by_id(paths).lock_file(paths)?, doing, |holder| {
        format!("Something else is being done to {subject}{holder}. Try again when it has ended")
    })
}

// ── Masking ─────────────────────────────────────────────────────────────────

/// The texts that must not be shown: each secret, and each longer line of
/// one, as a key or a certificate is split over lines. The longest first, so
/// a part does not leave the rest of a whole.
pub fn hidden<'a>(secrets: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut texts = vec![];
    for value in secrets {
        texts.push(value.to_owned());
        texts.extend(
            value
                .lines()
                .map(str::trim)
                .filter(|line| line.len() >= 8)
                .map(str::to_owned),
        );
    }
    texts.retain(|text| !text.is_empty());
    texts.sort_by_key(|text| std::cmp::Reverse(text.len()));
    texts.dedup();
    texts
}

/// A line with each hidden text replaced.
pub fn mask(line: &[u8], hidden: &[String]) -> Vec<u8> {
    let mut line = line.to_vec();
    for text in hidden {
        let text = text.as_bytes();
        let mut out = Vec::with_capacity(line.len());
        let mut rest = &line[..];
        while let Some(at) = rest.windows(text.len()).position(|window| window == text) {
            out.extend_from_slice(&rest[..at]);
            out.extend_from_slice(b"[secret]");
            rest = &rest[at + text.len()..];
        }
        out.extend_from_slice(rest);
        line = out;
    }
    line
}

/// Output on its way to a log or a terminal, with the hidden texts masked.
/// With nothing to hide it passes on as it comes; otherwise it passes on a
/// line at a time, since a secret may arrive in two pieces.
pub struct Output<'a> {
    inner: &'a mut dyn Write,
    hidden: Vec<String>,
    pending: Vec<u8>,
    log: Option<PathBuf>,
}

impl<'a> Output<'a> {
    pub fn new(inner: &'a mut dyn Write, hidden: Vec<String>) -> Output<'a> {
        Output {
            inner,
            hidden,
            pending: vec![],
            log: None,
        }
    }

    /// Where the job's output is kept, when it is kept.
    pub fn log(&self) -> Option<&Path> {
        self.log.as_deref()
    }

    /// Hide these texts too from here on, for work that learns of secrets
    /// while it runs, such as a provisioner it prepares then.
    pub fn hide<'s>(&mut self, secrets: impl IntoIterator<Item = &'s str>) {
        let mut all = std::mem::take(&mut self.hidden);
        all.extend(hidden(secrets));
        all.sort_by_key(|text| std::cmp::Reverse(text.len()));
        all.dedup();
        self.hidden = all;
    }

    fn pass_lines(&mut self) -> std::io::Result<()> {
        while let Some(end) = self
            .pending
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r')
        {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            self.inner.write_all(&mask(&line, &self.hidden))?;
        }
        Ok(())
    }
}

impl Write for Output<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.hidden.is_empty() {
            return self.inner.write(data);
        }
        self.pending.extend_from_slice(data);
        self.pass_lines()?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.pass_lines()?;
        self.inner.flush()
    }
}

impl Drop for Output<'_> {
    fn drop(&mut self) {
        let rest = std::mem::take(&mut self.pending);
        let _ = self.inner.write_all(&mask(&rest, &self.hidden));
        let _ = self.inner.flush();
    }
}

// ── What runs ───────────────────────────────────────────────────────────────

/// A host's controller as a job asks it, waiting for each answer. The
/// server's link and a test's double are each one.
pub trait Controller: Send {
    fn get(&mut self, path: &str) -> std::result::Result<Value, String>;
    /// End a session on the host, for a job that is stopped.
    fn stop(&mut self, _id: &str) -> std::result::Result<(), String> {
        Err("this controller cannot stop sessions".into())
    }
}

/// A program and what it is given: its standard output and error are the
/// job's output, its exit code the job's.
pub struct Program {
    pub command: Command,
    /// Written to its standard input, which is otherwise empty.
    pub input: Option<Vec<u8>>,
    /// Private files it reads, removed once it has ended.
    pub files: Vec<tempfile::NamedTempFile>,
}

pub enum Work {
    /// A function of rookery: its output goes to the writer, and it says
    /// whether it went well.
    Function(Box<dyn FnOnce(&mut Output) -> bool + Send>),
    /// A function of rookery that ends with an exit code of its own, such as
    /// that of a command it ran.
    Coded(Box<dyn FnOnce(&mut Output) -> i32 + Send>),
    Program(Box<Program>),
    /// A session an action of a host's controller runs, by its id there,
    /// followed until it ends.
    Session {
        controller: Box<dyn Controller>,
        id: String,
    },
}

/// What a job is and how it may run.
pub struct Request {
    pub subject: Subject,
    pub action: String,
    /// The hosts it is about, for a reader.
    pub host: String,
    pub token: Option<String>,
    /// Whether it changes the subject: then it holds the subject's lock.
    pub changes: bool,
    /// The subject's lock, taken already by what prepared the work.
    pub held: Option<Lock>,
    /// Texts that never reach the output, such as a password the work is given.
    pub secrets: Vec<String>,
    /// The role that may start it, which may also stop it.
    pub role: Role,
    /// What it was started with that its record shows, such as the command
    /// of `exec`. Made with [`rookery_api::Argument::shown`] for an action's
    /// arguments, so that a secret one never gets here.
    pub arguments: Vec<String>,
}

impl Request {
    pub fn new(subject: Subject, action: &str) -> Request {
        let host = match &subject {
            Subject::Host(name) => name.clone(),
            _ => String::new(),
        };
        Request {
            subject,
            action: action.to_owned(),
            host,
            token: None,
            changes: true,
            held: None,
            secrets: vec![],
            role: Role::Admin,
            arguments: vec![],
        }
    }
}

/// Run a program to its end with its output to `out`: its exit code. A
/// function of rookery that runs one as a step uses this too.
pub fn run_program(program: Program, out: &mut Output) -> Option<i32> {
    let Program {
        mut command,
        input,
        files,
    } = program;
    let fail = |out: &mut Output, error: String| {
        let _ = writeln!(out, "✗ {error}");
        None
    };
    let (mut reader, writer) = match std::io::pipe() {
        Ok(pipe) => pipe,
        Err(error) => return fail(out, error.to_string()),
    };
    let copy = match writer.try_clone() {
        Ok(copy) => copy,
        Err(error) => return fail(out, error.to_string()),
    };
    command
        .stdin(match input {
            Some(_) => Stdio::piped(),
            None => Stdio::null(),
        })
        .stdout(writer)
        .stderr(copy);
    own_group(&mut command);
    let program_name = command.get_program().to_string_lossy().into_owned();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return fail(out, format!("{program_name}: {error}")),
    };
    let _group = watch(&child);
    // The command holds the pipe's other ends; they close when it is dropped.
    drop(command);
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        // One that does not read its input must not hold this up.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let mut buffer = [0; 8192];
    while let Ok(count) = reader.read(&mut buffer)
        && count > 0
    {
        let _ = out.write_all(&buffer[..count]);
        let _ = out.flush();
    }
    let status = child.wait().ok();
    drop(files);
    status.and_then(|status| status.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        (dir, paths)
    }

    #[test]
    fn subjects_are_keys_and_back() {
        for subject in [
            Subject::Host("studio".into()),
            Subject::Provisioner("incus/vm".into()),
            Subject::Resource("r-1".into()),
            Subject::Installation,
        ] {
            assert_eq!(Subject::parse(&subject.key()), Some(subject));
        }
        assert_eq!(Subject::parse("host:"), None);
        assert_eq!(Subject::parse("planet:x"), None);
    }

    #[test]
    fn a_secret_is_masked_also_in_two_pieces() {
        let hidden = hidden(["hunter2-longer", "a\nline of a key\n"]);
        let mut kept = vec![];
        {
            let mut out = Output::new(&mut kept, hidden);
            out.write_all(b"one hunter2-").unwrap();
            out.write_all(b"longer two\nline of a key and the rest")
                .unwrap();
        }
        assert_eq!(
            String::from_utf8(kept).unwrap(),
            "one [secret] two\n[secret] and the rest"
        );
        // Nothing to hide passes on as it comes, a progress meter included.
        let mut plain = vec![];
        Output::new(&mut plain, vec![]).write_all(b"50%\r").unwrap();
        assert_eq!(plain, b"50%\r");
    }

    #[test]
    fn a_refused_lock_says_who_holds_it() {
        let (_dir, paths) = paths();
        let held = lock(&paths, &Subject::Installation, "rollout").unwrap();
        let refused = lock(&paths, &Subject::Installation, "rollout")
            .err()
            .unwrap();
        let said = refused.to_string();
        assert!(
            said.contains(&format!("(rollout, process {})", std::process::id())),
            "{said}"
        );
        drop(held);
        let again = lock(&paths, &Subject::Installation, "apply").unwrap();
        drop(again);
    }

    #[test]
    fn a_program_runs_with_its_input_and_says_its_exit_code() {
        let mut command = Command::new("sh");
        command.args(["-c", "read line; echo \"got $line\"; exit 3"]);
        let program = Program {
            command,
            input: Some(b"hunter2\n".to_vec()),
            files: vec![],
        };
        let mut kept = vec![];
        let code = {
            let mut out = Output::new(&mut kept, hidden(["hunter2"]));
            run_program(program, &mut out)
        };
        assert_eq!(code, Some(3));
        assert_eq!(String::from_utf8(kept).unwrap(), "got [secret]\n");
    }
}
