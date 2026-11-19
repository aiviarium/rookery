//! Plugins: Hatchery packages with a `rookery` target. The package's
//! `package.kdl` names it and the Rookery versions it works with, and the
//! target's `plugin.json` says what it brings: here, its host features and
//! provisioners. A target that still has a `rookery.json` is read as a
//! `plugin.json` with those alone. Each contributes features to
//! the groups of the Features page, and actions to the overview of a host.
//! The role does the installing from the same manifest. A plugin can also
//! bring provisioners, programs that create the machine a profile describes;
//! rookery runs those itself (see [`crate::provision`]). Installing,
//! updating and removing them is in [`crate::eggs`].

use rookery_api::{
    AppliedPlugin, PluginAction as ListedAction, PluginFeature, PluginInfo, PluginOrigin,
    PluginService, ProvisionerInfo,
};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub use rookery_api::plugin_contract::{LEGACY_MANIFEST, MANIFEST, TARGET, valid_name};

/// The host features and provisioners of a target's folder, in the shape of
/// `rookery.json`, from whichever manifest it has.
pub fn host_parts(dir: &Path) -> Option<Value> {
    rookery_api::plugin_contract::read_dir(dir)
        .ok()
        .map(|read| read.host_parts())
}

pub struct PluginAction {
    pub name: String,
    pub label: String,
    pub help: &'static str,
    /// Shell command the host runs. rookery does not run it from here: the
    /// role writes it into the host's list of actions.
    pub run: String,
    /// `agent`, `operator` or `root`.
    pub user: String,
    /// Runs in a terminal, because it asks something or shows a link.
    pub terminal: bool,
    /// The group of the overview it is listed in; the plugin's own when empty.
    pub group: String,
    /// Actions of rookery it is part of, such as `status`. It runs after them and
    /// has no row of its own.
    pub extends: Vec<String>,
    /// The agent may run it with `box run`.
    pub agent: bool,
    /// What it asks for before it runs.
    pub arguments: Vec<rookery_api::Argument>,
    /// A sentence to say yes to before it runs.
    pub confirm: Option<String>,
}

/// One thing a plugin can install, switched on its own.
pub struct Feature {
    pub id: String,
    /// The group of the Features page it is listed in.
    pub group: String,
    pub label: String,
    pub help: &'static str,
    pub actions: Vec<PluginAction>,
    /// Programs it keeps running on the host to answer calls; names without
    /// the plugin's id.
    pub services: Vec<rookery_api::ServiceSpec>,
    /// Defaults and limits of its channel topics; topics without the plugin's id.
    pub channels: Vec<rookery_api::Topic>,
}

/// What a feature says of the hosts it is for and what it gives them,
/// beside what [`Feature`] holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosting {
    pub feature: String,
    /// The types of host it is for: a workstation unless the manifest names
    /// others, such as `node` for what a server needs.
    pub types: Vec<rookery_api::HostType>,
    /// The ids of the plugin's provisioners it may offer once it is applied.
    pub offers: Vec<String>,
    /// The rookery secrets of a client certificate and its key that the host
    /// is to trust, which rookery makes when it has none.
    pub trusts: Option<(String, String)>,
}

/// What each feature of a plugin says of the hosts it is for, read from its
/// manifest.
pub fn hosting(plugin: &Plugin) -> Vec<Hosting> {
    let Some(data) = host_parts(&plugin.dir) else {
        return vec![];
    };
    data["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|feature| {
            let id = feature["id"].as_str().filter(|id| valid_name(id))?;
            let strings = |value: &Value| -> Vec<String> {
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            };
            Some(Hosting {
                feature: id.to_owned(),
                types: match feature["types"].as_array() {
                    Some(_) => strings(&feature["types"])
                        .iter()
                        .filter_map(|name| rookery_api::HostType::parse(name))
                        .collect(),
                    None => vec![rookery_api::HostType::Workstation],
                },
                offers: strings(&feature["offers"])
                    .into_iter()
                    .filter(|id| valid_name(id))
                    .collect(),
                trusts: match (
                    feature["trusts"]["certificate"].as_str(),
                    feature["trusts"]["key"].as_str(),
                ) {
                    (Some(certificate), Some(key))
                        if valid_name(certificate) && valid_name(key) =>
                    {
                        Some((certificate.to_owned(), key.to_owned()))
                    }
                    _ => None,
                },
            })
        })
        .collect()
}

/// How a machine a provisioner created becomes the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// It boots with rookery's cloud seed and sets itself up.
    Seed,
    /// rookery applies the profile over SSH once it answers.
    Apply,
}

impl Setup {
    pub fn name(self) -> &'static str {
        match self {
            Setup::Seed => "seed",
            Setup::Apply => "apply",
        }
    }
}

/// A rookery secret a provisioner is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionerSecret {
    pub name: String,
    /// Without a value it cannot run; one that is not required is passed when set.
    pub required: bool,
}

/// The version of the provisioner protocol rookery speaks; a provisioner
/// declares it as `protocol` in its manifest.
pub const PROTOCOL: u64 = 2;

/// A program of a plugin that creates machines, and maybe other kinds of
/// things, and has actions of its own.
#[derive(Debug, Clone)]
pub struct Provisioner {
    pub id: String,
    pub label: String,
    pub description: String,
    /// The executable, resolved and inside the package.
    pub command: PathBuf,
    /// How a machine it makes is set up; `Apply` for one that makes none.
    pub setup: Setup,
    /// The plugin puts the release archive into the machine, so the seed
    /// needs no `release_url`.
    pub archive: bool,
    /// Of `start`, `stop` and `list`, those it answers for machines.
    pub verbs: Vec<String>,
    pub secrets: Vec<ProvisionerSecret>,
    /// What creating a machine takes: its own arguments, then its
    /// machines'. Values are strings; `choices` is not used.
    pub arguments: Vec<rookery_api::Argument>,
    /// The names of its own arguments, which say where it is.
    pub own: Vec<String>,
    /// What it can make, the machine kind among them when it makes machines.
    pub kinds: Vec<rookery_api::ProvisionerKind>,
    /// Its own actions, each run with the verb `act`.
    pub actions: Vec<rookery_api::Action>,
    /// What it holds that can be listed, with the actions of their items.
    pub collections: Vec<rookery_api::Collection>,
    /// Why rookery does not run it, as a sentence that says what to change.
    pub refused: Option<String>,
}

/// The verbs a provisioner may answer besides `create`, `status` and `destroy`.
pub const OPTIONAL_VERBS: &[&str] = &["start", "stop", "list"];

/// The verbs a kind may answer besides `create`, `status` and `destroy`.
pub const KIND_VERBS: &[&str] = &["start", "stop", "change", "logs", "exec"];

/// The kind of the machines a provisioner makes for hosts.
pub const MACHINE: &str = "machine";

