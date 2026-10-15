# DeckCraft schema 1: the shared presentation

The Yjs document a DeckCraft room shares (craft plugin protocol 2, [section 5](../protocol-2.md#5-the-shared-documents)).
`collab/joined` says `app: "deckcraft"`, `schema: 1`. This page is normative: it is what any peer
(another DeckCraft, a web build, a bot, the conformance kit) needs to read and write slides,
shapes, text and notes. DeckCraft's own `docs/collab.md` describes how the app behaves around it;
where the two disagree, fix one of them.

The reference implementation is `crates/conformance/src/deckcraft.rs` (reader and writer of what the
transcripts use) and DeckCraft's `crates/collab` (all of it). The `deckcraft-1-*` transcripts in
[`conformance/`](../../conformance) check an app against it.

## Roots

| Root | Yjs type | Holds |
|---|---|---|
| `meta` | Map | `app` = `"deckcraft"`, `schema` = `1` (a number), `title` (string) |
| `presentation` | Map | presentation-wide settings, one key per setting (below) |
| `slides` | Array | slide ids (strings), in slide order |
| `slide:<slideId>` | Map | one slide (below), for each id in `slides` |
| `masters` | Map | master id → JSON text of a slide master |
| `layouts` | Map | layout id → JSON text of a slide layout |
| `theme` | Map | master id → JSON text of that master's theme |
| `media` | Map | media id → Map `{name, contentType, data, link?}` |

A peer whose `meta.app` isn't `deckcraft` or whose `meta.schema` isn't `1` doesn't sync: the app
drops such an update and says "This room is for <app> <schema>". A document without `meta` yet
(a newcomer before the first state) is empty.

**Values are JSON text.** Every property below that isn't a Yjs type is a Yjs string holding the
JSON of DeckCraft's model field (camelCase names, points for lengths, as DeckCraft's `.deckcraft`
format writes them). Each property is its own map entry, so concurrent changes to different
properties of one object both survive and concurrent changes to the same property keep the later
write (Yjs map semantics). A key that is missing means the field's default. Unknown keys are kept
and ignored, so later versions can add some without a new schema number; a value a reader can't
take is skipped (the field keeps its default), never fatal.

## Ids

- Slides, shapes, layouts, masters and media have **ids that never change**: 8 characters of
  `0-9a-z`, the base-36 form, zero-padded, of a number from 1 to 2^32 − 1 (`0000002s` is 100,
  `01z141z3` is 4,294,967,295). The number is the object's id in DeckCraft's model, so references
  *inside* JSON values (a connector's ends, an animation's or a comment's shape, a hyperlink to a
  slide, a picture's media, a slide's `layout`) are those plain numbers and mean the same on every
  peer.
- **New ids are random**: a writer picks a random block (DeckCraft: a random start from 2^24 to
  2^31 with the next 65,536 numbers unused, renewed after 32,768) and takes ids from it in turn,
  so two people adding slides or shapes at the same time don't pick the same one. Ids already in a
  document (from a file opened before sharing) may be small.
- An id that isn't such a string (another implementation's, `k3v9x0qa` is beyond 32 bits) is
  skipped by readers.
