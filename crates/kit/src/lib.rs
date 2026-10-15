//! craft-collab-kit: what the craft-collab transport plugins share. A transport speaks the craft
//! plugin protocol 2 (`docs/protocol-2.md`): it moves opaque Yjs bytes between the people in a
//! room and knows nothing about documents.
//!
//! - [`runtime`]: the push channel over standard input and output, the rooms a transport holds,
//!   relaying `collab/*`, presence, status and snapshot compaction. A transport plugs in a
//!   [`runtime::Plugin`] (its commands) and a [`runtime::Link`] per room (its network).
//! - [`frame`]: the binary messages of the MQTT binding (Appendix A), reused by every transport.
//! - [`collab`]: parsing what the app sends, users, limits.
//! - [`ui`]: dialogs and the commands every transport has (leave, status).
//! - [`store`], [`ids`], [`b64`]: settings on disk, random ids and keys, base64.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

pub mod b64;
pub mod collab;
pub mod frame;
pub mod ids;
pub mod runtime;
pub mod store;
pub mod ui;