impl Provisioner {
    /// What is said of it, as `PLUGIN/ID`, with the required secrets that
    /// have no value.
    pub fn info(&self, plugin: &str, missing_secrets: Vec<String>) -> ProvisionerInfo {
        ProvisionerInfo {
            id: format!("{plugin}/{}", self.id),
            name: format!("{plugin}/{}", self.id),
            kind: format!("{plugin}/{}", self.id),
            label: self.label.clone(),
            description: self.description.clone(),
            setup: self.setup.name().into(),
            verbs: self.verbs.clone(),
            secrets: self
                .secrets
                .iter()
                .map(|secret| secret.name.clone())
                .collect(),
            missing_secrets,
            arguments: self.arguments.clone(),
            own: self.own.clone(),
            settings: Default::default(),
            node: String::new(),
            node_id: String::new(),
            kinds: self.kinds.clone(),
            actions: self.actions.clone(),
            collections: self.collections.clone(),
            problem: self.refused.clone().unwrap_or_default(),
            state: String::new(),
            message: String::new(),
        }
    }

    /// Its machine kind, when it makes machines for hosts.
    pub fn machine(&self) -> Option<&rookery_api::ProvisionerKind> {
        self.kinds.iter().find(|kind| kind.id == MACHINE)
    }
}

pub struct Plugin {
    pub id: String,
    pub label: String,
    /// Its package, `GROUP/ID`.
    pub package: String,
    pub version: String,
    pub description: String,
    /// The folder of its `rookery` target, which holds `rookery.json` and
    /// what it names.
    pub dir: PathBuf,
    /// The package's folder, with its `package.kdl`.
    pub package_dir: PathBuf,
    /// Where it comes from.
    pub origin: PluginOrigin,
    /// The registry, repository or file it was installed from.
    pub source: String,
    /// Why Rookery does not use it. Such a plugin has no features or
    /// provisioners, and [`load`] leaves it out.
    pub problem: Option<String>,
    pub features: Vec<Feature>,
    pub provisioners: Vec<Provisioner>,
    /// The interface module the manifest names, relative to its target folder. The extension host
    /// writes `interface.json` from it.
    pub interface: Option<String>,
}

/// The plugins found in the given directories, without those Rookery does
/// not use. A later directory replaces a plugin of the same id from an
/// earlier one.
pub fn load(dirs: &[PathBuf]) -> Vec<Plugin> {
    scan(dirs)
        .into_iter()
        .filter(|plugin| plugin.problem.is_none())
        .collect()
}

/// Every plugin package locked in the given install folders, with the
/// reason for each that Rookery does not use. A package without a
/// `rookery` target is no plugin.
pub fn scan(dirs: &[PathBuf]) -> Vec<Plugin> {
    let mut plugins: Vec<Plugin> = vec![];
    for dir in dirs {
        // The package that holds each id in this folder: another package of the same id here is
        // not used, as a plugin's settings, secrets and data are kept by its id.
        let mut held: std::collections::HashMap<String, String> = Default::default();
        for (package, origin, source) in packages(dir) {
            if let Some(mut plugin) = read(&package) {
                plugin.origin = origin;
                plugin.source = source;
                match held.get(&plugin.id) {
                    Some(holder) if *holder != plugin.package => {
                        plugin.problem = Some(format!(
                            "the plugin id {} is held by the package {holder}",
                            plugin.id
                        ));
                    }
                    _ => {
                        held.insert(plugin.id.clone(), plugin.package.clone());
                    }
                }
                // One that cannot be used leaves a usable one of the same id.
                if plugin.problem.is_some()
                    && plugins
                        .iter()
                        .any(|known| known.id == plugin.id && known.problem.is_none())
                {
                    continue;
                }
                plugins.retain(|known| known.id != plugin.id);
                plugins.push(plugin);
            }
        }
    }
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    plugins
}

/// The package folders an install folder locks, with where each comes from.
fn packages(dir: &Path) -> Vec<(PathBuf, PluginOrigin, String)> {
    if !dir.join(hatchery::lock::FILE).is_file() {
        return vec![];
    }
    let Ok(lock) = hatchery::Lock::load(dir) else {
        return vec![];
    };
    lock.packages
        .iter()
        .map(|locked| {
            let (origin, source) = crate::eggs::origin(locked);
            (dir.join(locked.name.dir()), origin, source)
        })
        .collect()
}

/// Why a package's `rookery` target cannot be installed here, or None. Its
/// `requires` block has to name the Rookery versions it works with.
pub fn unmet(target: &hatchery::TargetSpec) -> Option<String> {
    if !target.requires.iter().any(|line| line.fact == TARGET) {
        return Some(
            "its package.kdl does not say which Rookery it works with: its rookery target needs a line such as rookery \">=0.6\" in requires".into(),
        );
    }
    let (line, found) = hatchery::target::unmet(
        &rookery_api::plugin_contract::RookeryTarget,
        &target.requires,
    )?;
    Some(match (line.fact.as_str(), found) {
        (TARGET, _) => format!(
            "it works with Rookery {}, and this is {}",
            line.values.join(" or "),
            rookery_api::VERSION
        ),
        (fact, Some(found)) => format!(
            "it needs {fact} {}, and this machine has {found}",
            line.values.join(" or ")
        ),
        (fact, None) => format!("it needs {fact}, which Rookery does not have"),
    })
}

