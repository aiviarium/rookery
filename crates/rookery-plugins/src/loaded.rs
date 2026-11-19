//! What the server knows of each installed plugin: its package, its manifest as `plugin.json` has it
//! (from a `rookery.json` for one release), why it cannot be used, and its client half.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use rookery_api::PluginOrigin;
use rookery_api::plugin_contract::{self, Format, SCHEMA};
use rookery_proto::{ErrorBody, ErrorCode};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// One installed plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub id: String,
    /// `GROUP/ID`.
    pub package: String,
    pub version: String,
    pub label: String,
    pub origin: PluginOrigin,
    pub source: String,
    /// The folder of its `rookery` target, which the manifest's paths are relative to.
    pub dir: PathBuf,
    /// Which manifest it has; `None` when it has none that can be read.
    pub format: Option<Format>,
    /// The manifest, completed with its id, version and every list a client reads. `None` when the
    /// plugin is invalid.
    pub manifest: Option<Value>,
    /// Why it cannot be used, with each problem in `details.problems`.
    pub problem: Option<ErrorBody>,
    pub client: Option<ClientHalf>,
    /// Its collections, raw schema and migrations, which the datastore applies.
    pub declared: crate::data::Declared,
    /// SHA-256 over the target's files, as hosts record it.
    pub digest: String,
}

/// A plugin's built client half: the folder of its entry, and its stylesheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHalf {
    /// The entry, relative to the target's folder, such as `client/index.js`.
    pub entry: String,
    pub styles: Option<String>,
    /// The folder served with the entry, relative to the target's folder: every file of a build, its
    /// chunks and assets among them. Empty for the target's folder itself.
    pub root: String,
    pub hash: String,
}

impl Loaded {
    pub fn valid(&self) -> bool {
        self.problem.is_none() && self.manifest.is_some()
    }

    /// The host half's entry, relative to the target's folder.
    pub fn host(&self) -> Option<&str> {
        self.manifest.as_ref()?.get("host")?.as_str()
    }

    /// Whether its host half starts with the server rather than when first used.
    pub fn starts_at_once(&self) -> bool {
        self.manifest
            .as_ref()
            .and_then(|m| m.get("activate"))
            .and_then(Value::as_str)
            == Some("startup")
    }

    /// The shell it contributes: its entry, relative to the target's folder.
    pub fn shell(&self) -> Option<&str> {
        self.manifest
            .as_ref()?
            .pointer("/contributes/shell/entry")?
            .as_str()
    }

