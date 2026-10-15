//! The binary messages of the MQTT binding (protocol 2, Appendix A): one byte for the kind, then
//! the data. The loopback transport uses the same framing.

/// One message from a client to the others in a room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// Kind 0: a Yjs state vector ("here is what I have").
    Sync1(Vec<u8>),
    /// Kind 1: the answer to a sync1, for `to` (0: everyone).
    Sync2 { to: u64, update: Vec<u8> },
    /// Kind 2: an incremental update (ciphertext in a keyed room).
    Update(Vec<u8>),
    /// Kind 3: an awareness update (ciphertext in a keyed room).
    Awareness(Vec<u8>),
    /// Kind 4: the client left.
    Leave,
}

impl Msg {
    pub fn kind(&self) -> u8 {
        match self {
            Msg::Sync1(_) => 0,
            Msg::Sync2 { .. } => 1,
            Msg::Update(_) => 2,
            Msg::Awareness(_) => 3,
            Msg::Leave => 4,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![self.kind()];
        match self {
            Msg::Sync1(data) | Msg::Update(data) | Msg::Awareness(data) => out.extend_from_slice(data),
            Msg::Sync2 { to, update } => {
                out.extend_from_slice(&to.to_be_bytes());
                out.extend_from_slice(update);
            }
            Msg::Leave => {}
        }
        out
    }

    /// `None` for an unknown kind or a sync2 shorter than its addressee.
    pub fn decode(bytes: &[u8]) -> Option<Msg> {
        let (&kind, data) = bytes.split_first()?;
        Some(match kind {
            0 => Msg::Sync1(data.to_vec()),
            1 => {
                let to_bytes: [u8; 8] = data.get(..8)?.try_into().ok()?;
                Msg::Sync2 { to: u64::from_be_bytes(to_bytes), update: data.get(8..)?.to_vec() }
            }
            2 => Msg::Update(data.to_vec()),
            3 => Msg::Awareness(data.to_vec()),
            4 => Msg::Leave,
            _ => return None,
        })
    }
}

/// A room id as one topic level: `/`, `+`, `#`, `%`, control characters and non-ASCII bytes
/// percent-encoded (upper-case hex).
pub fn topic_segment(room: &str) -> String {
    let mut out = String::with_capacity(room.len());
    for b in room.bytes() {
        if matches!(b, b'/' | b'+' | b'#' | b'%') || !(0x20..0x7f).contains(&b) {
            out.push_str(&format!("%{b:02X}"));
        } else {
            out.push(char::from(b));
        }
    }
    out
}

/// A client id from a topic level (decimal, below 2^53).
pub fn parse_client(level: &str) -> Option<u64> {
    if level.is_empty() || level.len() > 16 || !level.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    level.parse::<u64>().ok().filter(|c| *c < crate::collab::MAX_CLIENT_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        for m in [
            Msg::Sync1(vec![1, 2, 3]),
            Msg::Sync2 { to: 0, update: vec![] },
            Msg::Sync2 { to: 9_007_199_254_740_991, update: vec![9, 9] },
            Msg::Update(vec![0; 40]),
            Msg::Awareness(vec![7]),
            Msg::Leave,
        ] {
            assert_eq!(Msg::decode(&m.encode()), Some(m));
        }
        assert_eq!(Msg::Sync2 { to: 258, update: vec![5] }.encode(), vec![1, 0, 0, 0, 0, 0, 0, 1, 2, 5]);
        assert_eq!(Msg::decode(&[]), None);
        assert_eq!(Msg::decode(&[1, 0, 0]), None);
        assert_eq!(Msg::decode(&[9]), None);
    }

    #[test]
    fn topics_escape_wildcards() {
        assert_eq!(topic_segment("mqtt:budget-7f3a"), "mqtt:budget-7f3a");
        assert_eq!(topic_segment("a/b+c#d%e"), "a%2Fb%2Bc%23d%25e");
        assert_eq!(topic_segment("città"), "citt%C3%A0");
        assert_eq!(parse_client("42"), Some(42));
        assert_eq!(parse_client("-1"), None);
        assert_eq!(parse_client("99999999999999999"), None);
    }
}