- **Duplicates**: concurrent edits can leave an id twice in `slides`, `shapes` or `children`.
  Readers keep the first occurrence and skip the others (and anything that isn't a string).
- Root maps can't be deleted: a deleted slide's id leaves `slides`, and its `slide:<id>` map stays
  (it's what an undo brings back). A writer that adds an id again first clears that map.

## The presentation

`presentation` holds one key per presentation-wide field of the model: `slideSize`, `notesSize`,
`sections`, `customShows`, `show`, `headerFooter`, `props`, `defaultTextStyle`,
`firstSlideNumber`, `embeddedFonts`, `tableStyles`, `tags`, `keptParts`, `guides`, `rawExt`,
`vbaProject`, `vbaParts`, `ribbon`, `notesMaster`, `handoutMaster`; and `masters`, the JSON array
of master ids in order.

**Masters, layouts and the theme** are JSON values merged as wholes (last writer wins per
master, per layout, per theme): they change rarely and together, and merging their insides gains
nothing. A master's JSON is the model's master without `id` and `theme`, with `layouts` the array
of its layout ids in order; a layout's is the model's layout without `id`. A room without masters
(a peer that shares only slides) is read with the reader's own default master; slides whose
`layout` isn't in the room use the first master's first layout.

**Media** are immutable: an id's bytes (`data`, a Yjs binary) never change, so adding a picture
adds an entry and removing the last use may remove it.

## A slide: `slide:<slideId>`

| Key | Yjs type | Holds |
|---|---|---|
| `shapes` | Array | the ids of the slide's top-level shapes, back to front |
| `shape:<shapeId>` | Map | one shape (below), for each shape of the slide, groups' children included |
| `notes` | Text | the speaker notes ([text](#text)) |
| `notesBody` | JSON | the notes' body properties: `{body, listStyle}` |
| `layout` | JSON | the layout id (a number) |
| `name`, `background`, `hidden`, `showMasterShapes`, `transition`, `animations`, `comments`, `colorMap`, `tags`, `rawExt` | JSON | the slide's other fields |

## A shape: `shape:<shapeId>`

| Key | Yjs type | Holds |
|---|---|---|
| `xfrm` | JSON | its box `{x, y, w, h, rot?, flipH?, flipV?}` in points; missing: a placeholder taking its box from the layout |
| `kind` | JSON | `{"type": "shape"}`, `picture`, `connector`, `table`, `chart`, `media`, `ink`, `model3d`, `opaque` with their data; a group is `{"type": "group", "child": {…}}` (its children are in `children`) |
| `children` | Array | a group's shape ids, back to front (each has its own `shape:<id>` in the slide) |
| `text` | Text | its text ([text](#text)); missing: the shape has no text body |
| `textBody` | JSON | the text's body properties: `{body, listStyle}` |
| `geom`, `name`, `ph`, `fill`, `line`, `effects`, `style`, `hidden`, `locked`, `textBox`, `descr`, `title`, `decorative`, `click`, `hover`, `tags`, `rawExt`, `diagram` | JSON | the shape's other fields |

So moving a shape (`xfrm`) while someone else recolours it (`fill`) keeps both; two people moving
the same shape keep the later move. A table's cells and a chart's data are inside `kind`, merged as
a whole.

## Text

A text (a shape's `text`, a slide's `notes`) is a Yjs Text, offsets in UTF-16 code units:

- each paragraph's characters, then `\n` (U+000A), which ends it: every paragraph, the last one
  included, has its `\n`. Characters after the last `\n` (a peer that wrote one without) make a
  last paragraph with no properties. An empty text has no paragraphs.
- a line break inside a paragraph is U+000B.
- **formatting is Yjs formatting attributes**, one per property, with native values: a
  character's attributes are its run's properties as DeckCraft's model names them (`bold: true`,
  `italic`, `size: 24`, `font: "Arial"`, `underline: "sng"`, `fill: {…}`, `link: {…}`…, only those
  set); a field has `field` (`slidenum`, `datetime1`…) with its last value as its characters; an
  equation has `math` (its OMML) with its linear form as its characters.
- the `\n` carries the paragraph: `level` (0–8, missing when 0), its paragraph properties as
  `p:<name>` (`p:align: "ctr"`, `p:spaceBefore: {…}`), its default run properties as `defRPr`
  (a map), and the end-of-paragraph mark's run properties as plain run attributes. Paragraph
  attributes on other characters, and run attributes readers don't know, are ignored.

Writers edit texts the way Yjs editors do: typing inserts characters (with all their attributes
set, so they don't inherit others'), deleting removes them, formatting sets attributes on the
range (`null` removes one). So two people typing in one text box both keep what they typed, and
two people formatting different properties of the same words both keep their change.

## Edits

| Edit | Transaction |
|---|---|
| Add a slide | its `slide:<id>` map (`shapes`, `notes`, properties), then its id inserted into `slides` |
| Delete a slide | its id removed from `slides` (every occurrence) |
| Move a slide | its id removed and inserted at the new place |
| Add a shape | its `shape:<id>` map in the slide, its id appended to `shapes` (or a group's `children`) |
| Delete a shape | its `shape:<id>` removed from the slide map, its id from `shapes` |
| Change z-order, group, ungroup | the ids move between and within `shapes` and `children` |
| Move, resize, recolour… | the property's key set to the new JSON |
| Type, delete, format text | Yjs text inserts, deletes and formats |

Each committed command is one transaction and one `collab/update` (protocol 2, section 6). Selection
and other view state never enter the document. Reading the document gives back what was written:
a peer applying a remote change reads the slides it touched again.

## Awareness

Each peer's awareness state is `{user: {id, name, color}, selection?}`:

- `selection`: `{slide: <slideId>, shapes: [<shapeId>…], text?}` — the slide the person is on and
  the shapes they selected;
- `text`, when they're editing text: `{shape: <shapeId>, anchor, head}` for a shape's text, or
  `{notes: true, anchor, head}` for the notes, `anchor` and `head` being Yjs relative positions
  (`RelativePosition`, base64 of its v1 encoding) in that text, so they stay put while others type.

Ids or positions that are gone are left out by readers.

## The model the transcripts compare

The transcripts describe a presentation by what a person sees:

```json
{"title": "Quarterly review", "slides": [{"shapes": [{"rect": [60, 40, 840, 80], "text": "Quarterly review"}], "notes": "Welcome everyone"}]}
```

Slides in order; each slide's top-level shapes back to front with their box (`rect`, `[x, y, w, h]`
in points, `null` when the shape has none) and text (paragraphs joined with `\n`, `null` when the
shape has no text body); the notes' text.