    /// The ids of the plugins it needs, with the ranges it accepts.
    pub fn dependencies(&self) -> BTreeMap<String, String> {
        self.manifest
            .as_ref()
            .and_then(|m| m.get("dependencies"))
            .and_then(Value::as_object)
            .map(|deps| {
                deps.iter()
                    .map(|(id, range)| (id.clone(), range.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub fn origin_name(origin: PluginOrigin) -> &'static str {
    match origin {
        PluginOrigin::Shipped => "shipped",
        PluginOrigin::Registry => "registry",
        PluginOrigin::Git => "git",
        PluginOrigin::File => "file",
    }
}

fn invalid(message: String, problems: Vec<String>) -> ErrorBody {
    ErrorBody {
        code: ErrorCode::Invalid,
        message,
        details: (!problems.is_empty()).then(|| json!({ "problems": problems })),
    }
}

/// The lists of `contributes` a client reads without looking whether they are there.
const LISTS: &[&str] = &[
    "commands",
    "keybindings",
    "views",
    "nav",
    "adminPages",
    "routes",
    "contexts",
    "settings",
    "collections",
    "entityTypes",
    "spaceKinds",
    "tools",
    "jobs",
    "rpc",
    "topics",
    "schedules",
    "webhooks",
    "connectionTypes",
    "widgets",
    "blocks",
    "hostFeatures",
    "provisioners",
];

/// A manifest with the id and version of its package, its label as the name when it has none, and the
/// defaults of every list and map, so a client finds each.
pub fn complete(mut manifest: Value, id: &str, version: &str, label: &str) -> Value {
    let object = manifest.as_object_mut().expect("a manifest is an object");
    object.remove("$schema");
    object.insert("id".into(), json!(id));
    object.insert("version".into(), json!(version));
    object.entry("name").or_insert_with(|| json!(label));
    object
        .entry("apiVersion")
        .or_insert_with(|| json!(plugin_contract::API_VERSION));
    for key in ["dependencies", "optionalDependencies"] {
        object.entry(key).or_insert_with(|| json!({}));
    }
    object
        .entry("provides")
        .or_insert_with(|| json!({"services": []}));
    object.entry("extensionPoints").or_insert_with(|| json!([]));
    object.entry("activate").or_insert_with(|| json!("lazy"));
    let contributes = object
        .entry("contributes")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("contributes is an object");
    for key in LISTS {
        contributes.entry(*key).or_insert_with(|| json!([]));
    }
    contributes.entry("extensions").or_insert_with(|| json!({}));
    manifest
}

/// Reads one plugin that rookery-core found in the install folder.
pub fn load(plugin: &rookery_core::plugins::Plugin) -> Loaded {
    let mut loaded = Loaded {
        id: plugin.id.clone(),
        package: plugin.package.clone(),
        version: plugin.version.clone(),
        label: plugin.label.clone(),
        origin: plugin.origin,
        source: plugin.source.clone(),
        dir: plugin.dir.clone(),
        format: None,
        manifest: None,
        problem: None,
        client: None,
        declared: Default::default(),
        digest: rookery_core::plugins::digest(&plugin.dir),
    };
    let read = match plugin_contract::read_dir(&plugin.dir) {
        Ok(read) => read,
        Err(problem) => {
            loaded.problem = Some(invalid(problem.message, vec![]));
            return loaded;
        }
    };
    loaded.format = Some(read.format);
    let file = read.format.file();
    // Hatchery checked a rookery.json against its own schema when it installed it.
    let mut problems: Vec<String> = match read.format {
        Format::Plugin => plugin_contract::schema_problems(SCHEMA, &read.manifest, file)
            .into_iter()
            .map(|p| p.message)
            .collect(),
        Format::Legacy => vec![],
    };
    let dir = plugin.dir.clone();
    let has = move |path: &str| inside(&dir, path).is_some_and(|p| p.is_file());
    problems.extend(
        plugin_contract::manifest_problems(&read, &has)
            .into_iter()
            .map(|p| p.message),
    );
    if let Some(problem) = &plugin.problem {
        problems.insert(0, problem.clone());
    }
    match crate::data::declared(&plugin.dir, &read.manifest) {
        Ok(declared) => loaded.declared = declared,
        Err(found) => problems.extend(found),
    }
    if !problems.is_empty() {
        loaded.problem = Some(invalid(
            format!("{file} of {} is not valid: {}", plugin.id, problems[0]),
            problems,
        ));
        return loaded;
    }
    let manifest = complete(read.manifest, &plugin.id, &plugin.version, &plugin.label);
    loaded.client = client_half(&plugin.dir, &manifest);
    loaded.manifest = Some(manifest);
    loaded
}

/// A path of a manifest inside `dir`, or `None` for one that leaves it.
pub fn inside(dir: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path.trim_start_matches("./"));
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(dir.join(relative))
}

/// The client half of a manifest, with the hash of its files.
fn client_half(dir: &Path, manifest: &Value) -> Option<ClientHalf> {
    let entry = manifest.get("client")?.as_str()?.trim_start_matches("./");
    let styles = manifest
        .get("styles")
        .and_then(Value::as_str)
        .map(|s| s.trim_start_matches("./").to_owned());
    let root = Path::new(entry)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut files = vec![];
    walk(&dir.join(&root), Path::new(&root), &mut files);
    if let Some(styles) = &styles
        && !files.iter().any(|f| f == Path::new(styles))
    {
        files.push(PathBuf::from(styles));
    }
    files.sort();
    let mut hash = Sha256::new();
    for file in &files {
        let data = fs::read(dir.join(file)).unwrap_or_default();
        hash.update(file.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update((data.len() as u64).to_be_bytes());
        hash.update(&data);
    }
    let hash: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(ClientHalf {
        entry: entry.to_owned(),
        styles,
        root,
        hash,
    })
}

fn walk(dir: &Path, relative: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        match fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => walk(&path, &relative.join(&name), files),
            Ok(_) => files.push(relative.join(&name)),
            Err(_) => {}
        }
    }
}

impl ClientHalf {
    /// Whether a path, relative to the target's folder, is one of the files served with it.
    pub fn serves(&self, path: &str) -> bool {
        let under_root = self.root.is_empty()
            || path
                .strip_prefix(self.root.as_str())
                .is_some_and(|rest| rest.starts_with('/'));
        let hidden = path.split('/').any(|part| part.starts_with('.'));
        (under_root && !hidden) || self.styles.as_deref() == Some(path)
    }

    pub fn info(&self, id: &str) -> crate::wire::ClientInfo {
        crate::wire::ClientInfo {
            script: format!("/plugins/{id}/{}/{}", self.hash, self.entry),
            styles: self
                .styles
                .as_ref()
                .map(|styles| format!("/plugins/{id}/{}/{styles}", self.hash)),
            hash: self.hash.clone(),
        }
    }
}
