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
