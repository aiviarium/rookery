//! Where the `rookery serve` of this machine has its Unix socket, which its own CLI and the programs
//! it starts reach it on. The server makes the first of [`places`]; a client takes the first there.

use std::path::PathBuf;

/// The places in order: `ROOKERY_SOCKET`, the user's runtime folder, the system's for root, and the
/// data folder (`XDG_DATA_HOME`, or `~/.local/share`) for a user with no runtime folder, as the
/// server in a container is.
pub fn places(var: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    let mut places = vec![];
    if let Some(path) = set("ROOKERY_SOCKET") {
        places.push(PathBuf::from(path));
    }
    if let Some(dir) = set("XDG_RUNTIME_DIR") {
        places.push(PathBuf::from(dir).join("rookery/server.sock"));
    }
    if is_root() {
        places.push(PathBuf::from("/run/rookery/server.sock"));
    }
    let data = set("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    if let Some(data) = data {
        places.push(data.join("rookery/server.sock"));
    }
    places
}

/// Where the server makes its socket.
pub fn bound(var: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    places(var).into_iter().next()
}

/// The socket a client reaches: the first of the places that is there.
pub fn found(var: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    places(var).into_iter().find(|path| path.exists())
}

#[cfg(unix)]
fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    // The owner of this process's own entry in /proc, without libc.
    std::fs::metadata("/proc/self").is_ok_and(|meta| meta.uid() == 0)
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(set: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            set.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn each_place_in_its_order() {
        // As the server in its image: no runtime folder, its data in /data.
        let image = vars(&[("XDG_DATA_HOME", "/data"), ("HOME", "/home/rookery")]);
        assert_eq!(
            places(&image).last(),
            Some(&PathBuf::from("/data/rookery/server.sock"))
        );
        let named = vars(&[
            ("ROOKERY_SOCKET", "/srv/rookery.sock"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        assert_eq!(bound(&named), Some(PathBuf::from("/srv/rookery.sock")));
        assert_eq!(
            places(&named)[1],
            PathBuf::from("/run/user/1000/rookery/server.sock")
        );
        let home = vars(&[("HOME", "/home/ada")]);
        assert_eq!(
            places(&home).last(),
            Some(&PathBuf::from("/home/ada/.local/share/rookery/server.sock"))
        );
    }
}
