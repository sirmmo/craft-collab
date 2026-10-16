//! Document chat (`docs/chat.md`, protocol 2.1): the root array `chat` of message maps and the
//! root map `chatMeta`, the same in every app's schema. Reading applies the spec's validation and
//! limits; writing is what `chat.send`, `chat.edit`, `chat.delete` and `chat.clear` do. The same
//! JSON form serves the transcripts' `chat` lines and the file part (`/craft/chat.json`).

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Map as JsonMap, Value as Json, json};
use yrs::any::Number;
use yrs::updates::decoder::Decode;
use yrs::{Any, Array, Doc, Map, MapPrelim, MapRef, Out, ReadTxn, Transact, TransactionMut, Update, WriteTxn};

/// The root array of messages.
pub const ROOT: &str = "chat";
/// The root map with `clearedAt`.
pub const META: &str = "chatMeta";
/// At most this many messages; readers ignore the rest, senders refuse more.
pub const MAX_MESSAGES: usize = 10_000;
/// A message's text: 1 to this many UTF-16 units.
pub const MAX_TEXT: usize = 4_000;
/// What the app says when the chat is full.
pub const FULL: &str = "The chat is full: clear it to go on";
/// The file part's `version`.
pub const PART_VERSION: i64 = 1;

/// Who wrote a message: the `user` of the room's join, or the app's own user.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Author {
    pub id: String,
    pub name: String,
    pub color: String,
}

impl Author {
    pub fn to_json(&self) -> Json {
        json!({"id": self.id, "name": self.name, "color": self.color})
    }

    /// `{id, name, color}`: `id` and `name` non-empty strings, `color` a string.
    pub fn from_json(j: &Json) -> Option<Author> {
        let s = |k: &str| j.get(k).and_then(Json::as_str).map(str::to_string);
        let (id, name, color) = (s("id")?, s("name")?, s("color")?);
        (!id.is_empty() && !name.is_empty()).then_some(Author { id, name, color })
    }
}

/// One message.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Message {
    pub id: String,
    pub author: Author,
    /// Milliseconds since 1970 UTC, the sender's clock.
    pub at: i64,
    /// `None` once deleted.
    pub text: Option<String>,
    pub reply_to: Option<String>,
    pub edited: Option<i64>,
    pub deleted: bool,
}

impl Message {
    pub fn to_json(&self) -> Json {
        let mut o = JsonMap::new();
        o.insert("id".into(), json!(self.id));
        o.insert("author".into(), self.author.to_json());
        o.insert("at".into(), json!(self.at));
        if let Some(t) = &self.text {
            o.insert("text".into(), json!(t));
        }
        if let Some(r) = &self.reply_to {
            o.insert("replyTo".into(), json!(r));
        }
        if let Some(e) = self.edited {
            o.insert("edited".into(), json!(e));
        }
        if self.deleted {
            o.insert("deleted".into(), json!(true));
        }
        Json::Object(o)
    }

    /// A message in the JSON form (`author` an object); `None` when malformed.
    pub fn from_json(j: &Json) -> Option<Message> {
        let id = j.get("id").and_then(Json::as_str).filter(|s| valid_id(s))?.to_string();
        let author = Author::from_json(j.get("author")?)?;
        let at = j.get("at").and_then(json_ms)?;
        let deleted = j.get("deleted").and_then(Json::as_bool) == Some(true);
        let text = if deleted { None } else { Some(j.get("text").and_then(Json::as_str).filter(|t| valid_text(t).is_ok())?.to_string()) };
        let reply_to = j.get("replyTo").and_then(Json::as_str).filter(|s| valid_id(s)).map(str::to_string);
        let edited = j.get("edited").and_then(json_ms);
        Some(Message { id, author, at, text, reply_to, edited, deleted })
    }
}

/// The chat as readers see it: valid messages in order, at most [`MAX_MESSAGES`].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Chat {
    pub cleared_at: Option<i64>,
    pub messages: Vec<Message>,
}

