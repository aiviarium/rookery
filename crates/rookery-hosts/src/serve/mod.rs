//! `rookery serve`: one API over all hosts, with the store. It reaches each
//! host through a link, as the UI does, and adds nothing to a host's own API.

pub mod actions;
pub mod api;
pub mod auth;
pub mod keys;
pub mod provisioners;
pub mod resources;
pub mod setup;

use futures_util::StreamExt;
use rookery_api::{Message, Version, client};
use rookery_core::{
    host::Host,
    link::{Link, LinkSpec},
    paths::Paths,
    store::{self, Store, Stored},
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::broadcast;

/// How a host's answer starts when rookery logged in and found no rookeryd.
pub(crate) const NO_ROOKERYD: &str = "no rookeryd on the host yet";

/// How long an answer about a host's reachability is used again.
const SEEN_FOR: Duration = Duration::from_secs(10);

/// Something that happened, for whoever listens.
#[derive(Debug, Clone)]
pub enum Event {
    /// A message from a host's channel, kept or not.
    Channel { host: String, message: Message },
    /// A message that was stored.
    Inbox(Stored),
    /// A job started or ended.
    Job(Value),
    Kv {
        namespace: String,
        key: String,
        value: Option<Value>,
        revision: i64,
    },
    /// A saved host was added, removed or renamed, or its profile changed.
    Host {
        name: String,
        change: &'static str,
        /// Its name before, when it was renamed.
        from: Option<String>,
    },
    /// A resource was created, removed, or its record changed.
    Resource {
        id: String,
        /// The id of its provisioner, or a type no provisioner is registered of.
        provisioner: String,
        /// The provisioner's name.
        provisioner_name: String,
        kind: String,
        change: &'static str,
    },
}

/// What the store held of the hosts and resources when last looked at, so a
/// change to it can be told as an event whoever made it.
struct Watch {
    paths: Paths,
    events: broadcast::Sender<Event>,
    seen: Mutex<Seen>,
}

/// Unknown until the store could first be read, which tells nothing.
#[derive(Default)]
struct Seen {
    hosts: Option<BTreeMap<String, Value>>,
    resources: Option<BTreeMap<String, rookery_core::resource::Record>>,
}

/// How each key of `now` differs from `before`: added, removed or changed.
fn differences<'a, T: PartialEq>(
    before: &'a BTreeMap<String, T>,
    now: &'a BTreeMap<String, T>,
) -> Vec<(&'a str, &'static str)> {
    let gone = before
        .keys()
        .filter(|name| !now.contains_key(*name))
        .map(|name| (name.as_str(), "removed"));
    let came = now
        .iter()
        .filter_map(|(name, value)| match before.get(name) {
            None => Some((name.as_str(), "added")),
            Some(was) if was != value => Some((name.as_str(), "changed")),
            Some(_) => None,
        });
    gone.chain(came).collect()
}

impl Watch {
    fn hosts(&self) -> Option<BTreeMap<String, Value>> {
        rookery_core::profiles::list(&self.paths, store::OWN)
            .ok()
            .map(|saved| {
                saved
                    .into_iter()
                    .map(|(place, profile)| (place.argument(), profile))
                    .collect()
            })
    }

    fn resources(&self) -> Option<BTreeMap<String, rookery_core::resource::Record>> {
        let kept = rookery_core::resource::list(&self.paths).ok()?;
        Some(
            kept.into_iter()
                .map(|kept| {
                    // When it was last looked at is no change of it.
                    let record = rookery_core::resource::Record {
                        checked: String::new(),
                        ..kept.record
                    };
                    (record.id.clone(), record)
                })
                .collect(),
        )
    }

