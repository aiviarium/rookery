//! The user's own plugins in rookery's store. They are installed as eggs in
//! `plugins/` of the configuration directory, which Ansible, the builder and
//! the provisioners use as it is. The store holds what rebuilds that folder
//! (rows of kind `plugin` in the installation's own scope):
//!
//! - `hatchery.kdl`, the registries they are installed from
//! - `eggs.lock`, what is installed and from where, with each checksum
//! - `seen.toml`, when each version was first seen, which the minimum
//!   release age counts from
//! - `upload/NAME` for each egg that was sent and that the lock names
//!
//! [`remember`] writes them after every change rookery makes to the folder.
//! [`restore`] puts back what the folder lacks of them, as on a new machine
//! restored from the store, and Hatchery installs what the lock names;
//! [`plan`] is what it decides, without installing.

use crate::Result;
use crate::eggs::{self, UPLOADS, valid_upload};
use crate::paths::Paths;
use crate::store::{OWN, Store, hold_lock, kind};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const CONFIG: &str = eggs::CONFIG;
pub const LOCK: &str = hatchery::lock::FILE;
pub const SEEN: &str = "seen.toml";
const UPLOAD: &str = "upload/";
/// A row an earlier rookery wrote, when it installed plugins with npm.
const NPM_MANIFEST: &str = "package.json";

/// Where Hatchery keeps when it first saw each version, in an install folder.
fn seen_file(own: &Path) -> PathBuf {
    own.join(".hatchery").join(SEEN)
}

/// The names of the eggs that were sent that a lock file names.
fn uploads_named(lock: &[u8]) -> Vec<String> {
    let Ok(lock) = toml::from_slice::<toml::Value>(lock) else {
        return vec![];
    };
    lock.get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|package| package.get("file")?.as_str())
        .filter_map(|file| {
            let path = Path::new(file);
            (path.parent()?.file_name()? == UPLOADS).then(|| path.file_name()?.to_str())?
        })
        .filter(|name| valid_upload(name))
        .map(str::to_owned)
        .collect()
}

/// A lock file whose eggs that were sent are found in `own`'s folder of
/// them, wherever they were kept on the machine that installed them. It
/// leaves out what came with the rookery that installed them: this
/// machine's rookery installs its own (see [`eggs::builtins`]).
fn lock_here(lock: &[u8], own: &Path) -> Vec<u8> {
    let Ok(mut parsed) = toml::from_slice::<toml::Table>(lock) else {
        return lock.to_vec();
    };
    if let Some(packages) = parsed
        .get_mut("package")
        .and_then(toml::Value::as_array_mut)
    {
        packages.retain(|package| {
            package.get("registry").and_then(toml::Value::as_str) != Some(crate::builtin::NAME)
        });
        for package in packages.iter_mut().filter_map(toml::Value::as_table_mut) {
            let Some(name) = package
                .get("file")
                .and_then(toml::Value::as_str)
                .map(Path::new)
                .filter(|path| path.parent().and_then(Path::file_name) == Some(UPLOADS.as_ref()))
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .filter(|name| valid_upload(name))
                .map(str::to_owned)
            else {
                continue;
            };
            package.insert(
                "file".into(),
                eggs::uploads(own).join(name).display().to_string().into(),
            );
        }
    }
    toml::to_string(&parsed)
        .map(String::into_bytes)
        .unwrap_or_else(|_| lock.to_vec())
}

/// The packages a lock file names whose folders `own` lacks.
fn missing(lock: &[u8], own: &Path) -> Vec<String> {
    let Ok(lock) = toml::from_slice::<toml::Value>(lock) else {
        return vec![];
    };
    lock.get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|package| package.get("name")?.as_str())
        .filter(|name| {
            hatchery::Name::parse(name).is_ok_and(|name| {
                !own.join(name.dir())
                    .join(hatchery::manifest::FILE)
                    .is_file()
            })
        })
        .map(str::to_owned)
        .collect()
}

