//! Plugins, installed as Hatchery eggs. They are installed in `plugins/` of
//! the configuration directory, the install folder Hatchery keeps with its
//! `eggs.lock`, from the registries `hatchery.kdl` beside it lists, after
//! the one that comes with Rookery (see [`crate::builtin`]). Every change
//! goes through Hatchery, and the store is told of it afterwards (see
//! [`crate::plugin_state`]).

use crate::builtin;
use crate::paths::Paths;
use crate::plugins::{self, Plugin, TARGET};
use crate::{Refused, Result};
use hatchery::{
    Client, Config, LockedPackage, Manifest, Name, Options, Platform, RegistryConfig, Spec,
    block_on,
};
use rookery_api::{
    PluginList, PluginOrigin, PluginRegistries, PluginRegistry, PluginSpec, PluginUpdate,
    PluginUpdates,
};
use std::fs;
use std::io::Write;
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

/// The name of a registry line: its `name`, or for an `off` line the
/// registry it turns off.
pub fn line_name(line: &RegistryConfig) -> Option<&str> {
    match line.kind == builtin::OFF {
        true => Some(line.location.as_str()),
        false => line.props.get("name").map(String::as_str),
    }
}

/// The lines of `hatchery.kdl`, after the registry of the source when no
/// line has its name. `off` lines are kept, for a list.
fn lines(config: &Config, source: Option<&Path>) -> Vec<RegistryConfig> {
    let mut lines: Vec<RegistryConfig> = config
        .registries
        .iter()
        .filter(|line| line.kind != builtin::KIND)
        .cloned()
        .collect();
    let named = lines
        .iter()
        .any(|line| line_name(line) == Some(builtin::NAME));
    if let Some(source) = source.filter(|_| !named) {
        lines.insert(0, builtin::line(source));
    }
    lines
}

/// The registries plugins are installed from, in search order: those of
/// `hatchery.kdl`, after the one of the provisioning source unless a line
/// has its name.
pub fn registries_of(paths: &Paths, source: Option<&Path>) -> Result<Config> {
    let mut config = config(paths)?;
    config.registries = lines(&config, source)
        .into_iter()
        .filter(|line| line.kind != builtin::OFF)
        .collect();
    Ok(config)
}

/// How `eggs.lock` names the registry of a line.
fn label(line: &RegistryConfig) -> String {
    match line.kind == builtin::KIND {
        true => builtin::NAME.into(),
        false => line.location.trim_end_matches('/').to_owned(),
    }
}

/// How `eggs.lock` names the registry the plugins that come with Rookery
/// are installed from, the one named `rookery`. None when it is off.
fn shipped_from(config: &Config) -> Option<String> {
    config
        .registries
        .iter()
        .find(|line| line_name(line) == Some(builtin::NAME))
        .map(label)
}

/// A Hatchery client that installs Rookery's target from the registries.
pub fn client(paths: &Paths) -> Result<Client> {
    client_with(paths, paths.source().ok().as_deref())
}

/// A Hatchery client that installs Rookery's target from the registries,
/// the one of the given provisioning source among them.
fn client_with(paths: &Paths, source: Option<&Path>) -> Result<Client> {
    client_for(paths, registries_of(paths, source)?)
}

fn client_for(paths: &Paths, config: Config) -> Result<Client> {
    Ok(Client::builder()
        .config(config)
        .kind(builtin::KIND, builtin::factory())
        .target(RookeryTarget)
        .fetch(hatchery::fetch::HttpFetch::new())
        .cache_dir(cache_dir(paths))
        .git(git())
        .build()?)
}

/// Reads the parrot variant from the same source as the installed rookery target.
pub(crate) fn parrot_egg(
    paths: &Paths,
    pin: &LockedPackage,
    variant: Option<String>,
) -> Result<hatchery::Egg> {
    let variants: Vec<String> = variant.iter().map(|v| format!("parrot.{v}")).collect();
    // A variant is only ever the one the install pinned: a registry that
    // serves another under the same version must not reach the host.
    for key in &variants {
        if !pin.variants.contains_key(key) {
            return Err(format!(
                "{}: {key} has no installed pin; install the plugin again",
                pin.name
            )
            .into());
        }
    }
    // Hold the first verified variant along with the base pin. Later applies can use
    // it when a registry no longer serves that version.
    let key = crate::server_image::sha256(&serde_json::to_vec(&(pin, &variant))?);
    let cache = paths.data_dir.join("parrot-eggs");
    fs::create_dir_all(&cache)?;
    let saved = cache.join(format!("{key}.egg"));
    if saved.is_file() {
        let cached = hatchery::Egg::read(&saved)?;
        verify_pinned(pin, &cached, &variants)?;
        return Ok(cached);
    }
    let (config, spec) = variant_source(paths, pin)?;
    let client = client_for(paths, config)?;
    let fetched =
        block_on(client.fetch(&spec, Some(&variants), &Options::new().allow_young(true)))?;
    verify_base(pin, &fetched.base)?;
    if fetched.variants.len() != variants.len() {
        return Err(format!(
            "{}: the source served {} of the {} pinned variants",
            pin.name,
            fetched.variants.len(),
            variants.len()
        )
        .into());
    }
    for (key, egg) in variants.iter().zip(&fetched.variants) {
        if pin.variants.get(key).map(String::as_str) != Some(egg.sha256()) {
            return Err(format!("{}: {key} does not match its installed pin", pin.name).into());
        }
    }
    let fat = fetched.base.fatten(&fetched.variants)?;
    let mut file = tempfile::NamedTempFile::new_in(&cache)?;
    file.write_all(fat.bytes())?;
    match file.persist_noclobber(&saved) {
        Ok(_) => Ok(fat),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let cached = hatchery::Egg::read(&saved)?;
            verify_pinned(pin, &cached, &variants)?;
            Ok(cached)
        }
        Err(error) => Err(error.into()),
    }
}

