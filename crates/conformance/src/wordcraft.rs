//! WordCraft schema 1 (`docs/schemas/wordcraft-1.md`): the reference reader and writer of the
//! shared document, limited to what the transcripts need (the title, the body's paragraphs with
//! their text, formatting and properties, and tables).
//!
//! Roots: `meta` (map `{app: "wordcraft", schema: 1, title}`), `body` (an XmlFragment of `<p>`
//! and `<table>` elements; a `<p>`'s one XmlText child is its text, with formatting as Yjs text
//! attributes and inline objects as embeds; its `pPr.<key>` attributes are its properties).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::{Value as Json, json};
use yrs::types::text::{Diff, YChange};
use yrs::types::{Attrs, Delta};
use yrs::{
    Any, Map, Out, ReadTxn, Text, TransactionMut, WriteTxn, Xml, XmlElementPrelim, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim,
    XmlTextRef,
};

use craft_collab_kit::collab::put;

use crate::gridcraft::Ids;

pub const APP: &str = "wordcraft";
pub const SCHEMA: i64 = 1;

/// Tables in tables are read this deep.
pub const MAX_DEPTH: usize = 16;
/// Prefix of the paragraph property attributes.
pub const PPR: &str = "pPr.";
/// The object replacement character an embed reads as.
pub const OBJ: char = '\u{FFFC}';

// ---------------------------------------------------------------------------------------------
// The model

/// What a peer sees: the title and the body.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub title: String,
    pub body: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Para(Para),
    /// Rows of cells of blocks.
    Table(Vec<Vec<Vec<Block>>>),
}

/// A paragraph: its properties (`pPr.*` without the prefix) and its runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Para {
    pub props: BTreeMap<String, Json>,
    pub runs: Vec<Run>,
}

/// Text with one set of attributes, or one embed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub attrs: BTreeMap<String, Json>,
    pub embed: Option<Json>,
}

impl Run {
    pub fn plain(text: &str) -> Run {
        Run { text: text.into(), ..Run::default() }
    }

    pub fn with(text: &str, attrs: &[(&str, Json)]) -> Run {
        Run { text: text.into(), attrs: attrs.iter().map(|(k, v)| (k.to_string(), norm(v))).collect(), embed: None }
    }
}

impl Para {
    /// A paragraph of runs.
    pub fn of(runs: Vec<Run>) -> Para {
        Para { props: BTreeMap::new(), runs }.normalized()
    }

    /// A paragraph of plain text.
    pub fn text(text: &str) -> Para {
        Para::of(vec![Run::plain(text)])
    }

    pub fn styled(mut self, style: &str) -> Para {
        self.props.insert("style".into(), json!(style));
        self
    }

    /// Its text (an embed is U+FFFC).
    pub fn plain_text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Adjacent runs with the same attributes merged, empty ones dropped.
    pub fn normalized(mut self) -> Para {
        let mut out: Vec<Run> = Vec::new();
        for r in self.runs {
            if r.text.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.embed.is_none() && r.embed.is_none() && last.attrs == r.attrs => last.text.push_str(&r.text),
                _ => out.push(r),
            }
        }
        self.runs = out;
        self
    }

    /// The attributes of the character at byte `off` of [`Para::plain_text`] (`None` past the end).
    pub fn attrs_at(&self, off: usize) -> Option<&BTreeMap<String, Json>> {
        let mut start = 0;
        for r in &self.runs {
            if off < start + r.text.len() {
                return Some(&r.attrs);
            }
            start += r.text.len();
        }
        None
    }
}

/// Integral numbers as integers (Yjs numbers are JavaScript numbers).
pub fn norm(v: &Json) -> Json {
    match v {
        Json::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 9.0e15 => json!(f as i64),
            _ => v.clone(),
        },
        Json::Array(a) => Json::Array(a.iter().map(norm).collect()),
        Json::Object(o) => Json::Object(o.iter().map(|(k, v)| (k.clone(), norm(v))).collect()),
        _ => v.clone(),
    }
}