/// The plugin of a package folder, or None when it is no package with a
/// `rookery` target. One Rookery cannot use has a problem.
pub fn read(package: &Path) -> Option<Plugin> {
    let text = fs::read_to_string(package.join(hatchery::manifest::FILE)).ok()?;
    let manifest = hatchery::Manifest::parse(&text).ok()?;
    let target = manifest.target(TARGET)?;
    let dir = package.join(target.dir());
    let real = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    let id = manifest.package.id.clone();
    let label = manifest.about.name.clone().unwrap_or_else(|| id.clone());
    let mut plugin = Plugin {
        id: id.clone(),
        label: label.clone(),
        package: manifest.name().to_string(),
        version: manifest.package.version.to_string(),
        description: manifest.about.summary.clone().unwrap_or_default(),
        dir: real.clone(),
        package_dir: package.to_owned(),
        origin: PluginOrigin::File,
        source: String::new(),
        problem: None,
        features: vec![],
        provisioners: vec![],
        interface: None,
    };
    // `sign-in:NAME` are rookery's own sign-ins.
    plugin.problem = if !rookery_api::plugin_contract::valid_plugin_id(&id) {
        Some(format!(
            "{id} is no plugin id: ids are lower-case letters, digits, - and _, start with a letter, and are neither rookery nor sign-in"
        ))
    } else if target
        .manifest
        .as_deref()
        .is_some_and(|name| name != MANIFEST && name != LEGACY_MANIFEST)
    {
        Some(format!(
            "its rookery target names {} as its manifest, and Rookery reads {MANIFEST}",
            target.manifest.as_deref().unwrap_or("")
        ))
    } else {
        unmet(target)
    };
    let rookery = match rookery_api::plugin_contract::read_dir(&real) {
        Ok(read) => {
            plugin.interface = read.interface().map(str::to_owned);
            read.host_parts()
        }
        Err(problem) => {
            plugin.problem.get_or_insert_with(|| match &problem.path {
                Some(path) => format!("{}/{path}: {}", target.dir(), problem.message),
                None => problem.message.clone(),
            });
            return Some(plugin);
        }
    };
    if plugin.problem.is_some() {
        return Some(plugin);
    }
    let rookery = &rookery;
    let text = |value: &Value, key: &str| value[key].as_str().unwrap_or("").to_owned();
    let or = |first: String, second: &str| {
        if first.is_empty() {
            second.to_owned()
        } else {
            first
        }
    };
    // Help texts live as long as the program; there are only a handful.
    let keep = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    let list = |value: &Value| value.as_array().cloned().unwrap_or_default();
    let features = list(&rookery["features"])
        .iter()
        .filter(|feature| valid_name(&text(feature, "id")))
        .map(|feature| Feature {
            id: text(feature, "id"),
            group: or(text(feature, "group"), &label),
            label: or(text(feature, "label"), &text(feature, "id")),
            help: keep(text(feature, "description")),
            actions: list(&feature["actions"])
                .iter()
                .filter(|action| {
                    valid_name(&text(action, "name")) && !text(action, "run").is_empty()
                })
                .map(|action| PluginAction {
                    name: text(action, "name"),
                    label: or(text(action, "label"), &text(action, "name")),
                    help: keep(text(action, "help")),
                    run: text(action, "run"),
                    user: match action["user"].as_str() {
                        Some(user @ ("operator" | "root")) => user.to_owned(),
                        _ => "agent".to_owned(),
                    },
                    terminal: action["terminal"].as_bool().unwrap_or(false),
                    group: text(action, "group"),
                    extends: match &action["extends"] {
                        Value::String(name) => vec![name.clone()],
                        names => list(names)
                            .iter()
                            .filter_map(|name| name.as_str().map(str::to_owned))
                            .collect(),
                    },
                    agent: action["agent"].as_bool().unwrap_or(false),
                    arguments: list(&action["arguments"])
                        .into_iter()
                        .filter_map(|argument| {
                            serde_json::from_value::<rookery_api::Argument>(argument).ok()
                        })
                        .filter(|argument| valid_name(&argument.name))
                        // Only a provisioner's arguments size a machine or are a
                        // list of values; another action offers them with `choices`.
                        .map(|argument| rookery_api::Argument {
                            kind: String::new(),
                            values: vec![],
                            ..argument
                        })
                        .collect(),
                    confirm: action["confirm"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned),
                })
                .collect(),
            services: list(&feature["services"])
                .into_iter()
                .filter_map(|service| {
                    serde_json::from_value::<rookery_api::ServiceSpec>(service).ok()
                })
                .filter(|service| valid_name(&service.name))
                .collect(),
            channels: list(&feature["channels"])
                .into_iter()
                .filter_map(|topic| serde_json::from_value::<rookery_api::Topic>(topic).ok())
                .collect(),
        })
        .collect();
    let provisioners = list(&rookery["provisioners"])
        .iter()
        .filter_map(|provisioner| read_provisioner(&id, provisioner, &real))
        .collect();
    plugin.features = features;
    plugin.provisioners = provisioners;
    Some(plugin)
}

/// The arguments of a provisioner's manifest that rookery uses. A kind rookery
/// does not know makes an ordinary argument, and `choices` is not run: a
/// provisioner offers values with its status instead.
fn provisioner_arguments(entries: &Value) -> Vec<rookery_api::Argument> {
    entries
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|argument| serde_json::from_value::<rookery_api::Argument>(argument).ok())
        .filter(|argument| valid_name(&argument.name))
        .map(|argument| rookery_api::Argument {
            choices: String::new(),
            kind: match argument.kind.as_str() {
                rookery_api::Argument::RESOURCE
                | rookery_api::Argument::TEXT
                | rookery_api::Argument::BOOLEAN => argument.kind.clone(),
                _ => String::new(),
            },
            ..argument
        })
        .collect()
}

/// The kinds a provisioner of protocol 2 makes. A machine kind needs a set-up.
fn provisioner_kinds(entries: &Value) -> Vec<rookery_api::ProvisionerKind> {
    let mut kinds: Vec<rookery_api::ProvisionerKind> = vec![];
    for entry in entries.as_array().cloned().unwrap_or_default() {
        let text = |key: &str| entry[key].as_str().unwrap_or("").to_owned();
        let id = text("id");
        let setup = text("setup");
        if !valid_name(&id)
            || kinds.iter().any(|known| known.id == id)
            || (id == MACHINE && !matches!(setup.as_str(), "seed" | "apply"))
        {
            continue;
        }
        let declared = entry["verbs"].as_array().cloned().unwrap_or_default();
        let arguments = provisioner_arguments(&entry["arguments"]);
        kinds.push(rookery_api::ProvisionerKind {
            label: match text("label") {
                label if label.is_empty() => id.clone(),
                label => label,
            },
            description: text("description"),
            setup: match id == MACHINE {
                true => setup,
                false => String::new(),
            },
            verbs: KIND_VERBS
                .iter()
                .filter(|verb| declared.iter().any(|known| known == **verb))
                .map(|verb| (*verb).to_owned())
                .collect(),
            spec: entry.get("spec").filter(|spec| spec.is_object()).cloned(),
            actions: provisioner_actions(&entry["actions"], rookery_api::ops::Role::Admin),
            catalogue: provisioner_catalogue(&entry["catalogue"], &arguments),
            arguments,
            id,
        });
    }
    kinds
}

/// What a plugin offers to choose from when a kind is made: its sizes, images
/// and regions, each a set of the kind's arguments. A choice that names no
/// id, or nothing the kind takes, is left out.
fn provisioner_catalogue(
    entry: &Value,
    arguments: &[rookery_api::Argument],
) -> rookery_api::Catalogue {
    let choices = |key: &str| -> Vec<rookery_api::CatalogueChoice> {
        let mut found: Vec<rookery_api::CatalogueChoice> = vec![];
        for choice in entry[key].as_array().cloned().unwrap_or_default() {
            let id = choice["id"].as_str().unwrap_or("");
            if !valid_name(id) || found.iter().any(|known| known.id == id) {
                continue;
            }
            let values: std::collections::BTreeMap<String, String> = choice["values"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(name, _)| arguments.iter().any(|known| known.name == **name))
                .filter_map(|(name, value)| {
                    value.as_str().map(|text| (name.clone(), text.to_owned()))
                })
                .collect();
            if values.is_empty() {
                continue;
            }
            let label = choice["label"].as_str().unwrap_or("");
            found.push(rookery_api::CatalogueChoice {
                id: id.to_owned(),
                label: match label.is_empty() {
                    true => id.to_owned(),
                    false => label.to_owned(),
                },
                help: choice["help"].as_str().unwrap_or("").to_owned(),
                default: choice["default"].as_bool().unwrap_or(false),
                values,
            });
        }
        found
    };
    rookery_api::Catalogue {
        sizes: choices("sizes"),
        images: choices("images"),
        regions: choices("regions"),
    }
}

/// The actions of a provisioner's manifest, or of one of its kinds. Each
/// runs where rookery runs; one that changes something needs an admin unless
/// it says otherwise, and one that only reads needs `reading`.
fn provisioner_actions(
    entries: &Value,
    reading: rookery_api::ops::Role,
) -> Vec<rookery_api::Action> {
    let mut actions: Vec<rookery_api::Action> = vec![];
    for entry in entries.as_array().cloned().unwrap_or_default() {
        let Ok(action) = serde_json::from_value::<rookery_api::Action>(entry.clone()) else {
            continue;
        };
        if !valid_name(&action.name) || actions.iter().any(|known| known.name == action.name) {
            continue;
        }
        let role = match (entry.get("role"), action.changes) {
            (Some(_), _) => action.role,
            (None, true) => rookery_api::ops::Role::Admin,
            (None, false) => reading,
        };
        actions.push(rookery_api::Action {
            arguments: provisioner_arguments(&entry["arguments"]),
            runs: rookery_api::Runs::Rookery,
            terminal: false,
            available: true,
            why: String::new(),
            steps: vec![],
            collection: String::new(),
            role,
            ..action
        });
    }
    actions
}

