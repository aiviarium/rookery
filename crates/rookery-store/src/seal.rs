//! Sealing: XChaCha20-Poly1305 under the server's 32-byte key.
//!
//! A sealed value has its nonce and ciphertext in columns of its row, the id of the key in `key_id`, and
//! associated data that names the row (`rookery.<table>|<id>`), so a value copied to another row does
//! not open. Rookery's key file holds 64 hex digits and is read as it is.

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{Error, Result};

/// What the URL-signing key is derived under, so it is not the key the AEAD uses.
const SIGNING_LABEL: &[u8] = b"rookery/url-signing/v1";

#[derive(Clone)]
pub struct Key {
    bytes: [u8; 32],
    id: String,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Key({})", self.id)
    }
}

/// What a sealed value is kept as.
#[derive(Debug, Clone, PartialEq)]
pub struct Sealed {
    pub key_id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// The associated data of a row.
pub fn row(table: &str, id: Uuid) -> Vec<u8> {
    named_row(table, &id.to_string())
}

/// The associated data of a row with a text id.
pub fn named_row(table: &str, id: &str) -> Vec<u8> {
    format!("rookery.{table}|{id}").into_bytes()
}

impl Key {
    pub fn from_bytes(bytes: [u8; 32]) -> Key {
        let digest = Sha256::new()
            .chain_update(b"rookery key id\n")
            .chain_update(bytes)
            .finalize();
        Key {
            bytes,
            id: format!("k{}", hex::encode(&digest[..4])),
        }
    }

    /// 64 hex digits, as Rookery's key file has them, or 32 bytes in base64.
    pub fn parse(text: &str) -> Result<Key> {
        let text = text.trim();
        let bytes = if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
            hex::decode(text).ok()
        } else {
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .or_else(|_| {
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(text.trim_end_matches('='))
                })
                .ok()
        };
        let bytes: [u8; 32] = bytes.and_then(|b| b.try_into().ok()).ok_or_else(|| {
            Error::Invalid("a secret key is 64 hex digits, or 32 bytes in base64".into())
        })?;
        Ok(Key::from_bytes(bytes))
    }

    pub fn generate() -> Key {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        Key::from_bytes(bytes)
    }

    /// The key as 64 hex digits, for its file.
    pub fn to_hex(&self) -> String {
        hex::encode(self.bytes)
    }

    /// The id rows sealed under this key carry. It says which key a row needs, and nothing of the key.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// A signature over `data` under this key, for a short-lived address the server hands out.
    pub fn sign(&self, data: &[u8]) -> String {
        use hmac::{Hmac, KeyInit, Mac};
        let mut derive =
            <Hmac<Sha256> as KeyInit>::new_from_slice(&self.bytes).expect("any key length");
        derive.update(SIGNING_LABEL);
        let subkey = derive.finalize().into_bytes();
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&subkey).expect("any key length");
        mac.update(data);
        hex::encode(mac.finalize().into_bytes())
    }

    pub fn seal(&self, aad: &[u8], plain: &[u8]) -> Result<Sealed> {
        let cipher = XChaCha20Poly1305::new((&self.bytes).into());
        let mut nonce = [0u8; 24];
        rand::rng().fill_bytes(&mut nonce);
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
            .map_err(|_| Error::Invalid("sealing failed".into()))?;
        Ok(Sealed {
            key_id: self.id.clone(),
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    pub fn open(&self, aad: &[u8], sealed: &Sealed) -> Result<Vec<u8>> {
        if sealed.key_id != self.id {
            return Err(Error::Unavailable(format!(
                "this value is sealed under the key {}, and this server has the key {}",
                sealed.key_id, self.id
            )));
        }
        if sealed.nonce.len() != 24 {
            return Err(Error::Invalid(
                "a sealed value has a nonce of 24 bytes".into(),
            ));
        }
        let cipher = XChaCha20Poly1305::new((&self.bytes).into());
        cipher
            .decrypt(
                XNonce::from_slice(&sealed.nonce),
                Payload {
                    msg: &sealed.ciphertext,
                    aad,
                },
            )
            .map_err(|_| {
                Error::Unavailable(
                    "a sealed value does not open: it was changed, or moved from another row"
                        .into(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_opens_in_its_own_row_only() {
        let key = Key::parse(&"ab".repeat(32)).unwrap();
        assert_eq!(key.to_hex(), "ab".repeat(32));
        let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
        let sealed = key.seal(&row("secrets", one), b"hunter2").unwrap();
        assert_eq!(key.open(&row("secrets", one), &sealed).unwrap(), b"hunter2");
        assert!(key.open(&row("secrets", two), &sealed).is_err());
        assert!(key.open(&row("connections", one), &sealed).is_err());
        let other = Key::generate();
        assert_ne!(other.id(), key.id());
        assert!(matches!(
            other.open(&row("secrets", one), &sealed),
            Err(Error::Unavailable(_))
        ));
        assert!(Key::parse("too short").is_err());
        let b64 = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        assert_eq!(
            Key::parse(&b64).unwrap().id(),
            Key::from_bytes([7u8; 32]).id()
        );
    }
}