/// The registry or file an installed pin names, and the exact version to ask
/// it for: `=`, so a version range cannot answer with another release.
fn variant_source(paths: &Paths, pin: &LockedPackage) -> Result<(Config, Spec)> {
    let mut config = registries_of(paths, paths.source().ok().as_deref())?;
    let spec = if let Some(registry) = &pin.registry {
        config.registries.retain(|line| &label(line) == registry);
        if config.registries.is_empty() {
            return Err(format!("{}: the pinned registry is unavailable", pin.name).into());
        }
        Spec::parse(&format!("{}@={}", pin.name, pin.version))?
    } else if let Some(file) = &pin.file {
        config.registries.clear();
        Spec::File(file.clone())
    } else {
        return Err(format!(
            "{}: install the agent egg from a registry or upload it as a fat egg",
            pin.name
        )
        .into());
    };
    Ok((config, spec))
}

/// Whether a fetched base egg is the installed pin's.
fn verify_base(pin: &LockedPackage, base: &hatchery::Egg) -> Result<()> {
    let manifest = base.manifest();
    if manifest.name() != pin.name
        || manifest.version() != &pin.version
        || base.sha256() != pin.sha256
    {
        return Err(format!(
            "{}: the fetched egg does not match its installed pin",
            pin.name
        )
        .into());
    }
    Ok(())
}

/// Whether a fat egg still holds what the pin says: the base's checksum and
/// each variant's, so a file that changed on disk is not staged.
fn verify_pinned(pin: &LockedPackage, egg: &hatchery::Egg, variants: &[String]) -> Result<()> {
    let base = egg.base()?;
    verify_base(pin, &base)?;
    for key in variants {
        let Some(sha) = pin.variants.get(key) else {
            return Err(format!(
                "{}: {key} has no installed pin; install the plugin again",
                pin.name
            )
            .into());
        };
        let (target, variant) = key
            .split_once('.')
            .ok_or_else(|| format!("{}: {key} is no variant", pin.name))?;
        let found = egg.embedded_variant(target, variant)?.ok_or_else(|| {
            format!(
                "{}: the cached egg does not hold {key}; install the plugin again",
                pin.name
            )
        })?;
        if found.sha256() != sha {
            return Err(format!(
                "{}: the cached {key} does not match its installed pin; install the plugin again",
                pin.name
            )
            .into());
        }
    }
    Ok(())
}

/// The `parrot` target a package folder offers for this server's platform.
pub(crate) enum ParrotTarget {
    /// There is no `parrot` target: the package has no driver.
    None,
    /// The target has no variants, so its egg is the driver itself.
    Base,
    /// The driver is this variant, by its key such as `linux-x86_64`.
    Variant(String),
}

/// What a package folder has of the `parrot` target for Linux x86-64. A
/// target with variants, none of them this server's, is refused: no host
/// could run it.
pub(crate) fn parrot_target(package_dir: &Path) -> Result<ParrotTarget> {
    let manifest = Manifest::load(package_dir)?;
    let Some(target) = manifest.target("parrot") else {
        return Ok(ParrotTarget::None);
    };
    if target.variants.is_empty() {
        return Ok(ParrotTarget::Base);
    }
    let variant = target
        .select_variant(&Platform::new("linux", "x86_64"))
        .ok_or_else(|| format!("{} has no Parrot driver for Linux x86_64", manifest.name()))?;
    Ok(ParrotTarget::Variant(variant.key()))
}