impl Chat {
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.cleared_at.is_none()
    }

    /// `{clearedAt?, messages}`: the transcripts' `chat` lines.
    pub fn to_json(&self) -> Json {
        let mut o = JsonMap::new();
        if let Some(c) = self.cleared_at {
            o.insert("clearedAt".into(), json!(c));
        }
        o.insert("messages".into(), Json::Array(self.messages.iter().map(Message::to_json).collect()));
        Json::Object(o)
    }

    /// The file part's content: `{version: 1, clearedAt?, messages}`.
    pub fn to_part(&self) -> Json {
        let mut j = self.to_json();
        if let Some(o) = j.as_object_mut() {
            o.insert("version".into(), json!(PART_VERSION));
        }
        j
    }

    /// The JSON form (a `chat` line, or a file part with `version` 1). Malformed messages are
    /// skipped, duplicates after the first and messages beyond [`MAX_MESSAGES`] too; `Err` when
    /// the whole thing isn't a chat (the app warns and opens the file without it).
    pub fn from_json(j: &Json) -> Result<Chat, String> {
        let o = j.as_object().ok_or("the chat isn't a JSON object")?;
        if let Some(v) = o.get("version")
            && v.as_i64() != Some(PART_VERSION)
        {
            return Err(format!("unknown chat version {v}"));
        }
        let list = o.get("messages").and_then(Json::as_array).ok_or("the chat has no `messages` list")?;
        let mut seen = HashSet::new();
        let messages = list.iter().filter_map(Message::from_json).filter(|m| seen.insert(m.id.clone())).take(MAX_MESSAGES).collect();
        Ok(Chat { cleared_at: o.get("clearedAt").and_then(json_ms), messages })
    }
}

