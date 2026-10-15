# Conformance

Two halves, as in [protocol 2, Appendix C](protocol-2.md#appendix-c-conformance):

- **Transports** are checked by `craft-collab-conformance transport`, which starts the plugin
  several times and plays the apps.
- **Apps** replay the transcripts in [`conformance/`](../conformance): what a transport sends the
  app, what the app must send back, and the document it must have after each step.

## Checking a transport

```sh
cargo run -p craft-collab-conformance -- transport \
    --share '{"broker": "mqtt://127.0.0.1:1883"}' --encrypted \
    --env CRAFT_COLLAB_SNAPSHOT_EVERY=5 -- ./target/debug/craft-mqtt
cargo run -p craft-collab-conformance -- transport --dir path/to/plugin-folder
```

| Option | Means |
|---|---|
| `--dir DIR` / `-- PROGRAM ARGS…` | The plugin: a folder with `plugin.json`, or a command line |
| `--share JSON`, `--join JSON` | Extra params for the transport's `share` and `join` commands (a broker, credentials) |
| `--encrypted` | Test an end-to-end encrypted room |
| `--env K=V` | Environment for the plugin processes (repeatable) |
| `--timeout SECS` | How long to wait for the network (default 20) |
| `--require-persistence` | Fail, not just warn, when a newcomer can't get the room's state with nobody online |

What a transport must offer: a `share` command that, given `{name, color, encrypt}` plus the
`--share` params, answers `collab.join` (mode `share`) and `result.link`; a `join` command that,
given `{link, name, color}` plus the `--join` params, answers `collab.join` (mode `join`) for the
same room and key.

The checker starts apps A and B (and later C), each a plugin process driven by the reference app
(`crates/conformance/src/refapp.rs`, GridCraft schema 1 on `yrs`), and checks: the manifest's
`collab`; `initialize`; the shape of both `collab.join`s; `collab/status` going online; the joiner
getting the document; `from` on relayed `sync1`; peers in the status; edits both ways and 20
rounds of concurrent edits converging; ciphertext arriving intact; awareness; snapshot requests;
`leave` sending `collab/leave`, the others seeing the leaver go, and nothing arriving after it;
a newcomer getting the stored snapshot (as `sync2` with `snapshot: true`) with nobody else
online; malformed input dropped; every command answering odd params; exiting on `shutdown` and
when standard input closes. It prints one line per check and exits 1 when one fails.

## Transcripts

`conformance/*.jsonl`, one JSON object per line, each with a type `t`:

| `t` | Fields | The app under test… |
|---|---|---|
| `meta` | `name`, `app`, `schema`, `format` (1), `description` | (first line) |
| `note` | `text` | nothing: a comment |
| `run` | `command`, `params` | runs its own command, programmatically. `collab.join` comes with `plugin: "org.craft.conformance"`: the replayer stands in for that transport |
| `in` | `msg`, `valid?` | receives `msg` from the transport (a notification, or a request with `id`). `valid: false` marks input it must drop |
| `out` | `msg` (a pattern), `decodesTo?`, `optional?` | must have sent a matching message since the last `out` |
| `quiet` | | must have sent nothing since the last `out` but the messages below |
| `model` | `model` | must hold this document now |
| `notice` | `contains`, `lang` | must have shown a status message containing this text (English UI) |

**Matching `out`.** The app's messages are taken in order. Between two expected ones it may send
any number of `collab/awareness`, `collab/sync1` and `host/log` messages; anything else must match
the next `out`. A pattern matches when every key it has matches: objects key by key, arrays item
by item, other values exactly, and these strings specially: `"$any"` (anything present), `"$b64"`
(a base64 string), `"$int"`, `"$absent"` (the key must be missing), and `"$clientId"` (bound to
the first value it meets, the app's client id in `collab/joined`, and equal afterwards). An
`optional` `out` may be missing; when the app sends something else instead, the line is skipped.

**`decodesTo`.** What another peer would see: apply, to an empty Yjs document, every update in the
`in` lines so far (but those marked `valid: false`) and every update the app has sent (`sync2`,
`update`, `collab/snapshot` answers), decrypting with the room key from `collab.join` and the
method name of section 4 (`collab/snapshot` for a snapshot answer and for `sync2` with
`snapshot: true`). The resulting document must read as this model.

**Models.** `{"title", "sheets": [{"name", "cells": {"A1": "input"}}]}` for GridCraft schema 1
([schemas/gridcraft-1.md](schemas/gridcraft-1.md#the-model-the-transcripts-compare));
`{"title", "body": [{"text", "runs": [{"text", …attributes}], "props"?} | {"table": [[[blocks]]]}]}`
for WordCraft schema 1 ([schemas/wordcraft-1.md](schemas/wordcraft-1.md#the-model-the-transcripts-compare)).
The `meta` line's `app` says which.

**WordCraft's commands** in `run` lines: `select.text {text}` (selects the first match in the
body), `caret.set {pos: {story: "body", path, off}}` (a caret at byte `off` of the paragraph at
block path `path`), `text.insert {text}` (replaces the selection; the text takes the formatting of
the character before), `text.newParagraph` (Enter), `format.bold {value}` (sets `bold` and
`boldCs`).

| Transcript | Covers |
|---|---|
| `gridcraft-1-join` | joining: the first state, remote cell edits, a row inserted with its formula rewritten, a column deleted, sheets added and renamed, awareness, another room's message ignored, answering a newcomer's `sync1` with `to`, a local edit, `collab/leave` from the transport, editing after leaving (nothing sent) |
| `gridcraft-1-share` | sharing: edits before joining aren't sent, the seed, answering `collab/snapshot` and `sync1`, local edits as one update each (a cell, an inserted row), a concurrent remote title and sheet merging in |
| `gridcraft-1-encrypted` | a keyed room: the stored snapshot (`snapshot: true`), updates, dropping ciphertext for another method, tampered, for another room, with another key, garbage; answering with ciphertext; "Wrong room key?" after 10 failures |
| `gridcraft-1-robustness` | missing fields, bad base64, bytes that aren't an update, a truncated update, a bad state vector, an unknown status, a WordCraft document ("This room is for wordcraft 1"), an unknown method, bad awareness |
| `gridcraft-1-offline` | offline and back: local and remote edits meanwhile, both growing the sheet past its last row at once, `sync1` after coming back online, the answer to the peer's `sync1` holding the offline edits |
| `wordcraft-1-join` | joining: the first state (a heading, bold text, a table), remote typing, a remote bold range, a remote Enter splitting a paragraph, typing in a table cell, awareness with relative positions, another room's message ignored, answering a newcomer's `sync1` with `to`, a local edit, `collab/leave`, editing after leaving (nothing sent) |
| `wordcraft-1-share` | sharing: typing before joining isn't sent, the seed, answering `collab/snapshot` and `sync1`, local edits as one update each (bold, typing over a selection, Enter, typing), a concurrent remote title and paragraph merging in |
| `wordcraft-1-encrypted` | a keyed room, as `gridcraft-1-encrypted` |
| `wordcraft-1-robustness` | as `gridcraft-1-robustness`, with a GridCraft document ("This room is for gridcraft 1") |
| `wordcraft-1-offline` | offline and back with both people typing in one paragraph meanwhile, twice at the same place: the edits merge character by character, the lower client id's text first |

The replayer in this repository (`craft-collab-conformance transcripts`) runs them against the
reference app; an app replays them with its own engine (`run` → its commands, `in` → its collab
inbox, `out` ← its outbox, `model` ← its workbook). Regenerate them after changing the generator
with `cargo run -p craft-collab-conformance -- generate conformance` (they're deterministic, and a
test fails when the committed ones are out of date).
