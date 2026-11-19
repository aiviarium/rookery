//! The ids of plugins: what a package may be named, and what Rookery keeps for itself.

/// The plugin ids Rookery keeps for itself: `rookery`, under which its own settings, secrets and
/// data are kept, and `sign-in`, whose actions are its own.
pub const RESERVED_IDS: &[&str] = &[ROOKERY, "sign-in"];

/// The plugin under which Rookery keeps its own settings, secrets and data.
pub const ROOKERY: &str = "rookery";

/// Whether a plugin id is one a package may have.
pub fn valid_plugin_id(id: &str) -> bool {
    valid_name(id) && !RESERVED_IDS.contains(&id)
}

/// A name that is safe as a directory name and inside a remote command,
/// such as the id of a plugin or of one of its features.
pub fn valid_name(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_lowercase())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}
