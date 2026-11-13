---
status: done
---

# 01 — Host: config documents and generic edits

## Scope

The host side the settings editor needs, with no UI: read a file as a document, apply one surgical edit at any key path, refuse stale or invalid edits, and announce changes. The existing `enabled` edit becomes one use of the generic edit. No window or tray change in this part.

Files expected to change: `src/crates/sdk/src/config/` (the edit module that holds `set_enabled`, plus a new generic edit), `src/crates/app/src/config_edit.rs`, `src/crates/app/src/bindings.rs`, `src/crates/app/src/host.rs` (commands, event emission after watcher batches and commits), the generated `src/ui/src/ipc/bindings.gen.ts`, and the docs below.

## Requirements

- **Key path.** A path type (working name `ConfigPath`): a list of segments, each a table key or an array index. Paths address scalars, tables, maps, arrays and array-of-tables elements, including keys that need quoting (`"CapsLock+a"`).
- **Operations** (working name `ConfigEditOp`):
  - `set { path, value }`: set or insert a JSON value (the UI's wire form) converted to the TOML item the schema expects: a scalar, an inline array or table for a value inside an inline container, and a standard table or array of tables when that is how the surrounding document already spells it or when the parent is the root/standard table and the value is an object/array of objects. Inserting into an absent parent creates the parent tables.
  - `remove { path }`: remove a key (reset to default) or an array element.
  - `insert { path, index, value }`: insert an element into an array or array of tables.
  - `move { path, from, to }`: reorder an array element, keeping each element's comments with it.
  - `rename { path, from, to }`: rename a map key, keeping its value and comments; refused if `to` exists.
- **Surgical text.** Only the span of the touched item changes. A test proves byte equality of everything else for each op, including the `#:schema` line, comments above and after the touched key, CRLF files and files without a trailing newline. Key insertion generalises the existing `enabled` rule: after the last key of the target table, before any sub-table header, in the file's line ending. `set_enabled` is reimplemented on top of `set` with its existing tests unchanged and passing.
- **Validation.** The candidate text is parsed, decoded and validated through the catalog exactly as a reload would before any write. A failure returns the diagnostic message and the field path when known; nothing is written. A candidate whose value at the path does not equal the requested value after re-parse is refused (as the `enabled` edit does today).
- **Refusals.** Unknown file, symlink, unreadable or currently invalid file, invalid candidate, path that crosses a non-table, out-of-range index, and `stale` (below). Each is a typed error that the bindings expose as a typed outcome, not a rejected promise.
- **Missing file.** The base is the generated template (as `plan_enabled` does); the commit uses the existing no-clobber create.
- **Hash.** A content hash of the exact bytes (a fixed, documented algorithm; `None` for a missing file). An edit carries `base_hash`; if the bytes read at plan time differ, the edit is `stale`. The existing re-read-before-rename still runs.
- **Commit.** The existing `PendingEdit::commit` protocol, then `ConfigStore::reload_one` once, as the enable edit does. The outcome returns the new hash and the reload outcome (applied / unchanged / rejected-with-diagnostic).
- **Document read.** A command (working name `config_document(file)`) returns: file name and path, state (missing/valid/invalid/symlink), hash, the parsed file value as JSON (only keys present in the file, `None` when invalid), the catalog default as JSON, the schema JSON, the diagnostic (message, line/column, field path) and warnings. It reads disk once and touches no module lock.
- **Catalog list.** A command (working name `config_files()`) returns the files in catalog order with state, diagnostic presence and warning count, for the sidebar badges. It may reuse `HostStatus.config`.
- **Change event.** A typed event (working name `config-changed { files }`) after every watcher batch that touched catalog files (whatever the reload outcome, including rejected and deleted) and after every committed edit. Emitted from the host, not the watcher thread directly, without holding store locks.
- **Open file.** A command (working name `open_config_file(file)`) opens the file with the default app (NSWorkspace / `open`), creating it from the template first when missing. Paths come only from the catalog; no path input.
- **Bindings.** Commands and event are added to `bindings::builder()`; `bun run generate-bindings` regenerates; `bun run check:generated` passes.
- **Docs.** `docs/architecture/config.md`: replace "Tray enable edits" with "Edits" (generic ops, surgical rule, validation, hash/stale, commit); add document reads and the change event. `docs/architecture/ui.md` IPC section lists the new commands/event.

## Acceptance criteria

- [x] TP-01-01: For each op, on fixture files with comments, CRLF, no trailing newline, quoted keys and arrays of tables, only the touched span changes (byte-equality test of the rest).
- [x] TP-01-02: `set_enabled` uses the generic edit and all its existing tests pass unchanged.
- [x] TP-01-03: An invalid value (wrong type, out of bounds, bad enum) is refused with the catalog's message and field path; the file bytes and mtime are unchanged.
- [x] TP-01-04: A stale `base_hash` is refused and writes nothing; a symlink, an invalid current file and an unknown file are refused.
- [x] TP-01-05: An edit to a missing file creates it from the template with the edit applied and `#:schema` on line 1.
- [x] TP-01-06: A committed edit reloads the module once; the watcher's later event for the same bytes reloads nothing.
- [x] TP-01-07: `config_document` returns present-keys value, default, schema, hash and diagnostic for missing, valid, invalid and symlinked files.
- [x] TP-01-08: `config-changed` fires after a commit and after an external save (including an invalid one and a delete).
- [x] TP-01-09: Bindings regenerated; docs describe the shipped behaviour; `bun run lint`, `bun run format:check`, `bun run check:generated` and `bun run test` pass.

## Tests

- SDK unit tests per op and per refusal, with snapshot-style before/after fixtures.
- A property-style test: random sequences of set/remove/insert/move on a commented fixture always yield text that parses, keeps every comment line not attached to a removed item, and round-trips the requested values.
- App tests for the commit + reload path in a temp config dir with an injected `ConfigEnvironment`.
- **End-to-end scenario** (app crate, real store and watcher on a temp dir): start from a realistic `tiling.toml` (comments, workspaces as arrays of tables, rules) and `keybindings.toml`; through the command layer change a tiling gap, add a workspace, reorder two workspaces, rename a keybinding, reset a key to default; after each, assert the file bytes outside the edit, the accepted config revision and exactly one `config-changed`. Then write the file externally (valid, then invalid, then valid again) and assert the events, the document state and that an edit with the old hash is `stale`.

## Notes

The JSON-to-TOML conversion must never write `null`; `set` with `null` on an `Option` field is `remove`.

Implementation decisions (2026-11-13): the hash is FNV-1a 64-bit over the exact bytes (16 hex digits). A watcher batch that only repeats bytes the host itself just committed (and was not rejected) is not announced again, so one edit yields exactly one `config-changed`; every outside save is announced. A change between plan and the final re-read maps to `stale`. JSON fields cross the wire through the SDK marker type `JsonValue`. Objects set inside an array of tables are inline; elsewhere a new table is appended as a `[table]` block. Renaming a dotted-key table is a typed `unsupported` refusal. `open_config_file` runs `/usr/bin/open <path>` and is tested only for refusals.
