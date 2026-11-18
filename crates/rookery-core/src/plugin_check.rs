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
}
