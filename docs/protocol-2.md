# Craft plugin protocol 2: the push channel and co-editing

Version 2.0.1, 2026-10-15. Shared by GridCraft, WordCraft and DeckCraft and by every transport
plugin. It adds to protocols 1, 1.1 and 1.2 (each app's `docs/plugins.md`;
[host-protocol-1.2.md](https://github.com/sirmmo/craft-nextcloud/blob/main/docs/host-protocol-1.2.md))
and changes nothing in them.

Co-editing is **native**: each app merges edits itself, shows who's there, and keeps per-user
undo. **Transports are plugins**: they move opaque bytes between the people in a room and know
nothing about documents. One transport serves the three apps. The wire format is Yjs (the `yrs`
crate in the apps), so a transport can also be a thin bridge to existing Yjs servers.

Contents: [1 Push channel](#1-the-push-channel) · [2 Rooms](#2-rooms) ·
[3 Messages](#3-collab-messages) · [4 Encryption](#4-end-to-end-encryption) ·
[5 Documents](#5-the-shared-documents) · [6 The app's side](#6-what-the-app-does) ·
[7 Limits](#7-limits-and-failures) · [A MQTT binding](#appendix-a-mqtt-binding) ·
[B y-websocket binding](#appendix-b-y-websocket-binding) · [C Conformance](#appendix-c-conformance)

## 1. The push channel

Until protocol 2, a plugin speaks only while the app waits for its answer. From protocol 2:

- The app **reads every running plugin's standard output all the time** (a reader thread per
  plugin) and hands messages to its main loop. A plugin may write at any moment.
- A plugin may send **notifications** (JSON-RPC without `id`) whenever it likes, and **requests**
  (with its own ids) to the methods marked *anytime* below. The app answers those
  asynchronously, in any order.
- The app may send the plugin notifications at any time, interleaved with its requests. A
  plugin must read standard input continuously and never assume the next line answers its last
  request: match answers by `id`.
- The protocol-1 rules still hold for the app's requests (one at a time per plugin, timeouts,
  failure counting). Notifications never count as failures. A malformed one is logged and
  dropped.
- A line is at most 64 MiB (room for a 16 MiB update in base64, section 7). A longer line is
  dropped, logged, and counts as one failure.

`initialize` params gain `host.features` entries `"push"` and `"collab"` and:

```json
"host": {"collab": {"formats": ["yjs-v1"], "app": "gridcraft", "schema": 1, "encryption": ["aes-256-gcm"]}}
```

A transport plugin declares itself in its manifest:

```json
"collab": {"transport": "mqtt", "label": "Live (MQTT)"}
```

Only plugins with `collab` in their manifest may join rooms. Live data (`STREAM()`, RTD) builds
on the same channel; it is specified separately.

## 2. Rooms

A **room** is one shared document being edited live. Each open document is in at most one room.
The room's id is chosen by the transport: at most 256 characters, unique within the transport
(for example `mqtt:budget-7f3a` or `nc:cloud.example:12345`). Everyone in a room uses the same
room id: it's part of what encryption authenticates (section 4).

### Joining

A plugin command's result (protocol 1.2 results may carry it with `dialog`, `open` and
`message`) or the app command `collab.join` (programmatic) asks the app to join:

```json
"collab": {"join": {
  "room": "mqtt:budget-7f3a",
  "mode": "share",
  "user": {"id": "u-1f2e", "name": "Marco", "color": "#3B7DD8"},
  "role": "edit",
  "key": "base64 of 32 bytes, optional",
  "path": "/abs/local/file.xlsx, optional",
  "title": "Budget, optional"
}}
```

- `mode: "share"` starts a room from the document that's open now (the active one, or `path`).
  The app seeds the room from it.
- `mode: "join"` joins a room others started. The app opens a new, empty document of its kind
  (or `path`, if given, as the starting point to merge with) and fills it from the room's
  state. Until the first state arrives the document shows "Joining…" and is read-only.
- `user`: `id` (1–128 characters, stable per person), `name` (1–128), `color` (`#RRGGBB`).
  The app may let the person change their name and colour.
- `role`: `edit` or `view`. A viewer's document is read-only. Roles are advisory between peers.
  Only a transport with a server (WOPI, an authenticated relay) can enforce them.
- The app refuses a join whose room is already joined, whose document is already in a room,
  or when the plugin isn't a transport, and says why.

After joining, the app sends `collab/joined` (below) and starts syncing. It shows the room in its
status bar (who's there, online or offline) and draws the others' cursors and selections in
their colours.

### Leaving

The person leaves from the app's UI or `collab.leave {room}`, or the transport sends
`collab/leave`. The document stays open, as an ordinary document with all the edits, and is
not saved automatically.

## 3. `collab/*` messages

All are notifications unless marked *request*. Binary payloads are base64 (standard alphabet,
with padding). Every message carries `room`. A message the transport relays from another peer
also carries `from`, that peer's `clientId`, when the transport knows it.

| Method | From | Params | Meaning |
|---|---|---|---|
| `collab/joined` | app | `{room, clientId, user, app, schema, encrypted}` | The app has joined. `clientId` is its Yjs client id (an integer below 2^53, new per join). The transport announces the peer |
| `collab/sync1` | both | `{room, stateVector, from?}` | Yjs sync step 1: "here is what I have". Sent by the app on join and whenever the transport reports `online` after another state; a transport relays it to the others. An app that receives one answers with `sync2` (`to`: the asker's `from`) and, when the state vector shows changes it lacks, sends its own `sync1` |
| `collab/sync2` | both | `{room, update, to?, snapshot?}` | Yjs sync step 2: the answer to a `sync1`, the update the asker lacks. `to` is the asker's `clientId` when the transport can address one peer; otherwise everyone receives it (harmless: applying an update twice changes nothing). From a transport, `snapshot: true` marks the room's stored state (Appendix A), encrypted with the method name `collab/snapshot` |
| `collab/update` | both | `{room, update}` | An incremental update: the app sends one per committed command (the command's whole effect), the transport relays others' |
| `collab/awareness` | both | `{room, update}` | A Yjs awareness update (y-protocols encoding) carrying each peer's `{user, cursor, selection}` in the app's own selection format (section 5). At most 20 per second per app |
| `collab/status` | transport | `{room, state, peers?, message?}` | `state`: `connecting`, `online`, `offline`, `ended`. `peers`: `[{clientId?, user}]`, the others in the room (not the app itself), when known. `message`: shown in the status bar |
| `collab/leave` | both | `{room, reason?}` | Leave the room (app: the person left; transport: the room was closed or access revoked) |
| `collab/snapshot` | transport, *request*, anytime | `{room, file?: bool}` | → `{update, file?, name?}`: the room's whole state as one update, and with `file: true` also the document as file bytes (the app's own format: `.xlsx`, `.docx`, `.pptx`) for the transport to store (a retained snapshot, WOPI PutFile) |
| `host/log` | transport, *request*, anytime | `{text}` | → `{}` |

The app applies remote updates in its main loop, between frames, never in the middle of a
command. A remote update that fails to apply (corrupt, or schema mismatch) is dropped, logged,
and reported as `collab/status`-style text in the app's status bar. The app never crashes on
one.

## 4. End-to-end encryption

When the join has a `key` (32 bytes), the app encrypts every `update` and awareness `update` it
sends, and decrypts every one it receives, with AES-256-GCM:

- payload = `nonce (12 random bytes) || ciphertext || tag (16 bytes)`, then base64;
- associated data = UTF-8 of `room` + `"\n"` + the method name (`collab/update`…), so a message
  can't be replayed into another room or as another kind. The room's whole state (the `update`
  of a `collab/snapshot` answer, and a stored snapshot handed back as `sync2` with
  `snapshot: true`) uses the method name `collab/snapshot`; the `file` bytes of a snapshot answer
  are not encrypted;
- `stateVector` is not encrypted. It reveals only how many changes each client made.

A message that fails to decrypt is dropped and counted. After 10 of them the app reports "Wrong
room key?" in the status bar. The transport carries only ciphertext and never stores or sends the
key (it may create one for a share, or read it from an invitation, only to hand it to the app);
the key travels in invitations as a URL fragment (`#k=…`) or out of band.

## 5. The shared documents

Each app defines its document as Yjs shared types under fixed root names, with a `schema`
number. Peers with a different `app` or `schema` don't sync: the app shows "This room is for
<app> <schema>". Every document has the root map `meta` = `{app, schema, title}`. The
per-app layout is specified in the app's `docs/collab.md`; it follows these rules:

- **Stable ids, not positions.** Rows and columns (GridCraft), paragraphs and table rows
  (WordCraft), slides and shapes (DeckCraft) have ids that never change. Order is a Yjs array of
  ids, so inserting a row or a slide doesn't shift anyone's references. Concurrent edits can
  leave an id twice in such an array: readers keep its first occurrence.
- **Values, not results.** Peers exchange what people type and set (cell inputs, text, formats,
  geometry), never derived results. Recalculation, layout and rendering stay local and
  deterministic.
- **Text is Yjs text** with formatting as attributes (WordCraft stories, shape text, cell rich
  text); other properties are last-writer-wins entries in maps.
- **Selections in awareness** use the app's ids: GridCraft `{sheet, ranges: [[rowId, colId, rowId, colId]]}`,
  WordCraft relative positions (Yjs `RelativePosition`, base64) for anchor and focus,
  DeckCraft `{slide, shapes: [ids]}` plus a text relative position when editing text.

Outline (normative detail in each app's `docs/collab.md`):

| App | Roots |
|---|---|
| GridCraft (schema 1) | `meta`; `sheets` (array of sheet ids); `sheet:<id>` map: `name`, `rows`/`cols` (arrays of ids), `cells` (map `<rowId>:<colId>` → map `{input, style}`), `rowProps`, `colProps`, `merges`, `tables`, `charts`, `cf`, `dv`; `names`; `styles` |
| WordCraft (schema 1) | `meta`; `body` (Yjs XML fragment: paragraphs, tables, inline content with formatting attributes); `headers`, `footers`, `notes` (maps of fragments); `styles`; `numbering`; `comments`; `settings` |
| DeckCraft (schema 1) | `meta`; `slides` (array of slide ids); `slide:<id>` map: `layout`, `shapes` (array of shape ids), `shape:<id>` maps (geometry, fill, line… plus `text` as Yjs XML fragment), `notes`, `transition`; `masters`; `layouts`; `theme` |

## 6. What the app does

- **Seeding** (`share`): build the shared document from the open document in one transaction
  with origin `seed`, then `collab/sync1`.
- **Local edits:** every committed command (the engine's undo step) becomes one Yjs
  transaction with the local origin, sent as one `collab/update`. Commands that change only
  view state (zoom, scroll, selection) don't. Selection goes to awareness instead.
- **Remote edits:** applied in a transaction with the remote origin, then mapped back into the
  app's model. They don't enter the local undo stack. Per-user undo (Yjs `UndoManager`
  tracking the local origin) reverts only the person's own changes.
- **Saving:** each peer saves to its own file as usual (the protocol 1.2 events still fire). The
  transport decides who stores the shared copy, using `collab/snapshot`.
- **Recovery:** the app keeps the room's state (the whole Yjs update) with its AutoRecover data,
  so a crash and rejoin merges instead of losing offline edits.
- **Commands** (everything is a command): `collab.join {plugin, …join params}`, `collab.leave
  {room?}`, `collab.status` → `{rooms: [{room, plugin, state, peers, role, encrypted}]}`,
  `collab.setUser {name?, color?}`. UI: a Share group (Start Live Session, Join, Leave), the
  people in the status bar, coloured cursors and selections with names.
- **Web builds** have no plugins: they speak the y-websocket binding (Appendix B) natively.

## 7. Limits and failures

- An update is at most 16 MiB before base64. A larger local transaction is split; a larger
  remote one is refused.
- Awareness at most 64 KiB and 20 per second. Rooms: at most 16 per transport, one per document.
- If the transport exits or reports `offline`, the room goes offline and editing goes on locally.
  On `online` (or the transport's restart and the app's re-join) the app sends `collab/sync1`
  again and the edits merge.
- A transport that floods (over 1,000 messages a second for 5 s) is stopped and marked failed.
- No guarantee of intent beyond CRDT convergence: two people typing in one cell keep the later
  write; simultaneous structural edits (deleting a row someone is typing in) converge to the same
  result on every peer, which may drop the typing. The app shows remote edits briefly highlighted
  so people notice.

## Appendix A: MQTT binding

So that any MQTT-speaking implementation interoperates. The room id is `mqtt:<name>`, where
`<name>` (1–128 of `A-Z a-z 0-9 . _ -`; random, 16 characters of `0-9a-z`, when a transport
makes one) is the last part of the invitation link
`craft+mqtt://<host>:<port>/<name>[?prefix=<prefix>][#k=<key>]` (`craft+mqtts://` over TLS;
`prefix` percent-encoded, given only when it isn't the default; the key in the URL-safe base64
alphabet without padding, and readers accept the standard one too). The broker's address isn't
part of the room id, so people who reach one broker by different addresses meet in the same room.
Topics under a prefix (default `craft/v1`), the room id percent-encoded (`/`, `+`, `#`, `%`, and
non-ASCII encoded):

| Topic | QoS | Retained | Payload |
|---|---|---|---|
| `<prefix>/<room>/m/<clientId>` | 1 | no | one message from that client (below) |
| `<prefix>/<room>/snapshot` | 1 | yes | the room's whole state: a Yjs update (encrypted as in section 4 when the room has a key, with the method name `collab/snapshot`) |
| `<prefix>/<room>/p/<clientId>` | 0 | yes | presence: JSON `{user, app, schema, encrypted}`; an empty retained payload (and the MQTT will message) clears it |

A client subscribes to `<prefix>/<room>/m/+`, `…/snapshot` and `…/p/+`, ignores messages on
its own `clientId`, and publishes on its own. Message payload: one byte for the kind, then the
data:

| Byte | Kind | Data |
|---|---|---|
| 0 | sync1 | the state vector |
| 1 | sync2 | 8 bytes big-endian addressee `clientId` (0 = everyone), then the update (encrypted when keyed) |
| 2 | update | the update (encrypted when keyed) |
| 3 | awareness | the awareness update (encrypted when keyed) |
| 4 | leave | nothing |

On joining, a client reads the retained snapshot (if any) and applies it, then sends sync1 (a
transport hands the snapshot to its app as `collab/sync2` with `snapshot: true`, and doesn't
hand back the echo of one it stored itself). Any client may replace the snapshot. The transport
does so after every 500 updates it sent or relayed, or 10 minutes after the last stored snapshot
when there were updates since, with the app's `collab/snapshot`, so newcomers don't need the
others to be online; a snapshot someone else stores restarts both counts. The transport of the
person who shared the room also stores one as soon as the room is first online.

## Appendix B: y-websocket binding

The y-websocket protocol (lib0 encoding): message type varuint `0` (sync) with step `0`/`1`/`2`
(sync1, sync2, update) and a varbytes payload, or `1` (awareness) with a varbytes awareness update. The
room is the URL path. A transport (or a web build) maps `collab/sync1|sync2|update|awareness`
one to one. Encrypted rooms carry ciphertext in the payloads; such rooms work through a plain
y-websocket relay but not with servers that read the document (Hocuspocus persistence
extensions), which then store ciphertext.

## Appendix C: Conformance

`craft-collab` ships transcripts (`conformance/*.jsonl`: messages in and out, with the expected
document after each step, for each app's schema; format in its `docs/conformance.md`), a checker
that drives a transport plugin as two and three apps would (`craft-collab-conformance transport`:
a transport's `share` command answers `collab.join` and `result.link`, and `join {link}` joins
that room) and a loopback transport plugin that echoes between two app processes. An app or
transport conforms when it passes them. Each app's fuzzers also check that random concurrent
command sequences on two peers converge to the same document.

## Revision history

| Date | Version | Change |
|---|---|---|
| 2026-10-15 | 2.0.1 | Clarifications found while building craft-collab, no change of intent: a line may be 64 MiB (a 16 MiB update doesn't fit 16 MiB in base64); relayed messages carry `from`, so a `sync2` answer can be addressed; an app answers a `sync1` and asks back with its own when the asker has changes it lacks, and sends `sync1` when the transport reports `online` after another state; a stored snapshot reaches the app as `sync2` with `snapshot: true`, and a room's whole state is encrypted with the method name `collab/snapshot`; `peers` are the others; the transport never stores or sends the key; readers keep the first of a duplicated id; the MQTT binding's room id (`mqtt:<name>`), invitation link and snapshot timing; the conformance checker's conventions |
| 2026-10-15 | 2.0 | Created: the push channel, rooms, `collab/*` messages, end-to-end encryption, the shared documents, MQTT and y-websocket bindings, conformance |

