# GridCraft schema 1: the shared workbook

The Yjs document a GridCraft room shares (craft plugin protocol 2, [section 5](../protocol-2.md#5-the-shared-documents)).
`collab/joined` says `app: "gridcraft"`, `schema: 1`. This page is what any peer (another GridCraft,
a web build, a bot, the conformance kit) needs to read and write cells, rows, columns and sheets.
GridCraft's own `docs/collab.md` is normative for everything this page leaves to it (styles,
names, row and column properties, merges, tables, charts, conditional formats, validation); where
the two disagree, fix one of them.

The reference implementation is `crates/conformance/src/gridcraft.rs`. The transcripts in
[`conformance/`](../../conformance) are written with it and check an app against it.

## Roots

| Root | Yjs type | Holds |
|---|---|---|
| `meta` | Map | `app` = `"gridcraft"` (string), `schema` = `1` (number; Yjs writes it as an integer), `title` (string) |
| `sheets` | Array | sheet ids (strings), in tab order |
| `sheet:<sheetId>` | Map | one sheet (below), for each id in `sheets` |
| `names` | Map | defined names (GridCraft's `docs/collab.md`) |
| `styles` | Map | cell styles by style id (GridCraft's `docs/collab.md`) |

A peer whose `meta.app` isn't `gridcraft` or whose `meta.schema` isn't `1` doesn't sync: the app
drops such an update and says "This room is for <app> <schema>". A document without `meta` yet
(a newcomer before the first state) is empty.

### A sheet: `sheet:<sheetId>`

| Key | Yjs type | Holds |
|---|---|---|
| `name` | string | the tab's name |
| `rows` | Array | row ids (strings), top to bottom: entry `i` is row `i + 1` |
| `cols` | Array | column ids (strings), left to right: entry `i` is column `i + 1` (`A` = 1) |
| `rowBlocks`, `colBlocks` | number | how many blocks of row or column ids this sheet has grown by (below) |
| `cells` | Map | key `"<rowId>:<colId>"` → a Map `{input: string, style?: string}` |
| `rowProps`, `colProps`, `merges`, `tables`, `charts`, `cf`, `dv` | Map | GridCraft's `docs/collab.md` |

Unknown keys are kept and ignored, so later versions can add some without a new schema number.

## Ids

- **Sheet ids, and the ids of inserted rows and columns:** 8 random characters of `0-9a-z`
  (lower case), for example `k3v9x0qa`. Unique within the document (sheets) or the sheet (rows,
  columns). Ids never change and are never reused.
- **Rows and columns past the end of the arrays** exist implicitly and are empty. A sheet grows
  (when a cell past the end gets an input, and when the document is seeded) by appending whole
  **blocks of 64 ids**: block `b` is `~b.0`, `~b.1`, … `~b.63` (`b` and the index in decimal).
  The next block number is the larger of the sheet's `rowBlocks` (or `colBlocks`) and one more
  than the highest block number present in the array; after growing, the writer sets
  `rowBlocks`/`colBlocks` to the next unused block number. Two people growing a sheet at the same
  time append the same ids, which collapse when read (next rule), so their cells land in the same
  rows. Seeding grows by as many blocks as the used range needs (a 4-row sheet has 64 row ids).
- **Duplicates:** concurrent edits can leave an id twice in `sheets`, `rows` or `cols`. Readers
  keep the first occurrence and skip the others (and anything that isn't a string). Positions
  (`A1` addresses, insert positions) count ids after that.

## Cells

- The key is the row id, a colon, the column id: `"k3v9x0qa:~0.1"`.
- The value is a Map. `input` is what the person typed, as GridCraft takes it: `42`, `3.5`,
  `Budget`, `=SUM(B2:B3)`, `'007`, `TRUE`. Never a calculated result: each app recalculates.
  `style` (optional) is a key of `styles`.
- Setting a cell that has a Map changes `input` in it (so a concurrent style change survives).
  A cell without one gets a new Map.
- Clearing a cell deletes its key (a Map with an empty `input` and no `style` is empty too).
- A key whose row or column id isn't in `rows`/`cols` (a row someone deleted while another typed
  in it) is ignored by readers, and may be removed by any writer.

## Edits

| Edit | Transaction |
|---|---|
| Set or clear a cell | as above; grows `rows`/`cols` first when the cell is past their end |
| Insert rows (columns) before position `p` | insert new random ids into `rows` (`cols`) before the `p`-th id; nothing when `p` is past the end (those rows exist implicitly). The writer also rewrites the inputs whose references shift, as Excel does, in the same transaction |
| Delete rows (columns) | delete the cells whose key uses those ids, then remove every occurrence of those ids from the array; rewrite the inputs whose references shift, in the same transaction |
| Add a sheet | a new id inserted into `sheets`, and its `sheet:<id>` map with `name`, `rows`, `cols`, `cells` |
| Rename a sheet | set `name` |
| Delete a sheet | remove its id from `sheets` (root maps can't be deleted: its contents may be cleared) |

A peer applying a remote structural edit maps rows and columns by id and **doesn't** rewrite
formulas again: the peer that made the edit already did, in the same update.

Each committed command is one transaction and one `collab/update` (protocol 2, section 6). Selection
and other view state never enter the document.

## Awareness

Each peer's awareness state is `{user: {id, name, color}, cursor?, selection?}`:

- `selection`: `{sheet: <sheetId>, ranges: [[rowId, colId, rowId, colId], …]}`, each range by its
  top-left and bottom-right cells' ids.
- `cursor`: `{sheet: <sheetId>, cell: [rowId, colId]}`, the active cell, when it isn't the first
  range's corner.

Rows and columns past the end of `rows`/`cols` (all of them on a sheet with no ids yet) are
named by the block id they would get if the sheet grew to them now: the `k`-th position past the
end (counting from 0) is `~<n + k div 64>.<k mod 64>`, where `n` is the next block number (the
larger of `rowBlocks`/`colBlocks` and one more than the highest block number present, as in
"Ids"). Naming them never writes to the document. A reader resolves an id it has in the array to
its position; a block id it doesn't have, numbered `n` or above by its own count, to the position
that block id would get, `length + (block - n) × 64 + index`; anything else (an index of 64 or
more, a block numbered below `n` that isn't there, a position past the last row or column) names
no cell, and the range is left out. When the sheet then grows, the same ids name the same rows
and columns. For example, on a sheet with no ids, rows 2 to 71 are `~0.1` to `~1.6`.

## The model the transcripts compare

The transcripts describe a document by what a person sees:

```json
{"title": "Budget", "sheets": [{"name": "Budget", "cells": {"A1": "Item", "B4": "=SUM(B2:B3)"}}]}
```

Sheets in tab order with their names, and every non-empty cell input by its A1 address (columns
`A`…`Z`, `AA`…; rows from 1).
