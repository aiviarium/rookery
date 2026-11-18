//! The plugins that come with Rookery. They are a static Hatchery registry
//! in the `plugins` folder of the provisioning source: `index.json` lists
//! the packages, and each package folder under `plugins/rookery/` holds its
//! `package.kdl` and `rookery/` folder beside the egg and the `index.json`
//! made from them. The source travels with every build, so the registry is
//! read with no network.
//!
//! The registry is named `rookery` and serves the group `rookery`. It comes
//! first when no line of `hatchery.kdl` has that name. A line named
//! `rookery` takes its place, and `off rookery` turns it off.

use hatchery::client::{SourceContext, SourceFactory};
use hatchery::semver::Version;
use hatchery::source::RegistryFiles;
use hatchery::source::registry::{DirFiles, StaticRegistry};
use hatchery::{BoxFuture, Bytes, Name, RegistryConfig, Source};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The name of the registry, and the group of its packages.
pub const NAME: &str = "rookery";
/// The kind of its line, which is never written to `hatchery.kdl`.
pub const KIND: &str = "builtin";
/// The kind of the line that turns off the registry its location names.
pub const OFF: &str = "off";

/// The registry folder of a provisioning source.
pub fn dir(source: &Path) -> PathBuf {
    source.join("plugins")
}

/// The line of the registry in a provisioning source. It is searched first
/// and holds no version back, as nothing in it is new to the machine.
pub fn line(source: &Path) -> RegistryConfig {
    let mut line = RegistryConfig::new(KIND, dir(source).display().to_string());
    line.only = vec![NAME.into()];
    line.min_age = Some(Duration::ZERO);
    line.props.insert("name".into(), NAME.into());
    line
}

/// The files of the registry folder. They are labelled `rookery`, and not
/// by their folder, so that `eggs.lock` names the same registry wherever
/// the source is unpacked.
struct Files(DirFiles);

impl RegistryFiles for Files {
    fn label(&self) -> &str {
        NAME
    }

    fn is_local(&self) -> bool {
        true
    }

    fn read<'a>(&'a self, path: &'a str) -> BoxFuture<'a, hatchery::Result<Option<Bytes>>> {
        self.0.read(path)
    }
}

/// What makes the source of a `builtin` line. Its error is Hatchery's own.
#[allow(clippy::result_large_err)]
pub fn factory() -> SourceFactory {
    Arc::new(|line: &RegistryConfig, _: &SourceContext| {
        let files = Files(DirFiles::new(&line.location));
        Ok(Arc::new(StaticRegistry::new(Arc::new(files), None)) as Arc<dyn Source>)
    })
}

