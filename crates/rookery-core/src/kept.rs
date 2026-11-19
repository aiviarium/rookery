//! Files that rookery writes from what the server keeps, such as the keys of its
//! key ring, which ssh and Ansible are given as files ([`crate::keys::ring`]).
//!
//! What is kept wins over a file that differs from it. Such a file is moved
//! aside to `<file>.replaced` rather than overwritten, as it may be the only
//! copy of a key some machine still trusts.

use crate::Result;
use crate::paths::write_private;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A name in the folder of `file` for a file moved aside: `<file>.replaced`,
/// numbered when that is taken.
fn aside(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".replaced");
    let first = PathBuf::from(&name);
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|number| {
            let mut numbered = name.clone();
            numbered.push(format!(".{number}"));
            PathBuf::from(numbered)
        })
        .find(|candidate| !candidate.exists())
        .unwrap_or(first)
}

/// Make `file` hold `value` with `mode`, moving a file that holds something
/// else aside first, as it may be the only copy of a key some machine trusts.
pub fn write_file(file: &Path, value: &[u8], mode: u32) -> Result<()> {
    match fs::read(file) {
        Ok(there) if there == value => {
            fs::set_permissions(file, fs::Permissions::from_mode(mode))?;
            return Ok(());
        }
        Ok(_) => {
            let moved = aside(file);
            fs::rename(file, &moved)?;
            eprintln!(
                "rookery: {} differed from rookery's store, which has the one in use; the file that was there is {} now",
                file.display(),
                moved.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", file.display()).into()),
    }
    write_private(file, value)?;
    fs::set_permissions(file, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_store_wins_and_a_differing_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let private = data_dir.join("keys/rookery");
        fs::create_dir_all(private.parent().unwrap()).unwrap();
        fs::write(&private, "other").unwrap();
        fs::write(data_dir.join("keys/rookery.replaced"), "older").unwrap();
        write_file(&private, b"private", 0o600).unwrap();
        assert_eq!(fs::read(&private).unwrap(), b"private");
        assert_eq!(mode(&private), 0o600);
        assert_eq!(
            fs::read_to_string(data_dir.join("keys/rookery.replaced.1")).unwrap(),
            "other"
        );
        // Settled, nothing moves again.
        write_file(&private, b"private", 0o600).unwrap();
        assert!(!data_dir.join("keys/rookery.replaced.2").exists());
    }
}
