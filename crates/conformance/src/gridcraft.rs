//! GridCraft schema 1 (`docs/schemas/gridcraft-1.md`): the reference reader and writer of the
//! shared document, limited to what the transcripts need (sheets, rows, columns, cell inputs).
//!
//! Roots: `meta` (map `{app: "gridcraft", schema: 1, title}`), `sheets` (array of sheet ids),
//! `sheet:<id>` (map: `name`, `rows` and `cols` arrays of ids, `cells` map keyed
//! `<rowId>:<colId>` whose values are maps `{input, style?}`).

use std::collections::{BTreeMap, HashSet};

use serde_json::{Value as Json, json};
use yrs::types::ToJson;
use yrs::{Any, Array, ArrayPrelim, ArrayRef, Map, MapPrelim, MapRef, Out, ReadTxn, TransactionMut, WriteTxn};

pub const APP: &str = "gridcraft";
pub const SCHEMA: i64 = 1;

/// What a peer sees: the title, and each sheet's name and cell inputs by A1 address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub title: String,
    pub sheets: Vec<SheetModel>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SheetModel {
    pub name: String,
    /// `A1` → input as typed.
    pub cells: BTreeMap<String, String>,
}

impl Model {
    /// `{"title", "sheets": [{"name", "cells": {"A1": "input"}}]}`.
    pub fn to_json(&self) -> Json {
        json!({
            "title": self.title,
            "sheets": self.sheets.iter().map(|s| json!({"name": s.name, "cells": s.cells})).collect::<Vec<_>>(),
        })
    }

    pub fn from_json(j: &Json) -> Result<Model, String> {
        let title = j.get("title").and_then(Json::as_str).unwrap_or("").to_string();
        let mut sheets = Vec::new();
        for s in j.get("sheets").and_then(Json::as_array).ok_or("a model needs `sheets`")? {
            let name = s.get("name").and_then(Json::as_str).ok_or("a sheet needs `name`")?.to_string();
            let mut cells = BTreeMap::new();
            if let Some(c) = s.get("cells").and_then(Json::as_object) {
                for (k, v) in c {
                    cells.insert(k.clone(), v.as_str().ok_or("cell inputs are strings")?.to_string());
                }
            }
            sheets.push(SheetModel { name, cells });
        }
        Ok(Model { title, sheets })
    }

    /// A one-sheet model from `[("A1", "input")]`.
    pub fn sheet(title: &str, name: &str, cells: &[(&str, &str)]) -> Model {
        Model {
            title: title.into(),
            sheets: vec![SheetModel { name: name.into(), cells: cells.iter().map(|(a, i)| (a.to_string(), i.to_string())).collect() }],
        }
    }
}

/// Ids of sheets and of inserted rows and columns: 8 characters of `0-9a-z`, from a seeded
/// generator (deterministic transcripts) or the system's randomness.
#[derive(Clone, Debug)]
pub struct Ids(u64);

impl Ids {
    pub fn seeded(seed: u64) -> Ids {
        Ids(seed)
    }

    pub fn random() -> Ids {
        Ids(craft_collab_kit::ids::random_u64())
    }

