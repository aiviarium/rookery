//! The store of this process: the server's tables once `rookery serve` or a command of its machine
//! put them in place, and in tests a store in memory for each installation. Besides, the locks of
//! files under the data directory, for work on this machine's own files.

use super::Store;
use crate::paths::Paths;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Arc;

/// Hold an exclusive `flock` on the file, made if it is not there; `None`
/// when another holds it. A child this process forks holds the lock too until
/// it runs its program, so a lock that is taken is tried again for a moment.
pub fn hold_lock(path: &Path) -> crate::Result<Option<fs::File>> {
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    for _ in 0..25 {
        // SAFETY: the descriptor is open; the lock goes when the file is closed.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(None)
}

/// Hold the lock, waiting for it up to half a minute; `what` says what the
/// other rookery is doing, for the error when it takes longer.
pub fn wait_lock(path: &Path, what: &str) -> crate::Result<fs::File> {
    // Each try waits half a second.
    for _ in 0..60 {
        if let Some(lock) = hold_lock(path)? {
            return Ok(lock);
        }
    }
    Err(format!(
        "Another rookery has been {what} for a while; {} is held",
        path.display()
    )
    .into())
}

/// The store of `rookery serve` on Postgres, which every installation of the process uses once set.
static SERVER: std::sync::RwLock<Option<Arc<Store>>> = std::sync::RwLock::new(None);

/// Make `store` the one [`shared`] gives from now on, or none again, before anything opens a store.
pub fn set_shared(store: Option<Arc<Store>>) {
    *SERVER.write().unwrap_or_else(|e| e.into_inner()) = store;
}

/// The store of this process. Outside the server, and the commands of its machine that put its
/// tables in place, there is none; a test gets one in memory for each installation.
pub fn shared(paths: &Paths) -> crate::Result<Arc<Store>> {
    if let Some(store) = SERVER.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return Ok(store);
    }
    #[cfg(any(test, feature = "testing"))]
    {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        type Opened = Mutex<HashMap<(std::path::PathBuf, std::path::PathBuf), Arc<Store>>>;
        static OPENED: OnceLock<Opened> = OnceLock::new();
        let mut opened = OPENED
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = (paths.config_dir.clone(), paths.data_dir.clone());
        Ok(opened
            .entry(key)
            .or_insert_with(|| Arc::new(Store::memory()))
            .clone())
    }
    #[cfg(not(any(test, feature = "testing")))]
    {
        let _ = paths;
        Err("Rookery's state is in the database of rookery serve: this runs in the server, or beside it with its ROOKERY_DATABASE_URL".into())
    }
}
