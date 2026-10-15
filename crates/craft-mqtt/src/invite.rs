//! Broker addresses (`mqtt://host:port`, `mqtts://host:port`) and invitation links
//! (`craft+mqtt://host:port/<room>?prefix=<topic prefix>#k=<key>`, Appendix A).

use craft_collab_kit::{b64, ids};

/// The topic prefix when none is given.
pub const DEFAULT_PREFIX: &str = "craft/v1";

/// An MQTT broker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broker {
    pub tls: bool,
    /// A DNS name or an IP address (an IPv6 address without brackets).
    pub host: String,
    pub port: u16,
}

fn valid_host(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

/// `host`, `host:port`, `[v6]`, `[v6]:port` → (host, port).
fn split_host_port(s: &str) -> Result<(String, Option<u16>), String> {
    let bad_port = || format!("“{s}” has a port that isn't a number from 1 to 65535");
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(bad_port);
    if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or_else(|| format!("“{s}” isn't a host"))?;
        if host.is_empty() || !host.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.') {
            return Err(format!("“{s}” isn't a host"));
        }
        return match after {
            "" => Ok((host.to_ascii_lowercase(), None)),
            p => Ok((host.to_ascii_lowercase(), Some(port(p.strip_prefix(':').ok_or_else(bad_port)?)?))),
        };
    }
    let (host, p) = match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, Some(port(p)?)),
        _ => (s, None),
    };
    if !valid_host(host) || host.contains(':') {
        return Err(format!("“{s}” isn't a host name"));
    }
    Ok((host.to_ascii_lowercase(), p))
}

impl Broker {
    /// `mqtt://host[:port]` (default port 1883), `mqtts://host[:port]` (8883); `tcp://`, `ssl://`,
    /// `tls://` and a bare `host[:port]` are understood too.
    pub fn parse(url: &str) -> Result<Broker, String> {
        let u = url.trim();
        let (tls, rest) = match u.split_once("://") {
            Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
                "mqtt" | "tcp" => (false, rest),
                "mqtts" | "ssl" | "tls" => (true, rest),
                other => return Err(format!("craft-mqtt connects to mqtt:// and mqtts:// brokers, not {other}://")),
            },
            None => (false, u),
        };
        let authority = rest.trim_end_matches('/');
        if authority.is_empty() {
            return Err("Type the broker's address, for example mqtts://broker.example:8883.".into());
        }
        if authority.contains('/') || authority.contains('@') || authority.contains('?') || authority.contains('#') {
            return Err(format!("“{url}” isn't a broker address: use mqtt://host:port (the user name goes in its own field)"));
        }
        let (host, port) = split_host_port(authority)?;
        Ok(Broker { tls, host, port: port.unwrap_or(if tls { 8883 } else { 1883 }) })
    }

    fn authority(&self) -> String {
        if self.host.contains(':') { format!("[{}]:{}", self.host, self.port) } else { format!("{}:{}", self.host, self.port) }
    }

    /// `mqtts://host:port`.
    pub fn url(&self) -> String {
        format!("{}://{}", if self.tls { "mqtts" } else { "mqtt" }, self.authority())
    }

    /// `host:port, TLS` for the status dialog.
    pub fn describe(&self) -> String {
        format!("{}{}", self.authority(), if self.tls { ", TLS" } else { ", not encrypted in transit" })
    }
}

/// Percent-encodes a query value.
fn encode_query(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode_query(s: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hex = [bytes.next()?, bytes.next()?];
                out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}

/// A topic prefix: 1–128 characters, no wildcards, no empty levels at the ends.
pub fn valid_prefix(p: &str) -> bool {
    (1..=128).contains(&p.len()) && !p.contains(['+', '#', '\0']) && !p.starts_with('/') && !p.ends_with('/') && !p.chars().any(char::is_control)
}

/// What an invitation link holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    pub broker: Broker,
    /// The room's name: the room id is `mqtt:<name>`.
    pub name: String,
    pub prefix: String,
    pub key: Option<[u8; 32]>,
}

impl Invitation {
    /// A new room on a broker, with a key when `encrypt`.
    pub fn create(broker: Broker, prefix: &str, name: Option<String>, encrypt: bool) -> Invitation {
        Invitation { broker, name: name.unwrap_or_else(ids::room_name), prefix: prefix.to_string(), key: encrypt.then(ids::key) }
    }

    /// The room id: `mqtt:<name>`, the same for everyone whatever address they reach the
    /// broker by.
    pub fn room(&self) -> String {
        format!("mqtt:{}", self.name)
    }