/// 8 characters of `0-9a-z`.
pub fn valid_id(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

/// 1 to [`MAX_TEXT`] UTF-16 units.
pub fn valid_text(text: &str) -> Result<(), String> {
    match text.encode_utf16().count() {
        0 => Err("A message can't be empty".into()),
        n if n > MAX_TEXT => Err(format!("A message holds at most {MAX_TEXT} characters")),
        _ => Ok(()),
    }
}

/// A time in milliseconds: a non-negative integer (at most 2^53).
fn json_ms(v: &Json) -> Option<i64> {
    let n = v.as_i64().or_else(|| v.as_f64().filter(|f| f.fract() == 0.0 && f.is_finite()).map(|f| f as i64))?;
    (0..=(1i64 << 53)).contains(&n).then_some(n)
}

fn any_ms(v: Option<Out>) -> Option<i64> {
    let n = match v? {
        Out::Any(Any::Number(Number::Int(i))) => i,
        Out::Any(Any::Number(Number::Float(f))) if f.fract() == 0.0 && f.is_finite() && f.abs() <= 9.0e15 => f as i64,
        _ => return None,
    };
    (0..=(1i64 << 53)).contains(&n).then_some(n)
}

fn any_string(v: Option<Out>) -> Option<String> {
    match v? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn ms(n: i64) -> Any {
    Any::Number(Number::Int(n))
}

/// One raw entry → a message, `None` when malformed.
fn entry<T: ReadTxn>(txn: &T, m: &MapRef) -> Option<Message> {
    let id = any_string(m.get(txn, "id")).filter(|s| valid_id(s))?;
    let author = any_string(m.get(txn, "author")).and_then(|s| serde_json::from_str::<Json>(&s).ok()).and_then(|j| Author::from_json(&j))?;
    let at = any_ms(m.get(txn, "at"))?;
    let deleted = matches!(m.get(txn, "deleted"), Some(Out::Any(Any::Bool(true))));
    let text = if deleted { None } else { Some(any_string(m.get(txn, "text")).filter(|t| valid_text(t).is_ok())?) };
    let reply_to = any_string(m.get(txn, "replyTo")).filter(|s| valid_id(s));
    let edited = any_ms(m.get(txn, "edited"));
    Some(Message { id, author, at, text, reply_to, edited, deleted })
}

/// The valid messages with their positions in the array (first of a duplicated id, at most
/// [`MAX_MESSAGES`]).
fn valid<T: ReadTxn>(txn: &T) -> Vec<(u32, MapRef, Message)> {
    let Some(arr) = txn.get_array(ROOT) else { return Vec::new() };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, v) in arr.iter(txn).enumerate() {
        if out.len() >= MAX_MESSAGES {
            break;
        }
        let Out::YMap(m) = v else { continue };
        let Some(msg) = entry(txn, &m) else { continue };
        if seen.insert(msg.id.clone()) {
            out.push((i as u32, m, msg));
        }
    }
    out
}

/// Reads the chat.
pub fn read<T: ReadTxn>(txn: &T) -> Chat {
    let cleared_at = txn.get_map(META).and_then(|m| any_ms(m.get(txn, "clearedAt")));
    Chat { cleared_at, messages: valid(txn).into_iter().map(|(_, _, m)| m).collect() }
}

/// The chat a peer with every update in `updates` (plaintext) sees.
pub fn read_updates(updates: &[Vec<u8>]) -> Result<Chat, String> {
    std::panic::catch_unwind(|| {
        let doc = Doc::with_client_id(1);
        let mut t = doc.transact_mut();
        for u in updates {
            let u = Update::decode_v1(u).map_err(|e| format!("not a Yjs update: {e}"))?;
            t.apply_update(u).map_err(|e| e.to_string())?;
        }
        Ok(read(&t))
    })
    .unwrap_or_else(|_| Err("not a Yjs update (yrs panicked)".into()))
}

/// The number of valid messages (what the limit counts).
pub fn count<T: ReadTxn>(txn: &T) -> usize {
    valid(txn).len()
}

fn fields(m: &Message) -> Vec<(&'static str, Any)> {
    let mut p: Vec<(&'static str, Any)> =
        vec![("id", Any::String(Arc::from(m.id.as_str()))), ("author", Any::String(Arc::from(m.author.to_json().to_string().as_str()))), ("at", ms(m.at))];
    if let Some(t) = &m.text {
        p.push(("text", Any::String(Arc::from(t.as_str()))));
    }
    if let Some(r) = &m.reply_to {
        p.push(("replyTo", Any::String(Arc::from(r.as_str()))));
    }
    if let Some(e) = m.edited {
        p.push(("edited", ms(e)));
    }
    if m.deleted {
        p.push(("deleted", Any::Bool(true)));
    }
    p
}

/// Appends a map with these keys, set one by one in this order (a `MapPrelim` would set them
/// in hash order, and the transcripts' bytes would change from run to run).
fn append(txn: &mut TransactionMut, entry: Vec<(&str, Any)>) {
    let arr = txn.get_or_insert_array(ROOT);
    let map = arr.push_back(txn, MapPrelim::default());
    for (k, v) in entry {
        map.insert(txn, k, v);
    }
}

/// Appends a message as is (no checks: the transcripts' peers use it to write odd entries).
pub fn push(txn: &mut TransactionMut, m: &Message) {
    append(txn, fields(m));
}

/// Appends a raw value (a malformed entry, for the transcripts).
pub fn push_raw(txn: &mut TransactionMut, entry: Vec<(&str, Any)>) {
    append(txn, entry);
}

/// Appends something that isn't a map (for the transcripts).
pub fn push_not_a_map(txn: &mut TransactionMut, text: &str) {
    let arr = txn.get_or_insert_array(ROOT);
    arr.push_back(txn, Any::String(Arc::from(text)));
}

/// Writes a chat into an empty document (sharing seeds the room from the file's chat). Writes
/// nothing for an empty chat.
pub fn seed(txn: &mut TransactionMut, chat: &Chat) {
    if let Some(c) = chat.cleared_at {
        txn.get_or_insert_map(META).insert(txn, "clearedAt", ms(c));
    }
    if chat.messages.is_empty() {
        return;
    }
    for m in &chat.messages {
        push(txn, m);
    }
}

/// `chat.send`: appends `m` after checking its text, its reply and the limit.
pub fn send(txn: &mut TransactionMut, m: &Message) -> Result<(), String> {
    valid_text(m.text.as_deref().unwrap_or(""))?;
    let all = valid(txn);
    if all.len() >= MAX_MESSAGES {
        return Err(FULL.into());
    }
    if all.iter().any(|(_, _, x)| x.id == m.id) {
        return Err(format!("There's already a message {}", m.id));
    }
    if let Some(r) = &m.reply_to
        && !all.iter().any(|(_, _, x)| &x.id == r)
    {
        return Err(format!("No message {r} to reply to"));
    }
    push(txn, m);
    Ok(())
}

/// The message `id` if `author` wrote it and it isn't deleted.
fn own(txn: &TransactionMut, id: &str, author: &str) -> Result<MapRef, String> {
    let (_, map, m) = valid(txn).into_iter().find(|(_, _, m)| m.id == id).ok_or_else(|| format!("No message {id}"))?;
    if m.author.id != author {
        return Err("Only the author can change a message".into());
    }
    if m.deleted {
        return Err("The message was deleted".into());
    }
    Ok(map)
}

/// `chat.edit`: the author only.
pub fn edit(txn: &mut TransactionMut, id: &str, author: &str, text: &str, at: i64) -> Result<(), String> {
    valid_text(text)?;
    let map = own(txn, id, author)?;
    map.insert(txn, "text", text);
    map.insert(txn, "edited", ms(at));
    Ok(())
}

/// `chat.delete`: the author only; the entry stays, without its text.
pub fn delete(txn: &mut TransactionMut, id: &str, author: &str) -> Result<(), String> {
    let map = own(txn, id, author)?;
    map.remove(txn, "text");
    map.insert(txn, "deleted", true);
    Ok(())
}

/// `chat.clear`: removes every entry and sets `clearedAt`; the number of messages removed.
pub fn clear(txn: &mut TransactionMut, at: i64) -> usize {
    let removed = count(txn);
    if let Some(arr) = txn.get_array(ROOT) {
        let len = arr.len(txn);
        if len > 0 {
            arr.remove_range(txn, 0, len);
        }
    }
    txn.get_or_insert_map(META).insert(txn, "clearedAt", ms(at));
    removed
}

/// `chat.list {since?, limit?}`: the messages after the one with id `since` (all without it),
/// the newest `limit` of them; `{messages, clearedAt?, count}` with `count` every message.
pub fn list(chat: &Chat, params: &Json) -> Json {
    let start = params.get("since").and_then(Json::as_str).and_then(|id| chat.messages.iter().position(|m| m.id == id)).map(|i| i + 1).unwrap_or(0);
    let after = chat.messages.get(start..).unwrap_or_default();
    let limit = params.get("limit").and_then(Json::as_u64).map(|l| l as usize).unwrap_or(usize::MAX);
    let from = after.len().saturating_sub(limit);
    let mut j = json!({"messages": after.get(from..).unwrap_or_default().iter().map(Message::to_json).collect::<Vec<_>>(), "count": chat.messages.len()});
    if let (Some(c), Some(o)) = (chat.cleared_at, j.as_object_mut()) {
        o.insert("clearedAt".into(), json!(c));
    }
    j
}

/// Who's typing: the names of the awareness states with `typing: true`.
pub fn typing<'a>(states: impl IntoIterator<Item = &'a Json>) -> Vec<String> {
    let mut names: Vec<String> = states
        .into_iter()
        .filter(|s| s.get("typing").and_then(Json::as_bool) == Some(true))
        .filter_map(|s| s.get("user").and_then(|u| u.get("name")).and_then(Json::as_str).map(str::to_string))
        .collect();
    names.sort();
    names
}

/// The chat of a document.
pub fn of(doc: &Doc) -> Chat {
    read(&doc.transact())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author(id: &str) -> Author {
        Author { id: id.into(), name: id.to_uppercase(), color: "#112233".into() }
    }

    fn msg(id: &str, who: &str, text: &str) -> Message {
        Message { id: id.into(), author: author(who), at: 1_760_000_000_000, text: Some(text.into()), ..Message::default() }
    }

    #[test]
    fn send_edit_delete_clear() {
        let doc = Doc::with_client_id(5);
        let mut t = doc.transact_mut();
        send(&mut t, &msg("aaaaaaaa", "a", "hi")).unwrap();
        assert!(send(&mut t, &msg("aaaaaaaa", "a", "again")).is_err());
        assert!(send(&mut t, &msg("bbbbbbbb", "a", "")).is_err());
        assert!(send(&mut t, &msg("bbbbbbbb", "a", &"x".repeat(MAX_TEXT + 1))).is_err());
        send(&mut t, &msg("bbbbbbbb", "b", &"é".repeat(MAX_TEXT))).unwrap();
        assert!(edit(&mut t, "aaaaaaaa", "b", "no", 1).is_err());
        edit(&mut t, "aaaaaaaa", "a", "hello", 7).unwrap();
        assert!(delete(&mut t, "bbbbbbbb", "a").is_err());
        delete(&mut t, "bbbbbbbb", "b").unwrap();
        assert!(edit(&mut t, "bbbbbbbb", "b", "back", 8).is_err());
        let c = read(&t);
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].text.as_deref(), Some("hello"));
        assert_eq!(c.messages[0].edited, Some(7));
        assert!(c.messages[1].deleted && c.messages[1].text.is_none());
        assert_eq!(Chat::from_json(&c.to_part()).unwrap(), c);
        assert_eq!(clear(&mut t, 9), 2);
        assert_eq!(read(&t), Chat { cleared_at: Some(9), messages: vec![] });
    }

    #[test]
    fn the_limit() {
        let doc = Doc::with_client_id(5);
        let mut t = doc.transact_mut();
        let mut ids = crate::gridcraft::Ids::seeded(3);
        let mut first = None;
        for _ in 0..MAX_MESSAGES {
            let id = ids.next_id();
            first.get_or_insert(id.clone());
            push(&mut t, &msg(&id, "a", "m"));
        }
        // Readers ignore entries beyond the limit; senders refuse more.
        push(&mut t, &msg("zzzzzzzz", "a", "beyond"));
        let c = read(&t);
        assert_eq!(c.messages.len(), MAX_MESSAGES);
        assert!(c.messages.iter().all(|m| m.id != "zzzzzzzz"));
        assert_eq!(send(&mut t, &msg("yyyyyyyy", "a", "one more")), Err(FULL.to_string()));
        // Deleting doesn't free a place; clearing does.
        delete(&mut t, first.as_deref().unwrap(), "a").unwrap();
        assert_eq!(send(&mut t, &msg("yyyyyyyy", "a", "one more")), Err(FULL.to_string()));
        assert_eq!(clear(&mut t, 1), MAX_MESSAGES);
        send(&mut t, &msg("yyyyyyyy", "a", "one more")).unwrap();
        // The JSON form applies the same limit.
        let many: Vec<Json> = (0..MAX_MESSAGES + 5).map(|i| msg(&format!("{i:08}"), "a", "m").to_json()).collect();
        assert_eq!(Chat::from_json(&json!({"version": 1, "messages": many})).unwrap().messages.len(), MAX_MESSAGES);
    }

    #[test]
    fn malformed_parts() {
        assert!(Chat::from_json(&json!([])).is_err());
        assert!(Chat::from_json(&json!({"version": 2, "messages": []})).is_err());
        assert!(Chat::from_json(&json!({"version": 1})).is_err());
        let c = Chat::from_json(&json!({"version": 1, "clearedAt": "x", "messages": [
            {"id": "aaaaaaaa", "author": {"id": "a", "name": "A", "color": "#000000"}, "at": 1, "text": "ok"},
            {"id": "aaaaaaaa", "author": {"id": "a", "name": "A", "color": "#000000"}, "at": 1, "text": "duplicate"},
            {"id": "AAAAAAAA", "author": {"id": "a", "name": "A", "color": "#000000"}, "at": 1, "text": "bad id"},
            {"id": "bbbbbbbb", "author": "A", "at": 1, "text": "bad author"},
            {"id": "cccccccc", "author": {"id": "a", "name": "A", "color": "#000000"}, "at": -1, "text": "bad at"},
            {"id": "dddddddd", "author": {"id": "a", "name": "A", "color": "#000000"}, "at": 1},
            {"id": "eeeeeeee", "author": {"id": "a", "name": "A", "color": "#000000"}, "at": 1, "deleted": true, "replyTo": 5},
            3
        ]}))
        .unwrap();
        assert_eq!(c.cleared_at, None);
        assert_eq!(c.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["aaaaaaaa", "eeeeeeee"]);
        assert!(c.messages[1].reply_to.is_none());
    }

    #[test]
    fn list_since_and_limit() {
        let c = Chat { cleared_at: Some(3), messages: ["aaaaaaaa", "bbbbbbbb", "cccccccc"].iter().map(|id| msg(id, "a", "x")).collect() };
        let l = list(&c, &json!({"since": "aaaaaaaa", "limit": 1}));
        assert_eq!(l["count"], 3);
        assert_eq!(l["clearedAt"], 3);
        assert_eq!(l["messages"].as_array().unwrap().len(), 1);
        assert_eq!(l["messages"][0]["id"], "cccccccc");
        assert_eq!(list(&c, &json!({}))["messages"].as_array().unwrap().len(), 3);
    }
}