fn block_json(b: &Block) -> Json {
    match b {
        Block::Para(p) => {
            let runs: Vec<Json> = p
                .runs
                .iter()
                .map(|r| {
                    let mut o = serde_json::Map::new();
                    o.insert("text".into(), json!(r.text));
                    for (k, v) in &r.attrs {
                        o.insert(k.clone(), v.clone());
                    }
                    if let Some(e) = &r.embed {
                        o.insert("embed".into(), e.clone());
                    }
                    Json::Object(o)
                })
                .collect();
            let mut o = json!({"text": p.plain_text(), "runs": runs});
            if !p.props.is_empty() {
                put(&mut o, "props", json!(p.props));
            }
            o
        }
        Block::Table(rows) => {
            json!({"table": rows.iter().map(|r| r.iter().map(|c| c.iter().map(block_json).collect::<Vec<_>>()).collect::<Vec<_>>()).collect::<Vec<_>>()})
        }
    }
}

fn block_from_json(j: &Json, depth: usize) -> Result<Block, String> {
    if depth > MAX_DEPTH {
        return Err("tables nested too deep".into());
    }
    if let Some(rows) = j.get("table").and_then(Json::as_array) {
        let mut out = Vec::new();
        for r in rows {
            let mut row = Vec::new();
            for c in r.as_array().ok_or("a table row is an array of cells")? {
                let mut cell = Vec::new();
                for b in c.as_array().ok_or("a table cell is an array of blocks")? {
                    cell.push(block_from_json(b, depth + 1)?);
                }
                row.push(cell);
            }
            out.push(row);
        }
        return Ok(Block::Table(out));
    }
    let mut runs = Vec::new();
    match j.get("runs").and_then(Json::as_array) {
        Some(rs) => {
            for r in rs {
                let o = r.as_object().ok_or("a run is an object")?;
                let text = o.get("text").and_then(Json::as_str).ok_or("a run needs `text`")?.to_string();
                let attrs = o.iter().filter(|(k, _)| *k != "text" && *k != "embed").map(|(k, v)| (k.clone(), norm(v))).collect();
                runs.push(Run { text, attrs, embed: o.get("embed").map(norm) });
            }
        }
        None => runs.push(Run::plain(j.get("text").and_then(Json::as_str).ok_or("a paragraph needs `text` or `runs`")?)),
    }
    let props = j.get("props").and_then(Json::as_object).map(|o| o.iter().map(|(k, v)| (k.clone(), norm(v))).collect()).unwrap_or_default();
    let p = Para { props, runs }.normalized();
    if let Some(t) = j.get("text").and_then(Json::as_str)
        && t != p.plain_text()
    {
        return Err(format!("a paragraph's `text` ({t:?}) isn't its runs' ({:?})", p.plain_text()));
    }
    Ok(Block::Para(p))
}

impl Model {
    /// `{"title", "body": [{"text", "runs": [{"text", …attributes}], "props"?} | {"table": [[[blocks]]]}]}`.
    pub fn to_json(&self) -> Json {
        json!({"title": self.title, "body": self.body.iter().map(block_json).collect::<Vec<_>>()})
    }

    pub fn from_json(j: &Json) -> Result<Model, String> {
        let title = j.get("title").and_then(Json::as_str).unwrap_or("").to_string();
        let body = j.get("body").and_then(Json::as_array).ok_or("a model needs `body`")?.iter().map(|b| block_from_json(b, 0)).collect::<Result<_, _>>()?;
        Ok(Model { title, body })
    }

    /// A model of paragraphs.
    pub fn of(title: &str, paras: Vec<Para>) -> Model {
        Model { title: title.into(), body: paras.into_iter().map(Block::Para).collect() }
    }