    /// `craft+mqtt://host:port/<name>[?prefix=…][#k=<key>]` (`craft+mqtts` over TLS).
    pub fn link(&self) -> String {
        let mut s = format!("craft+{}/{}", self.broker.url(), self.name);
        if self.prefix != DEFAULT_PREFIX {
            s.push_str(&format!("?prefix={}", encode_query(&self.prefix)));
        }
        if let Some(k) = &self.key {
            s.push_str(&format!("#k={}", b64::encode_url(k)));
        }
        s
    }

    /// Reads an invitation link.
    pub fn parse(link: &str) -> Result<Invitation, String> {
        let l = link.trim();
        let bad = || format!("“{}” isn't a craft-mqtt invitation (craft+mqtt://host:port/room#k=key)", l.chars().take(200).collect::<String>());
        let rest = l.strip_prefix("craft+").ok_or_else(bad)?;
        let (rest, fragment) = match rest.split_once('#') {
            Some((r, f)) => (r, Some(f)),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };
        let (scheme, after) = rest.split_once("://").ok_or_else(bad)?;
        if !matches!(scheme.to_ascii_lowercase().as_str(), "mqtt" | "mqtts") {
            return Err(bad());
        }
        let (authority, name) = after.split_once('/').ok_or_else(bad)?;
        let broker = Broker::parse(&format!("{scheme}://{authority}"))?;
        let name = name.trim_end_matches('/');
        if !ids::valid_room_name(name) {
            return Err(format!("“{name}” isn't a room name"));
        }
        let mut prefix = DEFAULT_PREFIX.to_string();
        for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            if let Some(v) = pair.strip_prefix("prefix=") {
                prefix = decode_query(v).filter(|p| valid_prefix(p)).ok_or_else(|| "the invitation's topic prefix isn't valid".to_string())?;
            }
        }
        let mut key = None;
        for pair in fragment.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            if let Some(v) = pair.strip_prefix("k=") {
                let bytes = b64::decode_lenient(v).ok_or("the invitation's key isn't readable: copy the whole link")?;
                key = Some(<[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "the invitation's key isn't 32 bytes: copy the whole link".to_string())?);
            }
        }
        Ok(Invitation { broker, name: name.to_string(), prefix, key })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brokers() {
        assert_eq!(Broker::parse("mqtts://Broker.Example").unwrap(), Broker { tls: true, host: "broker.example".into(), port: 8883 });
        assert_eq!(Broker::parse("broker.example:1884").unwrap(), Broker { tls: false, host: "broker.example".into(), port: 1884 });
        assert_eq!(Broker::parse("tcp://127.0.0.1:1883/").unwrap().url(), "mqtt://127.0.0.1:1883");
        assert_eq!(Broker::parse("mqtt://[::1]:1999").unwrap().url(), "mqtt://[::1]:1999");
        for bad in ["", "http://x", "mqtt://u:p@host", "mqtt://host:0", "mqtt://host:99999", "mqtt://ho st", "mqtt://host/path"] {
            assert!(Broker::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn invitations_round_trip() {
        let b = Broker::parse("mqtts://broker.example").unwrap();
        let inv = Invitation::create(b.clone(), DEFAULT_PREFIX, None, true);
        let link = inv.link();
        assert!(link.starts_with("craft+mqtts://broker.example:8883/"), "{link}");
        assert!(link.contains("#k="));
        assert_eq!(Invitation::parse(&link).unwrap(), inv);
        assert_eq!(inv.room(), format!("mqtt:{}", inv.name));
        let plain = Invitation { broker: b, name: "budget-7f3a".into(), prefix: "team a/craft".into(), key: None };
        assert_eq!(plain.link(), "craft+mqtts://broker.example:8883/budget-7f3a?prefix=team%20a/craft");
        assert_eq!(Invitation::parse(&plain.link()).unwrap(), plain);
        // A key in the standard alphabet with padding works too.
        let std_key = format!("craft+mqtt://h:1883/r#k={}", b64::encode(&[7u8; 32]));
        assert_eq!(Invitation::parse(&std_key).unwrap().key, Some([7u8; 32]));
        for bad in ["https://x/y", "craft+mqtt://h:1883/", "craft+mqtt://h:1883/a b", "craft+mqtt://h/r#k=abc", "craft+mqtt://h/r?prefix=%2B"] {
            assert!(Invitation::parse(bad).is_err(), "{bad}");
        }
    }
}
