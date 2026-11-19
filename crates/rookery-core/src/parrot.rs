//! The Parrot runtime and driver eggs that apply carries from the server to the host.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::PathBuf;

use hatchery::{Lock, Manifest, Name};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{Result, eggs, node, paths::Paths, plugins::Plugin};

/// The role variable that carries the staged Parrot release. Only an apply
/// sets it, from what it staged itself.
pub(crate) const VAR: &str = "rookery_parrot";

/// Stage the selected host features' exact Parrot targets for the platform the apply role supports.
pub(crate) fn stage(
    paths: &Paths,
    config: &Value,
    plugins: &[Plugin],
    vars: &mut Value,
) -> Result<Vec<NamedTempFile>> {
    // A profile may name this variable, and what the host runs must be what
    // the server staged, checksums and all.
    if let Some(vars) = vars.as_object_mut() {
        vars.remove(VAR);
    }
    let features = node::features(config);
    let selected: BTreeSet<_> = features
        .iter()
        .filter_map(|feature| feature.split_once('/').map(|(plugin, _)| plugin))
        .collect();
    let mut drivers = Vec::new();
    for plugin in plugins.iter().filter(|p| selected.contains(p.id.as_str())) {
        let manifest = Manifest::load(&plugin.package_dir)?;
        match eggs::parrot_target(&plugin.package_dir)? {
            eggs::ParrotTarget::None => continue,
            eggs::ParrotTarget::Base => drivers.push((
                Name::parse(&plugin.package)?,
                manifest.version().clone(),
                None,
            )),
            eggs::ParrotTarget::Variant(key) => drivers.push((
                Name::parse(&plugin.package)?,
                manifest.version().clone(),
                Some(key),
            )),
        }
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
    let mut runtime_digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let size = executable.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        let chunk = &buffer[..size];
        binary.write_all(chunk)?;
        digest.update(chunk);
        runtime_digest.update(chunk);
    }
    let runtime_sha256 = hex(runtime_digest.finalize());

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
    let release = hex(digest.finalize());
    vars[VAR] = json!({
        "runtime": files[0].path(),
        "runtime_sha256": runtime_sha256,
        "release": release,
        "eggs": pins,
    });
    Ok(files)
}

/// A digest as lower-case hex, as Ansible's checksums are written.
fn hex(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_cannot_stage_its_own_parrot_release() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_dir: home.path().join("config"),
            data_dir: home.path().join("data"),
        };
        // No host feature selects a driver, so nothing is staged; the
        // profile's own variable is gone all the same.
        let mut vars = json!({VAR: {"release": "00", "runtime": "/bin/sh"}});
        let files = stage(&paths, &json!({}), &[], &mut vars).unwrap();
        assert!(files.is_empty());
        assert!(vars.get(VAR).is_none(), "{vars}");
    }
}
