//! Random ids, client ids and room keys.

use std::time::{SystemTime, UNIX_EPOCH};

/// Fills `buf` with random bytes from the operating system; if that fails (it shouldn't), with a
/// time-seeded generator, so an id is never all zeros.
pub fn fill(buf: &mut [u8]) {
    if getrandom::fill(buf).is_ok() {
        return;
    }
    let mut x = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E37_79B9_7F4A_7C15) ^ u64::from(std::process::id());
    for b in buf.iter_mut() {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        *b = (z ^ (z >> 31)) as u8;
    }
}

/// A 32-byte AES-256 room key.
pub fn key() -> [u8; 32] {
    let mut k = [0u8; 32];
    fill(&mut k);
    k
}

/// A random 64-bit number.
pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}

/// `n` random characters of `0-9a-z` (unbiased).
pub fn base36(n: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = String::with_capacity(n);
    let mut pool = [0u8; 64];
    while out.len() < n {
        fill(&mut pool);
        for b in pool {
            // 252 = 7 × 36: rejecting the rest keeps every digit equally likely.
            if let Some(d) = DIGITS.get(usize::from(b % 36)).filter(|_| b < 252 && out.len() < n) {
                out.push(char::from(*d));
            }
        }
    }
    out
}

/// A room name: 16 base-36 characters (82 bits), hard to guess.
pub fn room_name() -> String {
    base36(16)
}

/// A room name others may type: 1–128 of `A-Z a-z 0-9 . _ -`.
pub fn valid_room_name(name: &str) -> bool {
    (1..=128).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_have_the_right_shape() {
        let a = super::base36(8);
        assert_eq!(a.len(), 8);
        assert!(a.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()));
        assert_ne!(super::room_name(), super::room_name());
        assert!(super::valid_room_name("budget-7f3a"));
        assert!(!super::valid_room_name("a/b"));
        assert!(!super::valid_room_name(""));
    }
}
