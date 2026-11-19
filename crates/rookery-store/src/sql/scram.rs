//! A SCRAM-SHA-256 verifier for a role's password, so `ALTER ROLE ... PASSWORD` carries the verifier and
//! never the password: the plaintext does not reach Postgres's statement log or `pg_stat_statements`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

const ITERATIONS: u32 = 4096;

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(data);
    m.finalize().into_bytes().into()
}

/// PBKDF2-HMAC-SHA-256 with one output block, which is all SCRAM needs.
fn hi(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut first = salt.to_vec();
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac(password, &first);
    let mut out = u;
    for _ in 1..iterations {
        u = hmac(password, &u);
        for (o, b) in out.iter_mut().zip(u) {
            *o ^= b;
        }
    }
    out
}

/// The verifier of `password` with `salt`: `SCRAM-SHA-256$<iterations>:<salt>$<StoredKey>:<ServerKey>`.
pub fn verifier_with_salt(password: &str, salt: &[u8]) -> String {
    let salted = hi(password.as_bytes(), salt, ITERATIONS);
    let client_key = hmac(&salted, b"Client Key");
    let stored_key = Sha256::digest(client_key);
    let server_key = hmac(&salted, b"Server Key");
    format!(
        "SCRAM-SHA-256${ITERATIONS}:{}${}:{}",
        STANDARD.encode(salt),
        STANDARD.encode(stored_key),
        STANDARD.encode(server_key)
    )
}

/// The verifier of `password` with a random salt.
pub fn verifier(password: &str) -> String {
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    verifier_with_salt(password, &salt)
}
