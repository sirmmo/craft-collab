//! End-to-end encryption (protocol 2, section 4): AES-256-GCM, payload `nonce (12) || ciphertext
//! || tag (16)`, associated data `room + "\n" + method`.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

/// Nonce and tag.
pub const OVERHEAD: usize = 28;

/// The associated data of a message.
pub fn aad(room: &str, method: &str) -> Vec<u8> {
    format!("{room}\n{method}").into_bytes()
}

/// Encrypts `plain` for `room` as `method`, with the given nonce.
pub fn seal(key: &[u8; 32], room: &str, method: &str, plain: &[u8], nonce: [u8; 12]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad: &aad(room, method) }).map_err(|e| e.to_string())?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypts, or `None` when the key, room, method or bytes don't match.
pub fn open(key: &[u8; 32], room: &str, method: &str, data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < OVERHEAD {
        return None;
    }
    let (nonce, ct) = data.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher.decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad: &aad(room, method) }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_room_and_method() {
        let k = [9u8; 32];
        let c = seal(&k, "r", "collab/update", b"hello", [1; 12]).unwrap();
        assert_eq!(c.len(), 5 + OVERHEAD);
        assert_eq!(open(&k, "r", "collab/update", &c).unwrap(), b"hello");
        assert!(open(&k, "r", "collab/awareness", &c).is_none());
        assert!(open(&k, "r2", "collab/update", &c).is_none());
        assert!(open(&[8u8; 32], "r", "collab/update", &c).is_none());
        let mut bad = c.clone();
        if let Some(last) = bad.last_mut() {
            *last ^= 1;
        }
        assert!(open(&k, "r", "collab/update", &bad).is_none());
    }
}
