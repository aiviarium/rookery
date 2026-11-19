//! The contract of a Rookery plugin, for Rookery and for the CI of the
//! repositories that publish plugins: the JSON Schema of `plugin.json`, the
//! older `rookery.json` that Rookery still reads for one release, and the
//! checks Rookery makes of a package beyond them. Installing an egg runs the
//! checks of its files, and `rookery plugin check` runs all of them on a
//! package folder.

use hatchery::{Problem, TargetFiles};
use serde_json::{Map, Value, json};
use std::path::Path;

/// The target of a package that Rookery installs, and its folder in the package.
pub const TARGET: &str = "rookery";
/// The manifest of the target, in its folder.
pub const MANIFEST: &str = "plugin.json";
/// The manifest a target had before `plugin.json`: host features and
/// provisioners alone. Rookery reads it for one release.
pub const LEGACY_MANIFEST: &str = "rookery.json";
/// The version of the manifest's format that Rookery reads.
pub const API_VERSION: &str = "1";

/// Which manifest a target has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `plugin.json`.
    Plugin,
    /// `rookery.json`, read as a `plugin.json` with host features and
    /// provisioners alone.
    Legacy,
}

impl Format {
    pub fn file(self) -> &'static str {
        match self {
            Format::Plugin => MANIFEST,
            Format::Legacy => LEGACY_MANIFEST,
        }
    }
}

/// A target's manifest as `plugin.json` has it, whichever file it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Read {
    pub format: Format,
    /// The manifest in the shape of `plugin.json`. One read from
    /// `rookery.json` has `apiVersion`, `contributes.hostFeatures` and
    /// `contributes.provisioners`, and no `name`: the package gives it.
    pub manifest: Value,
}

impl Read {
    /// The host features and provisioners in the shape of `rookery.json`,
    /// which the role and Rookery's host core read.
    pub fn host_parts(&self) -> Value {
        host_parts(&self.manifest)
    }
}

/// `{"features": [...], "provisioners": [...]}` of a manifest in the shape
/// of `plugin.json`.
pub fn host_parts(manifest: &Value) -> Value {
    let list = |key: &str| {
        manifest
            .pointer(&format!("/contributes/{key}"))
            .cloned()
            .unwrap_or_else(|| json!([]))
    };
    json!({"features": list("hostFeatures"), "provisioners": list("provisioners")})
}

/// A `rookery.json` as the `plugin.json` it stands for: its `features`
/// become `contributes.hostFeatures` and its `provisioners`
/// `contributes.provisioners`, with the same fields.
pub fn upgrade(legacy: &Value) -> Value {
    let mut contributes = Map::new();
    if let Some(features) = legacy.get("features") {
        contributes.insert("hostFeatures".into(), features.clone());
    }
    if let Some(provisioners) = legacy.get("provisioners") {
        contributes.insert("provisioners".into(), provisioners.clone());
    }
    json!({"apiVersion": API_VERSION, "contributes": contributes})
}

/// Reads a target's manifest from its files, `plugin.json` before
/// `rookery.json`. A target with neither, or with both, has a problem.
pub fn read(get: impl Fn(&str) -> Option<Vec<u8>>) -> Result<Read, Problem> {
    let (format, data) = match (get(MANIFEST), get(LEGACY_MANIFEST)) {
        (Some(_), Some(_)) => {
            return Err(Problem::new(format!(
                "the rookery target has both {MANIFEST} and {LEGACY_MANIFEST}: {MANIFEST} replaces {LEGACY_MANIFEST}, so remove that"
            )));
        }
        (Some(data), None) => (Format::Plugin, data),
        (None, Some(data)) => (Format::Legacy, data),
        (None, None) => {
            return Err(Problem::new(format!(
                "the rookery target has no {MANIFEST}"
            )));
        }
    };
    let value: Value = serde_json::from_slice(&data)
        .map_err(|error| Problem::in_file(format.file(), format!("not JSON: {error}")))?;
    if !value.is_object() {
        return Err(Problem::in_file(format.file(), "not a JSON object"));
    }
    let manifest = match format {
        Format::Plugin => value,
        Format::Legacy => upgrade(&value),
    };
    Ok(Read { format, manifest })
}

/// Reads the manifest of a target's folder.
pub fn read_dir(dir: &Path) -> Result<Read, Problem> {
    read(|name| std::fs::read(dir.join(name)).ok())
}

/// The plugin ids Rookery keeps for itself: `rookery`, under which its own settings, secrets and
/// data are kept, and `sign-in`, whose actions are its own.
pub const RESERVED_IDS: &[&str] = &["rookery", "sign-in"];

/// Whether a plugin id is one a package may have.
pub fn valid_plugin_id(id: &str) -> bool {
    valid_name(id) && !RESERVED_IDS.contains(&id)
}