/// The rows the store keeps of the user's plugins, by name.
pub fn snapshot(paths: &Paths) -> Result<Vec<(String, Vec<u8>)>> {
    let own = eggs::own_dir(&paths.config_dir);
    let mut rows = vec![];
    for (name, file) in [
        (CONFIG, eggs::config_file(paths)),
        (LOCK, own.join(LOCK)),
        (SEEN, seen_file(&own)),
    ] {
        match fs::read(&file) {
            Ok(data) => rows.push((name.to_owned(), data)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", file.display()).into()),
        }
    }
    let lock = rows
        .iter()
        .find(|(name, _)| name == LOCK)
        .map(|(_, data)| data.clone())
        .unwrap_or_default();
    for name in uploads_named(&lock) {
        if let Ok(data) = fs::read(eggs::uploads(&own).join(&name)) {
            rows.push((format!("{UPLOAD}{name}"), data));
        }
    }
    Ok(rows)
}

/// Write the user's plugins as they are now to the store, in place of what
/// the store had of them.
pub fn remember(paths: &Paths) -> Result<()> {
    let rows = snapshot(paths)?;
    let items: Vec<(&str, &str, Vec<u8>)> = rows
        .iter()
        .map(|(name, value)| (kind::PLUGIN, name.as_str(), value.clone()))
        .collect();
    let store = crate::store::shared(paths)?;
    store.replace(OWN, &[kind::PLUGIN], &items)?;
    Ok(())
}

/// What [`restore`] would do.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Files to write, by their whole path; none of them is there.
    pub files: Vec<(PathBuf, Vec<u8>)>,
    /// The packages of the lock file whose folders are not there, which
    /// Hatchery installs.
    pub install: Vec<String>,
    /// The store has plugins an earlier rookery installed with npm.
    pub npm: bool,
}

/// What the user's plugins lack of what the store has, and how to put it
/// back; `None` when they lack nothing. Nothing that is there is
/// overwritten or removed. Only the names of the store's rows are read
/// unless there is something to put back.
pub fn plan(paths: &Paths, store: &Store) -> Result<Option<Plan>> {
    let names = store.names(kind::PLUGIN, OWN)?;
    if names.is_empty() {
        return Ok(None);
    }
    let own = eggs::own_dir(&paths.config_dir);
    let read = |name: &str| store.read(kind::PLUGIN, OWN, name);
    let has = |name: &str| names.iter().any(|known| known == name);
    let mut plan = Plan {
        npm: has(NPM_MANIFEST),
        ..Plan::default()
    };
    for (name, file) in [(CONFIG, eggs::config_file(paths)), (SEEN, seen_file(&own))] {
        if has(name)
            && !file.exists()
            && let Some(data) = read(name)?
        {
            plan.files.push((file, data));
        }
    }
    let here = own.join(LOCK);
    let lock = match fs::read(&here) {
        Ok(data) => Some(data),
        Err(_) if has(LOCK) => {
            let stored = read(LOCK)?.map(|data| lock_here(&data, &own));
            if let Some(data) = &stored {
                plan.files.push((here.clone(), data.clone()));
            }
            stored
        }
        Err(_) => None,
    };
    let lock = lock.unwrap_or_default();
    for name in uploads_named(&lock) {
        let file = eggs::uploads(&own).join(&name);
        if !file.exists()
            && has(&format!("{UPLOAD}{name}"))
            && let Some(data) = read(&format!("{UPLOAD}{name}"))?
        {
            plan.files.push((file, data));
        }
    }
    plan.install = missing(&lock, &own);
    let nothing = plan.files.is_empty() && plan.install.is_empty() && !plan.npm;
    Ok((!nothing).then_some(plan))
}

