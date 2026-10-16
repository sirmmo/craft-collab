# Craft plugin protocol 2.2: documents carry an id; multi-line fields; literal text; functions that know their workbook

Version 2.2, 2026-10-16. Shared by GridCraft, WordCraft and DeckCraft. It adds to protocols 1,
1.1 and 1.2 (each app's `docs/plugins.md`), 2.0 ([protocol-2.md](protocol-2.md)), 2.1 (the
document chat, [chat.md](chat.md)) and manifests 1.3
([plugin-requirements.md](plugin-requirements.md)), and changes nothing in them.

It closes the gaps an add-in that runs code from documents (R, Python) runs into: it can tell
documents apart and decide per document what it may do (trust), show code in a dialog, and write
text that the app takes as it is. GridCraft's reference implementation is its commit "Gaps #21:
plugin functions know their workbook" (`docs/plugins.md`, "Protocol 2.2").

Contents: [1 Version and features](#1-version-and-features) ·
[2 Which app has what](#2-which-app-has-what) · [3 The document id](#3-the-document-id) ·
[4 Multi-line fields](#4-multi-line-fields-multiline) · [5 Literal text](#5-literal-text-literalvalues) ·
[6 Formula functions](#6-formula-functions-gridcraft) · [7 Limits](#7-limits)

## 1. Version and features

An app that implements this version sends `host.protocol` `"2.2"` in `initialize`. Each addition
has a name in `host.features`, and an app lists **exactly** the additions it implements: a name it
doesn't implement is absent, never present and inert. A plugin checks for the name and keeps
working without it on older hosts, or a manifest asks for it (`"requires": {"features":
["multiline"]}`, manifests 1.3); `"requires": {"protocol": ">=2.2"}` asks for the version only,
which doesn't promise the app-specific features.

| Feature | Adds | Section |
|---|---|---|
| (none) | `id` in `context.document` and in the document events | [3](#3-the-document-id) |
| `multiline` | Multi-line text fields in form dialogs, for code too | [4](#4-multi-line-fields-multiline) |
| `literalValues` | Text written exactly as given, never read as a number, date, formula or AutoFormat trigger | [5](#5-literal-text-literalvalues) |
| `functionDocument` | `functions/call` carries `document`: the calling document | [6.1](#61-the-calling-document-functiondocument) |
| `openedBeforeCalc` | `events/documentOpened` arrives before an opened document's first calculation | [6.2](#62-documentopened-before-the-first-calculation-openedbeforecalc) |
| `emptyResult` | `{"empty": true}`, a function result that shows blank | [6.3](#63-blank-results-emptyresult) |
| `invalidate` | `host/functions/invalidate`: kept function results are forgotten and recalculated | [6.4](#64-invalidation-invalidate) |

The document `id` has no feature name: every 2.2 app sends it (`host.protocol` `"2.2"` is the
promise).

## 2. Which app has what

Formula functions (manifest `functions`) are GridCraft's alone, so the four function features
concern GridCraft only. WordCraft's table formulas and fields (`=SUM(ABOVE)`, `DATE`) and
DeckCraft's text never call plugin functions.

| | GridCraft | WordCraft | DeckCraft |
|---|---|---|---|
| `host.protocol` `"2.2"` | yes | yes | yes |
| `id` in `context.document` and events | yes | yes | yes |
| `multiline` | yes | yes | yes, where it draws form dialogs |
| `literalValues` | `range.setValues {literal: true}` | `text.insert {literal: true}` | its text-insert command with `literal: true`, if it has one; else absent |
| `functionDocument` | yes | absent (no plugin functions) | absent (no plugin functions) |
| `openedBeforeCalc` | yes | absent: moot, there is no calculation that calls a plugin | absent: moot |
| `emptyResult` | yes | absent (no function results) | absent (no function results) |
| `invalidate` | yes | absent: `host/functions/invalidate` is refused as an unknown host method | absent, likewise |

In WordCraft and DeckCraft `documentOpened` keeps its protocol 1.2 behaviour: it's sent after the
document opened, from the events thread, and never holds the open up. A plugin that wants to key
trust on a document there uses the `id` of section 3, which is the same in the event and in every
later `context.document`.

## 3. The document id

`context.document` (`commands/run`, `macros/run`) and the params of `events/documentOpened`,
`events/documentSaved` and `events/documentClosed` gain `id`:

```json
"document": {"path": "/home/me/Report.docx", "name": "Report.docx", "dirty": false, "id": "doc-3"}
```

```json
{"jsonrpc":"2.0","id":4,"method":"events/documentSaved","params":{"path":"/home/me/Report.docx","name":"Report.docx","format":"docx","id":"doc-3"}}
```

- `id` is an **opaque** string of at most 64 characters. A plugin compares it for equality and
  never parses it (the apps happen to write `doc-<n>`).
- It names one open document **for as long as the app runs**: the same before and after a save
  and a Save As, with or without a file, edited or not.
- A document that **replaces** another in the same window (File › New and File › Open in an app
  whose windows hold one document, a mail merge's result, a recovered document) is another
  document and gets a new id. `documentClosed` for the one replaced carries the old id.
- An id is **never reused** for another document while the app runs, in any of its windows. It
  isn't saved in the file and doesn't survive a restart: a plugin keys lasting trust on `path`,
  and on `id` only for the session (a document with no file, or "trust until I close it").
- `functions/call`'s `document` (GridCraft, section 6.1) carries the same id, so a command
  ("Trust This Workbook"), the events and the formulas agree on which document is meant.

## 4. Multi-line fields (`multiline`)

A form dialog's `text` field (protocol 1.2) gains three keys:

```json
{"id": "code", "label": "R code", "kind": "text", "value": "m <- lm(y ~ x)\nsummary(m)", "multiline": true, "code": true, "rows": 10}
```

| Key | Meaning |
|---|---|
| `multiline` | `true`: a multi-line editor. Enter starts a new line (it doesn't press OK); it scrolls past its height |
| `code` | `true`: monospace, Tab indents (it doesn't move to the next field). Implies `multiline` |
| `rows` | Its height in lines: 2 to 30, default 8. Outside the range it's clamped; a fraction is rounded |

- Its value may hold up to **200,000 characters** (a script of a few thousand lines); longer
  values are cut at a character boundary. A one-line field keeps the app's own limit (GridCraft
  32,767, WordCraft 100,000).
- The dialog gets wider for it.
- The answer carries the text as typed, with `\n` between lines.
- On other field kinds (`secret`, `number`, `checkbox`, `choice`) the keys are ignored: a password
  box stays on one line. `multiline` or `code` that isn't a boolean, or `rows` that isn't a
  number, makes the dialog malformed (the command fails, as for any malformed dialog).
- An app without the feature ignores the keys and shows a one-line field.

## 5. Literal text (`literalValues`)

A plugin often writes text it got from elsewhere (a result, a cell of a data frame, a line of
code) and wants it kept exactly. With `literal: true` on the app's value-writing command, every
string is stored as text exactly as given:

- **GridCraft**, `range.setValues {range, values, literal: true}`: a string is never read as a
  number, a date, a percentage, a logical or a formula, and a leading `'` stays part of the text.
  An empty string clears the cell; numbers, logicals and `null` are written as without it. Text is
  cut at 32,767 characters.
- **WordCraft**, `text.insert {text, literal: true}`: the text goes in as given. Nothing typed is
  turned into something else: no AutoCorrect, no smart quotes, no AutoFormat as you type (lists
  from `1. ` or `* `, dashes from `--`), and paragraph breaks in the text (`\r`)
  separate paragraphs as pasted text does, not as Enter presses (an empty list item doesn't end the list).
  `raw: true`, which WordCraft already had, skips the automatic changes the same way.
- **DeckCraft**, its text-insert command with `literal: true` and the same meaning as
  WordCraft's, if it has one. An app with no such command doesn't list the feature.

This replaces the leading-apostrophe trick, which leaves the apostrophe in other hosts. Without
`literal`, the commands behave as before.

## 6. Formula functions (GridCraft)

This section applies to apps whose formulas call plugin functions: today GridCraft only. Another
app that gains plugin functions implements it before listing the features.

### 6.1 The calling document (`functionDocument`)

```json
{"jsonrpc":"2.0","id":7,"method":"functions/call","params":{"name":"R.EVAL","args":["x*2",3],
  "document":{"path":"/home/me/Model.xlsx","name":"Model.xlsx","id":"doc-3"}}}
```

`path` is the local file the document was opened from or last saved to (absolute), or `null`
for one without (new, imported, a sample); `name` is what the title bar shows; `id` is section
3's. `document` is `null` in the rare evaluations that belong to no document. A kept
(non-volatile) result is kept per document and per file: the same arguments in another document,
or after Save As, call the plugin again.

### 6.2 `documentOpened` before the first calculation (`openedBeforeCalc`)

When a document is opened from a local file, the app reads it, sends `events/documentOpened
{path, name, id}` to the plugins that declared it, waits for each one's answer (at most 10 s
each), and only then calculates. Guaranteed: no formula of a document opened from a file calls a
plugin function before every plugin that declared `documentOpened` answered or timed out, and
those calls carry the event's `id`. Events queued before it are delivered first. Not covered: a
plugin whose own command opened the file is busy and gets the event afterwards; a plugin that
takes longer than 10 s (or fails) doesn't hold the document up and its timeout counts as a
failure; documents not opened from a file get no event. The plugin must not wait for the person
while it answers.

### 6.3 Blank results (`emptyResult`)

A function result `null` is an empty value, which the cell shows as 0 (as Excel shows a VBA
function that returns `Empty`). `{"empty": true}`, as the result or as values in an array result,
shows **blank** when the formula is the call itself; other formulas read the cell as empty. In an
array result that holds `{"empty": true}`, the `null`s still show 0. Wrapped in another function
the blank is an empty value again.

### 6.4 Invalidation (`invalidate`)

```json
{"jsonrpc":"2.0","id":"p1","method":"host/functions/invalidate","params":{"names":["R.EVAL"]}}
{"jsonrpc":"2.0","id":"p1","result":{"functions":["R.EVAL"]}}
```

The app forgets the kept results of the named functions (all the plugin's own without `names`;
other plugins' names are ignored; at most 1,000) and recalculates the open documents whose
formulas call them. Allowed on the push channel, while the plugin runs a command or a macro, and
during an event. The recalculation never happens while the plugin is busy answering: after the
command's step (outside its undo step), or in the app's next turn. A document without unsaved
changes stays so. The answer lists the names invalidated.

## 7. Limits

| What | Limit |
|---|---|
| `id` | at most 64 characters, opaque |
| Multi-line field value | 200,000 characters |
| `rows` | 2 to 30 (default 8) |
| `documentOpened` before calculation | 10 s per plugin |
| `host/functions/invalidate` names | 1,000 |

Every value a plugin sends stays hostile input: the apps bound and check it as in protocol 1.2,
and a malformed message fails the command or is refused, never crashes the app.

## Revision history

| Date | Version | Change |
|---|---|---|
| 2026-10-16 | 2.2 | Created from GridCraft's implementation (found by the R add-in): the document `id` in `context.document` and the document events; `multiline` form fields; `literalValues` (GridCraft `range.setValues`, WordCraft `text.insert`); GridCraft's `functionDocument`, `openedBeforeCalc`, `emptyResult`, `invalidate`; which app has what |
