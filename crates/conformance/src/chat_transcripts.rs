//! The chat transcripts (`conformance/<app>-1-chat.jsonl`, `<app>-1-chat-share.jsonl`), the same
//! for every app's schema: chat is the common roots `chat` and `chatMeta` (`docs/chat.md`). The
//! peers write the chat with [`crate::chat`] next to a blank document of the app's schema.
//!
//! The 10,000-message limit isn't in a transcript: a chat at the limit is over a megabyte of
//! updates. `chat::tests::the_limit` checks the reference implementation directly, and each app
//! checks its own in its tests.

use std::sync::Arc;

use craft_collab_kit::b64;
use serde_json::{Value as Json, json};
use yrs::updates::encoder::Encode;
use yrs::{Any, StateVector, Transact};

use crate::awareness::{self, Entry};
use crate::chat::{self, Author, Message};
use crate::deckcraft::DeckCraft;
use crate::gridcraft::{GridCraft, Ids};
use crate::schema::Schema;
use crate::transcript::{HIGH_PEER, PEER, Peer, Script, app_user, peer_user};
use crate::wordcraft::WordCraft;

/// A client id below any app's, so its concurrent message lands before the app's.
const LOW_PEER: u64 = 2;
/// The peers' clock: 2025-10-16 09:00 UTC, a minute per message.
const T0: i64 = 1_760_605_200_000;

fn author_of(user: &Json) -> Author {
    Author::from_json(user).unwrap_or_default()
}

fn low_user() -> Json {
    json!({"id": "u-low", "name": "Low", "color": "#2BA84A"})
}

fn high_user() -> Json {
    json!({"id": "u-high", "name": "High", "color": "#8E44AD"})
}

/// A message from a peer.
fn message(ids: &mut Ids, user: &Json, minute: i64, text: &str) -> Message {
    Message { id: ids.next_id(), author: author_of(user), at: T0 + minute * 60_000, text: Some(text.into()), ..Message::default() }
}

/// A pattern for a message: its JSON with `"$absent"` for the optional keys it lacks.
fn pat(m: &Message) -> Json {
    let mut j = m.to_json();
    if let Some(o) = j.as_object_mut() {
        for k in ["text", "replyTo", "edited", "deleted"] {
            o.entry(k).or_insert(json!("$absent"));
        }
    }
    j
}

/// A pattern for one of the app's messages: its id bound to `var`.
fn mine(var: &str, author: Json, text: &str, reply_to: Option<&str>) -> Json {
    json!({"id": var, "author": author, "at": "$int", "text": text, "replyTo": reply_to.map_or(json!("$absent"), |r| json!(r)), "edited": "$absent", "deleted": "$absent"})
}

fn chat_line(s: &mut Script, messages: &[Json], cleared: bool) {
    let c = json!({"messages": messages, "clearedAt": if cleared { "$int" } else { "$absent" }});
    s.lines.push(json!({"t": "chat", "chat": c}));
}

/// An `out` line for the app's update, with what a peer would see of the chat.
fn update_out(s: &mut Script, messages: &[Json], cleared: bool) {
    let room = s.room.clone();
    let c = json!({"messages": messages, "clearedAt": if cleared { "$int" } else { "$absent" }});
    s.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "method": "collab/update", "params": {"room": room, "update": "$b64"}}, "chatDecodesTo": c}));
}

fn peer<S: Schema>(client: u64, seed: u64) -> Peer {
    Peer { doc: S::new_doc(client), ids: Ids::seeded(seed) }
}

/// A peer with the same state as `of`.
fn copy<S: Schema>(of: &Peer, client: u64, seed: u64) -> Peer {
    let mut p = peer::<S>(client, seed);
    let full = of.full();
    p.edit(|t, _| {
        if let Ok(u) = <yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&full) {
            let _ = t.apply_update(u);
        }
    });
    p
}

fn awareness_in(s: &mut Script, client: u64, clock: u64, user: Json, typing: bool) {
    let aw = awareness::encode(&[Entry { client, clock, state: json!({"user": user, "typing": typing}) }]);
    let u = s.wire("collab/awareness", &aw);
    let room = s.room.clone();
    s.input("collab/awareness", json!({"room": room, "update": u, "from": client}));
}

fn typing_line(s: &mut Script, names: &[&str]) {
    s.lines.push(json!({"t": "typing", "names": names}));
}