    /// Tell what changed in the store since it was last looked at.
    fn notice(&self) {
        let (hosts, resources) = (self.hosts(), self.resources());
        // Held while telling, so two callers cannot tell a change twice or out of order.
        let mut seen = self.seen.lock().unwrap();
        if let Some(now) = hosts {
            if let Some(before) = &seen.hosts {
                for (name, change) in differences(before, &now) {
                    let _ = self.events.send(Event::Host {
                        name: name.to_owned(),
                        change,
                        from: None,
                    });
                }
            }
            seen.hosts = Some(now);
        }
        if let Some(now) = resources {
            if let Some(before) = &seen.resources {
                for (id, change) in differences(before, &now) {
                    let record = now.get(id).or_else(|| before.get(id));
                    let change = match change {
                        "added" => "created",
                        other => other,
                    };
                    let provisioner = record.map(|r| r.provisioner.clone()).unwrap_or_default();
                    let _ = self.events.send(Event::Resource {
                        id: id.to_owned(),
                        provisioner_name: rookery_core::provisioner::label(
                            &self.paths,
                            &provisioner,
                        ),
                        provisioner,
                        kind: record.map(|r| r.kind.clone()).unwrap_or_default(),
                        change,
                    });
                }
            }
            seen.resources = Some(now);
        }
    }

    /// Tell a rename as one, and not as a host removed and another added.
    fn renamed(&self, old: &str, new: &str) {
        let mut seen = self.seen.lock().unwrap();
        if let Some(hosts) = seen.hosts.as_mut()
            && let Some(profile) = hosts.remove(old)
        {
            hosts.insert(new.to_owned(), profile);
        }
        let _ = self.events.send(Event::Host {
            name: new.to_owned(),
            change: "renamed",
            from: Some(old.to_owned()),
        });
    }
}

/// A host as `rookery serve` knows it.
pub struct Remote {
    pub name: String,
    /// The saved host's id, which its messages are kept by; empty for one
    /// that is not saved.
    pub id: String,
    pub address: String,
    pub link: Option<Link>,
    /// The SSH user's sudo password, for a host that asks for one.
    password: Option<String>,
    /// Why there is no link.
    pub problem: Option<String>,
    /// The saved profile it was taken up from, by which a change to the store
    /// from another process is noticed.
    profile: Option<Value>,
    seen: Mutex<Option<(Instant, std::result::Result<Version, String>)>>,
}

impl Remote {
    /// What its messages and channel cursor are kept by: its id, or its
    /// name when it has none.
    pub fn key(&self) -> &str {
        match self.id.is_empty() {
            true => &self.name,
            false => &self.id,
        }
    }

    pub fn new(name: &str, address: &str, spec: LinkSpec) -> Remote {
        Remote {
            name: name.to_owned(),
            id: String::new(),
            address: address.to_owned(),
            link: Some(Link::new(spec)),
            password: None,
            problem: None,
            profile: None,
            seen: Mutex::new(None),
        }
    }

    /// The host's version, asked for at most every few seconds.
    pub async fn version(&self) -> std::result::Result<Version, String> {
        let Some(link) = &self.link else {
            return Err(self.problem.clone().unwrap_or_else(|| "no link".into()));
        };
        // A simulated host changes within a step, which tests wait for.
        let fresh = match rookery_core::simulate::active() {
            Some(_) => rookery_core::simulate::delay().min(SEEN_FOR),
            None => SEEN_FOR,
        };
        if let Some((when, answer)) = &*self.seen.lock().unwrap()
            && when.elapsed() < fresh
        {
            return answer.clone();
        }
        // A host that refused the login a short while ago is not asked by
        // the list: each refused login counts against the server's address
        // at the host's sshd.
        if let Some(refused) = link.refused() {
            return Err(refused);
        }
        let asked = tokio::time::timeout(Duration::from_secs(15), link.version()).await;
        let answer = match asked {
            Ok(Ok(version)) => Ok(version),
            Ok(Err(error)) if Link::missing(&error) => {
                // The shared connection may have outlived a host key that changed since.
                let spec = link.spec().clone();
                match tokio::task::spawn_blocking(move || spec.changed_host_key()).await {
                    Ok(Some(changed)) => Err(changed),
                    _ => Err(format!("{NO_ROOKERYD}: {error}")),
                }
            }
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("no answer in time".into()),
        };
        *self.seen.lock().unwrap() = Some((Instant::now(), answer.clone()));
        answer
    }
}

