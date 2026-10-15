//! craft-collab-conformance: the conformance kit of craft plugin protocol 2
//! (`docs/protocol-2.md`, Appendix C).
//!
//! - [`transport`]: drives a transport plugin over stdio as two and three apps would, and checks
//!   what it does ([`transport::run`] → a [`transport::Report`]).
//! - [`transcript`]: the app-side transcripts (`conformance/*.jsonl`): generated with `yrs`,
//!   replayed against the reference app.
//! - [`refapp`]: the reference app both use, for any [`schema::Schema`].
//! - [`gridcraft`]: GridCraft schema 1, reader and writer (`docs/schemas/gridcraft-1.md`).
//! - [`wordcraft`]: WordCraft schema 1, reader and writer (`docs/schemas/wordcraft-1.md`).
//! - [`harness`], [`crypto`], [`awareness`]: plugin processes, AES-256-GCM, awareness encoding.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

pub mod awareness;
pub mod crypto;
pub mod gridcraft;
pub mod harness;
pub mod refapp;
pub mod schema;
pub mod transcript;
pub mod transport;
pub mod wordcraft;
mod wordcraft_transcripts;
