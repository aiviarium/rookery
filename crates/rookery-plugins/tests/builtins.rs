//! Rookery's own plugins load from their `plugin.json` and contribute what their `rookery.json` did. The
//! `rookery.json` each had before is kept in `fixtures/legacy/`, and loads as a legacy manifest too.

use std::fs;
use std::path::{Path, PathBuf};

use rookery_api::plugin_contract::{self, Format};
use serde_json::Value;

fn checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A package folder with the shipped package's `package.kdl` and files, but the old `rookery.json`.
fn legacy_copy(id: &str, into: &Path) -> PathBuf {
    let shipped = checkout().join("plugins/rookery").join(id);
    let dir = into.join(id);
    copy(&shipped, &dir);
    fs::remove_file(dir.join("rookery/plugin.json")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/legacy/{id}.json")),
        dir.join("rookery/rookery.json"),
    )
    .unwrap();
    dir
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy(&path, &target);
        } else if path.extension().is_none_or(|e| e != "egg") {
            fs::copy(&path, &target).unwrap();
        }
    }
}

/// What rookery-core makes of a plugin: its features with their actions, services and channels, and
/// its provisioners with their kinds, actions and collections.
fn contributions(package: &Path) -> Value {
    let plugin = rookery_core::plugins::read(package).expect("a plugin package");
    assert_eq!(plugin.problem, None, "{}", package.display());
    let mut info = serde_json::to_value(plugin.info()).unwrap();
    info.as_object_mut().unwrap().remove("dir");
    let hosting: Vec<String> = rookery_core::plugins::hosting(&plugin)
        .iter()
        .map(|h| format!("{h:?}"))
        .collect();
    serde_json::json!({"info": info, "hosting": hosting})
}

#[test]
fn each_shipped_plugin_contributes_what_its_rookery_json_did() {
    let scratch = tempfile::tempdir().unwrap();
    for id in ["backup", "docker", "github", "incus", "unity"] {
        let shipped = checkout().join("plugins/rookery").join(id);
        let read = plugin_contract::read_dir(&shipped.join("rookery")).unwrap();
        assert_eq!(read.format, Format::Plugin, "{id}");
        let legacy: Value = serde_json::from_str(
            &fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("tests/fixtures/legacy/{id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        // The host parts are the old manifest's, field for field.
        let mut parts = read.host_parts();
        if legacy.get("provisioners").is_none() {
            parts.as_object_mut().unwrap().remove("provisioners");
        }
        assert_eq!(parts, legacy, "{id}");
        // rookery-core reads the same features, actions and provisioners from both.
        let old = legacy_copy(id, scratch.path());
        assert_eq!(contributions(&shipped), contributions(&old), "{id}");
        // And the server's loader takes both, the old one as legacy.
        let new = rookery_plugins::loaded::load(&rookery_core::plugins::read(&shipped).unwrap());
        assert!(new.valid(), "{id}: {:?}", new.problem);
        let manifest = new.manifest.as_ref().unwrap();
        assert_eq!(manifest["id"], id);
        assert_eq!(manifest["contributes"]["hostFeatures"], legacy["features"]);
        assert_eq!(manifest["contributes"]["views"], serde_json::json!([]));
        let old = rookery_plugins::loaded::load(&rookery_core::plugins::read(&old).unwrap());
        assert!(old.valid(), "{id}: {:?}", old.problem);
        assert_eq!(old.format, Some(Format::Legacy));
        assert_eq!(
            old.manifest.as_ref().unwrap()["contributes"]["hostFeatures"],
            manifest["contributes"]["hostFeatures"],
            "{id}"
        );
    }
}

#[test]
fn a_plugin_json_with_problems_loads_as_invalid_and_says_each() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("bad");
    fs::create_dir_all(dir.join("rookery")).unwrap();
    fs::write(
        dir.join("package.kdl"),
        "format 1\npackage {\n  group example\n  id bad\n  version \"1.0.0\"\n}\ntargets {\n  rookery {\n    requires {\n      rookery \">=0.6\"\n    }\n  }\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("rookery/plugin.json"),
        r#"{"apiVersion": "1", "name": "Bad", "client": "client/index.js", "contributes": {"views": [{"id": "bad.main"}]}}"#,
    )
    .unwrap();
    let loaded = rookery_plugins::loaded::load(&rookery_core::plugins::read(&dir).unwrap());
    assert!(!loaded.valid());
    let problem = loaded.problem.unwrap();
    let problems = problem.details.unwrap()["problems"].clone();
    let said = problems.to_string();
    assert!(
        said.contains("\\\"title\\\" is a required property") || said.contains("title"),
        "{said}"
    );
    assert!(
        said.contains("the client half, client/index.js, is not in the target's folder"),
        "{said}"
    );
    assert!(
        problem
            .message
            .starts_with("plugin.json of bad is not valid:"),
        "{}",
        problem.message
    );
}
