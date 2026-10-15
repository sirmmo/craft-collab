//! What the reference app needs to know about an app's shared document: its `meta`, how to seed
//! and read it, and the app's commands the transcripts run. One implementation per schema:
//! [`crate::gridcraft::GridCraft`] (GridCraft schema 1), [`crate::deckcraft::DeckCraft`]
//! (DeckCraft schema 1) and [`crate::wordcraft::WordCraft`] (WordCraft schema 1).

use serde_json::Value as Json;
use yrs::{Doc, ReadTxn, TransactionMut};

use crate::gridcraft::Ids;

pub trait Schema {
    /// `meta.app` and `collab/joined`'s `app`.
    const APP: &'static str;
    /// `meta.schema`.
    const SCHEMA: i64;
    /// What the app calls its document, in error messages.
    const DOCUMENT: &'static str = "document";
    /// What a peer sees (the transcripts' `model` lines).
    type Model: Clone + PartialEq + std::fmt::Debug + Default;
    /// The app's view state: the active sheet, the selection.
    type View: Clone + std::fmt::Debug + Default;

    /// A Yjs document as the app makes them (its client id, its text offsets).
    fn new_doc(client_id: u64) -> Doc {
        Doc::with_client_id(client_id)
    }
    /// A new document, as the app starts with.
    fn blank() -> Self::Model;
    /// Builds the document from a model, in the caller's transaction (origin `seed`).
    fn seed(txn: &mut TransactionMut, model: &Self::Model, ids: &mut Ids);
    /// Reads the model; `Err` for another app's or schema's document.
    fn read<T: ReadTxn>(txn: &T) -> Result<Self::Model, String>;
    /// `Err("This room is for <app> <schema>")` when `meta` names another app or schema.
    fn check_meta<T: ReadTxn>(txn: &T) -> Result<(), String>;
    fn to_json(m: &Self::Model) -> Json;
    fn from_json(j: &Json) -> Result<Self::Model, String>;
    /// The view after joining a room, which rebuilds the document: `old` is the document before,
    /// `new` the shared one (`share`: seeded from the same model with new ids; `join`: empty
    /// until the room's state arrives). By default the view starts over.
    fn rejoin_view(_view: &Self::View, _old: &Doc, _new: &Doc, _share: bool) -> Self::View {
        Self::View::default()
    }
    /// A command that changes only the view (selecting, switching sheets): `None` when
    /// `command` isn't one.
    fn view_command<T: ReadTxn>(txn: &T, view: &mut Self::View, command: &str, params: &Json) -> Option<Result<(), String>>;
    /// Runs an editing command in the caller's transaction (origin `local`).
    fn edit(txn: &mut TransactionMut, view: &mut Self::View, command: &str, params: &Json, ids: &mut Ids) -> Result<(), String>;
}
