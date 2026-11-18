//! The user's own plugins, installed as Hatchery eggs. They are installed in
//! `plugins/` of the configuration directory, the install folder Hatchery
//! keeps with its `eggs.lock`, from the registries `hatchery.kdl` beside it
//! lists. Every change goes through Hatchery, and the store is told of it
//! afterwards (see [`crate::plugin_state`]).

use crate::paths::Paths;
use crate::plugins::{self, Plugin, TARGET};
use crate::{Refused, Result};
use hatchery::{Client, Config, LockedPackage, Name, Options, RegistryConfig, Spec, block_on};
use rookery_api::{
    PluginList, PluginOrigin, PluginRegistries, PluginRegistry, PluginSpec, PluginUpdate,
    PluginUpdates,
};
use std::fs;
use std::path::{Path, PathBuf};

/// The registries of the user's plugins, in the configuration directory.
pub const CONFIG: &str = "hatchery.kdl";
/// The folder of the install folder that keeps the eggs that were sent.
pub const UPLOADS: &str = ".uploads";

pub use rookery_api::plugin_contract::RookeryTarget;

/// Where the user's own plugins are installed, under a configuration directory.
pub fn own_dir(config_dir: &Path) -> PathBuf {
    plugins::own_dir(config_dir)
}

/// The file that lists the registries plugins are installed from.
pub fn config_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join(CONFIG)
}

/// Where Hatchery caches the git repositories it reads.
pub fn cache_dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("cache/hatchery")
}

/// The folder of the eggs that were sent, in an install folder.
pub fn uploads(own: &Path) -> PathBuf {
    own.join(UPLOADS)
}

/// Whether a name is one Rookery gives an egg that was sent. A name from a
/// stored lock file or a row of a shared store that is not one would put a
/// file outside that folder.
pub fn valid_upload(name: &str) -> bool {
    name.len() <= 160
        && name.ends_with(".egg")
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
}

/// Where a locked package came from, and the registry, repository or file
/// to name for it.
pub fn origin(locked: &LockedPackage) -> (PluginOrigin, String) {
    if let Some(registry) = &locked.registry {
        return (PluginOrigin::Registry, registry.clone());
    }
    if let Some(git) = &locked.git {
        let mut source = git.clone();
        if let Some(path) = &locked.path {
            source.push_str(&format!(", {path}"));
        }
        if let Some(git_ref) = &locked.git_ref {
            source.push_str(&format!(", at {git_ref}"));
        }
        return (PluginOrigin::Git, source);
    }
    let file = locked
        .file
        .clone()
        .or(locked.folder.clone())
        .unwrap_or_default();
    // An egg that was sent is named by itself, not by where it is kept.
    let shown = match Path::new(&file).parent().and_then(Path::file_name) {
        Some(folder) if folder == UPLOADS => "an egg that was sent".to_owned(),
        _ => file,
    };
    (PluginOrigin::File, shown)
}

/// The registries, from `hatchery.kdl`; none when there is no such file.
pub fn config(paths: &Paths) -> Result<Config> {
    let file = config_file(paths);
    match file.is_file() {
        true => Ok(Config::load(&file)?),
        false => Ok(Config::empty()),
    }
}

/// A Hatchery client that installs Rookery's target from the configured
/// registries.
pub fn client(paths: &Paths) -> Result<Client> {
    Ok(Client::builder()
        .config(config(paths)?)
        .target(RookeryTarget)
        .fetch(hatchery::fetch::HttpFetch::new())
        .cache_dir(cache_dir(paths))
        .git(git())
        .build()?)
}

/// The git command Hatchery runs.
#[cfg(not(test))]
fn git() -> hatchery::Git {
    hatchery::Git::new()
}

/// The git command of the tests, which reads no configuration of the user.
#[cfg(test)]
fn git() -> hatchery::Git {
    tests::GIT_ENV
        .iter()
        .fold(hatchery::Git::new(), |git, (key, value)| {
            git.env(*key, *value)
        })
}

