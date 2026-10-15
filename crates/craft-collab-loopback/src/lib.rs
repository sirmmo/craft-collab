//! craft-collab-loopback: a co-editing transport for one computer. The first instance starts a
//! tiny relay on `127.0.0.1` (port `CRAFT_COLLAB_LOOPBACK_PORT`, default 39390) inside its own
//! process; every instance, including the first, connects to it. When the process holding the
//! relay exits, the others reconnect and one of them starts a new relay (the apps then re-sync).
//! It is for development and for the apps' end-to-end tests: any local program can connect, and
//! nothing is stored once the relay stops.
//!
//! - [`relay`]: the relay (JSON lines over TCP, the MQTT binding's semantics: messages fanned out
//!   to the room, a retained snapshot and retained presence, a "will" on disconnect).
//! - [`link`]: one connection per room, reconnecting.
//! - [`commands`]: Share and Join with a room name, Leave, Status.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

pub mod commands;
pub mod link;
pub mod manifest;
pub mod relay;

/// The relay's port: `CRAFT_COLLAB_LOOPBACK_PORT`, else 39390.
pub fn port() -> u16 {
    std::env::var("CRAFT_COLLAB_LOOPBACK_PORT").ok().and_then(|p| p.trim().parse::<u16>().ok()).filter(|p| *p > 0).unwrap_or(39390)
}
