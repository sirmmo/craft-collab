# Craft plugin manifests 1.3: requirements

Version 1.3, 2026-10-15. The same in GridCraft, WordCraft and DeckCraft. An add-in can say which
other add-ins it needs, in which versions, and which app versions and protocol features. The app
checks this before starting it, explains what's missing, and offers to install it.

## The manifest

```json
{
  "id": "org.example.reports",
  "version": "1.4.0",
  "requires": {
    "apps": {"gridcraft": ">=0.7.0", "wordcraft": ">=0.7.0"},
    "protocol": ">=1.2",
    "features": ["dialogs", "events"],
    "addins": [
      {"id": "org.craft.vba", "version": "^0.1.4", "github": "sirmmo/craft-vba", "why": "Runs the report macros"},
      {"id": "org.craft.mqtt", "version": ">=0.1, <0.3", "github": "sirmmo/craft-collab", "optional": true, "why": "Live editing"}
    ]
  }
}
```

| Field | Meaning |
|---|---|
| `requires.apps` | Per app id, the app versions the add-in works with. An app missing from the map isn't constrained. (`apps`, the top-level list of apps the add-in is for, is unchanged) |
| `requires.protocol` | The plugin protocol versions it needs: `1.1`, `1.2`, `1.3`, `2.0`… (the app's own is in `initialize` as `host.protocol`, a new field, e.g. `"2.0"`) |
| `requires.features` | Names from `host.features` (`dialogs`, `open`, `events`, `document`, `push`, `collab`…) that must all be present |
| `requires.addins[]` | Other add-ins: `id`; `version`, a range (below; default `*`); `github` (optional `owner/repo` to install it from); `optional` (default false); `why` (shown to the person, at most 200 characters) |

At most 32 entries in `addins`, ranges at most 128 characters, ids as for `id`.

## Versions and ranges

Versions are `MAJOR.MINOR.PATCH` (missing parts are 0; a pre-release `-beta.1` sorts before the
release; `+build` is ignored). An Office add-in's four-part `1.0.0.0` compares as its first three
parts. Ranges are Cargo's: comma-separated comparators that must all hold —

- `^1.2.3` (also a bare `1.2.3`): `>=1.2.3, <2.0.0`; for 0.x, `^0.1.4` means `>=0.1.4, <0.2.0`
- `~1.2.3`: `>=1.2.3, <1.3.0`
- `>=`, `>`, `<=`, `<`, `=` with a full or partial version (`>=0.1` is `>=0.1.0`)
- `*` or empty: any version
- `1.*`, `1.2.*`: wildcards

A range that doesn't parse makes the add-in **blocked** ("its manifest's requirement … isn't a
version range"), never a crash.

## What the app does

When the add-ins are loaded (start-up, install, `plugins.load`, turning one on or off), the app
resolves them all together:

1. **App, protocol, features:** unmet means the add-in is **blocked**: not started, its
   functions, commands and ribbon items absent, with a reason ("Needs GridCraft 0.8 or later";
   "Needs plugin protocol 2.0 (this GridCraft has 1.3)").
2. **Required add-ins:** each must be installed, turned on, not failed or blocked itself, and
   its `version` in range. Otherwise the add-in is **blocked** with the reason ("Needs craft-vba
   ^0.1.4 (installed: 0.1.3)", "Needs craft-vba, which isn't installed", "Needs craft-vba, which
   is turned off").
3. **Optional add-ins** never block. The add-in starts either way and learns what's there from
   `initialize`, which gains `addins: [{id, version, enabled}]` (every installed add-in, so it
   can adapt).
4. **Order:** required add-ins start before the add-ins that need them. A cycle blocks every
   add-in in it ("Requirement cycle: a → b → a"). Chains deeper than 8 are blocked.
5. **Turning off or removing** an add-in others require asks first ("Reports needs craft-vba.
   Turn both off?"); programmatically, the dependents become blocked and the result lists them.
6. **Installing or updating** an add-in whose new version falls outside a dependent's range asks
   first ("Reports needs craft-vba ^0.1.4; 0.2.0 doesn't match") or, programmatically, fails
   unless `force: true`, in which case the dependent is blocked.

Blocking re-evaluates whenever anything changes: installing the missing add-in unblocks the
dependent without restarting the app.

## Installing what's missing

- After an add-in is installed from GitHub, the app checks its `requires.addins` and offers to
  install the missing or out-of-range ones that name `github`: "Reports needs craft-vba ^0.1.4
  (Runs the report macros). Install it too?". Optional ones are offered unchecked.
- It picks **the newest release whose tag is a version in range** (tags `v0.1.5` or `0.1.5`),
  not just the latest release, and says which one.
- The Add-ins window shows a blocked add-in with its reason and an **Install Missing** button
  when the missing add-ins name `github`.

## Commands

- `plugins.list` gains, per add-in: `state` `blocked` (alongside `loaded`, `failed`,
  `disabled`…), `blockedBy: [{id?, kind: app|protocol|feature|addin|cycle|range, need, have?, message}]`,
  `requires` (the manifest's), and `requiredBy` (ids of the add-ins that need it).
- `plugins.requirements {id}` → `{id, satisfied, tree: [{id, need, have, state, optional, children}]}`.
- `plugins.install {github, …}` gains `withRequirements` (default true from the UI, false
  programmatically: then the result lists `missing` for the caller to install) and `force`.
- `plugins.installMissing {id}` installs the missing required add-ins that name `github`.

## Office add-ins

An Office add-in's manifest `<Requirements>` (`<Sets><Set Name="ExcelApi" MinVersion="1.3"/>`,
`<Methods>`) is checked the same way against the API sets the app's Office.js library reports.
An unmet set blocks the add-in with the reason ("Needs ExcelApi 1.9; GridCraft supports up to
1.4"), instead of installing it and failing at the first call. `DefaultMinVersion` applies when
`MinVersion` is absent.