/// A name that is safe as a directory name and inside a remote command,
/// such as the id of a plugin or of one of its features.
pub fn valid_name(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_lowercase())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}

/// What Rookery tells Hatchery about itself: it installs the `rookery`
/// target, answers `rookery` with its version, and checks the target's
/// files against its contract.
pub struct RookeryTarget;

impl hatchery::Target for RookeryTarget {
    fn name(&self) -> &str {
        TARGET
    }

    fn fact(&self, name: &str) -> Option<String> {
        (name == TARGET).then(|| crate::VERSION.to_owned())
    }

    fn check(&self, files: &TargetFiles) -> std::result::Result<(), Vec<Problem>> {
        let problems = check_files(files);
        match problems.is_empty() {
            true => Ok(()),
            false => Err(problems),
        }
    }
}

/// The JSON Schema of `plugin.json`, as `schemas/plugin.schema.json` in the
/// repository publishes it.
pub const SCHEMA: &str = include_str!("../../../schemas/plugin.schema.json");

/// The JSON Schema of `rookery.json`, as `schemas/rookery-plugin.schema.json`
/// in the repository publishes it.
pub const LEGACY_SCHEMA: &str = include_str!("../../../schemas/rookery-plugin.schema.json");

/// Words in a task file that would make it depend on the role's own files
/// or on where the playbook is, which a plugin must not.
const ROLE_PATHS: &[&str] = &["role_path", "playbook_dir", "inventory_dir"];

/// What is wrong with the files of a package's `rookery` target.
pub fn check_files(files: &TargetFiles) -> Vec<Problem> {
    let read = match read(|name| files.get(name).map(<[u8]>::to_vec)) {
        Ok(read) => read,
        Err(problem) => return vec![problem],
    };
    let file = read.format.file();
    let mut problems = match read.format {
        Format::Plugin => schema_problems(SCHEMA, &read.manifest, file),
        Format::Legacy => {
            let original: Value = files
                .get(LEGACY_MANIFEST)
                .and_then(|data| serde_json::from_slice(data).ok())
                .unwrap_or_default();
            schema_problems(LEGACY_SCHEMA, &original, file)
        }
    };
    let has = |path: &str| files.get(path.trim_start_matches("./")).is_some();
    problems.extend(manifest_problems(&read, &has));
    for path in files.paths() {
        if !(path.ends_with(".yml") || path.ends_with(".yaml")) {
            continue;
        }
        let text = String::from_utf8_lossy(files.get(path).unwrap_or_default());
        for word in ROLE_PATHS {
            if text.contains(word) {
                problems.push(Problem::in_file(
                    path,
                    format!("uses {word}: a plugin's tasks use only their own files, through rookery_plugin_dir, and not the role's"),
                ));
            }
        }
    }
    problems
}