/// Pin the `parrot` variant of each installed package in `eggs.lock`, so that
/// an apply installs the driver that was published with the plugin. Lines for
/// what could not be pinned go to the caller; an apply refuses a variant
/// without a pin, so the plugin is installed again to try.
fn pin_parrot_variants(paths: &Paths, own: &Path) -> Vec<String> {
    if !own.join(hatchery::lock::FILE).is_file() {
        return vec![];
    }
    let mut lock = match hatchery::Lock::load(own) {
        Ok(lock) => lock,
        Err(error) => return vec![format!("! the plugins' lock could not be read: {error}")],
    };
    let mut lines = vec![];
    let mut changed = false;
    for package in lock.packages.clone() {
        let key = match parrot_target(&own.join(package.name.dir())) {
            Ok(ParrotTarget::Variant(variant)) => format!("parrot.{variant}"),
            Ok(ParrotTarget::None | ParrotTarget::Base) => continue,
            Err(error) => {
                lines.push(format!("! {}: {error}", package.name));
                continue;
            }
        };
        if package.variants.contains_key(&key) {
            continue;
        }
        let sha = match variant_sha256(paths, &package, &key) {
            Ok(sha) => sha,
            Err(error) => {
                lines.push(format!(
                    "! {}: {key} could not be pinned, and an apply will not use it: {error}",
                    package.name
                ));
                continue;
            }
        };
        if let Some(locked) = lock.get_mut(&package.name) {
            locked.variants.insert(key, sha);
        }
        changed = true;
    }
    if changed && let Err(error) = lock.save(own) {
        lines.push(format!("! the plugins' lock could not be written: {error}"));
    }
    lines
}

/// The checksum of a variant egg of an installed package, from its source.
fn variant_sha256(paths: &Paths, pin: &LockedPackage, key: &str) -> Result<String> {
    let (config, spec) = variant_source(paths, pin)?;
    let client = client_for(paths, config)?;
    let keys = [key.to_owned()];
    let fetched = block_on(client.fetch(&spec, Some(&keys), &Options::new().allow_young(true)))?;
    verify_base(pin, &fetched.base)?;
    let [egg] = fetched.variants.as_slice() else {
        return Err(format!("{}: the source served no {key}", pin.name).into());
    };
    Ok(egg.sha256().to_owned())
}

/// Install the packages of the registry of the provisioning source that the
/// install folder lacks, and put those it has at the registry's versions,
/// so that the plugins that come with Rookery are those of this Rookery.
/// Nothing is done when a line of `hatchery.kdl` takes the place of the
/// registry, or turns it off. Returns what to tell the user.
pub fn builtins(paths: &Paths) -> Result<Vec<String>> {
    builtins_from(paths, paths.source().ok().as_deref())
}

fn builtins_from(paths: &Paths, source: Option<&Path>) -> Result<Vec<String>> {
    let config = registries_of(paths, source)?;
    let Some(line) = config
        .registries
        .iter()
        .find(|line| line.kind == builtin::KIND)
    else {
        return Ok(vec![]);
    };
    let Some(offered) = builtin::offered(Path::new(&line.location)) else {
        return Ok(vec![]);
    };
    let own = own_dir(&paths.config_dir);
    let lock = match own.join(hatchery::lock::FILE).is_file() {
        true => hatchery::Lock::load(&own)?,
        false => hatchery::Lock::default(),
    };
    let behind = |name: &Name, version: &hatchery::semver::Version| {
        lock.get(name).is_none_or(|locked| {
            locked.registry.as_deref() != Some(builtin::NAME)
                || &locked.version != version
                || !own
                    .join(name.dir())
                    .join(hatchery::manifest::FILE)
                    .is_file()
        })
    };
    let wanted: Vec<_> = offered
        .into_iter()
        .filter(|(name, version)| behind(name, version))
        .collect();
    if wanted.is_empty() {
        return Ok(vec![]);
    }
    let Some(_one) = crate::store::hold_lock(&paths.data_dir.join("locks/plugins.lock"))? else {
        return Ok(vec![]);
    };
    let client = client_with(paths, source)?;
    fs::create_dir_all(&own)?;
    let mut installed = vec![];
    let mut lines = vec![];
    for (name, version) in wanted {
        let spec = Spec::parse(&format!("{name}@={version}"))?;
        match block_on(client.add(&spec, &own)) {
            Ok(report) => installed.extend(
                report
                    .packages
                    .iter()
                    .map(|change| format!("{} {}", change.name, change.version)),
            ),
            Err(error) => lines.push(format!(
                "rookery: {name} {version}, which comes with rookery, could not be installed: {error}"
            )),
        }
    }
    if !installed.is_empty() {
        lines.insert(
            0,
            format!(
                "rookery: installed the plugins that come with rookery: {}",
                installed.join(", ")
            ),
        );
        forget_uploads(&own);
        lines.extend(remembered(paths));
    }
    Ok(lines)
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
    said_of(&report.packages, &report.warnings)
}

/// What Hatchery changed and noted, as lines.
fn said_of(packages: &[hatchery::client::Change], warnings: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = packages
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
    lines.extend(warnings.iter().map(|warning| format!("  ! {warning}")));
    lines
}