/// Do what a plan says. Returns what to tell the user; plugins that cannot
/// be installed are one of those, not an error.
pub fn carry_out(paths: &Paths, plan: &Plan) -> Result<Vec<String>> {
    use std::os::unix::fs::DirBuilderExt;
    let own = eggs::own_dir(&paths.config_dir);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&own)?;
    let mut notices = vec![];
    for (path, data) in &plan.files {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        // Never over a file that is there.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut out) => out.write_all(data)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("{}: {error}", path.display()).into()),
        }
    }
    if plan.npm {
        notices.push(
            "rookery: rookery's store has plugins an earlier rookery installed with npm. Plugins are Hatchery eggs now, and those are not installed: add them again with rookery plugin add".into(),
        );
    }
    if plan.install.is_empty() {
        return Ok(notices);
    }
    let installed =
        eggs::client(paths).and_then(|client| Ok(hatchery::block_on(client.install(&own))?));
    notices.push(match installed {
        Ok(_) => format!(
            "rookery: installed the plugins of rookery's store in {}",
            own.display()
        ),
        Err(error) => format!(
            "rookery: the plugins of rookery's store could not be installed, and rookery goes on without them: {error}"
        ),
    });
    Ok(notices)
}

/// Put back what the user's plugins lack of the store's. Costs a look at
/// the store's names when they lack nothing. Another rookery doing the same
/// meanwhile is left to it.
pub fn restore(paths: &Paths) -> Result<Vec<String>> {
    let store = crate::store::shared(paths)?;
    if plan(paths, &store)?.is_none() {
        return Ok(vec![]);
    }
    let Some(_lock) = hold_lock(&paths.data_dir.join("locks/plugins.lock"))? else {
        return Ok(vec![]);
    };
    // Again, now that no other rookery does it.
    let Some(plan) = plan(paths, &store)? else {
        return Ok(vec![]);
    };
    let notices = carry_out(paths, &plan)?;
    // The rows of npm are said once, and then the store keeps what is here.
    if plan.npm {
        remember(paths)?;
    }
    Ok(notices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        (dir, paths)
    }

    fn store(paths: &Paths) -> std::sync::Arc<Store> {
        crate::store::shared(paths).unwrap()
    }

    fn put(store: &Store, rows: &[(&str, &[u8])]) {
        let items: Vec<_> = rows
            .iter()
            .map(|(name, value)| (kind::PLUGIN, *name, value.to_vec()))
            .collect();
        store.replace(OWN, &[kind::PLUGIN], &items).unwrap();
    }

    /// A lock file of one package from a registry and one from an egg that
    /// was sent, kept under `own`.
    fn locked(own: &Path) -> String {
        format!(
            r#"format = 1

[[package]]
name = "example/hello"
version = "1.0.0"
targets = ["rookery"]
registry = "https://eggs.example.org"
sha256 = "aa"

[[package]]
name = "example/sent"
version = "2.0.0"
targets = ["rookery"]
file = "{}"
sha256 = "bb"
"#,
            eggs::uploads(own)
                .join("example.sent-2.0.0-0123456789ab.egg")
                .display()
        )
    }

    #[test]
    fn a_snapshot_keeps_what_rebuilds_the_folder() {
        let (_dir, paths) = scratch();
        let own = eggs::own_dir(&paths.config_dir);
        fs::create_dir_all(eggs::uploads(&own)).unwrap();
        fs::create_dir_all(own.join(".hatchery")).unwrap();
        fs::write(eggs::config_file(&paths), "registries {\n}\n").unwrap();
        fs::write(own.join(LOCK), locked(&own)).unwrap();
        fs::write(seen_file(&own), "format = 1\n").unwrap();
        let uploads = eggs::uploads(&own);
        fs::write(uploads.join("example.sent-2.0.0-0123456789ab.egg"), "sent").unwrap();
        fs::write(uploads.join("example.old-1.0.0-0123456789ab.egg"), "older").unwrap();
        remember(&paths).unwrap();
        let store = crate::store::shared(&paths).unwrap();
        assert_eq!(
            store.names(kind::PLUGIN, OWN).unwrap(),
            [
                "eggs.lock",
                "hatchery.kdl",
                "seen.toml",
                "upload/example.sent-2.0.0-0123456789ab.egg"
            ]
        );

        // Elsewhere the plan puts it all back, with the egg that was sent
        // where that machine keeps them, and installs both packages.
        let (_other, elsewhere) = scratch();
        let fresh = eggs::own_dir(&elsewhere.config_dir);
        let plan = plan(&elsewhere, &store).unwrap().unwrap();
        let files: Vec<_> = plan.files.iter().map(|(file, _)| file.clone()).collect();
        assert_eq!(
            files,
            [
                eggs::config_file(&elsewhere),
                seen_file(&fresh),
                fresh.join(LOCK),
                eggs::uploads(&fresh).join("example.sent-2.0.0-0123456789ab.egg"),
            ]
        );
        let lock = String::from_utf8(plan.files[2].1.clone()).unwrap();
        assert!(
            lock.contains(&eggs::uploads(&fresh).display().to_string()),
            "{lock}"
        );
        assert_eq!(plan.install, ["example/hello", "example/sent"]);
        assert!(!plan.npm);
    }

    #[test]
    fn a_folder_that_has_it_all_needs_nothing() {
        let (_dir, paths) = scratch();
        let store = store(&paths);
        let own = eggs::own_dir(&paths.config_dir);
        assert_eq!(plan(&paths, &store).unwrap(), None);
        let lock = r#"[[package]]
name = "example/hello"
version = "1.0.0"
targets = ["rookery"]
registry = "https://eggs.example.org"
sha256 = "aa"
"#;
        put(&store, &[(LOCK, lock.as_bytes())]);
        assert_eq!(
            plan(&paths, &store).unwrap().unwrap().install,
            ["example/hello"]
        );
        fs::create_dir_all(own.join("example/hello")).unwrap();
        fs::write(own.join("example/hello/package.kdl"), "").unwrap();
        fs::write(own.join(LOCK), lock).unwrap();
        assert_eq!(plan(&paths, &store).unwrap(), None);
        // A lock that is there is the folder's own business.
        fs::write(own.join(LOCK), "format = 1\n").unwrap();
        assert_eq!(plan(&paths, &store).unwrap(), None);
    }

    #[test]
    fn rows_of_a_shared_store_cannot_write_outside_the_folder() {
        let (dir, paths) = scratch();
        let store = store(&paths);
        let own = eggs::own_dir(&paths.config_dir);
        let lock = r#"[[package]]
name = "example/a"
version = "1.0.0"
targets = ["rookery"]
file = "/elsewhere/.uploads/../../../escaped.egg"
sha256 = "aa"

[[package]]
name = "example/b"
version = "1.0.0"
targets = ["rookery"]
file = "/elsewhere/.uploads/-x.egg"
sha256 = "bb"

[[package]]
name = "example/c"
version = "1.0.0"
targets = ["rookery"]
file = "/elsewhere/.uploads/c.egg"
sha256 = "cc"
"#;
        put(
            &store,
            &[
                (LOCK, lock.as_bytes()),
                ("upload/../../../escaped.egg", b"bad"),
                ("upload/-x.egg", b"bad"),
                ("upload/c.egg", b"good"),
            ],
        );
        let plan = plan(&paths, &store).unwrap().unwrap();
        let files: Vec<_> = plan.files.iter().map(|(file, _)| file.clone()).collect();
        assert_eq!(files, [own.join(LOCK), eggs::uploads(&own).join("c.egg")]);
        assert!(!dir.path().join("escaped.egg").exists());
        assert!(valid_upload("example.c-1.0.0-0123456789ab.egg"));
        for bad in ["", "../x.egg", "a/b.egg", ".x.egg", "-x.egg", "x.tgz"] {
            assert!(!valid_upload(bad), "{bad}");
        }
    }

    #[test]
    fn plugins_npm_installed_are_named_once() {
        let (_dir, paths) = scratch();
        let store = crate::store::shared(&paths).unwrap();
        put(
            &store,
            &[(NPM_MANIFEST, b"{}"), ("package-lock.json", b"{}")],
        );
        let said = restore(&paths).unwrap();
        assert!(said[0].contains("add them again"), "{said:?}");
        assert!(store.names(kind::PLUGIN, OWN).unwrap().is_empty());
        assert!(restore(&paths).unwrap().is_empty());
    }
}