/// What is wrong with a manifest beyond its schema: what it names that is
/// not in the target's folder, ids declared twice, and offers and ranges
/// that point nowhere. `has` says whether a path is in the folder.
pub fn manifest_problems(read: &Read, has: &dyn Fn(&str) -> bool) -> Vec<Problem> {
    let file = read.format.file();
    let manifest = &read.manifest;
    let problem = |message: String| Problem::in_file(file, message);
    let mut problems = vec![];
    let parts = read.host_parts();
    let provisioners: Vec<&str> = parts["provisioners"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|provisioner| provisioner["id"].as_str())
        .collect();
    let (feature_word, provisioner_word) = match read.format {
        Format::Plugin => ("host feature", "provisioner"),
        Format::Legacy => ("feature", "provisioner"),
    };
    let mut features: Vec<&str> = vec![];
    for feature in parts["features"].as_array().into_iter().flatten() {
        let id = feature["id"].as_str().unwrap_or("");
        if features.contains(&id) {
            problems.push(problem(format!(
                "the {feature_word} {id} is declared twice"
            )));
        }
        features.push(id);
        for key in ["tasks", "guide"] {
            if let Some(path) = feature[key].as_str()
                && !has(path)
            {
                problems.push(problem(format!(
                    "the {key} of the {feature_word} {id}, {path}, is not in the target's folder"
                )));
            }
        }
        for offer in feature["offers"].as_array().into_iter().flatten() {
            let offer = offer.as_str().unwrap_or("");
            if !provisioners.contains(&offer) {
                problems.push(problem(format!(
                    "the {feature_word} {id} offers {offer}, which is none of the plugin's provisioners"
                )));
            }
        }
    }
    let mut seen: Vec<&str> = vec![];
    for provisioner in parts["provisioners"].as_array().into_iter().flatten() {
        let id = provisioner["id"].as_str().unwrap_or("");
        if seen.contains(&id) {
            problems.push(problem(format!(
                "the {provisioner_word} {id} is declared twice"
            )));
        }
        seen.push(id);
        if let Some(command) = provisioner["command"].as_str()
            && !has(command)
        {
            problems.push(problem(format!(
                "the command of the {provisioner_word} {id}, {command}, is not in the target's folder"
            )));
        }
    }
    if read.format == Format::Legacy {
        return problems;
    }
    for (key, what) in [
        ("/host", "host half"),
        ("/client", "client half"),
        ("/styles", "client half's stylesheet"),
        ("/interface", "interface"),
        ("/contributes/shell/entry", "shell's entry"),
    ] {
        if let Some(path) = manifest.pointer(key).and_then(Value::as_str)
            && !has(path)
        {
            problems.push(problem(format!(
                "the {what}, {path}, is not in the target's folder"
            )));
        }
    }
    for key in ["dependencies", "optionalDependencies"] {
        for (id, range) in manifest[key].as_object().into_iter().flatten() {
            let range = range.as_str().unwrap_or("");
            if hatchery::semver::VersionReq::parse(range).is_err() {
                problems.push(problem(format!(
                    "{key}: {id} {range:?} is no version range, such as \"^1.0\""
                )));
            }
        }
    }
    for (key, what) in [
        ("views", "view"),
        ("commands", "command"),
        ("settings", "setting"),
        ("nav", "navigation entry"),
        ("adminPages", "admin page"),
        ("jobs", "job"),
        ("topics", "topic"),
    ] {
        let mut ids: Vec<&str> = vec![];
        for item in manifest
            .pointer(&format!("/contributes/{key}"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = item["id"].as_str().or(item["key"].as_str()) else {
                continue;
            };
            if ids.contains(&id) {
                problems.push(problem(format!("the {what} {id} is declared twice")));
            }
            ids.push(id);
        }
    }
    problems
}

/// What a schema says of a manifest, each problem at the place in the file.
pub fn schema_problems(schema: &str, manifest: &Value, file: &str) -> Vec<Problem> {
    let schema: Value = serde_json::from_str(schema).expect("the schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("the schema is a JSON Schema");
    validator
        .iter_errors(manifest)
        .map(|error| {
            let at = error.instance_path().to_string();
            let at = if at.is_empty() { "/".to_owned() } else { at };
            Problem::in_file(file, format!("{at}: {error}"))
        })
        .collect()
}

/// What is wrong with a package folder as a Rookery plugin: its
/// `package.kdl`, which has to have a `rookery` target that says which
/// Rookery versions it works with, and that target's files. It is packed
/// the way Hatchery packs it, without a build, so what passes here installs.
/// A `package.kdl` that cannot be read is the one problem.
pub fn check_dir(dir: &Path) -> Vec<String> {
    let manifest = match hatchery::Manifest::load(dir) {
        Ok(manifest) => manifest,
        Err(error) => return vec![error.to_string()],
    };
    let mut problems: Vec<String> = vec![];
    let Some(target) = manifest.target(TARGET) else {
        return vec![format!("{} has no {TARGET} target", manifest.name())];
    };
    let id = &manifest.package.id;
    if !valid_plugin_id(id) {
        problems.push(format!(
            "package.kdl: {id} is no plugin id: lower-case letters, digits, - and _, starting with a letter, and neither rookery nor sign-in"
        ));
    }
    if target
        .manifest
        .as_deref()
        .is_some_and(|name| name != MANIFEST && name != LEGACY_MANIFEST)
    {
        problems.push(format!(
            "package.kdl: the rookery target's manifest is {MANIFEST}, which it does not have to name"
        ));
    }
    if !target.requires.iter().any(|line| line.fact == TARGET) {
        problems.push(
            "package.kdl: the rookery target needs a requires line that says which Rookery it works with, such as rookery \">=0.6\"".into(),
        );
    }
    for line in target.requires.iter().filter(|line| line.fact == TARGET) {
        for range in &line.values {
            if hatchery::semver::VersionReq::parse(range).is_err() {
                problems.push(format!(
                    "package.kdl: rookery {range:?} is no version range, such as \">=0.6\""
                ));
            }
        }
    }
    // Packing reads the folder as an egg holds it and runs the checks of
    // its files; a build would be the publisher's to run.
    match hatchery::Egg::pack(dir, &[&RookeryTarget]) {
        Ok(_) => {}
        Err(hatchery::Error::Check {
            problems: found, ..
        }) => {
            problems.extend(found.iter().map(|problem| match &problem.path {
                Some(path) => format!("{}/{path}: {}", target.dir(), problem.message),
                None => problem.message.clone(),
            }));
        }
        Err(error) => problems.push(error.to_string()),
    }
    problems
}
