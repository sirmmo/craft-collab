# Document chat

Version 1.0 (protocol 2.1), 2026-10-16. The same in GridCraft, WordCraft and DeckCraft. The
people editing a document can talk in a chat panel beside it. The chat belongs to the document:
it's shared live through the room (protocol 2), saved inside the file, and stays there until
someone removes it explicitly. It works outside a live session too, as notes left in the file.

## In the shared document

A root array `chat` (Yjs `Y.Array`) holds the messages in order, oldest first. Each message is a
`Y.Map`:

| Key | Value |
|---|---|
| `id` | 8 random lower-case base36 characters, unique in the chat |
| `author` | JSON text `{id, name, color}`: the `user` of the room's join (outside a session, the app's own user) |
| `at` | the sender's clock when sent: milliseconds since 1970 UTC, an integer |
| `text` | the message: plain text, 1–4,000 characters (UTF-16 units), newlines allowed |
| `replyTo` | optional: the `id` of the message it answers |
| `edited` | optional: `at` of the last edit (only the author edits; the app shows "edited") |
| `deleted` | optional: `true` when the author deleted it. `text` is then removed; the entry stays so replies still point somewhere ("Message deleted") |

A root map `chatMeta` holds `clearedAt` (integer, optional): when someone last cleared the chat.

Rules:

- Messages are only appended (`push`); readers order them by their position in the array (Yjs
  keeps concurrent appends in the same order on every peer), not by `at`, which comes from
  each sender's clock.
- **Clearing** (`chat.clear`) deletes every entry of `chat` in one transaction and sets
  `chatMeta.clearedAt`. It's the only way messages leave the document besides an author deleting
  their own. Nothing is trimmed automatically.
- Limits: at most 10,000 messages; at the limit the app refuses new ones and says "The chat is
  full: clear it to go on" (a deleted message still counts; clearing frees the chat). A reader
  ignores entries that aren't maps, entries with a malformed `id` (not 8 characters of
  `0-9a-z`), `author` (not JSON text of an object with non-empty `id` and `name` and a string
  `color`), `at` (not an integer from 0 to 2^53) or `text` (missing or outside 1–4,000 units,
  unless `deleted`), and duplicate `id`s after the first; it keeps the first 10,000 messages
  left. A malformed optional key (`replyTo`, `edited`) is ignored, not the message.
- Chat changes are not part of document undo: Undo never removes a message, and sending a
  message doesn't make an undo step. They do mark the document modified (it has to be saved for
  the chat to be kept).
- In an encrypted room the chat travels encrypted like every other update.

Presence: the awareness state may carry `typing: true` while the person is typing in the chat
(and `false`, or no key, once they stop or send); the app shows "Name is typing…".

## In the file

The chat is saved with the document, so it outlives the live session:

- **OOXML** (`.xlsx`/`.xlsm`, `.docx`/`.docm`, `.pptx`/`.pptm` and the template kinds): a part
  `/craft/chat.json`, content type `application/vnd.craft.chat+json`, related from the package
  root `_rels/.rels` with relationship type `urn:craft:relationships:chat`. Its content:
  `{"version": 1, "clearedAt"?: n, "messages": [{id, author: {id, name, color}, at, text?, replyTo?, edited?, deleted?}]}`.
  An empty chat writes no part. Reading applies the same validation and limits as above; a malformed part
  is ignored with a warning, never an error that stops the file from opening.
- **The apps' native formats** store the same JSON in their own way.
- Microsoft Office doesn't know this part and drops it when it saves the file; the chat then
  lives on only in copies saved by the craft apps. The apps say so in the chat panel's menu
  ("Kept in the file; Office drops it when it saves").
- **Sharing** (`collab.share`) seeds `chat` from the file's chat; **joining** brings the room's chat
  into the document, and saving writes it into the file.

## Commands

Everything is a command:

- `chat.list {since?, limit?}` → `{messages: [...], clearedAt?, count}` (newest last): the
  messages after the one whose `id` is `since` (all of them without it, or when it's gone), or,
  when `since` is a number, the messages sent after that time (milliseconds, their `at`); the
  newest `limit` of those; `count` is the number of messages in the chat.
- `chat.send {text, replyTo?}` → `{id}`.
- `chat.edit {id, text}`: the author only (the author's `id` is compared), not a deleted message.
- `chat.delete {id}`: the author only; leaves the "Message deleted" entry.
- Refused (an error, nothing sent): empty or too long text, a `replyTo` or `id` that isn't in
  the chat, someone else's message, a full chat.
- `chat.clear` → `{removed}`: removes every message. The UI asks first ("Remove the whole chat
  from the document for everyone?"); programmatic calls don't ask.
- `chat.typing {typing: bool}`: sets this person's awareness `typing` flag (the panel's box does
  it while someone writes; sending a message clears it); nothing outside a session.
- `chat.export {format?: "text"|"json"}` → the chat as text or JSON, for copying elsewhere.
- `view.chat` (UI): shows or hides the panel.

## The panel

A side panel (where the app shows its task panes), opened from the title bar's Share area (a
chat button with an unread count beside the people circles) and from View. Each message shows
the author's circle and colour, name, time (local, relative for today), the text (selectable;
links clickable; `@Name` highlighted when it names someone in the room), replies quoted, and
"edited"/"Message deleted" markers. A box at the bottom sends with Enter (Shift+Enter for a new
line). The menu has Export, Clear chat… and the Office note. New messages from others while the
panel is closed raise the unread count; a mention of you also flashes the button.

## Conformance

`craft-collab` has chat transcripts for each app's schema (`<app>-1-chat` and
`<app>-1-chat-share`, described in its [conformance.md](conformance.md)): the room's chat
arriving on join, sending, concurrent sends converging in the same order, replies, edit and
delete by the author only, text limits, malformed entries ignored, typing, clearing, seeding the
room from the document's chat on share, and an encrypted room. The 10,000-message limit and the
file part's JSON (`Chat::to_part`/`Chat::from_json` in `crates/conformance/src/chat.rs`) are
checked by unit tests there, since a chat at the limit is over a megabyte of updates; each app
tests its own limit and its file round trip.

## Revision history

| Date | Version | Change |
|---|---|---|
| 2026-10-16 | 1.0 | From the first implementations (DeckCraft, GridCraft): `chat.typing {typing}` sets the awareness `typing` flag; `chat.list`'s `since` may be a time in milliseconds as well as a message id |
| 2026-10-16 | 1.0 | Wording made precise with the reference implementation (no change of intent): what makes an entry malformed (the `id` format too), the first 10,000 valid messages kept, a deleted message still counting toward the limit, `chat.list`'s `since` as a message id, what the commands refuse, `typing: false` |
| 2026-10-16 | 1.0 | Created |