/// Write the plugins to the store; a line that says so when that failed, as
/// the change itself is done.
pub fn remembered(paths: &Paths) -> Option<String> {
    crate::plugin_state::remember(paths)
        .err()
        .map(|error| format!("✗ rookery's store does not have this change of the plugins: {error}"))
}

/// Remove the eggs that were sent that the lock no longer names.
fn forget_uploads(own: &Path) {
    let named: Vec<String> = hatchery::Lock::load(own)
        .map(|lock| {
            lock.packages
                .iter()
                .filter_map(|locked| locked.file.clone())
                .collect()
        })
        .unwrap_or_default();
    let Ok(entries) = fs::read_dir(uploads(own)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !named.iter().any(|file| Path::new(file) == path) {
            let _ = fs::remove_file(path);
        }
    }
}

/// What a report of Hatchery says, as lines.
fn said(report: &hatchery::Report) -> Vec<String> {
    let mut lines: Vec<String> = report
        .packages
        .iter()
        .map(|change| match &change.previous {
            Some(previous) if previous != &change.version => {
                format!("✓ {} {previous} → {}", change.name, change.version)
            }
            Some(_) => format!("✓ {} {} again", change.name, change.version),
            None => format!(
                "✓ Installed {} {} from {}",
                change.name, change.version, change.registry
            ),
        })
        .collect();
    lines.extend(
        report
            .warnings
            .iter()
            .map(|warning| format!("  ! {warning}")),
    );
    lines
}

/// Whether a git location is a folder of this machine rather than an address.
fn local_git(url: &str) -> bool {
    url.starts_with(['/', '.', '~']) || url.starts_with("file:") || !url.contains(':')
}

/// Where plugins are managed: this machine's own plugins, beside those of
/// the provisioning source.
pub struct Manager {
    paths: Paths,
    source: Option<PathBuf>,
    own: PathBuf,
}

impl Manager {
    pub fn new(paths: &Paths) -> Manager {
        Manager {
            paths: paths.clone(),
            source: paths.source().ok(),
            own: own_dir(&paths.config_dir),
        }
    }

    /// The folder the user's own plugins are installed in.
    pub fn own(&self) -> &Path {
        &self.own
    }

    /// Every plugin found, those Rookery cannot use among them.
    pub fn scan(&self) -> Vec<Plugin> {
        let mut dirs = vec![];
        if let Some(source) = &self.source {
            dirs.push(source.join("plugins"));
        }
        dirs.push(self.own.clone());
        plugins::scan(&dirs)
    }

    /// The plugins there are, and where the user's own are installed.
    pub fn list(&self) -> PluginList {
        PluginList {
            plugins: self.scan().iter().map(Plugin::info).collect(),
            dir: self.own.display().to_string(),
        }
    }

    /// One of the user's own plugins, by its id or package. One that comes
    /// with rookery is refused with `shipped`.
    fn own_plugin(&self, name: &str, shipped: &str) -> Result<Plugin> {
        let plugin = self
            .scan()
            .into_iter()
            .find(|plugin| plugin.id == name || plugin.package == name)
            .ok_or_else(|| format!("No plugin {name}. See: rookery plugin list"))?;
        if plugin.origin == PluginOrigin::Shipped {
            return Err(Refused(format!("{} {shipped}", plugin.label)).into());
        }
        Ok(plugin)
    }

    /// After a change: the lines for what Rookery cannot use of what was
    /// installed, the eggs that were sent and are no longer needed gone,
    /// and the store told.
    fn finish(&self, report: &hatchery::Report, lines: &mut Vec<String>) {
        let found = self.scan();
        for change in &report.packages {
            let name = change.name.to_string();
            if let Some(problem) = found
                .iter()
                .find(|plugin| plugin.package == name)
                .and_then(|plugin| plugin.problem.as_deref())
            {
                lines.push(format!(
                    "✗ {name} is installed, and Rookery does not use it: {problem}"
                ));
            }
        }
        forget_uploads(&self.own);
        lines.extend(remembered(&self.paths));
    }