pub struct Serve {
    pub paths: Paths,
    pub store: Arc<Store>,
    pub hosts: RwLock<BTreeMap<String, Arc<Remote>>>,
    pub events: broadcast::Sender<Event>,
    /// What runs the jobs, as jobs of the server.
    pub runner: crate::runner::Runner,
    /// The tasks that take in the hosts' channels, by host.
    followers: Mutex<BTreeMap<String, tokio::task::AbortHandle>>,
    /// The sudo passwords given at the start, which outlive a changed profile.
    passwords: Mutex<BTreeMap<String, String>>,
    /// How long provisioners may take and how a new machine is waited for,
    /// which tests make quick.
    pub provision: fn() -> rookery_core::provision::Options,
    /// Whether the hosts' channels are taken in, which only `rookery serve`
    /// does, holding its lock.
    pub follows: bool,
    /// The known-hosts file of the user the server runs as, which a first
    /// login from a caller on this machine takes a host's key from. Tests
    /// leave it out or name one of their own.
    pub known_hosts: Mutex<Option<PathBuf>>,
    watch: Arc<Watch>,
}

impl Serve {
    pub fn with(
        paths: Paths,
        store: Arc<Store>,
        runner: crate::runner::Runner,
        provision: fn() -> rookery_core::provision::Options,
    ) -> Arc<Serve> {
        let events = broadcast::channel(1024).0;
        let watch = Arc::new(Watch {
            paths: paths.clone(),
            events: events.clone(),
            seen: Mutex::new(Seen::default()),
        });
        watch.notice();
        Arc::new(Serve {
            paths,
            store,
            hosts: RwLock::new(BTreeMap::new()),
            events,
            runner,
            followers: Mutex::new(BTreeMap::new()),
            passwords: Mutex::new(BTreeMap::new()),
            provision,
            follows: true,
            known_hosts: Mutex::new(None),
            watch,
        })
    }

    /// Lets go of the links, as the server stops leading.
    pub fn close(&self) {
        let names: Vec<String> = self.hosts.read().unwrap().keys().cloned().collect();
        for name in names {
            self.forget(&name);
        }
    }