    /// The next 64 random bits (splitmix64).
    pub fn bits(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn next_id(&mut self) -> String {
        const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let mut x = self.bits();
        (0..8)
            .map(|_| {
                let d = DIGITS.get((x % 36) as usize).copied().unwrap_or(b'0');
                x /= 36;
                char::from(d)
            })
            .collect()
    }
}

/// Column letters: 0 → `A`, 25 → `Z`, 26 → `AA`.
pub fn col_letters(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

/// `B12` → (row 11, column 1), zero-based.
pub fn parse_a1(a1: &str) -> Option<(usize, usize)> {
    let a = a1.trim().trim_start_matches('$').to_ascii_uppercase();
    let split = a.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = a.split_at(split);
    let letters = letters.trim_end_matches('$');
    if letters.is_empty() || letters.len() > 3 || !letters.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let mut col: usize = 0;
    for b in letters.bytes() {
        col = col * 26 + usize::from(b - b'A' + 1);
    }
    let row: usize = digits.parse().ok().filter(|r| *r >= 1 && *r <= 1_048_576)?;
    (col <= 16_384).then_some((row - 1, col - 1))
}

/// `"3:5"` (rows) or `"B:C"` (columns) → (first, count), zero-based.
pub fn parse_span(span: &str, rows: bool) -> Option<(usize, usize)> {
    let (a, b) = span.split_once(':').unwrap_or((span, span));
    let one = |s: &str| -> Option<usize> {
        if rows { s.trim().parse::<usize>().ok().filter(|r| *r >= 1).map(|r| r - 1) } else { parse_a1(&format!("{}1", s.trim())).map(|(_, c)| c) }
    };
    let (x, y) = (one(a)?, one(b)?);
    Some((x.min(y), x.max(y) - x.min(y) + 1))
}

fn string(o: Option<Out>) -> Option<String> {
    match o? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// The ids in an array, first occurrence kept, non-strings skipped.
pub fn ids_of<T: ReadTxn>(txn: &T, arr: &ArrayRef) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for i in 0..arr.len(txn) {
        if let Some(id) = string(arr.get(txn, i))
            && seen.insert(id.clone())
        {
            out.push(id);
        }
    }
    out
}

fn sub_array<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<ArrayRef> {
    match map.get(txn, key)? {
        Out::YArray(a) => Some(a),
        _ => None,
    }
}

fn sub_map<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<MapRef> {
    match map.get(txn, key)? {
        Out::YMap(m) => Some(m),
        _ => None,
    }
}

/// `Err("This room is for <app> <schema>")` when `meta` names another app or schema; `Ok` when
/// it's GridCraft 1 or there's no `meta` yet.
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

/// The sheet ids in tab order (those with a `sheet:<id>` map).
pub fn sheet_ids<T: ReadTxn>(txn: &T) -> Vec<String> {
    let Some(sheets) = txn.get_array("sheets") else { return Vec::new() };
    ids_of(txn, &sheets).into_iter().filter(|id| txn.get_map(format!("sheet:{id}")).is_some()).collect()
}

/// Reads the model.
pub fn read<T: ReadTxn>(txn: &T) -> Result<Model, String> {
    check_meta(txn)?;
    let title = txn.get_map("meta").and_then(|m| string(m.get(txn, "title"))).unwrap_or_default();
    let mut sheets = Vec::new();
    for id in sheet_ids(txn) {
        let Some(sheet) = txn.get_map(format!("sheet:{id}")) else { continue };
        let name = string(sheet.get(txn, "name")).unwrap_or_default();
        let rows = sub_array(txn, &sheet, "rows").map(|a| ids_of(txn, &a)).unwrap_or_default();
        let cols = sub_array(txn, &sheet, "cols").map(|a| ids_of(txn, &a)).unwrap_or_default();
        let row_at: BTreeMap<&str, usize> = rows.iter().enumerate().map(|(i, r)| (r.as_str(), i)).collect();
        let col_at: BTreeMap<&str, usize> = cols.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
        let mut cells = BTreeMap::new();
        if let Some(map) = sub_map(txn, &sheet, "cells") {
            for (key, value) in map.iter(txn) {
                let Some((r, c)) = key.split_once(':') else { continue };
                let (Some(r), Some(c)) = (row_at.get(r), col_at.get(c)) else { continue };
                let Out::YMap(cell) = value else { continue };
                let input = string(cell.get(txn, "input")).unwrap_or_default();
                if !input.is_empty() {
                    cells.insert(format!("{}{}", col_letters(*c), r + 1), input);
                }
            }
        }
        sheets.push(SheetModel { name, cells });
    }
    Ok(Model { title, sheets })
}

/// The whole document as JSON (for debugging).
pub fn dump<T: ReadTxn>(txn: &T) -> Json {
    let mut out = serde_json::Map::new();
    for (name, value) in txn.root_refs() {
        let any = value.to_json(txn);
        out.insert(name.to_string(), serde_json::to_value(&any).unwrap_or(Json::Null));
    }
    Json::Object(out)
}

fn sheet_map(txn: &mut TransactionMut, id: &str) -> MapRef {
    txn.get_or_insert_map(format!("sheet:{id}"))
}

/// Rows and columns are added at the end in blocks of this many.
pub const BLOCK: usize = 64;

/// The block number of a block id (`~12.5` → 12).
pub fn block_of(id: &str) -> Option<u64> {
    let (b, i) = id.strip_prefix('~')?.split_once('.')?;
    i.parse::<u64>().ok()?;
    b.parse().ok()
}

/// The raw array index of the `n`-th id once duplicates are skipped (the array's length when
/// `n` is past the end).
fn raw_index<T: ReadTxn>(txn: &T, arr: &ArrayRef, n: usize) -> u32 {
    let mut seen = HashSet::new();
    let mut unique = 0;
    for i in 0..arr.len(txn) {
        if let Some(id) = string(arr.get(txn, i))
            && seen.insert(id)
        {
            if unique == n {
                return i;
            }
            unique += 1;
        }
    }
    arr.len(txn)
}

/// Grows `rows` (or `cols`) to at least `len` ids with whole blocks of deterministic ids
/// (`~<block>.<i>`), numbered from `max(<rowBlocks|colBlocks>, highest block present + 1)`. Two
/// people growing a sheet at once append the same ids, which collapse into one block when read.
fn grow(txn: &mut TransactionMut, sheet: &MapRef, rows: bool, len: usize) {
    let arr = child_array(txn, sheet, if rows { "rows" } else { "cols" });
    let have = ids_of(txn, &arr);
    if have.len() >= len {
        return;
    }
    let counter_key = if rows { "rowBlocks" } else { "colBlocks" };
    let counter = match sheet.get(txn, counter_key) {
        Some(Out::Any(Any::Number(n))) => n.as_f64().filter(|x| *x >= 0.0 && *x < 1e9).map(|x| x as u64).unwrap_or(0),
        _ => 0,
    };
    let highest = have.iter().filter_map(|id| block_of(id)).max().map(|b| b + 1).unwrap_or(0);
    let mut next = counter.max(highest);
    let mut new = Vec::new();
    while have.len() + new.len() < len {
        new.extend((0..BLOCK).map(|i| format!("~{next}.{i}")));
        next += 1;
    }
    let end = arr.len(txn);
    arr.insert_range(txn, end, new);
    sheet.insert(txn, counter_key, next as i64);
}

fn child_array(txn: &mut TransactionMut, map: &MapRef, key: &str) -> ArrayRef {
    match sub_array(txn, map, key) {
        Some(a) => a,
        None => map.insert(txn, key, ArrayPrelim::default()),
    }
}

fn child_map(txn: &mut TransactionMut, map: &MapRef, key: &str) -> MapRef {
    match sub_map(txn, map, key) {
        Some(m) => m,
        None => map.insert(txn, key, MapPrelim::default()),
    }
}

/// Builds the document from a model, in the caller's transaction (origin `seed`).
pub fn seed(txn: &mut TransactionMut, model: &Model, ids: &mut Ids) {
    let meta = txn.get_or_insert_map("meta");
    meta.insert(txn, "app", APP);
    meta.insert(txn, "schema", SCHEMA);
    meta.insert(txn, "title", model.title.as_str());
    let _ = txn.get_or_insert_array("sheets");
    for s in &model.sheets {
        let id = add_sheet(txn, &s.name, None, ids);
        for (a1, input) in &s.cells {
            if let Some((r, c)) = parse_a1(a1) {
                set_cell(txn, &id, r, c, input);
            }
        }
    }
}

/// Adds a sheet (at `before`, else at the end); its id.
pub fn add_sheet(txn: &mut TransactionMut, name: &str, before: Option<usize>, ids: &mut Ids) -> String {
    let id = ids.next_id();
    let sheet = sheet_map(txn, &id);
    sheet.insert(txn, "name", name);
    sheet.insert(txn, "rows", ArrayPrelim::default());
    sheet.insert(txn, "cols", ArrayPrelim::default());
    sheet.insert(txn, "cells", MapPrelim::default());
    let sheets = txn.get_or_insert_array("sheets");
    let n = sheets.len(txn);
    let at = before.map(|b| (b as u32).min(n)).unwrap_or(n);
    sheets.insert(txn, at, id.as_str());
    id
}

pub fn rename_sheet(txn: &mut TransactionMut, id: &str, name: &str) {
    sheet_map(txn, id).insert(txn, "name", name);
}

/// Sets (or, with an empty input, clears) a cell, growing the sheet's rows and columns to it.
pub fn set_cell(txn: &mut TransactionMut, sheet_id: &str, row: usize, col: usize, input: &str) {
    let sheet = sheet_map(txn, sheet_id);
    let rows = child_array(txn, &sheet, "rows");
    let cols = child_array(txn, &sheet, "cols");
    let cells = child_map(txn, &sheet, "cells");
    if input.is_empty() {
        let (r, c) = (ids_of(txn, &rows), ids_of(txn, &cols));
        if let (Some(r), Some(c)) = (r.get(row), c.get(col)) {
            cells.remove(txn, &format!("{r}:{c}"));
        }
        return;
    }
    grow(txn, &sheet, true, row + 1);
    grow(txn, &sheet, false, col + 1);
    let (r, c) = (ids_of(txn, &rows), ids_of(txn, &cols));
    let (Some(r), Some(c)) = (r.get(row), c.get(col)) else { return };
    let key = format!("{r}:{c}");
    match sub_map(txn, &cells, &key) {
        // Change the input of the existing cell, so a concurrent style change survives.
        Some(cell) => {
            cell.insert(txn, "input", input);
        }
        None => {
            let cell = cells.insert(txn, key, MapPrelim::default());
            cell.insert(txn, "input", input);
        }
    }
}

/// Inserts `count` rows (or columns) before `at`; past the end of the array nothing needs to
/// change (those rows exist implicitly).
pub fn insert_lines(txn: &mut TransactionMut, sheet_id: &str, rows: bool, at: usize, count: usize, ids: &mut Ids) {
    let sheet = sheet_map(txn, sheet_id);
    let arr = child_array(txn, &sheet, if rows { "rows" } else { "cols" });
    if at < ids_of(txn, &arr).len() {
        let new: Vec<String> = (0..count).map(|_| ids.next_id()).collect();
        let raw = raw_index(txn, &arr, at);
        arr.insert_range(txn, raw, new);
    }
}

/// Deletes `count` rows (or columns) from `at`, with their cells.
pub fn delete_lines(txn: &mut TransactionMut, sheet_id: &str, rows: bool, at: usize, count: usize) {
    let sheet = sheet_map(txn, sheet_id);
    let arr = child_array(txn, &sheet, if rows { "rows" } else { "cols" });
    let cells = child_map(txn, &sheet, "cells");
    let all = ids_of(txn, &arr);
    let gone: HashSet<String> = all.iter().skip(at).take(count).cloned().collect();
    if gone.is_empty() {
        return;
    }
    let doomed: Vec<String> =
        cells.keys(txn).filter(|k| k.split_once(':').is_some_and(|(r, c)| gone.contains(if rows { r } else { c }))).map(str::to_string).collect();
    for k in doomed {
        cells.remove(txn, &k);
    }
    // Remove by value, from the end, so duplicates and concurrent inserts don't shift the work.
    let mut i = arr.len(txn);
    while i > 0 {
        i -= 1;
        if string(arr.get(txn, i)).is_some_and(|id| gone.contains(&id)) {
            arr.remove(txn, i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::{Doc, Transact};

    #[test]
    fn addresses() {
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(25), "Z");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_letters(701), "ZZ");
        assert_eq!(parse_a1("B12"), Some((11, 1)));
        assert_eq!(parse_a1("$AA$1"), Some((0, 26)));
        assert_eq!(parse_a1("1A"), None);
        assert_eq!(parse_span("2:3", true), Some((1, 2)));
        assert_eq!(parse_span("C:B", false), Some((1, 2)));
    }

    #[test]
    fn concurrent_growth_lands_in_the_same_rows() {
        use yrs::updates::decoder::Decode;
        use yrs::{StateVector, Update};
        let a = Doc::with_client_id(1);
        {
            let mut t = a.transact_mut();
            seed(&mut t, &Model::sheet("", "S", &[("A1", "x")]), &mut Ids::seeded(1));
        }
        let b = Doc::with_client_id(2);
        let full = a.transact().encode_state_as_update_v1(&StateVector::default());
        b.transact_mut().apply_update(Update::decode_v1(&full).unwrap()).unwrap();
        let sheet = sheet_ids(&a.transact()).remove(0);
        let ua = {
            let mut t = a.transact_mut();
            set_cell(&mut t, &sheet, 69, 0, "a");
            t.encode_update_v1()
        };
        let ub = {
            let mut t = b.transact_mut();
            set_cell(&mut t, &sheet, 199, 1, "b");
            insert_lines(&mut t, &sheet, true, 100, 1, &mut Ids::seeded(9));
            t.encode_update_v1()
        };
        a.transact_mut().apply_update(Update::decode_v1(&ub).unwrap()).unwrap();
        b.transact_mut().apply_update(Update::decode_v1(&ua).unwrap()).unwrap();
        let (ma, mb) = (read(&a.transact()).unwrap(), read(&b.transact()).unwrap());
        assert_eq!(ma, mb);
        assert_eq!(ma.sheets[0].cells.get("A70").map(String::as_str), Some("a"));
        assert_eq!(ma.sheets[0].cells.get("B201").map(String::as_str), Some("b"));
    }

    #[test]
    fn seeds_reads_and_edits() {
        let doc = Doc::with_client_id(1);
        let mut ids = Ids::seeded(7);
        let model = Model::sheet("Budget", "Sheet1", &[("A1", "Item"), ("B1", "Cost"), ("A2", "Coffee"), ("B2", "3.5")]);
        {
            let mut t = doc.transact_mut();
            seed(&mut t, &model, &mut ids);
        }
        assert_eq!(read(&doc.transact()).unwrap(), model);
        let sheet = sheet_ids(&doc.transact()).remove(0);
        {
            let mut t = doc.transact_mut();
            insert_lines(&mut t, &sheet, true, 1, 1, &mut ids);
            delete_lines(&mut t, &sheet, false, 0, 1);
            set_cell(&mut t, &sheet, 1, 0, "Tea");
        }
        let m = read(&doc.transact()).unwrap();
        assert_eq!(m.sheets[0].cells, [("A1", "Cost"), ("A2", "Tea"), ("A3", "3.5")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect());
    }
}