    /// Install a plugin: a package of the plugin registries, the https
    /// address of an egg, or one folder of a git repository. A file or
    /// folder of a machine is sent instead, with [`Manager::upload`].
    pub fn add(&self, asked: &PluginSpec) -> Result<Vec<String>> {
        let spec = match &asked.git {
            Some(url) if local_git(url.trim()) => {
                return Err(Refused(format!(
                    "{} is a folder, and a server installs only what it can fetch: rookery plugin add sends a package folder of yours to it",
                    url.trim()
                ))
                .into());
            }
            Some(url) => Spec::git(url.trim(), asked.path.clone(), asked.git_ref.clone()),
            None => match Spec::parse(&asked.spec)? {
                Spec::File(file) if file.starts_with("https://") => Spec::File(file),
                Spec::File(_) | Spec::Folder(_) => {
                    return Err(Refused(format!(
                        "{} is a file or folder, and a server installs only what it can fetch: rookery plugin add sends a file or folder of yours to it",
                        asked.spec
                    ))
                    .into());
                }
                spec => spec,
            },
        };
        let client = client(&self.paths)?;
        fs::create_dir_all(&self.own)?;
        let options = Options::new().allow_young(asked.allow_young);
        let report = block_on(client.add_with(&spec, &self.own, &options))?;
        let mut lines = said(&report);
        if report.packages.is_empty() {
            lines.insert(0, "✓ It is installed already".into());
        }
        self.finish(&report, &mut lines);
        Ok(lines)
    }

    /// Install a plugin from an egg that was sent, as `hatchery pack` or
    /// `rookery plugin add` makes it. An egg of a version that is installed
    /// with other contents replaces it.
    pub fn upload(&self, data: &[u8]) -> Result<Vec<String>> {
        if !data.starts_with(&[0x1f, 0x8b]) {
            return Err(
                Refused("A plugin is sent as an egg, as hatchery pack makes it".into()).into(),
            );
        }
        let egg = hatchery::Egg::open(data.to_vec())?;
        let manifest = egg.manifest();
        let name = manifest.name();
        if manifest.target(TARGET).is_none() {
            return Err(Refused(format!("{name} has no {TARGET} target")).into());
        }
        let file = format!(
            "{}.{}-{}-{}.egg",
            name.group,
            name.id,
            manifest.version(),
            &egg.sha256()[..12]
        );
        if !valid_upload(&file) {
            return Err(Refused(format!(
                "{name} {} cannot be kept under that name",
                manifest.version()
            ))
            .into());
        }
        let dir = uploads(&self.own);
        fs::create_dir_all(&dir)?;
        let path = dir.join(&file);
        fs::write(&path, data)?;
        let client = client(&self.paths)?;
        let lock = hatchery::Lock::load(&self.own)?;
        if let Some(locked) = lock.get(&name)
            && &locked.version == manifest.version()
            && locked.sha256 != egg.sha256()
        {
            client.remove(&name, &self.own)?;
        }
        let report = block_on(client.add(&Spec::File(path.display().to_string()), &self.own))?;
        let mut lines: Vec<String> = said(&report)
            .into_iter()
            .filter(|line| !line.ends_with(" again"))
            .collect();
        match report.packages.first() {
            Some(change) if change.previous.is_none() => {
                lines[0] = format!(
                    "✓ Installed {name} {} from the egg that was sent",
                    manifest.version()
                );
            }
            Some(change) if change.previous.as_ref() != Some(&change.version) => {}
            _ => lines.insert(
                0,
                format!("✓ {name} {} is installed already", manifest.version()),
            ),
        }
        self.finish(&report, &mut lines);
        Ok(lines)
    }

