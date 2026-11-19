use rookery_api::{Config, HostAction, Level, ServiceSpec, Topic};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Root, as a service.
    System,
    /// Everything as the user who started it.
    Single,
}

/// Who is on the other end of a connection, as the system reports it.
#[derive(Debug, Clone, Copy)]
pub struct Caller {
    pub level: Level,
    pub uid: u32,
    pub pid: Option<i32>,
}

impl Caller {
    pub fn host(&self) -> bool {
        self.level == Level::Host
    }
}

/// The four files the role writes.
#[derive(Debug, Default)]
pub struct Loaded {
    pub rookeryd: Config,
    pub actions: Vec<HostAction>,
    pub services: Vec<ServiceSpec>,
    pub topics: Vec<Topic>,
}

fn read<T: serde::de::DeserializeOwned + Default>(
    dir: &Path,
    name: &str,
    problems: &mut Vec<String>,
) -> T {
    let path = dir.join(name);
    match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data).unwrap_or_else(|error| {
            problems.push(format!("{}: {error}", path.display()));
            T::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(error) => {
            problems.push(format!("{}: {error}", path.display()));
            T::default()
        }
    }
}

impl Loaded {
    /// Read the files; a missing one is empty. Problems are returned, not fatal.
    pub fn read(dir: &Path) -> (Loaded, Vec<String>) {
        let mut problems = vec![];
        let loaded = Loaded {
            rookeryd: read(dir, "rookeryd.json", &mut problems),
            actions: read(dir, "actions.json", &mut problems),
            services: read(dir, "services.json", &mut problems),
            topics: read(dir, "channels.json", &mut problems),
        };
        (loaded, problems)
    }
}

pub struct State {
    pub mode: Mode,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    loaded: RwLock<Arc<Loaded>>,
    pub sessions: crate::sessions::Sessions,
    pub channel: crate::channel::Channel,
    pub services: crate::services::Services,
}

impl State {
    pub fn new(mode: Mode, config_dir: PathBuf, state_dir: PathBuf) -> std::io::Result<Arc<State>> {
        let (loaded, problems) = Loaded::read(&config_dir);
        for problem in problems {
            eprintln!("rookeryd: {problem}");
        }
        let channel = crate::channel::Channel::open(&state_dir.join("channel"), &loaded.topics)?;
        let sessions = crate::sessions::Sessions::open(&state_dir.join("sessions"))?;
        Ok(Arc::new(State {
            mode,
            config_dir,
            state_dir,
            loaded: RwLock::new(Arc::new(loaded)),
            sessions,
            channel,
            services: Default::default(),
        }))
    }

    pub fn config(&self) -> Arc<Loaded> {
        self.loaded.read().unwrap().clone()
    }

    /// Read the configuration again. Sessions are not touched.
    pub fn reload(&self) -> Vec<String> {
        let (loaded, problems) = Loaded::read(&self.config_dir);
        self.channel.set_topics(&loaded.topics);
        *self.loaded.write().unwrap() = Arc::new(loaded);
        problems
    }

    /// Whether the calls that go to systemd work here.
    pub fn systemd(&self) -> bool {
        self.config()
            .rookeryd
            .systemd
            .unwrap_or(self.mode == Mode::System)
    }

    /// The folders the file calls may touch: the configuration's, or the agent's
    /// home on a system host with none.
    pub fn roots(&self) -> Vec<String> {
        let config = self.config();
        let mut roots = config.rookeryd.roots.clone();
        if roots.is_empty()
            && self.mode == Mode::System
            && let Some(agent) = crate::users::account(&config.rookeryd.agent)
        {
            roots.push(agent.home);
        }
        roots
    }

    /// Where checkouts go: the configured folder, else the first root with a
    /// `workspaces` folder, else the first root's. `None` without a root.
    pub fn workspaces(&self) -> Option<String> {
        let config = self.config();
        let configured = config.rookeryd.workspaces.trim_end_matches('/');
        if !configured.is_empty() {
            return Some(configured.to_owned());
        }
        let roots = self.roots();
        let first = roots.first()?.trim_end_matches('/');
        let existing = roots
            .iter()
            .map(|root| format!("{}/workspaces", root.trim_end_matches('/')))
            .find(|path| std::path::Path::new(path).is_dir());
        Some(existing.unwrap_or_else(|| format!("{first}/workspaces")))
    }
}
