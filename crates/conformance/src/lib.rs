//! craft-collab-conformance: the conformance kit of craft plugin protocol 2
//! (`docs/protocol-2.md`, Appendix C).
//!
//! - [`transport`]: drives a transport plugin over stdio as two and three apps would, and checks
//!   what it does ([`transport::run`] → a [`transport::Report`]).
//! - [`transcript`]: the app-side transcripts (`conformance/*.jsonl`): generated with `yrs`,
//!   replayed against the reference app of each transcript's app ([`transcript::App`] is what an
//!   app's own engine implements to replay them); [`deck_transcripts`] and
//!   `wordcraft_transcripts` write the `deckcraft-1-*` and `wordcraft-1-*` ones.
//! - [`chat`]: document chat (`docs/chat.md`), the same in every schema; `chat_transcripts`
//!   writes the `<app>-1-chat*` transcripts.
//! - [`refapp`]: the reference app both use, for any [`schema::Schema`] (GridCraft, DeckCraft,
//!   WordCraft).
//! - [`gridcraft`]: GridCraft schema 1, reader and writer (`docs/schemas/gridcraft-1.md`).
//! - [`deckcraft`]: DeckCraft schema 1, reader and writer (`docs/schemas/deckcraft-1.md`).
//! - [`wordcraft`]: WordCraft schema 1, reader and writer (`docs/schemas/wordcraft-1.md`).
//! - [`harness`], [`crypto`], [`awareness`]: plugin processes, AES-256-GCM, awareness encoding.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

pub mod awareness;
pub mod chat;
mod chat_transcripts;
pub mod crypto;
pub mod deck_transcripts;
pub mod deckcraft;
pub mod gridcraft;
pub mod harness;
pub mod refapp;
pub mod schema;
pub mod transcript;
pub mod transport;
pub mod wordcraft;
mod wordcraft_transcripts;
