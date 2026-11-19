//! The tests of the plugin contract, [`rookery_api::plugin_contract`], on
//! the plugins that come with Rookery and on packages that break it.

#[cfg(test)]
mod tests {
    use rookery_api::plugin_contract::{TARGET, check_dir};
    use std::fs;
    use std::path::Path;

    #[test]
    fn the_shipped_plugins_keep_the_contract() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/rookery");
        let mut checked = 0;
        for entry in fs::read_dir(&root).unwrap().flatten() {
            let dir = entry.path();
            if !dir.join("package.kdl").is_file() {
                continue;
            }
            let problems = check_dir(&dir);
            assert!(problems.is_empty(), "{}: {problems:?}", dir.display());
            // The role finds a shipped plugin's files in its rookery folder.
            let manifest = hatchery::Manifest::load(&dir).unwrap();
            assert_eq!(manifest.target(TARGET).unwrap().path, None);
            checked += 1;
        }
        assert_eq!(checked, 5);
    }

    #[test]
    fn a_package_that_breaks_the_contract_says_where() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("bad");
        crate::plugins::tests::write_package(
            &dir,
            "bad",
            "1.0.0",
            None,
            r#"{"id": "old", "features": [
                {"id": "one", "tasks": "tasks/one.yml", "guide": "none.md", "offers": ["vm"],
                 "actions": [{"name": "go"}]},
                {"id": "one"}],
              "provisioners": [{"id": "vm", "command": "bin/none", "protocol": 2}]}"#,
        );
        fs::create_dir_all(dir.join("rookery/tasks")).unwrap();
        fs::write(
            dir.join("rookery/tasks/one.yml"),
            "- ansible.builtin.include_tasks: \"{{ role_path }}/tasks/npm-agent.yml\"\n",
        )
        .unwrap();
        let problems = check_dir(&dir).join("\n");
        for said in [
            "rookery/rookery.json: /: Additional properties are not allowed ('id' was unexpected)",
            "/features/0/actions/0: \"run\" is a required property",
            "the guide of the feature one, none.md, is not in the target's folder",
            "the feature one is declared twice",
            "the command of the provisioner vm, bin/none, is not in the target's folder",
            "rookery/tasks/one.yml: uses role_path",
        ] {
            assert!(problems.contains(said), "{said}\n{problems}");
        }
        assert!(!problems.contains("offers vm"), "{problems}");

        let silent = base.path().join("silent");
        crate::plugins::tests::write_package(&silent, "silent", "1.0.0", None, "{}");
        let kdl = fs::read_to_string(silent.join("package.kdl")).unwrap();
        fs::write(
            silent.join("package.kdl"),
            kdl.replace("rookery \">=0.6\"", "os linux"),
        )
        .unwrap();
        let problems = check_dir(&silent);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("which Rookery it works with"));

        let crow = base.path().join("crow");
        fs::create_dir_all(&crow).unwrap();
        fs::write(
            crow.join("package.kdl"),
            "package {\n  group example\n  id crow\n  version \"1.0.0\"\n}\ntargets {\n  crow\n}\n",
        )
        .unwrap();
        assert_eq!(check_dir(&crow), ["example/crow has no rookery target"]);
    }

    /// A package with a `plugin.json` at the given path in its target.
    fn plugin_json(base: &Path, id: &str, manifest: &str) -> std::path::PathBuf {
        let dir = base.join(id);
        let target = crate::plugins::tests::write_package(&dir, id, "1.0.0", None, "{}");
        fs::remove_file(target.join("rookery.json")).unwrap();
        fs::write(target.join("plugin.json"), manifest).unwrap();
        dir
    }

    #[test]
    fn a_plugin_json_that_breaks_the_contract_says_where() {
        let base = tempfile::tempdir().unwrap();
        let dir = plugin_json(
            base.path(),
            "bad",
            r#"{"apiVersion": "1", "name": "Bad", "id": "bad", "host": "host.js",
                "client": "client/index.js", "dependencies": {"other": "not a range"},
                "contributes": {
                  "hostFeatures": [{"id": "one", "offers": ["vm"]}, {"id": "one"}],
                  "provisioners": [{"id": "pod", "command": "bin/none", "protocol": 2}],
                  "shell": {"entry": "shell/index.html"},
                  "environmentProviders": []}}"#,
        );
        let problems = check_dir(&dir).join("\n");
        for said in [
            "rookery/plugin.json: /: Additional properties are not allowed ('id' was unexpected)",
            "/contributes: Additional properties are not allowed ('environmentProviders' was unexpected)",
            "the host feature one is declared twice",
            "the host feature one offers vm, which is none of the plugin's provisioners",
            "the command of the provisioner pod, bin/none, is not in the target's folder",
            "the host half, host.js, is not in the target's folder",
            "the client half, client/index.js, is not in the target's folder",
            "the shell's entry, shell/index.html, is not in the target's folder",
            "dependencies: other \"not a range\" is no version range",
        ] {
            assert!(problems.contains(said), "{said}\n{problems}");
        }

        let both = plugin_json(
            base.path(),
            "both",
            r#"{"apiVersion": "1", "name": "Both"}"#,
        );
        fs::write(both.join("rookery/rookery.json"), "{}").unwrap();
        let problems = check_dir(&both);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("has both plugin.json and rookery.json")),
            "{problems:?}"
        );

        let good = plugin_json(
            base.path(),
            "good",
            r#"{"apiVersion": "1", "name": "Good", "contributes": {"hostFeatures": [{"id": "cli"}]}}"#,
        );
        assert_eq!(check_dir(&good), Vec::<String>::new());
    }

    #[test]
    fn both_manifests_give_the_host_core_the_same_features() {
        let base = tempfile::tempdir().unwrap();
        let features = r#"{"features": [{"id": "cli", "label": "CLI", "commands": ["x"]}]}"#;
        let old = base.path().join("old");
        crate::plugins::tests::write_package(&old, "old", "1.0.0", Some("Old"), features);
        let new = plugin_json(
            base.path(),
            "new",
            r#"{"apiVersion": "1", "name": "New",
                "contributes": {"hostFeatures": [{"id": "cli", "label": "CLI", "commands": ["x"]}]}}"#,
        );
        let read = |dir: &Path| crate::plugins::read(dir).unwrap();
        let (old, new) = (read(&old), read(&new));
        assert_eq!(
            (old.problem.as_deref(), new.problem.as_deref()),
            (None, None)
        );
        let ids = |plugin: &crate::plugins::Plugin| {
            plugin
                .features
                .iter()
                .map(|f| (f.id.clone(), f.label.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&old), ids(&new));
        assert_eq!(ids(&new), [("cli".to_owned(), "CLI".to_owned())]);
    }
}
