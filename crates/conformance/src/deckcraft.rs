//! DeckCraft schema 1 (`docs/schemas/deckcraft-1.md`): the reference reader and writer of the
//! shared presentation, limited to what the transcripts need (slides, shapes' geometry and text,
//! notes).
//!
//! Roots: `meta` (map `{app: "deckcraft", schema: 1, title}`), `presentation` (map of JSON
//! texts), `slides` (array of slide ids), `slide:<id>` (map: properties as JSON texts, `shapes`
//! array of top-level shape ids, `shape:<id>` maps, `notes` Yjs text, `notesBody`). A shape map
//! holds its properties as JSON texts (`xfrm`, `geom`, `kind`, `name`, `fill`…), `children` (a
//! group's shape ids), `text` (Yjs text: each paragraph's characters then `\n`) and `textBody`.

use std::collections::HashSet;

use serde_json::{Value as Json, json};
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, Doc, GetString, Map, MapPrelim, MapRef, OffsetKind, Options, Out, ReadTxn, Text, TextPrelim, TextRef, Transact,
    TransactionMut, WriteTxn,
};

use crate::gridcraft::Ids;

pub const APP: &str = "deckcraft";
pub const SCHEMA: i64 = 1;

/// What a peer sees: the title, and each slide's shapes (top level, back to front) with their
/// box and text, and its notes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub title: String,
    pub slides: Vec<SlideModel>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlideModel {
    pub shapes: Vec<ShapeModel>,
    /// The notes' text, paragraphs joined with `\n`.
    pub notes: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapeModel {
    /// `[x, y, w, h]` in points; `None` when the shape takes its box from its layout.
    pub rect: Option<[f64; 4]>,
    /// The text, paragraphs joined with `\n` (a line break is U+000B); `None`: no text body.
    pub text: Option<String>,
}

impl ShapeModel {
    pub fn new(rect: [f64; 4], text: &str) -> ShapeModel {
        ShapeModel { rect: Some(rect), text: Some(text.to_string()) }
    }
}

impl Model {
    /// `{"title", "slides": [{"shapes": [{"rect": [x, y, w, h] | null, "text": "…" | null}], "notes"}]}`.
    pub fn to_json(&self) -> Json {
        json!({
            "title": self.title,
            "slides": self.slides.iter().map(|s| json!({
                "shapes": s.shapes.iter().map(|sh| json!({"rect": sh.rect, "text": sh.text})).collect::<Vec<_>>(),
                "notes": s.notes,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn from_json(j: &Json) -> Result<Model, String> {
        let title = j.get("title").and_then(Json::as_str).unwrap_or("").to_string();
        let mut slides = Vec::new();
        for s in j.get("slides").and_then(Json::as_array).ok_or("a model needs `slides`")? {
            let mut shapes = Vec::new();
            for sh in s.get("shapes").and_then(Json::as_array).map(Vec::as_slice).unwrap_or_default() {
                let rect = match sh.get("rect") {
                    Some(Json::Array(a)) if a.len() == 4 => {
                        let n = |i: usize| a.get(i).and_then(Json::as_f64).ok_or("a rect holds 4 numbers");
                        Some([n(0)?, n(1)?, n(2)?, n(3)?])
                    }
                    None | Some(Json::Null) => None,
                    _ => return Err("`rect` is [x, y, w, h] or null".into()),
                };
                let text = sh.get("text").and_then(Json::as_str).map(str::to_string);
                shapes.push(ShapeModel { rect, text });
            }
            let notes = s.get("notes").and_then(Json::as_str).unwrap_or("").to_string();
            slides.push(SlideModel { shapes, notes });
        }
        Ok(Model { title, slides })
    }
}

/// A Yjs document as DeckCraft makes them: text offsets in UTF-16 code units.
pub fn new_doc(client: u64) -> Doc {
    let mut o = Options::with_client_id(yrs::block::ClientID::new(client));
    o.offset_kind = OffsetKind::Utf16;
    Doc::with_options(o)
}

/// An id: the base-36 form, zero-padded to 8 characters, of a number from 1 to 2^32 − 1.
pub fn id_str(n: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut v = u64::from(n);
    let mut out = [b'0'; 8];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS.get((v % 36) as usize).copied().unwrap_or(b'0');
        v /= 36;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The number of an id; `None` for anything that isn't one.
pub fn parse_id(s: &str) -> Option<u32> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()) {
        return None;
    }
    u32::try_from(u64::from_str_radix(s, 36).ok()?).ok().filter(|v| *v != 0)
}

/// A new random id, from 2^24 to 2^31 as writers choose them.
pub fn new_id(ids: &mut Ids) -> String {
    let n = (1u64 << 24) + ids.bits() % ((1u64 << 31) - (1u64 << 24));
    id_str(n as u32)
}

fn string(o: Option<Out>) -> Option<String> {
    match o? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn sub_map<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<MapRef> {
    match map.get(txn, key)? {
        Out::YMap(m) => Some(m),
        _ => None,
    }
}

fn sub_array<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<ArrayRef> {
    match map.get(txn, key)? {
        Out::YArray(a) => Some(a),
        _ => None,
    }
}

fn sub_text<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<TextRef> {
    match map.get(txn, key)? {
        Out::YText(t) => Some(t),
        _ => None,
    }
}

/// The ids in an array, first occurrence kept, non-strings skipped.
pub fn ids_of<T: ReadTxn>(txn: &T, arr: &ArrayRef) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for v in arr.iter(txn) {
        if let Out::Any(Any::String(s)) = v
            && seen.insert(s.to_string())
        {
            out.push(s.to_string());
        }
    }
    out
}

/// `Err("This room is for <app> <schema>")` when `meta` names another app or schema.
pub fn check_meta<T: ReadTxn>(txn: &T) -> Result<(), String> {
    let Some(meta) = txn.get_map("meta") else { return Ok(()) };
    let app = string(meta.get(txn, "app"));
    let schema = match meta.get(txn, "schema") {
        Some(Out::Any(Any::Number(n))) => Some(match n {
            yrs::any::Number::Int(i) => i as f64,
            yrs::any::Number::Float(f) => f,
        }),
        _ => None,
    };
    match (app.as_deref(), schema) {
        (None, None) => Ok(()),
        (Some(APP), Some(s)) if s == SCHEMA as f64 => Ok(()),
        (a, s) => Err(format!("This room is for {} {}", a.unwrap_or("another app"), s.map(|s| s.to_string()).unwrap_or_default())),
    }
}

/// The slide ids in order (those with a `slide:<id>` map).
pub fn slide_ids<T: ReadTxn>(txn: &T) -> Vec<String> {
    let Some(arr) = txn.get_array("slides") else { return Vec::new() };
    ids_of(txn, &arr).into_iter().filter(|id| parse_id(id).is_some() && txn.get_map(format!("slide:{id}")).is_some()).collect()
}

/// The top-level shape ids of a slide.
pub fn shape_ids<T: ReadTxn>(txn: &T, slide: &str) -> Vec<String> {
    let Some(root) = txn.get_map(format!("slide:{slide}")) else { return Vec::new() };
    let Some(arr) = sub_array(txn, &root, "shapes") else { return Vec::new() };
    ids_of(txn, &arr).into_iter().filter(|id| parse_id(id).is_some() && sub_map(txn, &root, &format!("shape:{id}")).is_some()).collect()
}

/// A Yjs text as the model writes it: its characters but the last paragraph's `\n`.
fn plain<T: ReadTxn>(txn: &T, t: &TextRef) -> String {
    let s = t.get_string(txn);
    s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
}

/// Reads the model.
pub fn read<T: ReadTxn>(txn: &T) -> Result<Model, String> {
    check_meta(txn)?;
    let title = txn.get_map("meta").and_then(|m| string(m.get(txn, "title"))).unwrap_or_default();
    let mut slides = Vec::new();
    for id in slide_ids(txn) {
        let Some(root) = txn.get_map(format!("slide:{id}")) else { continue };
        let mut shapes = Vec::new();
        for sid in shape_ids(txn, &id) {
            let Some(m) = sub_map(txn, &root, &format!("shape:{sid}")) else { continue };
            let rect = string(m.get(txn, "xfrm")).and_then(|t| serde_json::from_str::<Json>(&t).ok()).and_then(|x| {
                let n = |k: &str| x.get(k).and_then(Json::as_f64);
                Some([n("x")?, n("y")?, n("w")?, n("h")?])
            });
            let text = sub_text(txn, &m, "text").map(|t| plain(txn, &t));
            shapes.push(ShapeModel { rect, text });
        }
        let notes = sub_text(txn, &root, "notes").map(|t| plain(txn, &t)).unwrap_or_default();
        slides.push(SlideModel { shapes, notes });
    }
    Ok(Model { title, slides })
}

/// Text as schema 1 holds it: each paragraph followed by `\n` (no formatting).
fn marked(text: &str) -> String {
    let mut s: String = text.split('\n').map(|p| format!("{p}\n")).collect();
    if text.is_empty() {
        s = "\n".into();
    }
    s
}

fn insert_plain(t: &TextRef, txn: &mut TransactionMut, at: u32, s: &str) {
    let len = t.len(txn);
    t.insert_with_attributes(txn, at.min(len), s, Default::default());
}

fn put_json(m: &MapRef, txn: &mut TransactionMut, key: &str, v: Json) {
    m.insert(txn, key, v.to_string());
}

/// Builds the document from a model, in the caller's transaction (origin `seed`): every slide
/// gets an id, every shape a rectangle with its text.
pub fn seed(txn: &mut TransactionMut, model: &Model, ids: &mut Ids) {
    let meta = txn.get_or_insert_map("meta");
    meta.insert(txn, "app", APP);
    meta.insert(txn, "schema", SCHEMA);
    meta.insert(txn, "title", model.title.as_str());
    let pres = txn.get_or_insert_map("presentation");
    put_json(&pres, txn, "slideSize", json!({"width": 960.0, "height": 540.0}));
    let _ = txn.get_or_insert_array("slides");
    for (i, s) in model.slides.iter().enumerate() {
        let id = add_slide(txn, i, ids);
        for sh in &s.shapes {
            add_shape(txn, &id, sh.rect.unwrap_or([0.0, 0.0, 100.0, 100.0]), sh.text.as_deref(), ids);
        }
        if !s.notes.is_empty() {
            set_notes(txn, &id, &s.notes);
        }
    }
}

/// Adds an empty slide at position `at` (the end when past it); its id.
pub fn add_slide(txn: &mut TransactionMut, at: usize, ids: &mut Ids) -> String {
    let id = new_id(ids);
    let root = txn.get_or_insert_map(format!("slide:{id}"));
    root.insert(txn, "shapes", ArrayPrelim::default());
    root.insert(txn, "notes", TextPrelim::new(""));
    put_json(&root, txn, "notesBody", json!({}));
    let slides = txn.get_or_insert_array("slides");
    let n = ids_of(txn, &slides).len();
    let raw = raw_index(txn, &slides, at.min(n));
    slides.insert(txn, raw, id.as_str());
    id
}

/// Removes a slide from the list (its root map stays: root maps can't be deleted).
pub fn delete_slide(txn: &mut TransactionMut, id: &str) {
    let slides = txn.get_or_insert_array("slides");
    let mut i = slides.len(txn);
    while i > 0 {
        i -= 1;
        if string(slides.get(txn, i)).as_deref() == Some(id) {
            slides.remove(txn, i);
        }
    }
}

/// The array index of the `n`-th distinct id (the length when past the end).
fn raw_index<T: ReadTxn>(txn: &T, arr: &ArrayRef, n: usize) -> u32 {
    let mut seen = HashSet::new();
    for (i, v) in arr.iter(txn).enumerate() {
        if let Out::Any(Any::String(s)) = v
            && seen.insert(s.to_string())
            && seen.len() > n
        {
            return i as u32;
        }
    }
    arr.len(txn)
}

/// Adds a rectangle on top of a slide's shapes; its id.
pub fn add_shape(txn: &mut TransactionMut, slide: &str, rect: [f64; 4], text: Option<&str>, ids: &mut Ids) -> String {
    let id = new_id(ids);
    let root = txn.get_or_insert_map(format!("slide:{slide}"));
    let m: MapRef = root.insert(txn, format!("shape:{id}"), MapPrelim::default());
    put_json(&m, txn, "name", json!(format!("Rectangle {}", parse_id(&id).unwrap_or(0))));
    put_json(&m, txn, "kind", json!({"type": "shape"}));
    put_json(&m, txn, "geom", json!({"kind": "preset", "name": "rect"}));
    put_json(&m, txn, "xfrm", json!({"x": rect[0], "y": rect[1], "w": rect[2], "h": rect[3]}));
    if let Some(text) = text {
        put_json(&m, txn, "textBody", json!({"body": {}}));
        let t: TextRef = m.insert(txn, "text", TextPrelim::new(""));
        insert_plain(&t, txn, 0, &marked(text));
    }
    let arr = match sub_array(txn, &root, "shapes") {
        Some(a) => a,
        None => root.insert(txn, "shapes", ArrayPrelim::default()),
    };
    let end = arr.len(txn);
    arr.insert(txn, end, id.as_str());
    id
}

/// Moves or resizes a shape (its `xfrm`, last writer wins).
pub fn set_rect(txn: &mut TransactionMut, slide: &str, shape: &str, rect: [f64; 4]) {
    let root = txn.get_or_insert_map(format!("slide:{slide}"));
    if let Some(m) = sub_map(txn, &root, &format!("shape:{shape}")) {
        put_json(&m, txn, "xfrm", json!({"x": rect[0], "y": rect[1], "w": rect[2], "h": rect[3]}));
    }
}

/// The Yjs text of a shape (or, `shape` `None`, of the slide's notes).
pub fn text_ref<T: ReadTxn>(txn: &T, slide: &str, shape: Option<&str>) -> Option<TextRef> {
    let root = txn.get_map(format!("slide:{slide}"))?;
    match shape {
        Some(s) => sub_text(txn, &sub_map(txn, &root, &format!("shape:{s}"))?, "text"),
        None => sub_text(txn, &root, "notes"),
    }
}

/// The UTF-16 offset of `(paragraph, char)` in a text.
pub fn offset<T: ReadTxn>(txn: &T, t: &TextRef, pos: (usize, usize)) -> u32 {
    let s = t.get_string(txn);
    let mut at = 0u32;
    for (i, para) in s.split('\n').enumerate() {
        if i == pos.0 {
            return at + para.chars().take(pos.1).map(|c| c.len_utf16() as u32).sum::<u32>();
        }
        at += para.encode_utf16().count() as u32 + 1;
    }
    at.saturating_sub(1)
}

/// Types `s` at `(paragraph, char)` of a shape's text (`\n` starts a paragraph).
pub fn type_text(txn: &mut TransactionMut, slide: &str, shape: Option<&str>, pos: (usize, usize), s: &str) {
    let Some(t) = text_ref(txn, slide, shape) else { return };
    let at = offset(txn, &t, pos);
    insert_plain(&t, txn, at, s);
}

/// Deletes `len` UTF-16 code units at `(paragraph, char)` of a shape's text.
pub fn delete_text(txn: &mut TransactionMut, slide: &str, shape: Option<&str>, pos: (usize, usize), len: u32) {
    let Some(t) = text_ref(txn, slide, shape) else { return };
    let at = offset(txn, &t, pos);
    let total = t.len(txn);
    if at.saturating_add(len) <= total && len > 0 {
        t.remove_range(txn, at, len);
    }
}

/// Replaces the slide's notes.
pub fn set_notes(txn: &mut TransactionMut, slide: &str, text: &str) {
    let root = txn.get_or_insert_map(format!("slide:{slide}"));
    let t = match sub_text(txn, &root, "notes") {
        Some(t) => t,
        None => root.insert(txn, "notes", TextPrelim::new("")),
    };
    let len = t.len(txn);
    if len > 0 {
        t.remove_range(txn, 0, len);
    }
    if !text.is_empty() {
        insert_plain(&t, txn, 0, &marked(text));
    }
}

/// Text being edited: the slide, the shape (`None`: the notes), and the selection as
/// `(paragraph, char)` positions.
#[derive(Clone, Debug, PartialEq)]
pub struct Editing {
    pub slide: String,
    pub shape: Option<String>,
    pub anchor: (usize, usize),
    pub caret: (usize, usize),
}

/// DeckCraft's view: the current slide (index), the selected shapes on it, the text being edited.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    pub current: usize,
    pub selected: Vec<String>,
    pub editing: Option<Editing>,
}

/// DeckCraft schema 1 for the reference app. Its commands are DeckCraft's, with the same
/// parameters, so the app replays the transcripts with its own engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeckCraft;

/// `(paragraph, char)` from JSON `[p, c]`.
fn pos(v: Option<&Json>) -> Option<(usize, usize)> {
    let a = v?.as_array()?;
    Some((a.first()?.as_u64()? as usize, a.get(1)?.as_u64()? as usize))
}

/// The `ids` parameter (numbers) as ids.
fn ids_param(params: &Json) -> Option<Vec<String>> {
    Some(params.get("ids")?.as_array()?.iter().filter_map(|v| v.as_u64().map(|n| id_str(n as u32))).collect())
}

/// The end of a text: `(last paragraph, its length)`.
fn end_of<T: ReadTxn>(txn: &T, slide: &str, shape: Option<&str>) -> (usize, usize) {
    let Some(t) = text_ref(txn, slide, shape) else { return (0, 0) };
    let s = t.get_string(txn);
    let s = s.strip_suffix('\n').unwrap_or(&s);
    let paras: Vec<&str> = s.split('\n').collect();
    (paras.len().saturating_sub(1), paras.last().map(|p| p.chars().count()).unwrap_or(0))
}

fn current_slide<T: ReadTxn>(txn: &T, view: &View) -> Result<String, String> {
    slide_ids(txn).get(view.current).cloned().ok_or_else(|| "no current slide".into())
}

impl crate::schema::Schema for DeckCraft {
    const APP: &'static str = APP;
    const SCHEMA: i64 = SCHEMA;
    const DOCUMENT: &'static str = "presentation";
    type Model = Model;
    type View = View;

    fn new_doc(client_id: u64) -> Doc {
        new_doc(client_id)
    }
    /// One empty slide (DeckCraft's own new presentation has a title slide; the transcripts
    /// replace it before they look).
    fn blank() -> Model {
        Model { title: String::new(), slides: vec![Default::default()] }
    }
    fn seed(txn: &mut TransactionMut, model: &Model, ids: &mut Ids) {
        seed(txn, model, ids)
    }
    fn read<T: ReadTxn>(txn: &T) -> Result<Model, String> {
        read(txn)
    }
    fn check_meta<T: ReadTxn>(txn: &T) -> Result<(), String> {
        check_meta(txn)
    }
    fn to_json(m: &Model) -> Json {
        m.to_json()
    }
    fn from_json(j: &Json) -> Result<Model, String> {
        Model::from_json(j)
    }

    /// Sharing keeps the current slide and the selection (by position: the shared document has
    /// new ids); joining starts over.
    fn rejoin_view(view: &View, old: &Doc, new: &Doc, share: bool) -> View {
        if !share {
            return View::default();
        }
        let positions: Vec<usize> = {
            let txn = old.transact();
            let shapes = slide_ids(&txn).get(view.current).map(|s| shape_ids(&txn, s)).unwrap_or_default();
            view.selected.iter().filter_map(|id| shapes.iter().position(|x| x == id)).collect()
        };
        let txn = new.transact();
        let shapes = slide_ids(&txn).get(view.current).map(|s| shape_ids(&txn, s)).unwrap_or_default();
        View { current: view.current, selected: positions.into_iter().filter_map(|i| shapes.get(i).cloned()).collect(), editing: None }
    }

    /// `slide.go {index}`, `edit.select {ids}`, `text.edit {id?, at?}`,
    /// `text.select {anchor, caret}`.
    fn view_command<T: ReadTxn>(txn: &T, view: &mut View, command: &str, params: &Json) -> Option<Result<(), String>> {
        let r = match command {
            "slide.go" => (|| {
                let n = slide_ids(txn).len();
                view.current = params.get("index").and_then(Json::as_u64).map(|i| i as usize).filter(|i| *i < n).ok_or("no such slide")?;
                view.selected.clear();
                view.editing = None;
                Ok(())
            })(),
            "edit.select" => (|| {
                view.selected = ids_param(params).ok_or("edit.select needs `ids`")?;
                view.editing = None;
                Ok(())
            })(),
            "text.edit" => (|| {
                let slide = current_slide(txn, view)?;
                let shape = match params.get("id").and_then(Json::as_u64) {
                    Some(n) => id_str(n as u32),
                    None => view.selected.first().cloned().ok_or("select a shape with text")?,
                };
                let at = pos(params.get("at")).unwrap_or_else(|| end_of(txn, &slide, Some(&shape)));
                view.selected = vec![shape.clone()];
                view.editing = Some(Editing { slide, shape: Some(shape), anchor: at, caret: at });
                Ok(())
            })(),
            "text.select" => (|| {
                let e = view.editing.as_mut().ok_or("not editing text")?;
                e.anchor = pos(params.get("anchor")).ok_or("text.select needs `anchor`")?;
                e.caret = pos(params.get("caret")).ok_or("text.select needs `caret`")?;
                Ok(())
            })(),
            _ => return None,
        };
        Some(r)
    }

    /// `slide.new {layout?, at?}`, `slide.delete {index?}`, `shape.insert {preset, rect, text?}`,
    /// `shape.setBounds {ids?, x, y, w, h}`, `text.insert {text}`, `slide.notes {text, index?}`.
    fn edit(t: &mut TransactionMut, view: &mut View, command: &str, params: &Json, ids: &mut Ids) -> Result<(), String> {
        let text = |k: &str| params.get(k).and_then(Json::as_str).map(str::to_string);
        match command {
            "slide.new" => {
                let n = slide_ids(t).len();
                let at = params.get("at").and_then(Json::as_u64).map(|a| a as usize).unwrap_or(if n == 0 { 0 } else { view.current + 1 }).min(n);
                add_slide(t, at, ids);
                *view = View { current: at, ..View::default() };
            }
            "slide.delete" => {
                let all = slide_ids(t);
                let i = params.get("index").and_then(Json::as_u64).map(|i| i as usize).unwrap_or(view.current);
                let id = all.get(i).ok_or("no such slide")?;
                delete_slide(t, id);
                *view = View { current: i.min(all.len().saturating_sub(2)), ..View::default() };
            }
            "shape.insert" => {
                let r = params.get("rect").and_then(Json::as_array).ok_or("shape.insert needs `rect`")?;
                let n = |i: usize| r.get(i).and_then(Json::as_f64).ok_or("`rect` is [x, y, w, h]");
                let rect = [n(0)?, n(1)?, n(2)?, n(3)?];
                let slide = current_slide(t, view)?;
                let id = add_shape(t, &slide, rect, text("text").as_deref(), ids);
                view.selected = vec![id];
                view.editing = None;
            }
            "shape.setBounds" => {
                let slide = current_slide(t, view)?;
                let n = |k: &str| params.get(k).and_then(Json::as_f64).ok_or(format!("shape.setBounds needs `{k}`"));
                let rect = [n("x")?, n("y")?, n("w")?, n("h")?];
                for id in ids_param(params).unwrap_or_else(|| view.selected.clone()) {
                    set_rect(t, &slide, &id, rect);
                }
            }
            "text.insert" => {
                let what = text("text").ok_or("text.insert needs `text`")?;
                let e = view.editing.clone().ok_or("not editing text")?;
                let (a, b) = if e.anchor <= e.caret { (e.anchor, e.caret) } else { (e.caret, e.anchor) };
                if a != b
                    && let Some(tr) = text_ref(t, &e.slide, e.shape.as_deref())
                {
                    let (x, y) = (offset(t, &tr, a), offset(t, &tr, b));
                    delete_text(t, &e.slide, e.shape.as_deref(), a, y.saturating_sub(x));
                }
                type_text(t, &e.slide, e.shape.as_deref(), a, &what);
                let lines: Vec<&str> = what.split('\n').collect();
                let caret = if lines.len() > 1 {
                    (a.0 + lines.len() - 1, lines.last().map(|l| l.chars().count()).unwrap_or(0))
                } else {
                    (a.0, a.1 + what.chars().count())
                };
                view.editing = Some(Editing { anchor: caret, caret, ..e });
            }
            "slide.notes" => {
                let all = slide_ids(t);
                let i = params.get("index").and_then(Json::as_u64).map(|i| i as usize).unwrap_or(view.current);
                let id = all.get(i).ok_or("no such slide")?;
                set_notes(t, id, &text("text").unwrap_or_default());
            }
            other => return Err(format!("the reference app can't run {other}")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::Transact;

    #[test]
    fn ids_are_32_bit_numbers() {
        assert_eq!(id_str(5), "00000005");
        assert_eq!(parse_id(&id_str(u32::MAX)), Some(u32::MAX));
        assert_eq!(parse_id("k3v9x0qa"), None);
        let mut ids = Ids::seeded(3);
        for _ in 0..100 {
            let n = parse_id(&new_id(&mut ids)).unwrap();
            assert!((1 << 24..1 << 31).contains(&n));
        }
    }

    #[test]
    fn seeds_reads_and_edits() {
        let doc = new_doc(1);
        let mut ids = Ids::seeded(7);
        let model = Model {
            title: "Talk".into(),
            slides: vec![SlideModel { shapes: vec![ShapeModel::new([10.0, 20.0, 300.0, 40.0], "Hello\nworld")], notes: "Say hi".into() }],
        };
        {
            let mut t = doc.transact_mut();
            seed(&mut t, &model, &mut ids);
        }
        assert_eq!(read(&doc.transact()).unwrap(), model);
        let slide = slide_ids(&doc.transact()).remove(0);
        let shape = shape_ids(&doc.transact(), &slide).remove(0);
        {
            let mut t = doc.transact_mut();
            type_text(&mut t, &slide, Some(&shape), (1, 5), "!");
            type_text(&mut t, &slide, Some(&shape), (0, 0), "é𝄞 ");
            delete_text(&mut t, &slide, Some(&shape), (0, 0), 1);
            set_rect(&mut t, &slide, &shape, [1.0, 2.0, 3.0, 4.0]);
            add_slide(&mut t, 0, &mut ids);
        }
        let m = read(&doc.transact()).unwrap();
        assert_eq!(m.slides.len(), 2);
        assert_eq!(m.slides[1].shapes[0], ShapeModel::new([1.0, 2.0, 3.0, 4.0], "𝄞 Hello\nworld!"));
    }
}
