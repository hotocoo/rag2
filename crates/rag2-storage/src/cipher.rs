//! Encryption at rest: XChaCha20-Poly1305 over every stored value. A random 24-byte nonce per
//! value (safe to generate randomly at this size) is prepended; the record's key is bound as
//! associated data, so a ciphertext copied under another key (another tenant's, say) fails to
//! decrypt instead of being read as that key's value.

use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rag2_core::{Error, Result};

const NONCE_LEN: usize = 24;
pub const KEY_LEN: usize = 32;

pub struct Cipher(XChaCha20Poly1305);

impl Cipher {
    pub fn new(key: &[u8]) -> Result<Self> {
        if key.len() != KEY_LEN {
            return Err(Error::InvalidInput(format!(
                "encryption key must be {KEY_LEN} bytes"
            )));
        }
        XChaCha20Poly1305::new_from_slice(key)
            .map(Cipher)
            .map_err(|_| Error::InvalidInput("invalid encryption key".into()))
    }

    pub fn seal(&self, key: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
        let nonce = XNonce::generate();
        let ct = self
            .0
            .encrypt(
                &nonce,
                Payload {
                    msg: plain,
                    aad: key,
                },
            )
            .map_err(|_| Error::Internal("encryption failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn open(&self, key: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < NONCE_LEN {
            return Err(Error::Storage("encrypted value is truncated".into()));
        }
        let (n, ct) = sealed.split_at(NONCE_LEN);
        let nonce = XNonce::try_from(n).map_err(|_| Error::Storage("bad nonce".into()))?;
        self.0
            .decrypt(&nonce, Payload { msg: ct, aad: key })
            .map_err(|_| {
                Error::Storage("value failed authentication (wrong key or tampered data)".into())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_and_are_bound_to_their_key() {
        let c = Cipher::new(&[7u8; KEY_LEN]).expect("cipher");
        let s = c.seal(b"t1\0doc", b"secret").expect("seal");
        assert_ne!(&s[NONCE_LEN..], b"secret");
        assert_eq!(c.open(b"t1\0doc", &s).expect("open"), b"secret");
        assert!(
            c.open(b"t2\0doc", &s).is_err(),
            "moved ciphertext must not decrypt"
        );
        let other = Cipher::new(&[8u8; KEY_LEN]).expect("cipher");
        assert!(other.open(b"t1\0doc", &s).is_err());
        assert!(Cipher::new(&[0u8; 16]).is_err());
    }
}
