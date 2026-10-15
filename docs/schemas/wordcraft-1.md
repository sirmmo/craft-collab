# WordCraft schema 1: the shared document

The Yjs document a WordCraft room shares (craft plugin protocol 2, [section 5](../protocol-2.md#5-the-shared-documents)).
`collab/joined` says `app: "wordcraft"`, `schema: 1`. This page is what any peer (another WordCraft,
a web build, a bot, the conformance kit) needs to read and write a document's text, formatting,
tables and stories. WordCraft's own `docs/collab.md` is normative for everything this page leaves
to it (the JSON of each property, styles, numbering, settings); where the two disagree, fix one of
them.

The reference implementation is `crates/conformance/src/wordcraft.rs` (the body's paragraphs,
runs and tables: what the transcripts need). The transcripts in
[`conformance/`](../../conformance) named `wordcraft-1-*` are written with it and check an app
against it.

## Roots

| Root | Yjs type | Holds |
|---|---|---|
| `meta` | Map | `app` = `"wordcraft"` (string), `schema` = `1` (number), `title` (string) |
| `body` | XmlFragment | the main story: block elements (below) |
| `headers`, `footers`, `notes`, `textBoxes` | Map | story id → a story map (below): headers; footers; footnotes and endnotes; the text of text boxes and shapes |
| `comments` | Map | comment id → a comment map (below), its text included |
| `styles` | Map | style id → the style (JSON object); the key `@defaults` → the document defaults (character and paragraph formatting every style starts from) |
| `styleOrder` | Array | style ids, in the order the document lists its styles |
| `numbering` | Map | `a<id>` → an abstract list definition, `n<id>` → a list (an instance of a definition, with its overrides); JSON objects |
| `settings` | Map | document-wide properties, one key each (below) |
| `media` | Map | media key → the bytes (Uint8Array) of a picture or embedded object the text refers to |
| `files` | Map | package parts kept verbatim (custom XML, macros), part name → bytes |

A peer whose `meta.app` isn't `wordcraft` or whose `meta.schema` isn't `1` doesn't sync: the app
drops such an update and says "This room is for <app> <schema>". A document without `meta` yet (a
newcomer before the first state) is empty. Unknown roots, keys, elements and attributes are kept
and ignored, so later versions can add some without a new schema number.

JSON values are stored as Yjs values (maps, arrays, strings, numbers, booleans, null), not as
JSON text. Numbers are JavaScript numbers; a reader converts integral ones back to integers.

## Blocks

A story (`body`, and a story map's or comment map's `content`) is an XmlFragment whose children
are blocks, in reading order:

### Paragraph: `<p>`

| Attribute | Value |
|---|---|
| `pPr.<key>` | one paragraph property: `<key>` is a key of WordCraft's paragraph properties (`style`, `align`, `indentLeft`, `spaceBefore`, `numId`, `level`, …), its value as JSON. `pPr.style` is the paragraph style's id (`Heading1`). A missing attribute: not set (inherited from the style) |
| `rPr` | the paragraph mark's formatting (an object of character properties, as for text below), when it has any |
| `sectPr` | the section's properties (an object), when the paragraph ends a section |

Its one child is an **XmlText**: the paragraph's text. A reader concatenates every XmlText child,
in order, and skips other children. An empty paragraph has an empty XmlText.

- **Characters** are what was typed, with `\t` (tab), `\n` (line break), U+000C (page break),
  U+000E (column break), U+00AD (optional hyphen), U+2011 (non-breaking hyphen). Never U+FFFC:
  a reader drops it.
- **Formatting** is Yjs text attributes: the keys of WordCraft's character properties with JSON
  values: `bold`, `italic`, `underline`, `strike`, `size` (points), `font`, `color`, `highlight`,
  `style` (a character style's id), `lang`, `vertAlign`, … A missing attribute (or `null`) is not
  set. Formatting a range sets or clears attributes on it (Yjs `format`), so one person's bold
  and another's italic on overlapping text both stay, and typing keeps merging character by
  character.
- **Inline objects** (pictures, charts, fields, note and comment references, bookmarks,
  equations, shapes, content-control marks) are **embeds**: the object as JSON (WordCraft's inline
  object, tagged by `type`), one embed per object, with formatting attributes like any character.
  A picture refers to its bytes by key in `media`; a note reference to its story id in `notes`; a
  comment anchor to its comment id in `comments`; a shape with text to its story in `textBoxes`.

### Table: `<table>`

| Element | Attributes | Children |
|---|---|---|
| `<table>` | `tblPr` (object: style, width, borders, …), `grid` (array of column widths, points), `controls` | `<tr>` elements |
| `<tr>` | `trPr` (object: height, header row, …), `controls` | `<tc>` elements |
| `<tc>` | `tcPr` (object: span, vertical merge, shading, …), `controls` | blocks (paragraphs, nested tables) |

`controls` (any of the three) holds content controls around the table, row or cell. A cell with
no paragraph, or ending with a table, reads as if an empty paragraph followed.

### Identity

Paragraphs, tables, rows and cells are identified by their Yjs elements: an element's item id never
changes, and the order of a story is the order of its fragment. Inserting a paragraph doesn't
shift anyone's references, and two people typing in one paragraph merge character by character.
There is no `id` attribute.

## Stories other than the body

A **story map** (a value in `headers`, `footers`, `notes` or `textBoxes`) is a Map:

| Key | Holds |
|---|---|
| `kind` | `"header"`, `"footer"`, `"footnote"`, `"endnote"` or `"textBox"` |
| `content` | XmlFragment: the story's blocks |
| `body` | text boxes only, when not the default: text direction, vertical alignment, margins, the box the text continues in (an object) |

A **comment map** (a value in `comments`) holds the comment's fields, each its own key (so one
person resolving a comment while another edits its author survives): `story` (number: the id the
comment's text has as a story), `content` (XmlFragment: the comment's text), `author`, `initials`,
`date` (ISO 8601), `parent` (number: the comment replied to; missing for a top-level comment),
`resolved` (boolean), `durableId`, `dateUtc` (strings, when known).

**Ids** of stories and comments are integers from 0 to 2³² − 1, written in decimal as map keys.
They're unique across `headers`, `footers`, `notes`, `textBoxes` and the comments' `story`. Ids
below 65,536 are the ones the document had when it was shared. A peer that creates a story or a
comment picks an id nobody else will: WordCraft uses the block of 65,536 ids starting at
65,536 × (1 + clientId mod 32,767), its Yjs client id at the time. Section properties refer to
headers and footers by story id; inline objects to notes, comments and text boxes.

## Document-wide values

`settings` has one key per document property, each a JSON value written whole (last writer wins):

| Key | Holds |
|---|---|
| `settings` | document settings (tracking, protection, compatibility, zoom, grids, theme fonts and colours) |
| `lastSection` | the last section's properties (page size, margins, columns, headers and footers by story id) |
| `core` | title, author, subject, keywords, created and modified dates |
| `sources` | bibliography sources |
| `customProps` | custom document properties |
| `revisions` | the tracked changes' authors and dates, by number (runs and paragraphs refer to them) |
| `footnoteSeparators`, `endnoteSeparators` | the document's own note separators |
| `ribbon` | the document's own ribbon customisation |

Styles (`styles`), list definitions and lists (`numbering`) are written one entry each, whole.
These are values where finer merging doesn't matter: two people rarely restyle the same style at
once, and the later one wins. The exception worth knowing: `revisions` is a list; two people
tracking changes at the same moment can see one of their new revisions labelled with the other's
name until the next change. WordCraft's `docs/collab.md` lists every key's JSON.

## Edits

| Edit | Transaction |
|---|---|
| Typing, deleting | insert or delete characters in the paragraph's XmlText (with the typed text's formatting as attributes) |
| Formatting text | `format` the range: set the attributes that changed, `null` the ones cleared |
| Paragraph formatting | set or remove `pPr.<key>` attributes of the `<p>` |
| Enter (split a paragraph at an offset) | delete the text after the offset, and insert a new `<p>` after it with the same attributes and the deleted text (formatting and embeds included) |
| Joining two paragraphs (Backspace at the start) | append the second's text (with its formatting) to the first, then remove the second |
| Inserting or deleting paragraphs and tables | insert or remove elements of the fragment |
| Editing a table | the cells' blocks as above; rows and cells inserted or removed as elements |
| Adding a header, footer, note or text box | a new story map; the reference to it (section properties, an inline object) in the same transaction |
| Adding a comment | a new comment map and its anchors in the text, in the same transaction |

Each committed command is one transaction and one `collab/update` (protocol 2, section 6).
Selection and other view state never enter the document.

## Awareness

Each peer's awareness state is `{user: {id, name, color}, selection?}`:

- `selection`: `{story, anchor, focus}`. `story` is `"body"` or a story id in decimal (`"65537"`).
  `anchor` and `focus` are Yjs relative positions (`Y.encodeRelativePosition`, `yrs`
  `StickyIndex::encode_v1`), base64, each relative to the XmlText of the paragraph it's in (the
  index counts characters and embeds). `focus` is where the caret is; `anchor == focus` is a
  caret.

A position whose paragraph is gone resolves where Yjs puts it (the nearest surviving text), or
isn't shown.

## The model the transcripts compare

The transcripts describe a document by what a person sees: the title, and the body's blocks:

```json
{"title": "Minutes", "body": [
  {"text": "Minutes", "props": {"style": "Heading1"}, "runs": [{"text": "Minutes"}]},
  {"text": "Present: Ada, Bo", "runs": [{"text": "Present: ", "bold": true}, {"text": "Ada, Bo"}]},
  {"table": [[[{"text": "Item", "runs": [{"text": "Item"}]}], [{"text": "", "runs": []}]]]}
]}
```

- A paragraph: `text` (its characters, an embed as U+FFFC), `runs` (its text split where the
  formatting changes: each run's `text` and its attributes, an embed's run also `embed`: the
  object; adjacent runs with the same attributes are one; an empty paragraph has none), `props`
  (its `pPr.*` attributes without the prefix; left out when there are none).
- A table: `table`, rows of cells of blocks.
- Integral numbers are written as integers.