/// The packages of a registry folder, each at its newest version that is
/// not withdrawn. None when the folder is no registry.
pub fn offered(dir: &Path) -> Option<Vec<(Name, Version)>> {
    let read = |path: PathBuf| std::fs::read(path).ok();
    let catalog: hatchery::index::Catalog =
        hatchery::index::from_json("index.json", &read(dir.join("index.json"))?).ok()?;
    let mut offered = vec![];
    for name in catalog.packages {
        let Some(index) = read(dir.join(name.dir()).join("index.json")).and_then(|data| {
            hatchery::index::from_json::<hatchery::index::PackageIndex>("index.json", &data).ok()
        }) else {
            continue;
        };
        let newest = index
            .versions
            .iter()
            .filter(|(_, entry)| entry.withdrawn.is_none() && entry.sha256.is_some())
            .filter_map(|(text, _)| Version::parse(text).ok())
            .max();
        if let Some(version) = newest {
            offered.push((name, version));
        }
    }
    Some(offered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eggs::RookeryTarget;
    use std::fs;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
    }

    /// The package folders of the registry, sorted.
    fn packages() -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = fs::read_dir(root().join(NAME))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join(hatchery::manifest::FILE).is_file())
            .collect();
        found.sort();
        found
    }

    /// Packs a built-in package with a fixed origin. Packing reads the git
    /// commit and its time into the origin otherwise, and every commit would
    /// change the egg.
    fn pack(package: &Path) -> hatchery::Egg {
        let origin =
            hatchery::manifest::Origin::new().repository("https://github.com/aiviarium/rookery");
        let options = hatchery::PackOptions::new()
            .base(true)
            .no_build(true)
            .check(&RookeryTarget)
            .origin(origin);
        hatchery::pack(package, &options).unwrap().pop().unwrap()
    }

    /// The registry's eggs and indexes are made from its package folders.
    /// With `ROOKERY_UPDATE_DOCS=1` they are written again. A version that
    /// was published keeps its contents, so a change to a package comes
    /// with a new version in its `package.kdl`.
    #[test]
    fn the_registry_is_made_from_its_packages() {
        let update = std::env::var_os("ROOKERY_UPDATE_DOCS").is_some();
        let root = root();
        let mut catalog = hatchery::index::Catalog::default();
        let mut stale = vec![];
        for package in packages() {
            let egg = pack(&package);
            let manifest = egg.manifest();
            let name = manifest.name();
            assert_eq!(name.group, NAME, "{}", package.display());
            assert_eq!(
                package.file_name().unwrap().to_str(),
                Some(name.id.as_str())
            );
            catalog.add(&name);
            let index = fs::read(package.join("index.json")).ok().and_then(|data| {
                hatchery::index::from_json::<hatchery::index::PackageIndex>("index.json", &data)
                    .ok()
            });
            let known = index
                .as_ref()
                .and_then(|index| index.versions.get(&manifest.version().to_string()));
            let current = known.is_some_and(|entry| entry.sha256.as_deref() == Some(egg.sha256()))
                && index
                    .as_ref()
                    .is_some_and(|index| index.versions.len() == 1)
                && fs::read(package.join(egg.file_name())).ok().as_deref() == Some(egg.bytes());
            if current {
                continue;
            }
            assert!(
                known.is_none_or(|entry| entry.sha256.as_deref() == Some(egg.sha256())),
                "{name} {} was published with other contents: give it a new version in {}",
                manifest.version(),
                package.join("package.kdl").display()
            );
            stale.push(name.to_string());
            if update {
                // The registry keeps only the version of the source.
                for entry in fs::read_dir(&package).unwrap().flatten() {
                    let path = entry.path();
                    let made = path.extension().is_some_and(|ext| ext == "egg")
                        || path.file_name().is_some_and(|file| file == "index.json");
                    if made {
                        fs::remove_file(path).unwrap();
                    }
                }
                hatchery::publish::publish_dir(&egg, &root, None, hatchery::Timestamp::now())
                    .unwrap();
            }
        }
        let written = fs::read(root.join("index.json")).ok();
        let wanted = hatchery::index::to_json(&catalog).unwrap();
        if written.as_deref() != Some(wanted.as_slice()) {
            stale.push("index.json".into());
            if update {
                fs::write(root.join("index.json"), &wanted).unwrap();
            }
        }
        assert!(
            update || stale.is_empty(),
            "The plugin registry is behind its packages for {}: run ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-core builtin",
            stale.join(", ")
        );
        assert_eq!(catalog.packages.len(), 5);
    }

    #[test]
    fn the_registry_offers_each_package_at_its_version() {
        let found = offered(&root()).unwrap();
        let names: Vec<String> = found.iter().map(|(name, _)| name.to_string()).collect();
        assert_eq!(
            names,
            [
                "rookery/backup",
                "rookery/docker",
                "rookery/github",
                "rookery/incus",
                "rookery/unity"
            ]
        );
        for (name, version) in &found {
            let manifest = hatchery::Manifest::load(&root().join(name.dir())).unwrap();
            assert_eq!(manifest.version(), version, "{name}");
        }
        assert_eq!(offered(&root().join(NAME)), None);
    }
}
