//! craft-mqtt: live co-editing over MQTT for the craft apps (GridCraft, WordCraft, DeckCraft), as
//! a transport plugin speaking craft plugin protocol 2 (`docs/protocol-2.md`, Appendix A).
//!
//! - [`invite`]: broker addresses and invitation links (`craft+mqtt://host:port/<room>#k=<key>`).
//! - [`link`]: one MQTT connection per room: topics, presence with a will, the retained
//!   snapshot, reconnecting with backoff.
//! - [`commands`]: Share, Join, Leave, Status, Settings, with protocol-1.2 dialogs.
//! - [`manifest`]: the `plugin.json` that `--manifest` prints.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

pub mod commands;
pub mod invite;
pub mod link;
pub mod manifest;
pub mod tls;
