//! Ids, tokens and their hashes.

use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A new id, ordered by time.
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

/// `prefix` and 32 random bytes in hex.
pub fn random_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

/// What the store keeps of a token: its SHA-256.
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The last four characters of a token, to tell tokens apart in a list.
pub fn hint(token: &str) -> String {
    token
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// A slug of a name for URLs: lower-case ASCII letters, digits and single dashes.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.truncate(48);
    if out.is_empty() {
        "space".to_string()
    } else {
        out
    }
}

/// An identifier, quoted for SQL. Names come from validated definitions.
pub(crate) fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A string cut to at most `max` bytes, on a character boundary.
pub(crate) fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
