//! Sealing source credentials (API keys, app tokens) at rest.
//!
//! XChaCha20-Poly1305 with a 32-byte master key from `ATLAS_MASTER_KEY`. The
//! associated data binds each ciphertext to its row (`user|connection|kind`),
//! so a sealed key copied into another user's row fails to open rather than
//! working for them. `KEY_VERSION` is stored alongside, for rotation later.

use crate::{random_bytes, Result, StateError};
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

/// The version written with every new ciphertext.
pub const KEY_VERSION: i64 = 1;

pub struct MasterKey(XChaCha20Poly1305);

/// A sealed secret, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: Vec<u8>,
    pub cipher: Vec<u8>,
    pub key_version: i64,
}

impl MasterKey {
    /// From the base64 text in `ATLAS_MASTER_KEY` (32 bytes once decoded;
    /// `openssl rand -base64 32` makes one).
    pub fn from_base64(text: &str) -> Result<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .map_err(|e| StateError::Crypto(format!("ATLAS_MASTER_KEY is not base64: {e}")))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(StateError::Crypto(format!(
                "ATLAS_MASTER_KEY must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        Ok(Self(XChaCha20Poly1305::new(bytes.into())))
    }

    pub fn seal(&self, plaintext: &[u8], aad: &str) -> Result<Sealed> {
        let nonce = random_bytes(24);
        let cipher = self
            .0
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad: aad.as_bytes() })
            .map_err(|_| StateError::Crypto("sealing failed".into()))?;
        Ok(Sealed { nonce, cipher, key_version: KEY_VERSION })
    }

    pub fn open(&self, sealed: &Sealed, aad: &str) -> Result<Vec<u8>> {
        if sealed.key_version != KEY_VERSION {
            return Err(StateError::Crypto(format!("unknown key version {}", sealed.key_version)));
        }
        if sealed.nonce.len() != 24 {
            return Err(StateError::Crypto("bad nonce".into()));
        }
        self.0
            .decrypt(XNonce::from_slice(&sealed.nonce), Payload { msg: &sealed.cipher, aad: aad.as_bytes() })
            .map_err(|_| StateError::Crypto("the stored credential can't be opened with this key".into()))
    }
}

/// What a connection's credential is bound to.
pub fn credential_aad(user: crate::UserId, connection_id: i64, kind: &str) -> String {
    format!("{}|{}|{}", user.get(), connection_id, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> MasterKey {
        MasterKey::from_bytes(&[byte; 32]).unwrap()
    }

    #[test]
    fn round_trips() {
        let k = key(7);
        let s = k.seal(b"immich-api-key", "1|2|immich").unwrap();
        assert_ne!(s.cipher, b"immich-api-key");
        assert_eq!(k.open(&s, "1|2|immich").unwrap(), b"immich-api-key");
    }

    #[test]
    fn nonces_differ() {
        let k = key(7);
        assert_ne!(k.seal(b"x", "a").unwrap().nonce, k.seal(b"x", "a").unwrap().nonce);
    }

    #[test]
    fn another_row_cant_open_it() {
        let k = key(7);
        let s = k.seal(b"secret", "1|2|immich").unwrap();
        assert!(k.open(&s, "3|2|immich").is_err(), "another user");
        assert!(k.open(&s, "1|4|immich").is_err(), "another connection");
        assert!(k.open(&s, "1|2|opencloud").is_err(), "another kind");
    }

    #[test]
    fn another_key_cant_open_it() {
        let s = key(7).seal(b"secret", "a").unwrap();
        assert!(key(8).open(&s, "a").is_err());
    }

    #[test]
    fn tampering_is_detected() {
        let k = key(7);
        let mut s = k.seal(b"secret", "a").unwrap();
        s.cipher[0] ^= 1;
        assert!(k.open(&s, "a").is_err());
    }

    #[test]
    fn parses_base64_keys_of_the_right_length() {
        assert!(MasterKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").is_ok());
        assert!(MasterKey::from_base64("c2hvcnQ=").is_err());
        assert!(MasterKey::from_base64("not base64!").is_err());
    }
}
