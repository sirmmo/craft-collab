//! Awareness updates in the y-protocols encoding: `varuint count`, then per client `varuint
//! clientId`, `varuint clock`, `varstring` JSON state (`null` when the client left).

use serde_json::Value as Json;

fn write_var(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn read_var(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut n: u64 = 0;
    for shift in (0..64).step_by(7) {
        let b = *data.get(*pos)?;
        *pos += 1;
        n |= u64::from(b & 0x7f).checked_shl(shift)?;
        if b & 0x80 == 0 {
            return Some(n);
        }
    }
    None
}

/// One client's entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub client: u64,
    pub clock: u64,
    pub state: Json,
}

pub fn encode(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    write_var(&mut out, entries.len() as u64);
    for e in entries {
        write_var(&mut out, e.client);
        write_var(&mut out, e.clock);
        let s = e.state.to_string();
        write_var(&mut out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }
    out
}

pub fn decode(data: &[u8]) -> Option<Vec<Entry>> {
    let mut pos = 0;
    let n = read_var(data, &mut pos)?;
    let mut out = Vec::new();
    for _ in 0..n.min(10_000) {
        let client = read_var(data, &mut pos)?;
        let clock = read_var(data, &mut pos)?;
        let len = usize::try_from(read_var(data, &mut pos)?).ok()?;
        let end = pos.checked_add(len)?;
        let state = serde_json::from_slice(data.get(pos..end)?).ok()?;
        pos = end;
        out.push(Entry { client, clock, state });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips() {
        let e = vec![Entry { client: 300, clock: 1, state: json!({"user": {"name": "Ada"}}) }, Entry { client: 2, clock: 9, state: Json::Null }];
        let bytes = encode(&e);
        assert_eq!(bytes.first(), Some(&2));
        assert_eq!(decode(&bytes).unwrap(), e);
        assert!(decode(&[1, 1]).is_none());
    }
}