/// Whether a git location is a folder of this machine rather than an address.
fn local_git(url: &str) -> bool {
    url.starts_with(['/', '.', '~']) || url.starts_with("file:") || !url.contains(':')
}

/// Whether a registry is read from a folder of this machine: a `dir`
/// registry, or a git repository given as a path.
pub fn local_registry(kind: &str, location: &str) -> bool {
    kind == "dir" || (kind == "git" && local_git(location.trim()))
}

/// Where plugins are managed: the install folder, and the provisioning
/// source whose registry comes first.
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

    /// Every plugin installed, those Rookery cannot use among them. Those
    /// from the registry named `rookery` come with rookery.
    pub fn scan(&self) -> Vec<Plugin> {
        let shipped = registries_of(&self.paths, self.source.as_deref())
            .ok()
            .and_then(|config| shipped_from(&config));
        let mut found = plugins::scan(std::slice::from_ref(&self.own));
        for plugin in &mut found {
            if plugin.origin == PluginOrigin::Registry
                && Some(plugin.source.trim_end_matches('/')) == shipped.as_deref()
            {
                plugin.origin = PluginOrigin::Shipped;
            }
        }
        found
    }

    /// See [`builtins`].
    pub fn builtins(&self) -> Result<Vec<String>> {
        builtins_from(&self.paths, self.source.as_deref())
    }

    fn client(&self) -> Result<Client> {
        client_with(&self.paths, self.source.as_deref())
    }

    /// The plugins there are, and where the user's own are installed.
    pub fn list(&self) -> PluginList {
        PluginList {
            plugins: self.scan().iter().map(Plugin::info).collect(),
            dir: self.own.display().to_string(),
        }
    }

    /// An installed plugin, by its id or package.
    fn plugin(&self, name: &str) -> Result<Plugin> {
        Ok(self
            .scan()
            .into_iter()
            .find(|plugin| plugin.id == name || plugin.package == name)
            .ok_or_else(|| format!("No plugin {name}. See: rookery plugin list"))?)
    }

    /// After a change: the lines for what Rookery cannot use of what was
    /// installed, the eggs that were sent and are no longer needed gone,
    /// and the store told.
    fn finish(&self, changes: &[hatchery::client::Change], lines: &mut Vec<String>) {
        let found = self.scan();
        for change in changes {
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
        lines.extend(pin_parrot_variants(&self.paths, &self.own));
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
        let client = self.client()?;
        fs::create_dir_all(&self.own)?;
        let options = Options::new().allow_young(asked.allow_young);
        let report = block_on(client.add_with(&spec, &self.own, &options))?;
        let mut lines = said(&report);
        if report.packages.is_empty() {
            lines.insert(0, "✓ It is installed already".into());
        }
        self.finish(&report.packages, &mut lines);
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
        let client = self.client()?;
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
        self.finish(&report.packages, &mut lines);
        Ok(lines)
    }

    /// Remove a plugin by its id or package. One that comes with rookery
    /// stays while its registry is on.
    pub fn remove(&self, name: &str) -> Result<Vec<String>> {
        let plugin = self.plugin(name)?;
        if plugin.origin == PluginOrigin::Shipped {
            return Err(Refused(shipped_stays(&plugin.label)).into());
        }
        let client = self.client()?;
        client.remove(&Name::parse(&plugin.package)?, &self.own)?;
        let mut lines = vec![format!("✓ Removed {}", plugin.label)];
        forget_uploads(&self.own);
        lines.extend(remembered(&self.paths));
        Ok(lines)
    }

    /// The packages the install folder's lock names, as `GROUP/ID`, all of
    /// them, those that share an id among them.
    pub fn locked(&self) -> Vec<String> {
        if !self.own.join(hatchery::lock::FILE).is_file() {
            return vec![];
        }
        hatchery::Lock::load(&self.own)
            .map(|lock| lock.packages.iter().map(|p| p.name.to_string()).collect())
            .unwrap_or_default()
    }

    /// Removes a package by its name, `GROUP/ID`, even when another package
    /// of the same id is the one Rookery uses.
    pub fn remove_package(&self, package: &str) -> Result<()> {
        let client = self.client()?;
        client.remove(&Name::parse(package)?, &self.own)?;
        forget_uploads(&self.own);
        let _ = remembered(&self.paths);
        Ok(())
    }

    /// Which plugins have a newer version that can be installed, as their
    /// registries say.
    pub fn check(&self) -> Result<PluginUpdates> {
        let own = self.scan();
        let from_registry = |plugin: &Plugin| {
            matches!(
                plugin.origin,
                PluginOrigin::Registry | PluginOrigin::Shipped
            )
        };
        let listing = match own.iter().any(from_registry) {
            true => {
                Some(block_on(self.client()?.offered(&self.own)).map_err(|error| error.to_string()))
            }
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
                PluginOrigin::Registry | PluginOrigin::Shipped => match &listing {
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
                PluginOrigin::File => {
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
                let plugin = self.plugin(name)?;
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
        let client = self.client()?;
        if !self.own.join(hatchery::lock::FILE).is_file() {
            return Ok(vec!["✓ No plugin has a newer version".into()]);
        }
        let mut changes = vec![];
        let mut warnings = vec![];
        // What came from the registry of the source, when a line has taken
        // its place, is installed from that line.
        let config = registries_of(&self.paths, self.source.as_deref())?;
        let moved = |locked: &&LockedPackage| {
            locked.registry.as_deref() == Some(builtin::NAME)
                && shipped_from(&config).is_some_and(|label| label != builtin::NAME)
                && one.as_ref().is_none_or(|(name, _)| name == &locked.name)
        };
        let lock = hatchery::Lock::load(&self.own)?;
        let moving: Vec<Name> = lock
            .packages
            .iter()
            .filter(moved)
            .map(|locked| locked.name.clone())
            .collect();
        for name in &moving {
            let added = block_on(client.add(&Spec::parse(&name.to_string())?, &self.own))?;
            changes.extend(added.packages);
            warnings.extend(added.warnings);
        }
        if moving.is_empty() || one.is_none() {
            let updated = block_on(client.update(one.as_ref().map(|(name, _)| name), &self.own))?;
            changes.extend(updated.packages);
            warnings.extend(updated.warnings);
        }
        let mut lines: Vec<String> = said_of(&changes, &warnings)
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
        self.finish(&changes, &mut lines);
        Ok(lines)
    }

    /// The registries plugins are installed from, in search order, with
    /// the lines that turn one off.
    pub fn registries(&self) -> Result<PluginRegistries> {
        Ok(PluginRegistries {
            registries: lines(&config(&self.paths)?, self.source.as_deref())
                .iter()
                .map(|line| PluginRegistry {
                    name: line_name(line).unwrap_or_default().to_owned(),
                    builtin: line.kind == builtin::KIND,
                    kind: line.kind.clone(),
                    location: line.location.clone(),
                    only: line.only.clone(),
                    key: line.key.clone(),
                    min_age: line
                        .min_age
                        .filter(|_| line.kind != builtin::KIND)
                        .map(hatchery::time::format_duration),
                    unsigned_tags: line.unsigned_tags,
                })
                .collect(),
            file: config_file(&self.paths).display().to_string(),
        })
    }

    /// Add a registry after the others, or change the one at the same
    /// location or with the same name. A registry named `rookery` takes the
    /// place of the one that comes with Rookery, and serves only its group
    /// unless it says otherwise. Kind `off` with the location `rookery`
    /// turns that one off.
    pub fn add_registry(&self, asked: &PluginRegistry) -> Result<Vec<String>> {
        if !["git", "http", "dir", builtin::OFF].contains(&asked.kind.as_str()) {
            return Err(Refused(format!(
                "A plugin registry is git, http or dir, or off to turn rookery's off, not {:?}",
                asked.kind
            ))
            .into());
        }
        let location = asked.location.trim();
        if location.is_empty() || location.starts_with('-') || location.contains(char::is_control) {
            return Err(Refused(format!("{location:?} is no address of a registry")).into());
        }
        let mut config = config(&self.paths)?;
        if asked.kind == builtin::OFF {
            if location != builtin::NAME {
                return Err(Refused(format!(
                    "Only the registry that comes with rookery, {}, is turned off; another is removed: rookery plugin registry remove {location}",
                    builtin::NAME
                ))
                .into());
            }
            let off = config
                .registries
                .iter()
                .any(|line| line.kind == builtin::OFF && line.location == location);
            if off {
                return Ok(vec![format!(
                    "✓ The plugin registry {location} is off already"
                )]);
            }
            forget_named(&mut config, location);
            config.add_registry(RegistryConfig::new(builtin::OFF, location));
            return self.save(
                &config,
                vec![format!(
                    "✓ Turned off the plugin registry {location}. The plugins that came from it stay, and can be removed now"
                )],
            );
        }
        let name = asked.name.trim();
        if !name.is_empty() {
            hatchery::name::validate_segment("name", name)?;
        }
        let mut registry = RegistryConfig::new(asked.kind.clone(), location);
        for group in &asked.only {
            hatchery::name::validate_segment("group", group)?;
        }
        registry.only = asked.only.clone();
        if name == builtin::NAME && registry.only.is_empty() {
            registry.only = vec![builtin::NAME.into()];
        }
        if let Some(key) = asked.key.as_deref().filter(|key| !key.is_empty()) {
            hatchery::sign::parse_verifying_key(key)?;
            registry.key = Some(key.to_owned());
        }
        if let Some(age) = asked.min_age.as_deref().filter(|age| !age.is_empty()) {
            registry.min_age = Some(hatchery::time::parse_duration(age)?);
        }
        registry.unsigned_tags = asked.unsigned_tags;
        if !name.is_empty() {
            registry.props.insert("name".into(), name.to_owned());
            forget_named(&mut config, name);
        }
        let known = config
            .registries
            .iter()
            .any(|known| known.location == location);
        config.add_registry(registry);
        let lines = vec![match (known, name == builtin::NAME) {
            (_, true) => format!(
                "✓ The plugins that come with rookery are installed from {location} now: rookery plugin update installs them from it"
            ),
            (true, false) => format!("✓ Changed the plugin registry {location}"),
            (false, false) => format!("✓ Added the plugin registry {location}"),
        }];
        self.save(&config, lines)
    }

    /// Write `hatchery.kdl`, and tell the store.
    fn save(&self, config: &Config, mut lines: Vec<String>) -> Result<Vec<String>> {
        fs::create_dir_all(&self.paths.config_dir)?;
        config.save(&config_file(&self.paths))?;
        lines.extend(remembered(&self.paths));
        Ok(lines)
    }

    /// Remove the registry at a location or with a name. What was installed
    /// from it stays, and is not updated. Without its line, the registry
    /// named `rookery` is the one that comes with Rookery again, and what
    /// that has is installed.
    pub fn remove_registry(&self, asked: &str) -> Result<Vec<String>> {
        let mut config = config(&self.paths)?;
        let found = config
            .registries
            .iter()
            .find(|line| line.location == asked || line_name(line) == Some(asked))
            .cloned();
        let Some(line) = found else {
            if asked == builtin::NAME {
                return Err(Refused(format!(
                    "{asked} comes with rookery and has no line in hatchery.kdl to remove: rookery plugin registry disable {asked} turns it off"
                ))
                .into());
            }
            return Err(
                format!("No plugin registry {asked}. See: rookery plugin registry list").into(),
            );
        };
        config.remove_registry(&line.location);
        let mut lines = self.save(
            &config,
            vec![match line.kind == builtin::OFF {
                true => format!("✓ Turned on the plugin registry {asked} again"),
                false => format!("✓ Removed the plugin registry {asked}"),
            }],
        )?;
        let gone = label(&line);
        let from: Vec<String> = self
            .scan()
            .into_iter()
            .filter(|plugin| {
                plugin.origin == PluginOrigin::Registry
                    && plugin.source.trim_end_matches('/') == gone
            })
            .map(|plugin| plugin.id)
            .collect();
        if !from.is_empty() && line.kind != builtin::OFF {
            lines.push(format!(
                "  ! {} came from it, and stay installed. They cannot be updated or installed again until it is added again",
                from.join(", ")
            ));
        }
        if line_name(&line) == Some(builtin::NAME) {
            lines.extend(self.builtins()?);
        }
        Ok(lines)
    }
}

/// Why a plugin that comes with rookery is not removed.
fn shipped_stays(label: &str) -> String {
    format!(
        "{label} comes with rookery and stays while its registry is on: rookery plugin registry disable {} turns that off",
        builtin::NAME
    )
}

/// Remove the lines with a name, wherever they point.
fn forget_named(config: &mut Config, name: &str) {
    let named: Vec<String> = config
        .registries
        .iter()
        .filter(|line| line_name(line) == Some(name))
        .map(|line| line.location.clone())
        .collect();
    for location in named {
        config.remove_registry(&location);
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
        let store = crate::store::shared(&fx.manager.paths).unwrap();
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
    fn apply_caches_the_parrot_variant_of_the_installed_egg() {
        let fx = fixture();
        let fat = parrot_fat(&fx, "made-up driver");

        fx.manager.upload(fat.bytes()).unwrap();
        let lock = hatchery::Lock::load(fx.manager.own()).unwrap();
        let pin = lock.get(&Name::parse("example/hello").unwrap()).unwrap();
        let staged = parrot_egg(&fx.manager.paths, pin, Some("linux-x86_64".into())).unwrap();
        assert_eq!(staged.base().unwrap().sha256(), pin.sha256);
        assert!(
            staged
                .embedded_variant("parrot", "linux-x86_64")
                .unwrap()
                .is_some()
        );
        fs::remove_file(pin.file.as_ref().unwrap()).unwrap();
        let cached = parrot_egg(&fx.manager.paths, pin, Some("linux-x86_64".into())).unwrap();
        assert_eq!(cached.sha256(), staged.sha256());
    }

    #[test]
    fn a_parrot_variant_is_pinned_at_install_and_checked_before_it_is_used() {
        let fx = fixture();
        let fat = parrot_fat(&fx, "made-up driver");
        fx.manager.upload(fat.bytes()).unwrap();
        let lock = hatchery::Lock::load(fx.manager.own()).unwrap();
        let pin = lock
            .get(&Name::parse("example/hello").unwrap())
            .unwrap()
            .clone();
        // Installing the plugin pinned the driver that came with it.
        assert_eq!(
            pin.variants.get("parrot.linux-x86_64"),
            Some(
                &fat.embedded_variant("parrot", "linux-x86_64")
                    .unwrap()
                    .unwrap()
                    .sha256()
                    .to_owned()
            )
        );
        let staged = parrot_egg(&fx.manager.paths, &pin, Some("linux-x86_64".into())).unwrap();

        // A cached egg that changed on disk is refused before it is staged.
        let cache = fx.manager.paths.data_dir.join("parrot-eggs");
        let saved = fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let other = parrot_fat(&fx, "another driver");
        fs::write(&saved, other.bytes()).unwrap();
        let error = parrot_egg(&fx.manager.paths, &pin, Some("linux-x86_64".into()))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("does not match its installed pin"),
            "{error}"
        );
        fs::write(&saved, staged.bytes()).unwrap();
        assert_eq!(
            parrot_egg(&fx.manager.paths, &pin, Some("linux-x86_64".into()))
                .unwrap()
                .sha256(),
            staged.sha256()
        );

        // A variant without an installed pin is refused, not fetched.
        let mut unpinned = pin.clone();
        unpinned.variants.clear();
        let error = parrot_egg(&fx.manager.paths, &unpinned, Some("linux-x86_64".into()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("has no installed pin"), "{error}");
    }

    /// The hello package with a Parrot driver for Linux x86-64, packed as one
    /// fat egg whose driver holds `driver`.
    fn parrot_fat(fx: &Fixture, driver: &str) -> hatchery::Egg {
        use hatchery::pack::{PackOptions, VariantPick};

        let dir = fx.repo.join("hello");
        let manifest = dir.join("package.kdl");
        let source = fs::read_to_string(&manifest).unwrap();
        if !source.contains("parrot {") {
            let with_parrot = format!(
                "{}  parrot {{\n    variants {{\n      variant os=linux arch=x86_64\n    }}\n  }}\n}}\n",
                source.strip_suffix("}\n").unwrap()
            );
            fs::write(&manifest, with_parrot).unwrap();
        }
        let variant = dir.join("parrot/variants/linux-x86_64");
        fs::create_dir_all(&variant).unwrap();
        fs::write(variant.join("driver"), driver).unwrap();
        let eggs = hatchery::pack::pack(
            &dir,
            &PackOptions::new()
                .no_build(true)
                .fat(true)
                .variant(VariantPick::new("parrot").os("linux").arch("x86_64"))
                .check(&RookeryTarget),
        )
        .unwrap();
        eggs.into_iter().find(hatchery::Egg::is_fat).unwrap()
    }

    /// The checkout, a provisioning source with the registry of the
    /// plugins that come with Rookery.
    fn checkout() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// A manager of an empty configuration directory, beside the checkout.
    fn builtin_fixture() -> (tempfile::TempDir, Manager) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
        };
        let manager = Manager {
            own: own_dir(&paths.config_dir),
            paths,
            source: Some(checkout()),
        };
        (tmp, manager)
    }

    fn names(manager: &Manager) -> Vec<(String, String, bool)> {
        manager
            .registries()
            .unwrap()
            .registries
            .into_iter()
            .map(|line| (line.name, line.kind, line.builtin))
            .collect()
    }

    #[test]
    fn the_registry_of_the_source_is_there_with_no_hatchery_kdl() {
        let (_tmp, manager) = builtin_fixture();
        assert!(!config_file(&manager.paths).exists());
        let listed = manager.registries().unwrap().registries;
        let [line] = listed.as_slice() else {
            panic!("{listed:?}")
        };
        assert_eq!(
            (line.name.as_str(), line.kind.as_str(), line.builtin),
            ("rookery", "builtin", true)
        );
        assert_eq!(line.only, ["rookery"]);
        assert_eq!(
            Path::new(&line.location),
            builtin::dir(&checkout()).as_path()
        );
        assert_eq!(line.min_age, None);

        // A registry added beside it comes after it.
        let other = PluginRegistry {
            kind: "http".into(),
            location: "https://eggs.example.org".into(),
            ..PluginRegistry::default()
        };
        manager.add_registry(&other).unwrap();
        assert_eq!(
            names(&manager),
            [
                ("rookery".into(), "builtin".into(), true),
                (String::new(), "http".into(), false)
            ]
        );
        let written = fs::read_to_string(config_file(&manager.paths)).unwrap();
        assert!(!written.contains("builtin"), "{written}");
    }

    #[test]
    fn the_plugins_that_come_with_rookery_are_installed_from_its_registry_offline() {
        let (_tmp, manager) = builtin_fixture();
        let said = manager.builtins().unwrap();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].starts_with(
                "rookery: installed the plugins that come with rookery: rookery/backup "
            ),
            "{said:?}"
        );
        let lock = hatchery::Lock::load(manager.own()).unwrap();
        let offered = builtin::offered(&builtin::dir(&checkout())).unwrap();
        assert_eq!(lock.packages.len(), offered.len());
        for (name, version) in &offered {
            let locked = lock.get(name).unwrap();
            assert_eq!(&locked.version, version, "{name}");
            assert_eq!(locked.registry.as_deref(), Some("rookery"));
        }
        let plugins = manager.scan();
        assert_eq!(plugins.len(), offered.len());
        for plugin in &plugins {
            assert_eq!(plugin.origin, PluginOrigin::Shipped, "{}", plugin.id);
            assert_eq!(plugin.problem, None, "{}", plugin.id);
            assert!(plugin.dir.starts_with(manager.own()));
        }
        assert!(manager.builtins().unwrap().is_empty());
        let error = manager.remove("github").unwrap_err().to_string();
        assert!(
            error.contains("rookery plugin registry disable rookery"),
            "{error}"
        );
        let updates = manager.check().unwrap();
        assert!(
            updates.plugins.iter().all(|update| update.latest.is_none()),
            "{updates:?}"
        );
    }

    #[test]
    fn a_line_named_rookery_takes_the_place_of_the_registry_of_the_source() {
        let (_tmp, manager) = builtin_fixture();
        let other = tempfile::tempdir().unwrap();
        let said = manager
            .add_registry(&PluginRegistry {
                kind: "dir".into(),
                location: other.path().display().to_string(),
                name: "rookery".into(),
                ..PluginRegistry::default()
            })
            .unwrap();
        assert!(said[0].contains("are installed from"), "{said:?}");
        let listed = manager.registries().unwrap().registries;
        let [line] = listed.as_slice() else {
            panic!("{listed:?}")
        };
        assert_eq!(
            (line.name.as_str(), line.kind.as_str(), line.builtin),
            ("rookery", "dir", false)
        );
        assert_eq!(line.only, ["rookery"]);
        assert!(manager.builtins().unwrap().is_empty());
        assert!(manager.scan().is_empty());

        // Pointing the name elsewhere changes the same line.
        let again = tempfile::tempdir().unwrap();
        manager
            .add_registry(&PluginRegistry {
                kind: "dir".into(),
                location: again.path().display().to_string(),
                name: "rookery".into(),
                ..PluginRegistry::default()
            })
            .unwrap();
        let listed = manager.registries().unwrap().registries;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].location, again.path().display().to_string());

        // Without its line, the registry of the source is back, and what it
        // has is installed.
        let said = manager.remove_registry("rookery").unwrap();
        assert_eq!(said[0], "✓ Removed the plugin registry rookery");
        assert!(
            said[1].starts_with("rookery: installed the plugins"),
            "{said:?}"
        );
        assert_eq!(
            names(&manager),
            [("rookery".into(), "builtin".into(), true)]
        );
    }

    #[test]
    fn the_registry_of_the_source_is_turned_off_and_on() {
        let (_tmp, manager) = builtin_fixture();
        manager.builtins().unwrap();
        let off = PluginRegistry {
            kind: "off".into(),
            location: "rookery".into(),
            ..PluginRegistry::default()
        };
        let said = manager.add_registry(&off).unwrap();
        assert!(
            said[0].starts_with("✓ Turned off the plugin registry rookery"),
            "{said:?}"
        );
        assert_eq!(
            manager.add_registry(&off).unwrap(),
            ["✓ The plugin registry rookery is off already"]
        );
        assert_eq!(names(&manager), [("rookery".into(), "off".into(), false)]);
        let written = fs::read_to_string(config_file(&manager.paths)).unwrap();
        assert!(written.contains("off rookery"), "{written}");
        assert!(manager.builtins().unwrap().is_empty());

        // What came from it stays, as plugins of a registry that is gone,
        // and can be removed.
        let plugins = manager.scan();
        assert_eq!(plugins.len(), 5);
        assert!(
            plugins
                .iter()
                .all(|plugin| (plugin.origin, plugin.source.as_str())
                    == (PluginOrigin::Registry, "rookery"))
        );
        assert_eq!(manager.remove("unity").unwrap()[0], "✓ Removed Unity");
        assert_eq!(manager.scan().len(), 4);

        let error = manager
            .add_registry(&PluginRegistry {
                kind: "off".into(),
                location: "elsewhere".into(),
                ..PluginRegistry::default()
            })
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Only the registry that comes with rookery"),
            "{error}"
        );

        let said = manager.remove_registry("rookery").unwrap();
        assert_eq!(said[0], "✓ Turned on the plugin registry rookery again");
        assert!(said[1].contains("rookery/unity"), "{said:?}");
        assert_eq!(manager.scan().len(), 5);
        let error = manager.remove_registry("rookery").unwrap_err().to_string();
        assert!(error.contains("disable rookery"), "{error}");
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
