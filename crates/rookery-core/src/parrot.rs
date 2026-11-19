//! The Parrot runtime and driver eggs that apply carries from the server to the host.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::PathBuf;

use hatchery::{Lock, Manifest, Name, Platform};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{Result, eggs, node, paths::Paths, plugins::Plugin};

/// Stage the selected host features' exact Parrot targets for the platform the apply role supports.
pub(crate) fn stage(
    paths: &Paths,
    config: &Value,
    plugins: &[Plugin],
    vars: &mut Value,
) -> Result<Vec<NamedTempFile>> {
    let features = node::features(config);
    let selected: BTreeSet<_> = features
        .iter()
        .filter_map(|feature| feature.split_once('/').map(|(plugin, _)| plugin))
        .collect();
    let mut drivers = Vec::new();
    for plugin in plugins.iter().filter(|p| selected.contains(p.id.as_str())) {
        let manifest = Manifest::load(&plugin.package_dir)?;
        let Some(target) = manifest.target("parrot") else {
            continue;
        };
        let variant = if target.variants.is_empty() {
            None
        } else {
            Some(
                target
                    .select_variant(&Platform::new("linux", "x86_64"))
                    .ok_or_else(|| {
                        format!("{} has no Parrot driver for Linux x86_64", plugin.package)
                    })?
                    .key(),
            )
        };
        drivers.push((
            Name::parse(&plugin.package)?,
            manifest.version().clone(),
            variant,
        ));
    }
    drivers.sort_unstable_by(|(a, _, _), (b, _, _)| a.cmp(b));
    if drivers.is_empty() {
        return Ok(vec![]);
    }

    let runtime = std::env::var_os("ROOKERY_PARROT_BIN")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("set ROOKERY_PARROT_BIN to an absolute path to the Linux x86_64 Parrot binary on the server")?;
    let mut executable = std::fs::File::open(runtime)?;
    let mut binary = NamedTempFile::new()?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let size = executable.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        let chunk = &buffer[..size];
        binary.write_all(chunk)?;
        digest.update(chunk);
    }

    let lock = Lock::load(&eggs::own_dir(&paths.config_dir))?;
    let mut pins = Vec::new();
    let mut files = vec![binary];
    for (name, version, variant) in drivers {
        let pin = lock
            .get(&name)
            .ok_or_else(|| format!("{name}: no installed pin"))?;
        if pin.version != version {
            return Err(
                format!("{name}: the installed version changed while preparing the apply").into(),
            );
        }
        let egg = eggs::parrot_egg(paths, pin, variant)?;
        let sha = egg.sha256();
        digest.update(sha.as_bytes());
        let mut file = NamedTempFile::new()?;
        file.write_all(egg.bytes())?;
        pins.push(json!({
            "source": file.path(),
            "file": egg.file_name(),
            "sha256": sha,
        }));
        files.push(file);
    }
    let release: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    vars["rookery_parrot"] = json!({
        "runtime": files[0].path(),
        "release": release,
        "eggs": pins,
    });
    Ok(files)
}