/// A provisioner of a manifest, or None when it cannot be run at all: a bad
/// id, or a command that is not a file inside the package. One of another
/// version of the protocol is kept, refused, so that it can say why.
fn read_provisioner(plugin: &str, entry: &Value, dir: &Path) -> Option<Provisioner> {
    let text = |key: &str| entry[key].as_str().unwrap_or("").to_owned();
    let id = text("id");
    let protocol = entry["protocol"].as_u64();
    let refused = match protocol {
        Some(PROTOCOL) => None,
        other => Some(format!(
            "{plugin}/{id} speaks {} of the provisioner protocol, and rookery reads only version {PROTOCOL}: update the plugin, or have its manifest declare \"protocol\": {PROTOCOL} with its kinds and actions",
            match other {
                Some(version) => format!("version {version}"),
                None => "version 1".to_owned(),
            }
        )),
    };
    let relative = PathBuf::from(text("command"));
    // rookery runs it as itself, so it must be the package's own file.
    let plain = relative.components().all(|part| {
        matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    if !valid_name(&id) || relative.as_os_str().is_empty() || !plain {
        return None;
    }
    let command = fs::canonicalize(dir.join(&relative)).ok()?;
    if !command.starts_with(dir) || !command.is_file() {
        return None;
    }
    let list = |key: &str| entry[key].as_array().cloned().unwrap_or_default();
    let mut secrets: Vec<ProvisionerSecret> = vec![];
    for secret in list("secrets") {
        let (name, required) = match &secret {
            Value::String(name) => (name.clone(), true),
            other => (
                other["name"].as_str().unwrap_or("").to_owned(),
                other["required"].as_bool().unwrap_or(true),
            ),
        };
        if valid_name(&name) && !secrets.iter().any(|known| known.name == name) {
            secrets.push(ProvisionerSecret { name, required });
        }
    }
    let mut own = provisioner_arguments(&entry["arguments"]);
    let (kinds, mut actions) = match refused {
        Some(_) => (vec![], vec![]),
        None => (
            provisioner_kinds(&entry["kinds"]),
            provisioner_actions(&entry["actions"], rookery_api::ops::Role::View),
        ),
    };
    let (collections, refused) = match refused {
        Some(refused) => (vec![], Some(refused)),
        None => match provisioner_collections(&entry["collections"], &mut actions, &kinds) {
            Ok(collections) => (collections, None),
            Err(why) => (
                vec![],
                Some(format!(
                    "{plugin}/{id} declares its collections wrong: {why}. Its manifest has to be fixed"
                )),
            ),
        },
    };
    let machine = kinds.iter().find(|kind| kind.id == MACHINE);
    let setup = match machine.map(|kind| kind.setup.as_str()) {
        Some("seed") => Setup::Seed,
        _ => Setup::Apply,
    };
    let lists = list("verbs").iter().any(|verb| verb == "list");
    let mut verbs: Vec<String> = OPTIONAL_VERBS
        .iter()
        .filter(|verb| {
            (**verb == "list" && lists)
                || machine.is_some_and(|kind| kind.verbs.iter().any(|known| known == **verb))
        })
        .map(|verb| (*verb).to_owned())
        .collect();
    verbs.sort_by_key(|verb| OPTIONAL_VERBS.iter().position(|known| known == verb));
    let names: Vec<String> = own.iter().map(|argument| argument.name.clone()).collect();
    // A machine's argument of the same name as one of the provisioner's is the provisioner's.
    if let Some(machine) = machine {
        own.extend(
            machine
                .arguments
                .iter()
                .filter(|argument| !names.contains(&argument.name))
                .cloned(),
        );
    }
    Some(Provisioner {
        label: match text("label") {
            label if label.is_empty() => id.clone(),
            label => label,
        },
        id,
        description: text("description"),
        command,
        setup,
        archive: setup == Setup::Seed
            && machine.is_some_and(|_| {
                entry["kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|kind| kind["id"] == MACHINE)
                    .is_some_and(|kind| kind["archive"].as_bool().unwrap_or(false))
            }),
        verbs,
        secrets,
        arguments: own,
        own: names,
        kinds,
        actions,
        collections,
        refused,
    })
}

/// The collections of a provisioner's manifest, checked against its actions
/// and kinds; each action they name is marked with the collection's id. A
/// collection that names an action, an argument, a kind or a collection
/// there is not is an error that says which.
fn provisioner_collections(
    entries: &Value,
    actions: &mut [rookery_api::Action],
    kinds: &[rookery_api::ProvisionerKind],
) -> std::result::Result<Vec<rookery_api::Collection>, String> {
    let Some(entries) = entries.as_array() else {
        return match entries.is_null() {
            true => Ok(vec![]),
            false => Err("collections is not a list".into()),
        };
    };
    let mut collections: Vec<rookery_api::Collection> = vec![];
    for entry in entries {
        let mut collection: rookery_api::Collection = serde_json::from_value(entry.clone())
            .map_err(|error| format!("a collection does not read: {error}"))?;
        let id = collection.id.clone();
        if !valid_name(&id) {
            return Err(format!("{id:?} is not a name for a collection"));
        }
        if collections.iter().any(|known| known.id == id) {
            return Err(format!("the collection {id} is declared twice"));
        }
        if collection.label.is_empty() {
            collection.label = id.clone();
        }
        if collection.plural.is_empty() {
            collection.plural = collection.label.clone();
        }
        if collection.key.is_empty() || collection.key.iter().any(|part| !valid_name(part)) {
            return Err(format!(
                "the collection {id} needs a key of argument names, such as [\"pool\"]"
            ));
        }
        let mut parent: Vec<String> = vec![];
        if !collection.within.is_empty() {
            let Some(holder) = collections
                .iter()
                .find(|known| known.id == collection.within)
            else {
                return Err(format!(
                    "the collection {id} is within {}, which is not declared before it",
                    collection.within
                ));
            };
            if !holder.kinds.is_empty()
                || collection.key.len() <= holder.key.len()
                || !collection.key.starts_with(&holder.key)
            {
                return Err(format!(
                    "the key of {id} has to start with the key of {}, {}, and name more",
                    holder.id,
                    holder.key.join(" ")
                ));
            }
            parent = holder.key.clone();
        }
        for kind in &collection.kinds {
            if !kinds.iter().any(|known| &known.id == kind) {
                return Err(format!("the collection {id} has items of no kind {kind}"));
            }
        }
        if !collection.kinds.is_empty()
            && (collection.key.len() != 1
                || !collection.within.is_empty()
                || !collection.new.is_empty()
                || !collection.list.is_empty())
        {
            return Err(format!(
                "the collection {id} of resources has a key of one name, and only item actions"
            ));
        }
        let mut seen: Vec<&str> = vec![];
        for column in &collection.columns {
            if column.name.is_empty() || seen.contains(&column.name.as_str()) {
                return Err(format!(
                    "the columns of {id} need names, each once: {:?}",
                    column.name
                ));
            }
            seen.push(&column.name);
        }
        // What fills an action's arguments: the whole key on an item, the
        // key of the item it is within when new or on the list.
        let sorts = [
            ("new", &collection.new, &parent),
            ("item", &collection.item, &collection.key),
            ("list", &collection.list, &parent),
        ];
        for (sort, listed, filled) in sorts {
            for used in listed {
                let Some(action) = actions.iter_mut().find(|known| known.name == used.action)
                else {
                    return Err(format!(
                        "{id} names the action {}, which the provisioner does not have",
                        used.action
                    ));
                };
                if !action.collection.is_empty() && action.collection != id {
                    return Err(format!(
                        "the action {} is in the collections {} and {id}; it can be in one",
                        used.action, action.collection
                    ));
                }
                for part in used.arguments.keys() {
                    if !collection.key.contains(part) {
                        return Err(format!(
                            "{id} fills an argument of {} from {part}, which is not in its key",
                            used.action
                        ));
                    }
                }
                for part in filled {
                    let argument = used.argument(part);
                    if !action.arguments.iter().any(|known| known.name == argument) {
                        return Err(format!(
                            "{id} fills the argument {argument} of {} from its {sort}'s {part}, and {} has no argument {argument}",
                            used.action, used.action
                        ));
                    }
                }
                action.collection = id.clone();
            }
        }
        if !collection.remove.is_empty()
            && !collection
                .item
                .iter()
                .any(|known| known.action == collection.remove)
        {
            return Err(format!(
                "{id} removes an item with {}, which is none of its item actions",
                collection.remove
            ));
        }
        if !collection.show.is_empty() {
            let Some(action) = actions
                .iter_mut()
                .find(|known| known.name == collection.show)
            else {
                return Err(format!(
                    "{id} is shown by the action {}, which the provisioner does not have",
                    collection.show
                ));
            };
            if action.changes || !action.collection.is_empty() {
                return Err(format!(
                    "{id} is shown by the action {}, which has to only read and be in no other collection",
                    collection.show
                ));
            }
            action.collection = id.clone();
        }
        collections.push(collection);
    }
    Ok(collections)
}

// ── Managing them ───────────────────────────────────────────────────────────

/// Where the user's own plugins are kept, under a configuration directory.
pub fn own_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("plugins")
}

/// The plugins installed under a configuration directory, those that come
/// with rookery among them.
pub fn installed(config_dir: &Path) -> Vec<Plugin> {
    load(&[own_dir(config_dir)])
}

impl Plugin {
    /// What is said of it to a screen or over the API.
    pub fn info(&self) -> PluginInfo {
        PluginInfo {
            id: self.id.clone(),
            label: self.label.clone(),
            package: self.package.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            origin: self.origin,
            source: self.source.clone(),
            dir: self.package_dir.display().to_string(),
            problem: self.problem.clone().unwrap_or_default(),
            features: self
                .features
                .iter()
                .map(|feature| PluginFeature {
                    id: feature.id.clone(),
                    label: feature.label.clone(),
                    group: feature.group.clone(),
                    help: feature.help.to_owned(),
                    actions: feature
                        .actions
                        .iter()
                        .map(|action| ListedAction {
                            name: action.name.clone(),
                            label: action.label.clone(),
                            user: action.user.clone(),
                            agent: action.agent,
                            extends: action.extends.clone(),
                            arguments: action
                                .arguments
                                .iter()
                                .map(|argument| argument.name.clone())
                                .collect(),
                        })
                        .collect(),
                    services: feature
                        .services
                        .iter()
                        .map(|service| PluginService {
                            name: service.name.clone(),
                            user: service.user.clone(),
                            methods: service
                                .methods
                                .iter()
                                .map(|method| method.name.clone())
                                .collect(),
                        })
                        .collect(),
                    channels: feature
                        .channels
                        .iter()
                        .map(|channel| channel.topic.clone())
                        .collect(),
                })
                .collect(),
            provisioners: self
                .provisioners
                .iter()
                .map(|provisioner| provisioner.info(&self.id, vec![]))
                .collect(),
        }
    }
}

/// SHA-256 over the files of a plugin's target folder, their paths and
/// contents, so a plugin that changed without a new version is told apart.
/// A `__pycache__` or `.git` folder is not part of it, as the release leaves
/// them out too.
pub fn digest(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    fn walk(base: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if name == "__pycache__" || name == ".git" {
                continue;
            }
            match fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => walk(base, &path, files),
                Ok(_) => files.push(path.strip_prefix(base).unwrap_or(&path).to_owned()),
                Err(_) => {}
            }
        }
    }
    let mut files = vec![];
    walk(dir, dir, &mut files);
    files.sort();
    let mut hash = Sha256::new();
    for file in files {
        let data = fs::read(dir.join(&file)).unwrap_or_default();
        hash.update(file.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update((data.len() as u64).to_be_bytes());
        hash.update(&data);
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What is recorded of each plugin on a host when it is applied.
pub fn records(plugins: &[Plugin]) -> Vec<AppliedPlugin> {
    plugins
        .iter()
        .map(|plugin| AppliedPlugin {
            id: plugin.id.clone(),
            package: plugin.package.clone(),
            version: plugin.version.clone(),
            digest: digest(&plugin.dir),
        })
        .collect()
}

/// How a host that says what it was applied from differs from what these
/// plugins and this rookery would apply now.
pub fn behind(applied: &rookery_api::Applied, plugins: &[Plugin]) -> Option<rookery_api::Behind> {
    rookery_api::Behind::compare(applied, rookery_api::VERSION, &records(plugins))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Lock the package folder `dir` in the install folder two levels up,
    /// as `GROUP/ID` of the folders it is in.
    pub(crate) fn lock_package(dir: &Path, version: &str) {
        let install = dir.parent().and_then(Path::parent).unwrap();
        let group = dir.parent().and_then(Path::file_name).unwrap();
        let id = dir.file_name().unwrap();
        let lock = install.join(hatchery::lock::FILE);
        let mut text = fs::read_to_string(&lock).unwrap_or_else(|_| "format = 1\n".into());
        text.push_str(&format!(
            "\n[[package]]\nname = \"{}/{}\"\nversion = \"{version}\"\ntargets = [\"rookery\"]\nfolder = \"{}\"\nsha256 = \"00\"\n",
            group.to_str().unwrap(),
            id.to_str().unwrap(),
            dir.display()
        ));
        fs::write(lock, text).unwrap();
    }

    /// A plugin package in `dir`, locked in the install folder two levels
    /// up: its `package.kdl`, with a label when given, and its target's
    /// `rookery.json`. Gives the target's folder.
    pub(crate) fn write_package(
        dir: &Path,
        id: &str,
        version: &str,
        label: Option<&str>,
        rookery: &str,
    ) -> PathBuf {
        let about = label
            .map(|label| format!("about {{\n  name \"{label}\"\n}}\n"))
            .unwrap_or_default();
        fs::create_dir_all(dir.join(TARGET)).unwrap();
        fs::write(
            dir.join("package.kdl"),
            format!(
                "format 1\npackage {{\n  group example\n  id \"{id}\"\n  version \"{version}\"\n}}\n{about}targets {{\n  rookery {{\n    requires {{\n      rookery \">=0.6\"\n    }}\n  }}\n}}\n"
            ),
        )
        .unwrap();
        fs::write(dir.join(TARGET).join(LEGACY_MANIFEST), rookery).unwrap();
        lock_package(dir, version);
        dir.join(TARGET)
    }

    #[test]
    fn packages_are_found_named_and_replaced() {
        let base = tempfile::tempdir().unwrap();
        let at = |dir: &str| base.path().join(dir);
        write_package(&at("a/example/one"), "one", "1.0.0", Some("First"), "{}");
        fs::create_dir_all(at("a/example/not-a-package")).unwrap();
        fs::create_dir_all(at("a/example/broken")).unwrap();
        fs::write(at("a/example/broken/package.kdl"), "package {").unwrap();
        lock_package(&at("a/example/broken"), "1.0.0");
        write_package(&at("a/example/bad"), "Bad_Name", "1.0.0", None, "{}");
        write_package(&at("a/example/noise"), "noise", "1.0.0", None, "[1]");
        // A package for another tool alone is no plugin.
        fs::create_dir_all(at("a/example/other")).unwrap();
        fs::write(
            at("a/example/other/package.kdl"),
            "package {\n  group example\n  id other\n  version \"1.0.0\"\n}\ntargets {\n  crow\n}\n",
        )
        .unwrap();
        lock_package(&at("a/example/other"), "1.0.0");
        write_package(
            &at("b/example/one"),
            "one",
            "2.0.0",
            Some("Second"),
            r#"{"features":[
                  {"id":"x","group":"Development Tools","actions":[
                    {"name":"go","run":"true","user":"root","agent":true,"confirm":"Sure?",
                     "arguments":[{"name":"folder","required":true},{"name":"Bad Name"}]},
                    {"name":"no command"},
                    {"name":"plain","run":"id"}]},
                  {"id":"y","services":[{"name":"manager","user":"root","methods":[{"name":"start"}]}],
                   "channels":[{"topic":"progress","keep":"none","max":1000}]},
                  {"id":"Not Valid"}]}"#,
        );
        let dirs = [at("a"), at("b"), "missing".into()];
        let plugins = load(&dirs);
        let ids: Vec<_> = plugins.iter().map(|plugin| plugin.id.as_str()).collect();
        assert_eq!(ids, ["one"]);
        // What Rookery cannot use is listed with the reason.
        let refused: Vec<_> = scan(&dirs)
            .into_iter()
            .filter(|plugin| plugin.problem.is_some())
            .map(|plugin| plugin.id)
            .collect();
        assert_eq!(refused, ["Bad_Name", "noise"]);
        let one = &plugins[0];
        assert_eq!(
            (
                one.label.as_str(),
                one.version.as_str(),
                one.package.as_str()
            ),
            ("Second", "2.0.0", "example/one")
        );
        assert!(one.dir.ends_with("b/example/one/rookery"));
        let features: Vec<_> = one
            .features
            .iter()
            .map(|feature| (feature.id.as_str(), feature.group.as_str()))
            .collect();
        // A feature without a group gets one named after its plugin.
        assert_eq!(features, [("x", "Development Tools"), ("y", "Second")]);
        let actions: Vec<_> = one.features[0]
            .actions
            .iter()
            .map(|action| (action.name.as_str(), action.user.as_str()))
            .collect();
        assert_eq!(actions, [("go", "root"), ("plain", "agent")]);
        let go = &one.features[0].actions[0];
        assert!(go.agent && go.confirm.as_deref() == Some("Sure?"));
        assert_eq!(go.arguments.len(), 1);
        assert!(go.arguments[0].required && !one.features[0].actions[1].agent);
        let extra = &one.features[1];
        assert_eq!(extra.services[0].methods[0].name, "start");
        assert_eq!(extra.channels[0].keep, Some(rookery_api::Keep::None));
    }

    #[test]
    fn a_plugin_says_which_rookery_it_works_with() {
        let base = tempfile::tempdir().unwrap();
        let write = |id: &str, requires: &str| {
            let dir = base.path().join("example").join(id);
            lock_package(&dir, "1.0.0");
            fs::create_dir_all(dir.join("rookery")).unwrap();
            fs::write(dir.join("rookery/rookery.json"), "{}").unwrap();
            fs::write(
                dir.join("package.kdl"),
                format!("package {{\n  group example\n  id {id}\n  version \"1.0.0\"\n}}\ntargets {{\n  rookery {{\n    requires {{\n{requires}\n    }}\n  }}\n}}\n"),
            )
            .unwrap();
        };
        write("fits", "      rookery \">=0.6\"");
        write("silent", "      os linux");
        write("later", "      rookery \">=99\"");
        write("gpu", "      rookery \">=0.6\"\n      gpu");
        let said: Vec<(String, String)> = scan(&[base.path().to_owned()])
            .into_iter()
            .map(|plugin| (plugin.id, plugin.problem.unwrap_or_default()))
            .collect();
        assert_eq!(said[0].0, "fits");
        assert_eq!(said[0].1, "");
        assert_eq!(said[1].1, "it needs gpu, which Rookery does not have");
        assert!(
            said[2]
                .1
                .starts_with("it works with Rookery >=99, and this is "),
            "{}",
            said[2].1
        );
        assert!(
            said[3].1.contains("does not say which Rookery"),
            "{}",
            said[3].1
        );
    }

    /// The provisioner `vm` of a plugin whose manifest declares `collections`
    /// beside a few actions and the kinds machine and instance.
    fn with_collections(collections: &str) -> Provisioner {
        let base = tempfile::tempdir().unwrap();
        let package = base.path().join("plugins/example/cloud");
        let dir = package.join(TARGET);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/provision"), "#!/bin/sh\n").unwrap();
        let manifest = format!(
            r#"{{"provisioners": [
                {{"id": "vm", "command": "bin/provision", "protocol": 2,
                 "kinds": [{{"id": "machine", "setup": "apply"}}, {{"id": "instance"}}],
                 "actions": [
                   {{"name": "pools", "changes": false}},
                   {{"name": "new-pool", "arguments": [{{"name": "name"}}]}},
                   {{"name": "grow-pool", "arguments": [{{"name": "pool"}}, {{"name": "size"}}]}},
                   {{"name": "remove-pool", "arguments": [{{"name": "name"}}]}},
                   {{"name": "new-volume", "arguments": [{{"name": "pool"}}, {{"name": "name"}}]}},
                   {{"name": "remove-volume", "arguments": [{{"name": "pool"}}, {{"name": "name"}}]}},
                   {{"name": "grow-disk", "arguments": [{{"name": "instance"}}, {{"name": "size"}}]}},
                   {{"name": "clean"}}, {{"name": "check", "changes": false}}],
                 "collections": {collections}}}]}}"#
        );
        write_package(&package, "cloud", "1.0.0", None, &manifest);
        load(&[base.path().join("plugins")])
            .remove(0)
            .provisioners
            .remove(0)
    }

    #[test]
    fn collections_name_their_actions_and_mark_them() {
        let vm = with_collections(
            r#"[{"id": "pools", "label": "Storage pool", "key": ["pool"],
                 "columns": [{"name": "driver", "label": "Driver"}],
                 "new": [{"action": "new-pool"}], "list": [{"action": "clean"}],
                 "item": [{"action": "grow-pool", "label": "Grow"},
                          {"action": "remove-pool", "arguments": {"pool": "name"}}],
                 "remove": "remove-pool", "show": "pools"},
                {"id": "volumes", "label": "Volume", "plural": "Volumes", "key": ["pool", "volume"],
                 "within": "pools", "new": [{"action": "new-volume"}],
                 "item": [{"action": "remove-volume", "arguments": {"volume": "name"}}]},
                {"id": "disks", "key": ["instance"], "kinds": ["machine", "instance"],
                 "item": [{"action": "grow-disk"}]}]"#,
        );
        assert!(vm.refused.is_none(), "{:?}", vm.refused);
        let ids: Vec<&str> = vm
            .collections
            .iter()
            .map(|known| known.id.as_str())
            .collect();
        assert_eq!(ids, ["pools", "volumes", "disks"]);
        // The plural is the label when it is not given.
        assert_eq!(vm.collections[0].plural, "Storage pool");
        assert_eq!(vm.collections[1].item[0].argument("volume"), "name");
        assert_eq!(vm.collections[1].item[0].argument("pool"), "pool");
        let marked: Vec<(&str, &str)> = vm
            .actions
            .iter()
            .map(|action| (action.name.as_str(), action.collection.as_str()))
            .collect();
        assert_eq!(
            marked,
            [
                ("pools", "pools"),
                ("new-pool", "pools"),
                ("grow-pool", "pools"),
                ("remove-pool", "pools"),
                ("new-volume", "volumes"),
                ("remove-volume", "volumes"),
                ("grow-disk", "disks"),
                ("clean", "pools"),
                ("check", "")
            ]
        );
        assert_eq!(vm.info("cloud", vec![]).collections, vm.collections);
    }

    #[test]
    fn a_collection_that_names_what_is_not_there_refuses_the_provisioner() {
        for (collections, said) in [
            (
                r#"[{"id": "pools", "key": ["pool"], "item": [{"action": "shrink-pool"}]}]"#,
                "pools names the action shrink-pool, which the provisioner does not have",
            ),
            (
                r#"[{"id": "pools", "key": ["pool"], "item": [{"action": "remove-pool"}]}]"#,
                "remove-pool has no argument pool",
            ),
            (
                r#"[{"id": "pools", "key": ["pool"],
                     "item": [{"action": "remove-pool", "arguments": {"name": "name"}}]}]"#,
                "from name, which is not in its key",
            ),
            (
                r#"[{"id": "volumes", "key": ["pool", "volume"], "within": "pools"}]"#,
                "is within pools, which is not declared before it",
            ),
            (
                r#"[{"id": "pools", "key": ["pool"]},
                    {"id": "volumes", "key": ["volume"], "within": "pools"}]"#,
                "has to start with the key of pools",
            ),
            (
                r#"[{"id": "pools", "key": ["pool"]},
                    {"id": "volumes", "key": ["pool", "volume"], "within": "pools",
                     "new": [{"action": "new-pool"}]}]"#,
                "new-pool has no argument pool",
            ),
            (
                r#"[{"id": "disks", "key": ["instance"], "kinds": ["deployment"]}]"#,
                "has items of no kind deployment",
            ),
            (
                r#"[{"id": "pools", "key": ["pool"], "remove": "remove-pool"}]"#,
                "which is none of its item actions",
            ),
            (
                r#"[{"id": "pools", "key": ["pool"], "show": "clean"}]"#,
                "has to only read",
            ),
            (
                r#"[{"id": "a", "key": ["pool"], "list": [{"action": "clean"}]},
                    {"id": "b", "key": ["pool"], "list": [{"action": "clean"}]}]"#,
                "clean is in the collections a and b",
            ),
            (r#"[{"id": "pools"}]"#, "does not read"),
            (r#"[{"id": "pools", "key": []}]"#, "needs a key"),
            (r#"{"pools": {}}"#, "collections is not a list"),
        ] {
            let vm = with_collections(collections);
            let refused = vm.refused.unwrap_or_default();
            assert!(refused.contains(said), "{said:?} in {refused:?}");
            assert!(
                refused.starts_with("cloud/vm declares its collections wrong: "),
                "{refused}"
            );
            assert!(vm.collections.is_empty());
        }
    }

    #[test]
    fn the_shipped_provisioners_declare_their_collections_right() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/rookery");
        let plugins: Vec<Plugin> = fs::read_dir(root)
            .unwrap()
            .flatten()
            .filter_map(|entry| read(&entry.path()))
            .collect();
        for (plugin, provisioner, collections) in [
            (
                "incus",
                "vm",
                vec!["pools", "volumes", "images", "instances"],
            ),
            (
                "incus",
                "container",
                vec!["pools", "volumes", "images", "instances"],
            ),
            ("docker", "container", vec!["images", "volumes"]),
        ] {
            let found = plugins
                .iter()
                .find(|known| known.id == plugin)
                .and_then(|known| {
                    known
                        .provisioners
                        .iter()
                        .find(|known| known.id == provisioner)
                })
                .unwrap();
            assert!(found.refused.is_none(), "{:?}", found.refused);
            let ids: Vec<&str> = found
                .collections
                .iter()
                .map(|known| known.id.as_str())
                .collect();
            assert_eq!(ids, collections);
        }
    }

    #[test]
    fn provisioners_are_read_and_their_commands_kept_inside_the_package() {
        let base = tempfile::tempdir().unwrap();
        let package = base.path().join("plugins/example/cloud");
        let dir = package.join(TARGET);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/provision"), "#!/bin/sh\n").unwrap();
        fs::write(package.join("outside"), "#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(package.join("outside"), dir.join("bin/out")).unwrap();
        write_package(
            &package,
            "cloud",
            "1.0.0",
            None,
            r#"{"provisioners": [
                {"id": "vm", "label": "A VM", "description": "One machine.", "command": "bin/provision",
                 "protocol": 2, "verbs": ["list", "reboot"],
                 "secrets": ["key", {"name": "cert", "required": false}, {"name": "Bad Name"}, "key"],
                 "arguments": [{"name": "server", "label": "Server", "required": true}],
                 "kinds": [
                   {"id": "machine", "setup": "seed", "archive": true, "verbs": ["stop", "logs", "fly"],
                    "arguments": [{"name": "server"},
                                  {"name": "cpus", "default": "4", "choices": "seq 8", "kind": "resource"},
                                  {"name": "image", "kind": "colour", "help": "What it starts from.",
                                   "values": ["a", "b"]}, {"name": "Bad"}]},
                   {"id": "deployment", "label": "Compose project", "verbs": ["change"],
                    "spec": {"format": "compose", "label": "Compose file"}},
                   {"id": "machine", "setup": "apply"}],
                 "actions": [
                   {"name": "prune", "label": "Remove unused images", "group": "Storage",
                    "confirm": "Remove them?", "runs": "host", "terminal": true},
                   {"name": "images", "changes": false, "arguments": [{"name": "filter", "choices": "ls"}]},
                   {"name": "grow", "role": "operate"},
                   {"name": "Bad Name"}, {"name": "images"}]},
                {"id": "ssh", "command": "./bin/provision", "protocol": 2,
                 "kinds": [{"id": "machine", "setup": "apply", "archive": true}]},
                {"id": "old", "command": "bin/provision", "setup": "apply", "verbs": ["start"]},
                {"id": "three", "command": "bin/provision", "protocol": 3},
                {"id": "up", "command": "../outside", "protocol": 2},
                {"id": "absolute", "command": "/bin/sh", "protocol": 2},
                {"id": "linked", "command": "bin/out", "protocol": 2},
                {"id": "missing", "command": "bin/none", "protocol": 2},
                {"id": "folder", "command": "bin", "protocol": 2},
                {"id": "odd", "command": "bin/provision", "protocol": 2,
                 "kinds": [{"id": "machine", "setup": "magic"}]},
                {"id": "Bad Id", "command": "bin/provision", "protocol": 2}]}"#,
        );
        // Provisioners and no features is a plugin too.
        let plugins = load(&[base.path().join("plugins")]);
        assert_eq!(plugins.len(), 1);
        let ids: Vec<&str> = plugins[0]
            .provisioners
            .iter()
            .map(|provisioner| provisioner.id.as_str())
            .collect();
        assert_eq!(ids, ["vm", "ssh", "old", "three", "odd"]);
        let vm = &plugins[0].provisioners[0];
        assert_eq!(vm.command, plugins[0].dir.join("bin/provision"));
        assert_eq!((vm.setup, vm.archive), (Setup::Seed, true));
        assert_eq!(vm.verbs, ["stop", "list"]);
        assert!(vm.refused.is_none());
        // The machine kind's own verbs, the first machine kind, and the kinds rookery does not know yet.
        let kinds: Vec<(&str, &str, Vec<String>)> = vm
            .kinds
            .iter()
            .map(|kind| (kind.id.as_str(), kind.setup.as_str(), kind.verbs.clone()))
            .collect();
        assert_eq!(
            kinds,
            [
                (
                    "machine",
                    "seed",
                    vec!["stop".to_owned(), "logs".to_owned()]
                ),
                ("deployment", "", vec!["change".to_owned()])
            ]
        );
        assert_eq!(vm.kinds[1].spec.as_ref().unwrap()["format"], "compose");
        assert_eq!(vm.own, ["server"]);
        // Actions run where rookery runs, and an admin runs what changes something.
        let actions: Vec<_> = vm
            .actions
            .iter()
            .map(|action| {
                (
                    action.name.as_str(),
                    action.role,
                    action.runs,
                    action.terminal,
                )
            })
            .collect();
        use rookery_api::{Runs, ops::Role};
        assert_eq!(
            actions,
            [
                ("prune", Role::Admin, Runs::Rookery, false),
                ("images", Role::View, Runs::Rookery, false),
                ("grow", Role::Operate, Runs::Rookery, false)
            ]
        );
        assert_eq!(vm.actions[0].confirm.as_deref(), Some("Remove them?"));
        assert!(vm.actions[1].arguments[0].choices.is_empty());
        // Another version is kept, refused with what to change.
        let old = &plugins[0].provisioners[2];
        let said = old.refused.as_deref().unwrap();
        assert!(said.starts_with("cloud/old speaks version 1"), "{said}");
        assert!(said.contains("\"protocol\": 2") && old.kinds.is_empty());
        assert!(
            plugins[0].provisioners[3]
                .refused
                .as_deref()
                .unwrap()
                .contains("version 3")
        );
        // A machine kind needs a set-up rookery knows; without one it makes nothing.
        assert!(plugins[0].provisioners[4].machine().is_none());
        assert_eq!(
            vm.secrets,
            [
                ProvisionerSecret {
                    name: "key".into(),
                    required: true
                },
                ProvisionerSecret {
                    name: "cert".into(),
                    required: false
                },
            ]
        );
        let arguments: Vec<_> = vm
            .arguments
            .iter()
            .map(|argument| {
                (
                    argument.name.as_str(),
                    argument.required,
                    argument.choices.as_str(),
                    argument.kind.as_str(),
                )
            })
            .collect();
        assert_eq!(
            arguments,
            [
                ("server", true, "", ""),
                ("cpus", false, "", "resource"),
                ("image", false, "", "")
            ]
        );
        // Only a seeded machine is given the archive.
        let ssh = &plugins[0].provisioners[1];
        assert_eq!(
            (ssh.setup, ssh.archive, ssh.label.as_str()),
            (Setup::Apply, false, "ssh")
        );
        let info = plugins[0].info();
        assert_eq!(info.provisioners[0].id, "cloud/vm");
        assert!(info.provisioners[0].problem.is_empty());
        assert!(info.provisioners[2].problem.contains("version 1"));
        // A provisioner's argument keeps its help and its values.
        assert_eq!(vm.arguments[2].help, "What it starts from.");
        assert_eq!(vm.arguments[2].values, ["a", "b"]);
        // The marker reaches the API and comes back from it.
        let sent = serde_json::to_value(&info.provisioners[0]).unwrap();
        assert_eq!(sent["arguments"][1]["kind"], "resource");
        assert!(sent["arguments"][0].get("kind").is_none());
        let back: rookery_api::ProvisionerInfo = serde_json::from_value(sent).unwrap();
        assert!(back.arguments[1].is_resource() && !back.arguments[0].is_resource());
        assert_eq!(info.provisioners[0].secrets, ["key", "cert"]);
        let kinds: Vec<&str> = info.provisioners[0]
            .kinds
            .iter()
            .map(|kind| kind.id.as_str())
            .collect();
        assert_eq!(kinds.len(), 2);
        assert_eq!(info.provisioners[0].verbs, ["stop", "list"]);
    }

    #[test]
    fn a_changed_file_changes_the_digest_and_shows_the_host_behind() {
        let base = tempfile::tempdir().unwrap();
        let dir = write_package(
            &base.path().join("plugins/example/x"),
            "x",
            "1.0.0",
            None,
            "{}",
        );
        fs::create_dir_all(dir.join("tasks")).unwrap();
        fs::write(dir.join("tasks/main.yml"), "- debug: {msg: one}\n").unwrap();
        fs::create_dir_all(dir.join("__pycache__")).unwrap();
        let plugins = load(&[base.path().join("plugins")]);
        let before = rookery_api::Applied {
            rookery: rookery_api::VERSION.into(),
            plugins: records(&plugins),
            ..Default::default()
        };
        assert_eq!(behind(&before, &plugins), None);
        // Python's cache beside it does not count; a task does.
        fs::write(dir.join("__pycache__/x.pyc"), "").unwrap();
        assert_eq!(behind(&before, &plugins), None);
        fs::write(dir.join("tasks/main.yml"), "- debug: {msg: two}\n").unwrap();
        let changed = behind(&before, &plugins).unwrap();
        assert_eq!(changed.text(), "x 1.0.0 (changed)");
        let older = rookery_api::Applied {
            rookery: "0.0.1".into(),
            ..before.clone()
        };
        assert_eq!(
            behind(&older, &[]).unwrap().text(),
            format!("rookery 0.0.1 → {}, x 1.0.0 (gone)", rookery_api::VERSION)
        );
    }
}
