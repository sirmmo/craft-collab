# craft-collab

Live co-editing for the craft apps (GridCraft, WordCraft and DeckCraft): the protocol, the kit
that checks implementations of it, and the first transports.

Co-editing is native to the apps: each app merges edits itself (Yjs, through the `yrs` crate),
shows who's there and keeps per-person undo. **Transports are plugins** that move opaque bytes
between the people in a room. They know nothing about documents, so one transport serves the
three apps, and a room can be end-to-end encrypted.

| | What |
|---|---|
| [docs/protocol-2.md](docs/protocol-2.md) | **Craft plugin protocol 2**: the push channel and co-editing (rooms, `collab/*` messages, encryption, the shared documents, the MQTT and y-websocket bindings). Normative |
| [docs/schemas/gridcraft-1.md](docs/schemas/gridcraft-1.md) | GridCraft's shared workbook (schema 1): roots, ids, cells |
| [docs/schemas/deckcraft-1.md](docs/schemas/deckcraft-1.md) | DeckCraft's shared presentation (schema 1): roots, ids, slides, shapes, text as Yjs text with formatting attributes, awareness |
| [docs/schemas/wordcraft-1.md](docs/schemas/wordcraft-1.md) | WordCraft's shared document (schema 1): stories as Yjs XML, text with formatting attributes, tables, relative-position selections |
| [docs/chat.md](docs/chat.md) | Document chat (protocol 2.1): the chat in the shared document and in the file, its commands and panel |
| [docs/conformance.md](docs/conformance.md) | Checking a transport, and the app-side transcripts |
| [`craft-mqtt`](crates/craft-mqtt) | Transport over any MQTT broker (Mosquitto, EMQX, HiveMQ…), end-to-end encrypted |
| [`craft-collab-loopback`](crates/craft-collab-loopback) | Transport between app processes on one computer, for development and the apps' tests |
| [`craft-collab-conformance`](crates/conformance) | The conformance kit: drives a transport as two and three apps would; generates and replays the transcripts in [conformance/](conformance) (21: five per app's schema, plus two chat transcripts per app) |
| [`craft-collab-kit`](crates/kit) | What the transports share: the push channel over stdio, rooms, relaying, presence, snapshot compaction, dialogs |

The apps implement the app side (GridCraft's, WordCraft's and DeckCraft's `docs/collab.md`); they need plugin
protocol 2. With
an older app, the transports' commands answer with an error saying so.

## Live (MQTT): craft-mqtt

**Share Live** starts a room on a broker and gives you an invitation link:

```
craft+mqtts://broker.example:8883/q0x7m2k9w4c1v8b5#k=Zm9vYmFy…
```

Send it to the people you want to edit with; they paste it in **Join Live**. Everyone edits the
same document at once, sees the others' cursors and selections in their colours, and keeps
working offline: the edits merge when the connection comes back (the transport reconnects by
itself, with backoff from 1 s to 60 s). Each person still saves to their own file.

| Button | Command | Does |
|---|---|---|
| Share Live | `share {broker, username?, password?, name?, color?, encrypt?, room?, prefix?}` | A form: broker (`mqtt://` or `mqtts://host:port`), user name and password if the broker wants them, your name and colour, **Encrypt end to end** (on). Starts the room from the open document and shows the invitation in a field you can copy |
| Join Live | `join {link, name?, color?, username?, password?}` | Joins from an invitation: a new document fills from the room |
| Leave | `leave {room?}` | Leaves the room; the document stays open with every edit |
| Status | `status` | The rooms, their state, who's in them, and their invitations |
| Settings | `settings` | Default broker, user name, your name and colour, topic prefix |

Settings live in `craft-mqtt/settings.json` in your data folder (`~/.local/share`,
`~/Library/Application Support`, `%APPDATA%`; or `CRAFT_MQTT_DATA`), shared by the three apps.
`rooms.json` remembers the rooms you joined lately and their broker, so a restarted transport takes
the app's re-join. Passwords and room keys are never written to disk.

What goes over MQTT ([Appendix A](docs/protocol-2.md#appendix-a-mqtt-binding)), under
`craft/v1/mqtt:<room>/`: `m/<clientId>` (QoS 1) carries each message, `snapshot` (retained) the
room's whole state, and `p/<clientId>` (retained, cleared by an MQTT will when a client vanishes)
who's there. Every 500 updates, or 10 minutes after the last snapshot when there were edits, the
transport asks the app for the room's state and stores it as the retained snapshot, so people who
join later get the document even when nobody else is online.

### Brokers

Any MQTT 3.1.1 broker works. It must accept retained messages and packets as large as your
documents' snapshots.

- **Mosquitto**: `mosquitto -c mosquitto.conf` with a listener (`listener 8883`, `certfile`,
  `keyfile` for TLS), `allow_anonymous false` and a `password_file` for accounts, and
  `message_size_limit 0` (or a size above your largest snapshot). An ACL can confine users to
  `craft/v1/#`.
- **EMQX**: the default maximum packet size is 1 MB: raise `mqtt.max_packet_size` (for example to
  `16MB`) for large workbooks. Add users under Authentication; restrict topics with Authorization.
- **HiveMQ** (and HiveMQ Cloud): works as is; Cloud needs TLS (`mqtts://…:8883`) and the
  credentials you create in its console. The public `broker.hivemq.com` is fine for trying it with
  encryption on, never for anything private.

### Security

- **End to end.** With **Encrypt end to end** (the default), every update, awareness update and
  snapshot is AES-256-GCM ciphertext bound to its room and message kind (section 4). The key is in
  the invitation's fragment (`#k=…`): it's never sent to the broker, and never saved. The broker
  sees ciphertext, plus what it can't avoid seeing: the room name, who connects, when and how
  much, presence (names and colours), and state vectors (how many changes each client made).
- **The invitation is the access.** Anyone with the link joins (with the key, reads). Send it
  through a channel you trust. Unencrypted rooms rely only on the room name being hard to guess
  (16 random characters) and on the broker's access control.
- **Roles are advisory.** `view` makes a peer's own document read-only, but a modified client
  could still write: between peers nobody can enforce it. Use broker ACLs to keep strangers out.
- **TLS** (`mqtts://`) checks the broker's certificate against the system's roots (Mozilla's when
  the system has none). Without TLS, an encrypted room's content stays private, but the broker
  password travels in clear.

### Limits

- An update is at most 16 MiB (protocol 2, section 7); MQTT packets up to 17 MiB. The broker must
  allow that size for big snapshots.
- A room is at most 16 per transport process; peers per room are listed up to 1,000.
- Messages are kept in a queue of 4,096 while the connection is down; beyond that, the app
  re-syncs when the room is back online.
- Concurrent edits converge, but intent can't always be kept (two people typing in one cell: the
  later one stays). See protocol 2, section 7.
- Only MQTT 3.1.1 over TCP or TLS; no WebSocket transport (the web apps use the y-websocket
  binding natively).

## Live (Loopback): craft-collab-loopback

For development and the apps' end-to-end tests. **Share Live (Loopback)** takes a room name (for
example `dev-1`); **Join Live (Loopback)** in another app process on the same computer joins it by
name (or by the invitation `craft+loopback:dev-1#k=…` of an encrypted room). The first instance
starts a tiny relay on `127.0.0.1:39390` (`CRAFT_COLLAB_LOOPBACK_PORT`) inside its own process;
when that process exits, the others reconnect and one of them takes over. `craft-collab-loopback
--relay` runs a relay alone. Any local program can connect to the relay, and nothing is kept once
it stops: don't use it for anything else.

Programmatic use (CLI, MCP, tests): `plugins.run {plugin: "org.craft.collab.loopback", command:
"share", params: {room: "e2e-1", name: "A"}}` in one app and `{command: "join", params: {room:
"e2e-1", name: "B"}}` in another.

## Installing

**craft-mqtt:** in any of the three apps, open **File › Options › Add-ins**, type
`sirmmo/craft-collab` in **Install from GitHub** and confirm. The app downloads the release zip for
your computer (`craft-mqtt-0.1.0-<platform>.zip`: Linux x86_64 static musl, macOS Apple silicon and
Intel, Windows x64) and loads it. A **Live** tab appears.

**The loopback transport and the conformance kit** are in each release's
`craft-collab-tools-0.1.0-<platform>.tar.gz` (not a zip, so the apps' installer leaves it alone):
load its `craft-collab-loopback` folder with Automate › Add-ins › Load, `plugins.load {"path": …}`
or `--plugin DIR`, and run `craft-collab-conformance` from the command line.

From a build of your own, load the folder holding a plugin's `plugin.json` (each crate has one;
the binary goes next to it).

## Building and testing

```sh
cargo build --release
./target/release/craft-mqtt --manifest > plugin.json        # the manifest, next to the binary
cargo test                                                  # all of it, with an in-process MQTT broker
cargo run -p craft-collab-conformance -- transcripts        # replay conformance/*.jsonl
cargo run -p craft-collab-conformance -- transport --encrypted \
    --share '{"broker": "mqtt://127.0.0.1:1883"}' -- ./target/debug/craft-mqtt
```

MQTT uses rumqttc with rustls and the ring provider (no OpenSSL), so the Linux release is a static
musl binary. The tests run craft-mqtt against rumqttd (in the test process): the conformance kit
on plain and encrypted rooms, presence and the will, the retained snapshot as the broker stores it
(ciphertext), reconnecting through a proxy that drops the connection, re-joining after a restart.
`CRAFT_COLLAB_SNAPSHOT_EVERY` and `CRAFT_COLLAB_SNAPSHOT_SECS` change when snapshots are stored
(tests and debugging).

Code rules (as in the craft apps): no `unwrap`, `expect`, `panic!` or unchecked indexing outside
tests, no `unsafe`; input from the app or the network is hostile (sizes capped, bad messages
logged and dropped); a bug in a command costs that request, never the plugin.

## Licence

MIT OR Apache-2.0, at your option.
