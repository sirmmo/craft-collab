//! craft-collab-conformance: the conformance kit of craft plugin protocol 2
//! (`docs/protocol-2.md`, Appendix C).
//!
//! - [`transport`]: drives a transport plugin over stdio as two and three apps would, and checks
//!   what it does ([`transport::run`] → a [`transport::Report`]).
//! - [`transcript`]: the app-side transcripts (`conformance/*.jsonl`): generated with `yrs`,
//!   replayed against the reference app.
//! - [`refapp`]: the reference app (GridCraft schema 1) both use.
//! - [`gridcraft`]: GridCraft schema 1, reader and writer (`docs/schemas/gridcraft-1.md`).
//! - [`deckcraft`], [`deckref`]: DeckCraft schema 1, reader and writer
//!   (`docs/schemas/deckcraft-1.md`), and its reference app; [`deck_transcripts`] writes the
//!   `deckcraft-1-*` transcripts.
//! - [`harness`], [`crypto`], [`awareness`]: plugin processes, AES-256-GCM, awareness encoding.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

pub mod awareness;
pub mod crypto;
pub mod deck_transcripts;
pub mod deckcraft;
pub mod deckref;
pub mod gridcraft;
pub mod harness;
pub mod refapp;
pub mod transcript;
pub mod transport;