    /// Remove one of the user's plugins by its id or package. One that
    /// comes with rookery stays.
    pub fn remove(&self, name: &str) -> Result<Vec<String>> {
        let plugin = self.own_plugin(name, "comes with rookery and stays")?;
        let client = client(&self.paths)?;
        client.remove(&Name::parse(&plugin.package)?, &self.own)?;
        let mut lines = vec![format!("✓ Removed {}", plugin.label)];
        forget_uploads(&self.own);
        lines.extend(remembered(&self.paths));
        Ok(lines)
    }

    /// Which of the user's plugins have a newer version that can be
    /// installed, as their registries say.
    pub fn check(&self) -> Result<PluginUpdates> {
        let own: Vec<Plugin> = self
            .scan()
            .into_iter()
            .filter(|plugin| plugin.origin != PluginOrigin::Shipped)
            .collect();
        let listing = match own
            .iter()
            .any(|plugin| plugin.origin == PluginOrigin::Registry)
        {
            true => Some(
                block_on(client(&self.paths)?.offered(&self.own))
                    .map_err(|error| error.to_string()),
            ),
            false => None,
        };
        let mut updates = vec![];
        for plugin in &own {
            let mut update = PluginUpdate {
                id: plugin.id.clone(),
                package: plugin.package.clone(),
                version: plugin.version.clone(),
                latest: None,
                note: String::new(),
            };
            match plugin.origin {
                PluginOrigin::Registry => match &listing {
                    Some(Ok(listing)) => {
                        match listing
                            .packages
                            .iter()
                            .find(|offered| offered.name.to_string() == plugin.package)
                        {
                            None => update.note = format!("{} no longer offers it", plugin.source),
                            Some(offered) => {
                                update.latest = offered.updatable.as_ref().map(ToString::to_string);
                                let installed = offered.installed.as_ref();
                                if let Some(newest) = offered.versions.first()
                                    && update.latest.is_none()
                                    && installed
                                        .is_some_and(|installed| &newest.version > installed)
                                {
                                    update.note = match (
                                        &newest.held_until,
                                        &newest.problem,
                                        &newest.withdrawn,
                                    ) {
                                        (_, _, Some(_)) => {
                                            format!("{} was withdrawn", newest.version)
                                        }
                                        (_, Some(problem), _) => format!(
                                            "{} cannot be installed here: {problem}",
                                            newest.version
                                        ),
                                        (Some(until), _, _) => format!(
                                            "{} waits for its minimum release age until {until}",
                                            newest.version
                                        ),
                                        _ => String::new(),
                                    };
                                }
                            }
                        }
                    }
                    Some(Err(error)) => {
                        update.note = format!("The registries could not be asked: {error}")
                    }
                    None => {}
                },
                PluginOrigin::Git => {
                    update.note = "From one folder of a git repository: an update takes the newest commit of its branch, and another tag is added again".into();
                }
                PluginOrigin::File | PluginOrigin::Shipped => {
                    update.note = "Installed from an egg file, which has no newer version to ask for: add the new one to replace it".into();
                }
            }
            updates.push(update);
        }
        Ok(PluginUpdates { plugins: updates })
    }

    /// Update one plugin, by its id or package, or all of them, to the
    /// newest versions that can be installed.
    pub fn update(&self, name: Option<&str>) -> Result<Vec<String>> {
        let one = match name {
            Some(name) => {
                let plugin = self.own_plugin(
                    name,
                    "comes with rookery and is updated with it: rookery update",
                )?;
                if plugin.origin == PluginOrigin::File {
                    return Err(Refused(format!(
                        "✗ {}: installed from an egg file, which has no newer version to ask for: add the new one to replace it",
                        plugin.id
                    ))
                    .into());
                }
                Some((Name::parse(&plugin.package)?, plugin))
            }
            None => None,
        };
        let client = client(&self.paths)?;
        if !self.own.join(hatchery::lock::FILE).is_file() {
            return Ok(vec!["✓ No plugin has a newer version".into()]);
        }
        let report = block_on(client.update(one.as_ref().map(|(name, _)| name), &self.own))?;
        let mut lines: Vec<String> = said(&report)
            .into_iter()
            .filter(|line| !line.ends_with(" again"))
            .collect();
        if !lines.iter().any(|line| line.starts_with('✓')) {
            lines.insert(
                0,
                match &one {
                    Some((_, plugin)) => {
                        format!("✓ {} {} is the latest", plugin.id, plugin.version)
                    }
                    None => "✓ No plugin has a newer version".into(),
                },
            );
        }
        self.finish(&report, &mut lines);
        Ok(lines)
    }

