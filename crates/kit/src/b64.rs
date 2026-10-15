//! Base64: the standard alphabet with padding on the wire (protocol 2, section 3); invitations
//! carry keys in the URL-safe alphabet without padding and accept either.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};

/// Standard alphabet, with padding.
pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Standard alphabet, with padding (what the protocol sends).
pub fn decode(text: &str) -> Option<Vec<u8>> {
    STANDARD.decode(text).ok()
}

/// URL-safe alphabet without padding (keys in invitation links).
pub fn encode_url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Any of the four common forms (standard or URL-safe, padded or not).
pub fn decode_lenient(text: &str) -> Option<Vec<u8>> {
    let t = text.trim();
    [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD].iter().find_map(|e| e.decode(t).ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trips() {
        let key: Vec<u8> = (0u8..32).map(|i| i.wrapping_mul(37)).collect();
        assert_eq!(super::decode(&super::encode(&key)).unwrap(), key);
        assert_eq!(super::decode_lenient(&super::encode_url(&key)).unwrap(), key);
        assert_eq!(super::decode_lenient(&super::encode(&key)).unwrap(), key);
        assert!(super::decode("not base64!").is_none());
    }
}