fn fails(s: &mut Script, command: &str, params: Json) {
    s.lines.push(json!({"t": "run", "command": command, "params": params, "fails": true}));
}

fn chat_transcript<S: Schema>() -> Script {
    let app = S::APP;
    let room = "conformance:chat";
    let mut s = Script::for_app(
        app,
        &format!("{app}-1-chat"),
        "Document chat (docs/chat.md): the room's chat arriving with the document, messages from others, sending, concurrent sends converging in one order, replies, edit and delete by the author only, odd text refused, malformed entries ignored, someone typing, clearing.",
        room,
        None,
    );
    s.join("join");
    s.note("The room's state holds a blank document and a message: joining brings the chat in.");
    let mut p = peer::<S>(PEER, 31);
    let m1 = message(&mut p.ids, &peer_user(), 0, "Hi! Shall we start with the totals?");
    p.edit(|t, ids| {
        S::seed(t, &S::blank(), ids);
        chat::push(t, &m1);
    });
    let full = p.full();
    s.input("collab/sync2", json!({"room": room, "update": b64::encode(&full), "from": PEER}));
    s.model_json(S::to_json(&S::read(&p.doc.transact()).unwrap_or_default()));
    chat_line(&mut s, &[pat(&m1)], false);
    let m2 = message(&mut p.ids, &peer_user(), 1, "I'm here.\nTwo lines, with a link: https://example.org/a?b=c");
    let u = p.edit(|t, _| chat::push(t, &m2));
    s.update(PEER, &u);
    chat_line(&mut s, &[pat(&m1), pat(&m2)], false);

    s.note("The app sends a message while two others send theirs, each knowing only the first two. Yjs puts concurrent appends in the same order everywhere: here Low (client 2, below the app's) before the app's, High (client 2^53-2) after.");
    s.run("chat.send", json!({"text": "Hello from the app"}));
    let me = app_user();
    let a1 = mine("$mine1", me.clone(), "Hello from the app", None);
    update_out(&mut s, &[pat(&m1), pat(&m2), a1.clone()], false);
    chat_line(&mut s, &[pat(&m1), pat(&m2), a1.clone()], false);
    let mut low = copy::<S>(&p, LOW_PEER, 32);
    let mut high = copy::<S>(&p, HIGH_PEER, 33);
    let lm = message(&mut low.ids, &low_user(), 2, "Low here");
    let hm = message(&mut high.ids, &high_user(), 2, "High here");
    let ul = low.edit(|t, _| chat::push(t, &lm));
    let uh = high.edit(|t, _| chat::push(t, &hm));
    s.update(HIGH_PEER, &uh);
    s.update(LOW_PEER, &ul);
    let order = vec![pat(&m1), pat(&m2), pat(&lm), a1.clone(), pat(&hm)];
    chat_line(&mut s, &order, false);
    s.note("High asks with its state vector: what it gets back gives it the same order.");
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&high.sv()), "from": HIGH_PEER}));
    s.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": HIGH_PEER}}, "chatDecodesTo": {"messages": order, "clearedAt": "$absent"}}));
    // High gets Low's message too. It writes the later messages: it never gets the app's updates
    // here, and with its client id above the app's, its appends land after the app's.
    high.edit(|t, _| {
        if let Ok(u) = <yrs::Update as yrs::updates::decoder::Decode>::decode_v1(&ul) {
            let _ = t.apply_update(u);
        }
    });

    s.note("A reply quotes the message it answers (replyTo).");
    s.run("chat.send", json!({"text": "Yes, the totals first.", "replyTo": m1.id}));
    let a2 = mine("$mine2", me.clone(), "Yes, the totals first.", Some(&m1.id));
    let mut all = order.clone();
    all.push(a2.clone());
    update_out(&mut s, &all, false);
    chat_line(&mut s, &all, false);

    s.note("Only the author edits or deletes a message; a deleted message stays as \"Message deleted\" without its text.");
    s.run("chat.edit", json!({"id": "$mine1", "text": "Hello from the app (edited)"}));
    let a1e = json!({"id": "$mine1", "author": me.clone(), "at": "$int", "text": "Hello from the app (edited)", "replyTo": "$absent", "edited": "$int", "deleted": "$absent"});
    if let Some(x) = all.get_mut(3) {
        *x = a1e.clone();
    }
    update_out(&mut s, &all, false);
    chat_line(&mut s, &all, false);
    fails(&mut s, "chat.edit", json!({"id": m1.id, "text": "Not mine to edit"}));
    fails(&mut s, "chat.delete", json!({"id": m1.id}));
    fails(&mut s, "chat.edit", json!({"id": "zzzzzzzz", "text": "No such message"}));
    s.lines.push(json!({"t": "quiet"}));
    s.run("chat.delete", json!({"id": "$mine2"}));
    let a2d = json!({"id": "$mine2", "author": me.clone(), "at": "$int", "text": "$absent", "replyTo": m1.id, "deleted": true});
    if let Some(x) = all.get_mut(5) {
        *x = a2d;
    }
    update_out(&mut s, &all, false);
    chat_line(&mut s, &all, false);
    fails(&mut s, "chat.edit", json!({"id": "$mine2", "text": "Back from the dead"}));
    s.note("Text is 1 to 4,000 UTF-16 units.");
    fails(&mut s, "chat.send", json!({"text": ""}));
    fails(&mut s, "chat.send", json!({"text": "x".repeat(chat::MAX_TEXT + 1)}));
    s.lines.push(json!({"t": "quiet"}));
    chat_line(&mut s, &all, false);

    s.note("The peer edits and deletes its own messages.");
    let u = p.edit(|t, _| {
        let _ = chat::edit(t, &m1.id, "u-peer", "Hi! Shall we start with the totals? (edited)", T0 + 10 * 60_000);
        let _ = chat::delete(t, &m2.id, "u-peer");
    });
    s.update(PEER, &u);
    let mut m1e = m1.clone();
    m1e.text = Some("Hi! Shall we start with the totals? (edited)".into());
    m1e.edited = Some(T0 + 10 * 60_000);
    let mut m2d = m2.clone();
    m2d.text = None;
    m2d.deleted = true;
    if let Some(x) = all.get_mut(0) {
        *x = pat(&m1e);
    }
    if let Some(x) = all.get_mut(1) {
        *x = pat(&m2d);
    }
    chat_line(&mut s, &all, false);

    s.note("Malformed entries are ignored: not a map, a bad id, an author that isn't JSON or lacks a name, a time that isn't a non-negative integer, no text, empty text, text over 4,000, a duplicated id. The good message after them shows.");
    let good = message(&mut high.ids, &high_user(), 12, "Still readable");
    let author = Any::String(Arc::from(author_of(&high_user()).to_json().to_string().as_str()));
    let st = |x: &str| Any::String(Arc::from(x));
    let num = |n: i64| Any::Number(yrs::any::Number::Int(n));
    let at = num(T0 + 11 * 60_000);
    let u = high.edit(|t, _| {
        chat::push_not_a_map(t, "just a string");
        chat::push_raw(t, vec![("id", st("NOT-AN-ID")), ("author", author.clone()), ("at", at.clone()), ("text", st("bad id"))]);
        chat::push_raw(t, vec![("id", st("bad00001")), ("author", st("Peer")), ("at", at.clone()), ("text", st("author isn't JSON"))]);
        chat::push_raw(t, vec![("id", st("bad00002")), ("author", st(r##"{"id":"u-high","color":"#8E44AD"}"##)), ("at", at.clone()), ("text", st("no name"))]);
        chat::push_raw(t, vec![("id", st("bad00003")), ("author", author.clone()), ("at", st("yesterday")), ("text", st("at is text"))]);
        chat::push_raw(t, vec![("id", st("bad00004")), ("author", author.clone()), ("at", num(-5)), ("text", st("at is negative"))]);
        chat::push_raw(t, vec![("id", st("bad00005")), ("author", author.clone()), ("at", at.clone())]);
        chat::push_raw(t, vec![("id", st("bad00006")), ("author", author.clone()), ("at", at.clone()), ("text", st(""))]);
        chat::push_raw(t, vec![("id", st("bad00007")), ("author", author.clone()), ("at", at.clone()), ("text", st(&"y".repeat(chat::MAX_TEXT + 1)))]);
        chat::push_raw(t, vec![("id", st(&m1.id)), ("author", author.clone()), ("at", at.clone()), ("text", st("a duplicated id"))]);
        chat::push(t, &good);
    });
    s.update(HIGH_PEER, &u);
    all.push(pat(&good));
    chat_line(&mut s, &all, false);

    s.note("Presence: typing: true in someone's awareness state shows \"Name is typing…\".");
    awareness_in(&mut s, PEER, 1, peer_user(), true);
    typing_line(&mut s, &["Peer"]);
    awareness_in(&mut s, HIGH_PEER, 1, high_user(), true);
    typing_line(&mut s, &["High", "Peer"]);
    awareness_in(&mut s, PEER, 2, peer_user(), false);
    typing_line(&mut s, &["High"]);

    s.note("Clearing removes every message for everyone and sets chatMeta.clearedAt; a message after it starts the new chat.");
    s.run("chat.clear", json!({}));
    update_out(&mut s, &[], true);
    chat_line(&mut s, &[], true);
    let m4 = message(&mut high.ids, &high_user(), 20, "Fresh start");
    let u = high.edit(|t, _| chat::push(t, &m4));
    s.update(HIGH_PEER, &u);
    chat_line(&mut s, &[pat(&m4)], true);
    s.run("chat.send", json!({"text": "Agreed"}));
    let a3 = mine("$mine3", me, "Agreed", None);
    update_out(&mut s, &[pat(&m4), a3.clone()], true);
    chat_line(&mut s, &[pat(&m4), a3], true);
    s
}

fn share_transcript<S: Schema>() -> Script {
    let app = S::APP;
    let room = "conformance:chat-share";
    let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(11).wrapping_add(5));
    let mut s = Script::for_app(
        app,
        &format!("{app}-1-chat-share"),
        "Document chat in an encrypted room: a message left in the document before sharing (by the app's own user) seeds the room's chat; the peer's message, typing and the app's message travel encrypted; answering sync1; after leaving, the chat stays and new messages aren't sent.",
        room,
        Some(key),
    );
    s.note("Outside a session the chat works as notes left in the file; nothing is sent.");
    s.run("chat.send", json!({"text": "A note left in the file"}));
    s.lines.push(json!({"t": "quiet"}));
    let any_author = json!({"id": "$any", "name": "$any", "color": "$any"});
    let note = mine("$note", any_author.clone(), "A note left in the file", None);
    chat_line(&mut s, std::slice::from_ref(&note), false);
    s.join("share");
    s.note("Sharing seeds the room's chat from the document's: the stored snapshot holds the note, encrypted.");
    s.request(1, "collab/snapshot", json!({"room": room}));
    s.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "id": 1, "result": {"update": "$b64"}}, "chatDecodesTo": {"messages": [note.clone()], "clearedAt": "$absent"}}));
    s.note("A peer that started from nothing (its client id above the app's) sends a message: it lands after the note.");
    let mut p = peer::<S>(HIGH_PEER, 41);
    let pm = message(&mut p.ids, &peer_user(), 0, "Got the note, thanks");
    let u = p.edit(|t, _| chat::push(t, &pm));
    s.update(HIGH_PEER, &u);
    chat_line(&mut s, &[note.clone(), pat(&pm)], false);
    awareness_in(&mut s, HIGH_PEER, 1, peer_user(), true);
    typing_line(&mut s, &["Peer"]);
    s.run("chat.send", json!({"text": "In the room now"}));
    let a1 = mine("$mine1", app_user(), "In the room now", None);
    let all = vec![note, pat(&pm), a1];
    update_out(&mut s, &all, false);
    chat_line(&mut s, &all, false);
    s.input("collab/sync1", json!({"room": room, "stateVector": b64::encode(&StateVector::default().encode_v1()), "from": 5005}));
    s.lines.push(json!({"t": "out", "msg": {"jsonrpc": "2.0", "method": "collab/sync2", "params": {"room": room, "update": "$b64", "to": 5005}}, "chatDecodesTo": {"messages": all, "clearedAt": "$absent"}}));
    s.input("collab/leave", json!({"room": room}));
    s.note("After leaving, the chat stays in the document, and a new message is a local note again.");
    s.run("chat.send", json!({"text": "After the session"}));
    s.lines.push(json!({"t": "quiet"}));
    let mut after = all.clone();
    after.push(mine("$after", any_author, "After the session", None));
    chat_line(&mut s, &after, false);
    s
}

/// The chat transcripts for every app.
pub(crate) fn all() -> Vec<Script> {
    vec![
        chat_transcript::<GridCraft>(),
        share_transcript::<GridCraft>(),
        chat_transcript::<DeckCraft>(),
        share_transcript::<DeckCraft>(),
        chat_transcript::<WordCraft>(),
        share_transcript::<WordCraft>(),
    ]
}