    /// The registries plugins are installed from, in search order.
    pub fn registries(&self) -> Result<PluginRegistries> {
        Ok(PluginRegistries {
            registries: config(&self.paths)?
                .registries
                .iter()
                .map(|registry| PluginRegistry {
                    kind: registry.kind.clone(),
                    location: registry.location.clone(),
                    only: registry.only.clone(),
                    key: registry.key.clone(),
                    min_age: registry.min_age.map(hatchery::time::format_duration),
                    unsigned_tags: registry.unsigned_tags,
                })
                .collect(),
            file: config_file(&self.paths).display().to_string(),
        })
    }

    /// Add a registry after the others, or change the one at the same location.
    pub fn add_registry(&self, asked: &PluginRegistry) -> Result<Vec<String>> {
        if !["git", "http", "dir"].contains(&asked.kind.as_str()) {
            return Err(Refused(format!(
                "A plugin registry is git, http or dir, not {:?}",
                asked.kind
            ))
            .into());
        }
        let location = asked.location.trim();
        if location.is_empty() || location.starts_with('-') || location.contains(char::is_control) {
            return Err(Refused(format!("{location:?} is no address of a registry")).into());
        }
        let mut registry = RegistryConfig::new(asked.kind.clone(), location);
        for group in &asked.only {
            hatchery::name::validate_segment("group", group)?;
        }
        registry.only = asked.only.clone();
        if let Some(key) = asked.key.as_deref().filter(|key| !key.is_empty()) {
            hatchery::sign::parse_verifying_key(key)?;
            registry.key = Some(key.to_owned());
        }
        if let Some(age) = asked.min_age.as_deref().filter(|age| !age.is_empty()) {
            registry.min_age = Some(hatchery::time::parse_duration(age)?);
        }
        registry.unsigned_tags = asked.unsigned_tags;
        let mut config = config(&self.paths)?;
        let known = config
            .registries
            .iter()
            .any(|known| known.location == location);
        config.add_registry(registry);
        fs::create_dir_all(&self.paths.config_dir)?;
        config.save(&config_file(&self.paths))?;
        let mut lines = vec![match known {
            true => format!("✓ Changed the plugin registry {location}"),
            false => format!("✓ Added the plugin registry {location}"),
        }];
        lines.extend(remembered(&self.paths));
        Ok(lines)
    }