    /// The paragraph at a block path (`[i]`, or `[table, row, cell, i, …]`).
    pub fn para_mut(&mut self, path: &[usize]) -> Option<&mut Para> {
        let (first, rest) = path.split_first()?;
        let mut b = self.body.get_mut(*first)?;
        let mut rest = rest;
        loop {
            match (b, rest) {
                (Block::Para(p), []) => return Some(p),
                (Block::Table(rows), [r, c, i, more @ ..]) => {
                    b = rows.get_mut(*r)?.get_mut(*c)?.get_mut(*i)?;
                    rest = more;
                }
                _ => return None,
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// JSON ↔ Yjs values

/// A JSON value as a Yjs value.
pub fn to_any(v: &Json) -> Any {
    match v {
        Json::Null => Any::Null,
        Json::Bool(b) => Any::Bool(*b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) if i.unsigned_abs() < (1 << 53) => Any::Number(yrs::any::Number::Int(i)),
            (_, Some(f)) => Any::Number(yrs::any::Number::Float(f)),
            _ => Any::Null,
        },
        Json::String(s) => Any::String(Arc::from(s.as_str())),
        Json::Array(a) => Any::Array(a.iter().map(to_any).collect::<Vec<_>>().into()),
        Json::Object(o) => Any::Map(Arc::new(o.iter().map(|(k, v)| (k.clone(), to_any(v))).collect::<HashMap<_, _>>())),
    }
}

/// A Yjs value as JSON (integral numbers as integers; bytes as an array of numbers).
pub fn to_json(a: &Any) -> Json {
    match a {
        Any::Null | Any::Undefined => Json::Null,
        Any::Bool(b) => json!(b),
        Any::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => json!(i),
            (_, Some(f)) => serde_json::Number::from_f64(f).map(Json::Number).unwrap_or(Json::Null),
            _ => Json::Null,
        },
        Any::String(s) => json!(s.as_ref()),
        Any::Buffer(b) => json!(b.as_ref()),
        Any::Array(v) => Json::Array(v.iter().map(to_json).collect()),
        Any::Map(m) => {
            let sorted: BTreeMap<&String, Json> = m.iter().map(|(k, v)| (k, to_json(v))).collect();
            Json::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
        }
    }
}

fn attrs_of(m: &BTreeMap<String, Json>) -> Attrs {
    m.iter().map(|(k, v)| (Arc::from(k.as_str()), to_any(v))).collect()
}

// ---------------------------------------------------------------------------------------------
// Reading

fn string(o: Option<Out>) -> Option<String> {
    match o? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// `Err("This room is for <app> <schema>")` when `meta` names another app or schema; `Ok` when
/// it's WordCraft 1 or there's no `meta` yet.
pub fn check_meta<T: ReadTxn>(txn: &T) -> Result<(), String> {
    let Some(meta) = txn.get_map("meta") else { return Ok(()) };
    let app = string(meta.get(txn, "app"));
    let schema = match meta.get(txn, "schema") {
        Some(Out::Any(Any::Number(n))) => n.as_f64(),
        _ => None,
    };
    match (app.as_deref(), schema) {
        (None, None) => Ok(()),
        (Some(APP), Some(s)) if s == SCHEMA as f64 => Ok(()),
        (a, s) => Err(format!("This room is for {} {}", a.unwrap_or("another app"), s.map(|s| s.to_string()).unwrap_or_default())),
    }
}

/// The block elements of a story or cell (`<p>` and `<table>`), other children skipped.
pub fn blocks_of<T: ReadTxn, F: XmlFragment>(txn: &T, f: &F) -> Vec<XmlElementRef> {
    f.children(txn)
        .filter_map(|c| match c {
            XmlOut::Element(e) if matches!(e.try_tag().map(|t| t.as_ref()), Some("p" | "table")) => Some(e),
            _ => None,
        })
        .collect()
}

fn children_tagged<T: ReadTxn>(txn: &T, e: &XmlElementRef, tag: &str) -> Vec<XmlElementRef> {
    e.children(txn)
        .filter_map(|c| match c {
            XmlOut::Element(e) if e.try_tag().map(|t| t.as_ref()) == Some(tag) => Some(e),
            _ => None,
        })
        .collect()
}

/// A paragraph's text (its first XmlText child).
pub fn text_of<T: ReadTxn>(txn: &T, p: &XmlElementRef) -> Option<XmlTextRef> {
    p.children(txn).find_map(|c| match c {
        XmlOut::Text(t) => Some(t),
        _ => None,
    })
}

/// The runs of one XmlText.
pub fn read_text<T: ReadTxn>(txn: &T, t: &XmlTextRef) -> Vec<Run> {
    let mut runs = Vec::new();
    for d in t.diff(txn, YChange::identity) {
        let Diff { insert, attributes, .. } = d;
        let attrs: BTreeMap<String, Json> = attributes
            .map(|a| a.iter().filter(|(_, v)| !matches!(v, Any::Null | Any::Undefined)).map(|(k, v)| (k.to_string(), to_json(v))).collect())
            .unwrap_or_default();
        match insert {
            Out::Any(Any::String(s)) => {
                let s: String = s.chars().filter(|c| *c != OBJ).collect();
                runs.push(Run { text: s, attrs, embed: None });
            }
            Out::Any(other) => runs.push(Run { text: OBJ.to_string(), attrs, embed: Some(to_json(&other)) }),
            _ => {}
        }
    }
    runs
}

/// A paragraph element.
pub fn read_para<T: ReadTxn>(txn: &T, p: &XmlElementRef) -> Para {
    let mut props = BTreeMap::new();
    for (k, v) in p.attributes(txn) {
        if let Some(key) = k.strip_prefix(PPR)
            && let Out::Any(a) = v
            && !matches!(a, Any::Null | Any::Undefined)
        {
            props.insert(key.to_string(), to_json(&a));
        }
    }
    let mut runs = Vec::new();
    for c in p.children(txn) {
        if let XmlOut::Text(t) = c {
            runs.extend(read_text(txn, &t));
        }
    }
    Para { props, runs }.normalized()
}

/// The blocks of a story or cell.
pub fn read_blocks<T: ReadTxn, F: XmlFragment>(txn: &T, f: &F, depth: usize) -> Vec<Block> {
    blocks_of(txn, f).iter().filter_map(|e| read_block(txn, e, depth)).collect()
}

fn read_block<T: ReadTxn>(txn: &T, e: &XmlElementRef, depth: usize) -> Option<Block> {
    match e.try_tag()?.as_ref() {
        "p" => Some(Block::Para(read_para(txn, e))),
        "table" if depth < MAX_DEPTH => {
            let rows = children_tagged(txn, e, "tr")
                .iter()
                .map(|tr| {
                    children_tagged(txn, tr, "tc")
                        .iter()
                        .map(|tc| {
                            let mut blocks = read_blocks(txn, tc, depth + 1);
                            if !matches!(blocks.last(), Some(Block::Para(_))) {
                                blocks.push(Block::Para(Para::default()));
                            }
                            blocks
                        })
                        .collect()
                })
                .collect();
            Some(Block::Table(rows))
        }
        _ => None,
    }
}

/// Reads the model.
pub fn read<T: ReadTxn>(txn: &T) -> Result<Model, String> {
    check_meta(txn)?;
    let title = txn.get_map("meta").and_then(|m| string(m.get(txn, "title"))).unwrap_or_default();
    let body = match txn.get_xml_fragment("body") {
        Some(f) => read_blocks(txn, &f, 0),
        None => Vec::new(),
    };
    Ok(Model { title, body })
}

// ---------------------------------------------------------------------------------------------
// Writing

/// The Yjs index of byte `off` of a paragraph's text (an embed's U+FFFC counts 1, other
/// characters their UTF-8 bytes: the documents use byte offsets).
pub fn y_index(text: &str, off: usize) -> u32 {
    text.char_indices().take_while(|(i, _)| *i < off).map(|(_, c)| if c == OBJ { 1 } else { c.len_utf8() as u32 }).sum()
}

/// Inserts runs into `t` at Yjs index `at`, each with exactly its attributes: keys set at `at`
/// (`before`: the attributes of the character before) but not in the run are cleared.
pub fn insert_runs(txn: &mut TransactionMut, t: &XmlTextRef, mut at: u32, runs: &[Run], before: &BTreeMap<String, Json>) {
    for r in runs {
        let mut attrs = attrs_of(&r.attrs);
        for k in before.keys() {
            if !r.attrs.contains_key(k) {
                attrs.insert(Arc::from(k.as_str()), Any::Null);
            }
        }
        match &r.embed {
            Some(e) => {
                t.insert_embed_with_attributes(txn, at, to_any(e), attrs);
                at += 1;
            }
            None => {
                let text: String = r.text.chars().filter(|c| *c != OBJ).collect();
                if !text.is_empty() {
                    t.insert_with_attributes(txn, at, &text, attrs);
                    at += text.len() as u32;
                }
            }
        }
    }
}

/// A new `<p>` at `index` of `f`.
pub fn insert_para<F: XmlFragment>(txn: &mut TransactionMut, f: &F, index: u32, p: &Para) -> XmlElementRef {
    let el = f.insert(txn, index, XmlElementPrelim::empty("p"));
    for (k, v) in &p.props {
        el.insert_attribute(txn, format!("{PPR}{k}"), to_any(v));
    }
    let t = el.push_back(txn, XmlTextPrelim::new(""));
    insert_runs(txn, &t, 0, &p.runs, &BTreeMap::new());
    el
}

/// A new block at `index` of `f`.
pub fn insert_block<F: XmlFragment>(txn: &mut TransactionMut, f: &F, index: u32, b: &Block) {
    match b {
        Block::Para(p) => {
            insert_para(txn, f, index, p);
        }
        Block::Table(rows) => {
            let table = f.insert(txn, index, XmlElementPrelim::empty("table"));
            for row in rows {
                let tr = table.push_back(txn, XmlElementPrelim::empty("tr"));
                for cell in row {
                    let tc = tr.push_back(txn, XmlElementPrelim::empty("tc"));
                    for (i, b) in cell.iter().enumerate() {
                        insert_block(txn, &tc, i as u32, b);
                    }
                }
            }
        }
    }
}

/// Builds the document from a model, in the caller's transaction (origin `seed`).
pub fn seed(txn: &mut TransactionMut, model: &Model) {
    let meta = txn.get_or_insert_map("meta");
    meta.insert(txn, "app", APP);
    meta.insert(txn, "schema", SCHEMA);
    meta.insert(txn, "title", model.title.as_str());
    let body: XmlFragmentRef = txn.get_or_insert_xml_fragment("body");
    for (i, b) in model.body.iter().enumerate() {
        insert_block(txn, &body, i as u32, b);
    }
}

/// The `<p>` element at a block path of the body.
pub fn para_el<T: ReadTxn>(txn: &T, path: &[usize]) -> Option<XmlElementRef> {
    let body = txn.get_xml_fragment("body")?;
    let (first, rest) = path.split_first()?;
    let mut el = blocks_of(txn, &body).get(*first)?.clone();
    let mut rest = rest;
    while let [r, c, i, more @ ..] = rest {
        let tr = children_tagged(txn, &el, "tr").get(*r)?.clone();
        let tc = children_tagged(txn, &tr, "tc").get(*c)?.clone();
        el = blocks_of(txn, &tc).get(*i)?.clone();
        rest = more;
    }
    (rest.is_empty() && el.try_tag().map(|t| t.as_ref()) == Some("p")).then_some(el)
}

/// The paths of the body's paragraphs, in reading order (tables' cells row by row).
pub fn para_paths(model: &Model) -> Vec<Vec<usize>> {
    fn walk(blocks: &[Block], prefix: &[usize], out: &mut Vec<Vec<usize>>) {
        for (i, b) in blocks.iter().enumerate() {
            let mut here = prefix.to_vec();
            here.push(i);
            match b {
                Block::Para(_) => out.push(here),
                Block::Table(rows) => {
                    for (r, row) in rows.iter().enumerate() {
                        for (c, cell) in row.iter().enumerate() {
                            let mut p = here.clone();
                            p.extend([r, c]);
                            walk(cell, &p, out);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&model.body, &[], &mut out);
    out
}

/// Inserts `text` at byte `off` of the paragraph at `path` with exactly `attrs`.
pub fn insert_text(txn: &mut TransactionMut, path: &[usize], off: usize, text: &str, attrs: &BTreeMap<String, Json>) -> Result<(), String> {
    let model = read(txn)?;
    let mut m = model;
    let p = m.para_mut(path).ok_or("no such paragraph")?.clone();
    let el = para_el(txn, path).ok_or("no such paragraph")?;
    let t = text_of(txn, &el).ok_or("the paragraph has no text")?;
    let plain = p.plain_text();
    let before = if off > 0 {
        p.attrs_at(plain.get(..off).and_then(|s| s.char_indices().last()).map(|(i, _)| i).unwrap_or(0)).cloned().unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let at = y_index(&plain, off);
    insert_runs(txn, &t, at, &[Run { text: text.into(), attrs: attrs.clone(), embed: None }], &before);
    Ok(())
}

/// Deletes bytes `a..b` of the paragraph at `path`.
pub fn delete_text(txn: &mut TransactionMut, path: &[usize], a: usize, b: usize) -> Result<(), String> {
    let mut m = read(txn)?;
    let p = m.para_mut(path).ok_or("no such paragraph")?.clone();
    let el = para_el(txn, path).ok_or("no such paragraph")?;
    let t = text_of(txn, &el).ok_or("the paragraph has no text")?;
    let plain = p.plain_text();
    let (ya, yb) = (y_index(&plain, a), y_index(&plain, b));
    if yb > ya {
        t.remove_range(txn, ya, yb - ya);
    }
    Ok(())
}

/// Sets attributes on bytes `a..b` of the paragraph at `path` (`null` clears one).
pub fn format_text(txn: &mut TransactionMut, path: &[usize], a: usize, b: usize, attrs: &[(&str, Json)]) -> Result<(), String> {
    let mut m = read(txn)?;
    let p = m.para_mut(path).ok_or("no such paragraph")?.clone();
    let el = para_el(txn, path).ok_or("no such paragraph")?;
    let t = text_of(txn, &el).ok_or("the paragraph has no text")?;
    let plain = p.plain_text();
    let (ya, yb) = (y_index(&plain, a), y_index(&plain, b));
    if yb > ya {
        t.format(txn, ya, yb - ya, attrs.iter().map(|(k, v)| (Arc::from(*k), to_any(v))).collect());
    }
    Ok(())
}

/// Splits the paragraph at `path` at byte `off` (Enter): the text after it moves to a new
/// paragraph after it, with the same properties.
pub fn split_para(txn: &mut TransactionMut, path: &[usize], off: usize) -> Result<(), String> {
    let mut m = read(txn)?;
    let p = m.para_mut(path).ok_or("no such paragraph")?.clone();
    let el = para_el(txn, path).ok_or("no such paragraph")?;
    let t = text_of(txn, &el).ok_or("the paragraph has no text")?;
    let plain = p.plain_text();
    // The runs after `off`.
    let mut tail = Vec::new();
    let mut start = 0;
    for r in &p.runs {
        let end = start + r.text.len();
        if end > off {
            let cut = off.saturating_sub(start);
            let text = r.text.get(cut..).unwrap_or_default().to_string();
            tail.push(Run { text, attrs: r.attrs.clone(), embed: r.embed.clone() });
        }
        start = end;
    }
    let (ya, len) = (y_index(&plain, off), y_index(&plain, plain.len()));
    if len > ya {
        t.remove_range(txn, ya, len - ya);
    }
    let parent = el.parent().ok_or("the paragraph has no parent")?;
    let new = Para { props: p.props.clone(), runs: tail };
    let index = sibling_index(txn, &parent, &el).ok_or("the paragraph isn't in its parent")? + 1;
    match parent {
        XmlOut::Fragment(f) => {
            insert_para(txn, &f, index, &new);
        }
        XmlOut::Element(e) => {
            insert_para(txn, &e, index, &new);
        }
        XmlOut::Text(_) => return Err("a paragraph in text".into()),
    }
    Ok(())
}

/// The raw index of `el` among its parent's children.
fn sibling_index<T: ReadTxn>(txn: &T, parent: &XmlOut, el: &XmlElementRef) -> Option<u32> {
    let id = AsRef::<yrs::branch::Branch>::as_ref(el).id();
    let children: Vec<XmlOut> = match parent {
        XmlOut::Fragment(f) => f.children(txn).collect(),
        XmlOut::Element(e) => e.children(txn).collect(),
        XmlOut::Text(_) => return None,
    };
    children.iter().position(|c| matches!(c, XmlOut::Element(e) if AsRef::<yrs::branch::Branch>::as_ref(e).id() == id)).map(|i| i as u32)
}

/// Sets the title.
pub fn set_title(txn: &mut TransactionMut, title: &str) {
    let meta = txn.get_or_insert_map("meta");
    meta.insert(txn, "title", title);
}

/// Appends a block to the body.
pub fn push_block(txn: &mut TransactionMut, b: &Block) {
    let body = txn.get_or_insert_xml_fragment("body");
    let n = body.len(txn);
    insert_block(txn, &body, n, b);
}

/// The paragraph text's delta, for debugging.
pub fn delta_of<T: ReadTxn>(txn: &T, path: &[usize]) -> Vec<Delta> {
    let Some(el) = para_el(txn, path) else { return Vec::new() };
    let Some(t) = text_of(txn, &el) else { return Vec::new() };
    t.diff(txn, YChange::identity).into_iter().map(|d| Delta::Inserted(d.insert, d.attributes)).collect()
}

// ---------------------------------------------------------------------------------------------
// The reference app's commands

/// The selection: a paragraph (block path) and two byte offsets in its text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub path: Vec<usize>,
    pub anchor: usize,
    pub focus: usize,
}

impl Selection {
    fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }
}

/// WordCraft schema 1 for the reference app.
#[derive(Clone, Copy, Debug, Default)]
pub struct WordCraft;

/// The formatting typing at byte `off` of `p` takes, as WordCraft's: the character before's (at
/// the start, the first character's; in an empty paragraph, none).
pub fn typing_attrs(p: &Para, off: usize) -> BTreeMap<String, Json> {
    let plain = p.plain_text();
    let at = if off == 0 { Some(0) } else { plain.get(..off).and_then(|s| s.char_indices().last()).map(|(i, _)| i) };
    at.and_then(|i| p.attrs_at(i)).cloned().unwrap_or_default()
}

impl crate::schema::Schema for WordCraft {
    const APP: &'static str = APP;
    const SCHEMA: i64 = SCHEMA;
    type Model = Model;
    type View = Selection;

    fn blank() -> Model {
        Model::of("", vec![Para::default()])
    }
    fn seed(txn: &mut TransactionMut, model: &Model, _ids: &mut Ids) {
        seed(txn, model)
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

    /// `select.text {text, occurrence?}`: the `occurrence`-th (from 1) match in the body's
    /// paragraphs, in reading order. `caret.set {pos: {story: "body", path, off}}`: a caret at
    /// byte `off` of the paragraph at block path `path`.
    fn view_command<T: ReadTxn>(txn: &T, view: &mut Selection, command: &str, params: &Json) -> Option<Result<(), String>> {
        if command == "caret.set" {
            return Some((|| {
                let pos = params.get("pos").ok_or("caret.set needs `pos`")?;
                let path: Vec<usize> = pos
                    .get("path")
                    .and_then(Json::as_array)
                    .ok_or("`pos` needs `path`")?
                    .iter()
                    .map(|i| i.as_u64().map(|i| i as usize).ok_or("`path` holds indexes"))
                    .collect::<Result<_, _>>()?;
                let off = pos.get("off").and_then(Json::as_u64).ok_or("`pos` needs `off`")? as usize;
                let mut m = read(txn)?;
                let len = m.para_mut(&path).ok_or("no such paragraph")?.plain_text().len();
                let off = off.min(len);
                *view = Selection { path, anchor: off, focus: off };
                Ok(())
            })());
        }
        if command != "select.text" {
            return None;
        }
        Some((|| {
            let needle = params.get("text").and_then(Json::as_str).filter(|t| !t.is_empty()).ok_or("select.text needs `text`")?;
            let n = params.get("occurrence").and_then(Json::as_u64).unwrap_or(1).max(1) as usize;
            let mut m = read(txn)?;
            let mut count = 0;
            for path in para_paths(&m.clone()) {
                let text = m.para_mut(&path).map(|p| p.plain_text()).unwrap_or_default();
                for (i, _) in text.match_indices(needle) {
                    count += 1;
                    if count == n {
                        *view = Selection { path, anchor: i, focus: i + needle.len() };
                        return Ok(());
                    }
                }
            }
            Err(format!("`{needle}` not found"))
        })())
    }

    /// `text.insert {text}` (replaces the selection), `text.newParagraph` (Enter),
    /// `format.bold {value}` and `format.italic {value}` (also their complex-script twins, as
    /// WordCraft's buttons do).
    fn edit(t: &mut TransactionMut, view: &mut Selection, command: &str, params: &Json, _ids: &mut Ids) -> Result<(), String> {
        if view.path.is_empty() {
            view.path = vec![0];
        }
        let (a, b) = view.range();
        let path = view.path.clone();
        let mut m = read(t)?;
        let p = m.para_mut(&path).ok_or("the selection's paragraph is gone")?.clone();
        match command {
            "text.insert" => {
                let text = params.get("text").and_then(Json::as_str).ok_or("text.insert needs `text`")?;
                let attrs = typing_attrs(&p, a);
                delete_text(t, &path, a, b)?;
                insert_text(t, &path, a, text, &attrs)?;
                *view = Selection { path, anchor: a + text.len(), focus: a + text.len() };
            }
            "text.newParagraph" => {
                delete_text(t, &path, a, b)?;
                split_para(t, &path, a)?;
                let mut next = path.clone();
                if let Some(last) = next.last_mut() {
                    *last += 1;
                }
                *view = Selection { path: next, anchor: 0, focus: 0 };
            }
            "format.bold" | "format.italic" => {
                let (k, cs) = if command == "format.bold" { ("bold", "boldCs") } else { ("italic", "italicCs") };
                let on = params.get("value").and_then(Json::as_bool).unwrap_or(true);
                format_text(t, &path, a, b, &[(k, json!(on)), (cs, json!(on))])?;
            }
            other => return Err(format!("the reference app can't run {other}")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::updates::decoder::Decode;
    use yrs::{Doc, StateVector, Transact, Update};

    fn minutes() -> Model {
        Model {
            title: "Minutes".into(),
            body: vec![
                Block::Para(Para::text("Minutes").styled("Heading1")),
                Block::Para(Para::of(vec![Run::with("Present: ", &[("bold", json!(true))]), Run::plain("Ada, Bo")])),
                Block::Table(vec![vec![vec![Block::Para(Para::text("Item"))], vec![Block::Para(Para::text("Owner"))]]]),
                Block::Para(Para::default()),
            ],
        }
    }

    #[test]
    fn seeds_and_reads() {
        let doc = Doc::with_client_id(1);
        seed(&mut doc.transact_mut(), &minutes());
        let m = read(&doc.transact()).unwrap();
        assert_eq!(m, minutes());
        assert_eq!(Model::from_json(&m.to_json()).unwrap(), m);
        assert_eq!(para_paths(&m), vec![vec![0], vec![1], vec![2, 0, 0, 0], vec![2, 0, 1, 0], vec![3]]);
    }

    #[test]
    fn edits_keep_formatting_exact() {
        let doc = Doc::with_client_id(1);
        seed(&mut doc.transact_mut(), &minutes());
        {
            let mut t = doc.transact_mut();
            // Typing after the bold run, without bold.
            insert_text(&mut t, &[1], 9, "all: ", &BTreeMap::new()).unwrap();
            format_text(&mut t, &[0], 0, 3, &[("italic", json!(true))]).unwrap();
            split_para(&mut t, &[1], 9).unwrap();
            insert_text(&mut t, &[3, 0, 1, 0], 5, "s", &BTreeMap::new()).unwrap();
        }
        let m = read(&doc.transact()).unwrap();
        let text: Vec<Json> = m.body.iter().map(block_json).collect();
        assert_eq!(text[0]["runs"], json!([{"text": "Min", "italic": true}, {"text": "utes"}]));
        assert_eq!(text[1]["runs"], json!([{"text": "Present: ", "bold": true}]));
        assert_eq!(text[2]["text"], json!("all: Ada, Bo"));
        assert_eq!(text[3]["table"][0][1][0]["text"], json!("Owners"));
    }

    #[test]
    fn concurrent_typing_merges() {
        let a = Doc::with_client_id(1);
        seed(&mut a.transact_mut(), &Model::of("", vec![Para::text("Hello world")]));
        let b = Doc::with_client_id(2);
        let full = a.transact().encode_state_as_update_v1(&StateVector::default());
        b.transact_mut().apply_update(Update::decode_v1(&full).unwrap()).unwrap();
        let ua = {
            let mut t = a.transact_mut();
            insert_text(&mut t, &[0], 5, ",", &BTreeMap::new()).unwrap();
            t.encode_update_v1()
        };
        let ub = {
            let mut t = b.transact_mut();
            insert_text(&mut t, &[0], 11, "!", &BTreeMap::new()).unwrap();
            format_text(&mut t, &[0], 0, 5, &[("bold", json!(true))]).unwrap();
            t.encode_update_v1()
        };
        a.transact_mut().apply_update(Update::decode_v1(&ub).unwrap()).unwrap();
        b.transact_mut().apply_update(Update::decode_v1(&ua).unwrap()).unwrap();
        let (ma, mb) = (read(&a.transact()).unwrap(), read(&b.transact()).unwrap());
        assert_eq!(ma, mb);
        // Whether the comma typed at the bold range's end is bold depends on the order Yjs gives
        // the concurrent insert and format boundary; both peers agree.
        assert_eq!(block_json(&ma.body[0])["text"], json!("Hello, world!"));
        assert_eq!(block_json(&ma.body[0])["runs"][0]["bold"], json!(true));
    }

    #[test]
    fn numbers_and_values_round_trip() {
        for v in [json!(1), json!(1.5), json!("x"), json!([1, {"a": null}]), json!({"b": true, "c": 12})] {
            assert_eq!(to_json(&to_any(&v)), v);
        }
        assert_eq!(norm(&json!(12.0)), json!(12));
        assert_eq!(y_index("a\u{FFFC}é", 6), 4);
    }
}
