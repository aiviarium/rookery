//! The contract of a Rookery plugin, for Rookery and for the CI of the
//! repositories that publish plugins: the JSON Schema of `rookery.json`, and
//! the checks Rookery makes of a package beyond it. Installing an egg runs
//! the checks of its files, and `rookery plugin check` runs all of them on a
//! package folder.

use hatchery::{Problem, TargetFiles};
use serde_json::Value;
use std::path::Path;

/// The target of a package that Rookery installs, and its folder in the package.
pub const TARGET: &str = "rookery";
/// The manifest of the target, in its folder.
pub const MANIFEST: &str = "rookery.json";

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

/// The JSON Schema of `rookery.json`, as `schemas/rookery-plugin.schema.json`
/// in the repository publishes it.
pub const SCHEMA: &str = include_str!("../../../schemas/rookery-plugin.schema.json");

/// Words in a task file that would make it depend on the role's own files
/// or on where the playbook is, which a plugin must not.
const ROLE_PATHS: &[&str] = &["role_path", "playbook_dir", "inventory_dir"];

/// What is wrong with the files of a package's `rookery` target.
pub fn check_files(files: &TargetFiles) -> Vec<Problem> {
    let Some(data) = files.get(MANIFEST) else {
        return vec![Problem::new(format!(
            "the rookery target has no {MANIFEST}"
        ))];
    };
    let manifest: Value = match serde_json::from_slice(data) {
        Ok(manifest) => manifest,
        Err(error) => return vec![Problem::in_file(MANIFEST, format!("not JSON: {error}"))],
    };
    let mut problems = schema_problems(&manifest);
    let has = |path: &str| files.get(path.trim_start_matches("./")).is_some();
    let mut features: Vec<&str> = vec![];
    for feature in manifest["features"].as_array().into_iter().flatten() {
        let id = feature["id"].as_str().unwrap_or("");
        if features.contains(&id) {
            problems.push(Problem::in_file(
                MANIFEST,
                format!("the feature {id} is declared twice"),
            ));
        }
        features.push(id);
        for key in ["tasks", "guide"] {
            if let Some(path) = feature[key].as_str()
                && !has(path)
            {
                problems.push(Problem::in_file(
                    MANIFEST,
                    format!("the {key} of the feature {id}, {path}, is not in the target's folder"),
                ));
            }
        }
        let provisioners: Vec<&str> = manifest["provisioners"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|provisioner| provisioner["id"].as_str())
            .collect();
        for offer in feature["offers"].as_array().into_iter().flatten() {
            let offer = offer.as_str().unwrap_or("");
            if !provisioners.contains(&offer) {
                problems.push(Problem::in_file(
                    MANIFEST,
                    format!("the feature {id} offers {offer}, which is none of the plugin's provisioners"),
                ));
            }
        }
    }
    let mut provisioners: Vec<&str> = vec![];
    for provisioner in manifest["provisioners"].as_array().into_iter().flatten() {
        let id = provisioner["id"].as_str().unwrap_or("");
        if provisioners.contains(&id) {
            problems.push(Problem::in_file(
                MANIFEST,
                format!("the provisioner {id} is declared twice"),
            ));
        }
        provisioners.push(id);
        if let Some(command) = provisioner["command"].as_str()
            && !has(command)
        {
            problems.push(Problem::in_file(
                MANIFEST,
                format!(
                    "the command of the provisioner {id}, {command}, is not in the target's folder"
                ),
            ));
        }
    }
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

/// What the schema says of a manifest.
fn schema_problems(manifest: &Value) -> Vec<Problem> {
    let schema: Value = serde_json::from_str(SCHEMA).expect("the schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("the schema is a JSON Schema");
    validator
        .iter_errors(manifest)
        .map(|error| {
            let at = error.instance_path().to_string();
            let at = if at.is_empty() { "/".to_owned() } else { at };
            Problem::in_file(MANIFEST, format!("{at}: {error}"))
        })
        .collect()
}

/// What is wrong with a package folder as a Rookery plugin: its
/// `package.kdl`, which has to have a `rookery` target that says which
/// Rookery versions it works with, and that target's files. It is packed
/// the way Hatchery packs it, without a build, so what passes here installs.
/// A `package.kdl` that cannot be read is the one problem.
/// The ids no plugin may have: `sign-in:NAME` are Rookery's own sign-ins, and `rookery` is the
/// server, whose secrets, settings and leases are kept under that id.
pub fn reserved(id: &str) -> bool {
    id == "sign-in" || id == "rookery" || id.starts_with("rookery.")
}

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
    if !valid_name(id) || reserved(id) {
        problems.push(format!(
            "package.kdl: {id} is no plugin id: lower-case letters, digits, - and _, starting with a letter, and not sign-in or rookery"
        ));
    }
    if target
        .manifest
        .as_deref()
        .is_some_and(|name| name != MANIFEST)
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
