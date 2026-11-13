---
status: done
---

# 04 — Settings editor

## Scope

The file tabs: a generic, schema-driven form (D6–D8) for every catalog file, with live sync and conflict handling (D10–D11), built on part 01's commands.

Files expected to change: `src/ui/src/windows/Popover/` (a `FileTab`, a `SchemaForm` with one component per control kind, field chrome), `src/ui/src/queries/` (document query, edit mutation, `config-changed` subscription), possibly a small shared schema-resolution utility, UI tests, the Playwright fake host (an in-memory config store with real TOML text so edits and external writes can be simulated), and the docs below.

## Requirements

- **Schema walk.** Resolve `$ref`/`$defs`, `anyOf [T, null]` as optional `T`, `oneOf` of string consts as an enum with descriptions, `type` arrays, `minimum`/`maximum`, `default`, `additionalProperties` as a map, `items` with object items as an array of tables. Unknown constructs fall back to a read-only TOML view plus "Open in editor" for that field only (D6). The walk is a pure, unit-tested function from schema to a field tree.
- **Values.** Each field shows the file's value when present, else the default (D7) with a "default" marker. "Reset to default" sends `remove`. Optional fields have "not set".
- **Controls.** As D6: switch, select, number (with bounds and integer steps), text, map rows (add/rename/remove), scalar lists, array-of-tables cards (add/remove/drag-to-reorder and keyboard move up/down, collapsible, with a title taken from a `name`/`id`-like field when present). Nested tables are collapsible sections. Every control has a label (the key, humanised), help text from `description`, and an accessible name.
- **Writes.** D8 timing. Each edit sends the document's current hash. On success, the document refetches (or applies the returned hash). On `stale`, refetch and keep the field's draft (D10). On validation failure, show the message on the field and keep the draft. While an edit is in flight the field shows pending; edits to one file are sent one at a time in order.
- **Sync.** Subscribe to `config-changed` and `popover-shown`; refetch affected documents. A field with focus or an unsaved draft is not overwritten: it shows "Changed on disk" with "Use disk value". Unfocused fields update in place without losing scroll position.
- **States.** Missing file: defaults, note "File not created yet; the first change creates it." Invalid file: diagnostic with line/column, form read-only, "Open in editor" (D11). Symlink: read-only with a note. Warnings (unknown keys): listed at the top of the tab.
- **Header.** File name, path (copy action), "Open in editor".
- **Performance.** `tiling.toml` (the largest schema) renders and stays responsive; long arrays are collapsed by default.
- **Docs.** `docs/features/popover.md` gets the settings editor section (coverage, defaults, write timing, sync, conflicts, invalid files). `docs/architecture/ui.md` describes the form structure. `docs/index.md` and `docs/roadmap.md` rows.

## Acceptance criteria

- [x] TP-04-01: The schema walk produces a field tree for all ten committed schemas with no unknown fallback (test over `schemas/*.json`), and handles a synthetic unknown construct with the fallback.
- [x] TP-04-02: Each control kind writes the right op (set/remove/insert/move/rename) with the current hash, at the D8 moment.
- [x] TP-04-03: Validation failure and `stale` keep the draft and show the message / refetch.
- [x] TP-04-04: An external change updates unfocused fields and shows "Changed on disk" on a focused one.
- [x] TP-04-05: Missing, invalid and symlinked files render as specified.
- [x] TP-04-06: Docs updated; `bun run lint`, `bun run format:check`, `bun run check:generated` and `bun run test` pass.

## Tests

- Vitest: the schema walk over the real schemas; each control in isolation; FileTab states.
- **End-to-end scenarios** (Playwright, fake host with real TOML text based on realistic `tiling.toml`, `bar.toml` and `keybindings.toml`): change the tiling gap and a layout enum; add, reorder and remove a workspace; rename and remove a keybinding; reset a key to default and see it marked default; type an out-of-range number and see the inline error with the file unchanged; simulate an external save while a field is focused and take the disk value; simulate an invalid external save and see the tab go read-only, then a fix make it editable. After each step assert the fake host's file text keeps the comments.
- Screenshot baselines for Overview, a simple tab (`keepAwake`), `tiling` and an invalid-file tab, light and dark.

## Notes

Manual check in the running app after this part (ask before launching): open with a left click on each display; type in a text field; click outside, press Esc and click the icon again to close; right-click menu; edit `tiling.toml` gap in the popover and watch the windows re-tile; edit the same file in an editor with the popover open and see it update; save an invalid file and see the read-only state; open over a full-screen app.

Implementation decisions (2026-11-13): out-of-range numbers are caught in the form from the schema bounds before sending (host refusals still show inline); an invalid file shows its diagnostic and a read-only form on the defaults with a note; new workspaces are seeded `{ name = "new" }` and new map keys `new_key`; an array that exists only as a default is written whole on its first change; "Use disk value" keeps focus so the blur does not save the draft. Drag-to-reorder has no automated test; button and keyboard moves do.