    /// Remove the registry at a location. What was installed from it stays,
    /// and is not updated.
    pub fn remove_registry(&self, location: &str) -> Result<Vec<String>> {
        let mut config = config(&self.paths)?;
        if !config.remove_registry(location) {
            return Err(format!(
                "No plugin registry {location}. See: rookery plugin registry list"
            )
            .into());
        }
        config.save(&config_file(&self.paths))?;
        let mut lines = vec![format!("✓ Removed the plugin registry {location}")];
        let from: Vec<String> = self
            .scan()
            .into_iter()
            .filter(|plugin| plugin.origin == PluginOrigin::Registry && plugin.source == location)
            .map(|plugin| plugin.id)
            .collect();
        if !from.is_empty() {
            lines.push(format!(
                "  ! {} came from it, and stay installed. They cannot be updated or installed again until it is added again",
                from.join(", ")
            ));
        }
        lines.extend(remembered(&self.paths));
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Keeps git from reading the user's configuration, with a fixed identity.
    pub(super) const GIT_ENV: [(&str, &str); 7] = [
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_AUTHOR_NAME", "Test"),
        ("GIT_AUTHOR_EMAIL", "test@example.org"),
        ("GIT_COMMITTER_NAME", "Test"),
        ("GIT_COMMITTER_EMAIL", "test@example.org"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ];

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .envs(GIT_ENV)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A package of the rookery target, as a registry keeps it.
    fn package(repo: &Path, id: &str, version: &str, rookery: &str) {
        let dir = repo.join(id);
        fs::create_dir_all(dir.join("rookery/tasks")).unwrap();
        fs::write(
            dir.join("package.kdl"),
            format!(
                "format 1\npackage {{\n  group example\n  id {id}\n  version \"{version}\"\n}}\nabout {{\n  name \"{id} plugin\"\n}}\ntargets {{\n  rookery {{\n    requires {{\n      rookery \"{rookery}\"\n    }}\n  }}\n}}\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("rookery/rookery.json"),
            r#"{"features": [{"id": "tool", "label": "Tool", "tasks": "tasks/tool.yml"}]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("rookery/tasks/tool.yml"),
            "- ansible.builtin.debug: {msg: hi}\n",
        )
        .unwrap();
    }

    /// Commit the repository and tag each package at its version.
    fn release(repo: &Path, tags: &[&str]) {
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "release"]);
        for tag in tags {
            git(repo, &["tag", tag]);
        }
    }

    struct Fixture {
        tmp: tempfile::TempDir,
        repo: PathBuf,
        manager: Manager,
    }

    /// A local git registry with example/hello 1.0.0 and example/needy,
    /// which needs a Rookery that does not exist yet, and a manager whose
    /// only registry it is.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("registry");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(
            repo.join("packages.kdl"),
            "format 1\npackages {\n  find \"*\"\n}\nrelease {\n  tag \"{id}-v{version}\"\n}\n",
        )
        .unwrap();
        package(&repo, "hello", "1.0.0", ">=0.6");
        package(&repo, "needy", "1.0.0", ">=99");
        release(&repo, &["hello-v1.0.0", "needy-v1.0.0"]);
        let paths = Paths {
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
        };
        let manager = Manager {
            own: own_dir(&paths.config_dir),
            paths,
            source: None,
        };
        let added = manager
            .add_registry(&PluginRegistry {
                kind: "git".into(),
                location: repo.display().to_string(),
                ..PluginRegistry::default()
            })
            .unwrap();
        assert_eq!(
            added,
            [format!("✓ Added the plugin registry {}", repo.display())]
        );
        Fixture { tmp, repo, manager }
    }

    fn spec(spec: &str) -> PluginSpec {
        PluginSpec {
            spec: spec.into(),
            ..PluginSpec::default()
        }
    }

    #[test]
    fn a_plugin_is_resolved_installed_updated_and_removed_from_a_git_registry() {
        let fx = fixture();
        let location = fx.repo.display().to_string();
        let lines = fx.manager.add(&spec("example/hello")).unwrap();
        assert_eq!(
            lines,
            [format!("✓ Installed example/hello 1.0.0 from {location}")]
        );

        let plugins = fx.manager.scan();
        let [hello] = plugins.as_slice() else {
            panic!("{} plugins", plugins.len())
        };
        assert_eq!(
            (
                hello.id.as_str(),
                hello.label.as_str(),
                hello.version.as_str()
            ),
            ("hello", "hello plugin", "1.0.0")
        );
        assert_eq!(
            (hello.origin, hello.source.as_str()),
            (PluginOrigin::Registry, location.as_str())
        );
        assert_eq!(hello.problem, None);
        assert!(hello.dir.join("tasks/tool.yml").is_file());
        assert!(fx.manager.own().join("example/hello/package.kdl").is_file());
        let again = fx.manager.add(&spec("example/hello")).unwrap();
        assert_eq!(again, ["✓ It is installed already"]);

        // A new release is offered, and an update installs it.
        package(&fx.repo, "hello", "1.1.0", ">=0.6");
        release(&fx.repo, &["hello-v1.1.0"]);
        let updates = fx.manager.check().unwrap();
        let [update] = updates.plugins.as_slice() else {
            panic!("{updates:?}")
        };
        assert_eq!(update.latest.as_deref(), Some("1.1.0"), "{update:?}");
        assert_eq!(
            fx.manager.update(Some("hello")).unwrap(),
            ["✓ example/hello 1.0.0 → 1.1.0"]
        );
        assert_eq!(
            fx.manager.update(None).unwrap(),
            ["✓ No plugin has a newer version"]
        );
        assert_eq!(fx.manager.scan()[0].version, "1.1.0");

        assert_eq!(
            fx.manager.remove("hello").unwrap(),
            ["✓ Removed hello plugin"]
        );
        assert!(fx.manager.scan().is_empty());
        assert!(!fx.manager.own().join("example/hello").exists());
    }

    #[test]
    fn a_plugin_for_another_rookery_is_not_used() {
        let fx = fixture();
        let error = fx
            .manager
            .add(&spec("example/needy"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(">=99"), "{error}");
        assert!(fx.manager.scan().is_empty());
        assert!(!fx.manager.own().join("example/needy").exists());
    }

    #[test]
    fn an_egg_that_was_sent_is_installed_and_restored_from_the_store() {
        let fx = fixture();
        let egg = hatchery::Egg::pack(&fx.repo.join("hello"), &[&RookeryTarget]).unwrap();
        let lines = fx.manager.upload(egg.bytes()).unwrap();
        assert_eq!(
            lines,
            ["✓ Installed example/hello 1.0.0 from the egg that was sent"]
        );
        let plugins = fx.manager.scan();
        assert_eq!(
            (plugins[0].origin, plugins[0].source.as_str()),
            (PluginOrigin::File, "an egg that was sent")
        );
        assert_eq!(
            fx.manager.upload(egg.bytes()).unwrap(),
            ["✓ example/hello 1.0.0 is installed already"]
        );
        assert_eq!(fs::read_dir(uploads(fx.manager.own())).unwrap().count(), 1);

        // Another machine of the same store gets the registry, the lock and
        // the egg, and installs from them.
        let other = Paths {
            config_dir: fx.tmp.path().join("other/config"),
            data_dir: fx.tmp.path().join("other/data"),
        };
        let store =
            crate::store::Store::open(&crate::store::default_file(&fx.manager.paths)).unwrap();
        let plan = crate::plugin_state::plan(&other, &store).unwrap().unwrap();
        let said = crate::plugin_state::carry_out(&other, &plan).unwrap();
        let own = own_dir(&other.config_dir);
        assert_eq!(
            said,
            [format!(
                "rookery: installed the plugins of rookery's store in {}",
                own.display()
            )]
        );
        let restored = plugins::load(&[own]);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].version, "1.0.0");
        assert!(config_file(&other).is_file());
    }

    #[test]
    fn a_file_of_the_server_is_not_installed_by_name() {
        let fx = fixture();
        let error = fx.manager.add(&spec("./hello")).unwrap_err().to_string();
        assert!(
            error.contains("a server installs only what it can fetch"),
            "{error}"
        );
        for folder in [
            fx.repo.display().to_string(),
            "file:///srv/x".into(),
            "repo".into(),
        ] {
            let asked = PluginSpec {
                git: Some(folder.clone()),
                ..PluginSpec::default()
            };
            let error = fx.manager.add(&asked).unwrap_err().to_string();
            assert!(error.contains("is a folder"), "{folder}: {error}");
        }
        let error = fx.manager.update(Some("hello")).unwrap_err().to_string();
        assert!(error.starts_with("No plugin hello"), "{error}");
    }
}