    /// The controller of a host through its link, for a job that waits on it from a thread of its
    /// own. Called inside the runtime.
    pub fn controller_of(&self, name: &str) -> Option<Box<dyn rookery_core::runner::Controller>> {
        let remote = self.host(name)?;
        remote.link.as_ref()?;
        Some(Box::new(actions::Through {
            remote,
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// [`Serve::refresh`] off the runtime's threads, as it reads the store.
    pub async fn refresh_now(self: &Arc<Self>, start: bool) {
        let state = self.clone();
        let handle = tokio::runtime::Handle::current();
        let _ = tokio::task::spawn_blocking(move || {
            let _entered = handle.enter();
            state.refresh(start)
        })
        .await;
    }

    /// Tell whoever follows changes what changed among the saved hosts and
    /// the resources since this was last asked.
    pub fn notice(&self) {
        self.watch.notice();
    }

    pub fn host(&self, name: &str) -> Option<Arc<Remote>> {
        self.hosts.read().unwrap().get(name).cloned()
    }

    /// Add a host and start taking in its channel.
    pub fn add(self: &Arc<Self>, remote: Remote) -> Arc<Remote> {
        let remote = Arc::new(remote);
        self.hosts
            .write()
            .unwrap()
            .insert(remote.name.clone(), remote.clone());
        if remote.link.is_some() && self.follows {
            let follower = tokio::spawn(take_in(self.clone(), remote.clone()));
            let before = self
                .followers
                .lock()
                .unwrap()
                .insert(remote.name.clone(), follower.abort_handle());
            if let Some(before) = before {
                before.abort();
            }
        }
        remote
    }

    /// Drop what is known of a host, as after its profile changed or went.
    /// [`Serve::refresh`] takes it up again from its profile.
    pub fn forget(&self, name: &str) {
        self.hosts.write().unwrap().remove(name);
        if let Some(follower) = self.followers.lock().unwrap().remove(name) {
            follower.abort();
        }
    }

    /// Drop what is known of a host that was given another name, and keep
    /// the sudo password it was given at the start for its new one. The next
    /// [`Serve::refresh`] takes it up under that name.
    pub fn renamed(&self, old: &str, new: &str) {
        self.forget(old);
        self.watch.renamed(old, new);
        // Its grants, owner and jobs go with the name, as env://NAME names it.
        let (from, to) = (crate::uri(old), crate::uri(new));
        let kernel = self.runner.kernel().clone();
        let moving = async move {
            if let Err(e) = kernel.store.access().move_resource(&from, &to).await {
                tracing::warn!(error = %e, %from, %to, "moving a host's grants failed");
            }
            if let Err(e) = kernel.store.jobs().move_subject(&from, &to).await {
                tracing::warn!(error = %e, %from, %to, "moving a host's jobs failed");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                // A thread of a job waits for it; one inside the runtime does not.
                if tokio::runtime::Handle::current().runtime_flavor()
                    == tokio::runtime::RuntimeFlavor::CurrentThread
                {
                    handle.spawn(moving);
                } else {
                    tokio::task::block_in_place(|| handle.block_on(moving));
                }
            }
            Err(_) => tracing::warn!("a host was renamed outside the runtime: its grants stay"),
        }
        let mut passwords = self.passwords.lock().unwrap();
        if let Some(password) = passwords.remove(old) {
            passwords.insert(new.to_owned(), password);
        }
    }

    /// Take up saved hosts that are not known yet: the server's own, whatever
    /// its settings say. A host whose profile went or changed meanwhile, as by
    /// a rookery command beside the server, is dropped or taken up again. Only at
    /// the start may a password be asked for on the terminal.
    pub fn refresh(self: &Arc<Self>, start: bool) {
        // A store that cannot be read says nothing of what was removed.
        let Ok(saved) = rookery_core::profiles::list(&self.paths, store::OWN) else {
            return;
        };
        self.notice();
        let stale: Vec<String> = {
            let hosts = self.hosts.read().unwrap();
            hosts
                .values()
                .filter(|remote| {
                    remote.profile.as_ref().is_some_and(|taken| {
                        !saved.iter().any(|(place, config)| {
                            place.argument() == remote.name && config == taken
                        })
                    })
                })
                .map(|remote| remote.name.clone())
                .collect()
        };
        for name in stale {
            self.forget(&name);
        }
        for (place, config) in saved {
            let name = place.argument();
            if self.host(&name).is_some() {
                continue;
            }
            let host = Host {
                name,
                place,
                config,
            };
            // A link that is a command of its own goes through no sudo of ours.
            let asks = host.asks_password() && host.access().link.is_none();
            let password = match (start, asks) {
                (_, false) => Ok(None),
                (true, true) => host
                    .sudo_password(&self.paths)
                    .map_err(|error| error.to_string()),
                (false, true) => self
                    .passwords
                    .lock()
                    .unwrap()
                    .get(&host.name)
                    .cloned()
                    .or_else(|| host.kept_sudo_password(&self.paths))
                    .map(Some)
                    .ok_or_else(|| {
                        format!(
                            "{name}'s SSH user's sudo asks for a password, which this server was not given. Keep it with rookery secret set sudo/{name}, or give it with the action that needs it",
                            name = host.name
                        )
                    }),
            };
            if let Ok(Some(password)) = &password {
                let mut passwords = self.passwords.lock().unwrap();
                passwords.insert(host.name.clone(), password.clone());
            }
            let reachable = host.access().reachable();
            let link = password.clone().and_then(|password| match reachable {
                true => host
                    .link(&self.paths, password.as_deref())
                    .map_err(|error| error.to_string()),
                false => Err("its profile has no SSH address yet".into()),
            });
            let remote = match link {
                Ok(spec) => Remote::new(&host.name, &host.address(), spec),
                Err(problem) => Remote {
                    name: host.name.clone(),
                    id: String::new(),
                    address: host.address(),
                    link: None,
                    password: None,
                    problem: Some(problem),
                    profile: None,
                    seen: Mutex::new(None),
                },
            };
            self.add(Remote {
                id: host.place.key(),
                password: password.ok().flatten(),
                profile: Some(host.config.clone()),
                ..remote
            });
        }
    }

    /// A message from a host: store what is kept, by the host's id, and tell
    /// whoever listens.
    fn accept(&self, host: &str, message: Message) {
        let key = api::host_key(&self.paths, host);
        let taken = self.store.take_in(&key, &message);
        let _ = self.events.send(Event::Channel {
            host: host.to_owned(),
            message,
        });
        match taken {
            Ok(Some(stored)) => {
                let _ = self.events.send(Event::Inbox(stored));
            }
            Ok(None) => {}
            Err(error) => eprintln!("rookery serve: storing a message of {host}: {error}"),
        }
    }

    /// Drop the messages older than the retention time: `ROOKERY_RETENTION_DAYS`, the setting
    /// `retention_days`, or 90 days. Says the days, for the jobs to be dropped by too.
    pub fn prune(&self) -> u64 {
        let days = std::env::var("ROOKERY_RETENTION_DAYS")
            .ok()
            .or_else(|| self.paths.setting("retention_days"))
            .and_then(|days| days.trim().parse().ok())
            .unwrap_or(store::RETENTION_DAYS);
        let _ = self.store.prune(days);
        days
    }
}

/// Follow a host's channel from where it was read up to, for as long as we run.
async fn take_in(state: Arc<Serve>, remote: Arc<Remote>) {
    let Some(link) = &remote.link else {
        return;
    };
    let mut wait = 1;
    loop {
        // A refused login is not tried again by itself until its back-off
        // has passed or someone logged in with it; the store says which.
        if link.refused().is_some() {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        let since = state.store.cursor(remote.key()).unwrap_or(0);
        if let Ok(mut stream) = link.stream(&format!("/v1/channel?since={since}")).await {
            wait = 1;
            while let Some(Ok(frame)) = stream.next().await {
                if let client::Frame::Text(text) = frame
                    && let Ok(message) = serde_json::from_str::<Message>(text.as_str())
                {
                    state.accept(&remote.name, message);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
        wait = (wait * 2).min(60);
    }
}

/// Frames both ways between a host's stream and a caller's, until either side closes: the host's
/// text frames as lines and its binary frames as they are, the caller's bytes as binary frames.
pub async fn pipe(
    upstream: rookery_core::link::LinkStream,
    stream: rookery_kernel::streams::Stream,
) {
    use futures_util::SinkExt;
    use rookery_kernel::streams::Inbound;
    let (mut reader, writer) = stream.split();
    let (mut to_host, mut from_host) = upstream.split();
    let towards_host = async {
        while let Some(inbound) = reader.read().await {
            match inbound {
                Inbound::Data(data) => {
                    if to_host
                        .send(client::Frame::Binary(data.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Inbound::Eof | Inbound::Closed(_) => break,
            }
        }
    };
    let towards_caller = async {
        while let Some(Ok(frame)) = from_host.next().await {
            let sent = match frame {
                client::Frame::Text(text) => {
                    let mut line = text.as_str().as_bytes().to_vec();
                    line.push(b'\n');
                    writer.write(&line).await
                }
                client::Frame::Binary(data) => writer.write(&data).await,
                client::Frame::Close(_) => break,
                _ => Ok(()),
            };
            if sent.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        () = towards_host => {}
        () = towards_caller => {}
    }
    let _ = to_host.send(client::Frame::Close(None)).await;
    writer.eof();
    writer.close(None);
}
