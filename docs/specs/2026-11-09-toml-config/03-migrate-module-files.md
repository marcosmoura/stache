---
status: done
---

# 03 — Migrate per-module `.jsonc` files

Read when implementing after part 02. Paths are under `src/crates/` unless rooted. This part adds the second migration source and the stale-file hint. Runtime loading is unchanged.

## Scope

`cli/src/migrate.rs` (source selection and the per-module plan), `cli/src/lib.rs` (`MigrateArgs` help text), `cli/src/config.rs` (`path` hint), `sdk/src/config/paths.rs` (stale-file listing helper), the host's initial configuration load in `app/src/host.rs` (one startup log line), and docs.

## Requirements

### Source selection (`config migrate [--from PATH] [-f]`)

1. `--from PATH`:
   - A directory selects **per-module mode** over that directory.
   - A file whose stem is a registered catalog id (including `global`) and whose extension is `.jsonc` selects **per-module mode** for that one file.
   - Any other file is the **legacy single file**, as today.
   - A missing path fails as today.
2. Without `--from`, if the selected configuration directory holds at least one registered `<id>.jsonc`, the mode is **per-module** over that directory. If a legacy `config.jsonc`/`config.json` also exists there, the report says it was not migrated and to use `--from` to migrate it.
3. Otherwise, today's legacy discovery order is used unchanged.

The report's first line names the mode and the source, for example `migrating per-module files from <dir>` or `migrating legacy file <path>`.

### Per-module mode

- Only registered ids with an existing `<id>.jsonc` are planned. Other ids get no file and no template; the loader's defaults and `config init` cover them. If nothing is planned, migration fails with a message listing the expected names.
- For each source file:
  - Parse it with the migration-only JSONC parser (same syntax options as today's loader). A non-object root is a shape error.
  - Remove a root `$schema` key.
  - Convert it with the part 01 converter against `catalog.schema(id)`. The `migrate_legacy_section` hooks are **not** called; these files already have the new shape, and keybindings must not be wrapped twice.
  - Validate with `catalog.inspect` on the rendered TOML, which proves the output round-trips.
  - Write `<id>.toml` with `#:schema` on line 1.
- Unknown keys are preserved with the same warning as legacy migration. Values above `i64::MAX` and nulls in unknown keys are handled as in part 02. Any invalid source fails the whole migration before anything is written. Every source is parsed, converted and validated first.
- The destination is the selected configuration directory (or `--config`), as today. Collisions with an existing `<id>.toml` refuse without `--force`; `--force` replaces only planned files. Source and destination can't be the same file, because the extensions differ, but the canonical-path check still runs.
- Sources are never modified, renamed or deleted. The last report line says the `.jsonc` files are unchanged and ignored, and can be removed after checking.

### Stale-file hint

- A `<id>.jsonc` for a registered id with no `<id>.toml` next to it is **stale**. `config path` prints, under that file's entry or in one trailing block, `note: <id>.jsonc is ignored; run \`stache config migrate\``. Exit status and markers don't change.
- The desktop host logs the same hint once at startup (a single `warn` record listing all stale files). Reloads and watcher batches don't repeat it. When both files exist there is no hint.

## Acceptance criteria

- [ ] TC-03-01: Source selection follows the order above, with one test per branch: directory, `bar.jsonc` file, legacy-named file, auto per-module, auto per-module with a legacy file present (with the note), auto legacy, and none.
- [ ] TC-03-02: End to end: a temp directory holds per-module `.jsonc` files in **today's** spellings (camelCase, kebab enums, `preset-on-open`, `$schema`, comments, trailing commas) for every registered id, with realistic non-default values (keybindings with `"CapsLock+a"` and `"§"`, tiling workspaces/presets/borders/gaps, proxyAudio `dependsOn`/`startsWith`, Hold-to-Quit durations, bar weather `visual-crossing`). `config migrate` writes `.toml` files. Each one loads with **zero** warnings, its typed value equals the expected typed value, and the `.jsonc` bytes are unchanged.
- [ ] TC-03-03: A partial directory (only `tiling.jsonc`) writes only `tiling.toml`. An invalid source writes nothing. A `.toml` collision refuses without `--force` and `--force` replaces only planned files.
- [ ] TC-03-04: The stale hint appears in `config path` for `tiling.jsonc` without `tiling.toml`, and is absent when `tiling.toml` exists. A host test (injected logger) shows exactly one startup record and none on reload.
- [ ] TC-03-05: Migrating a keybindings `.jsonc` that already has `{ enabled, bindings }` keeps that shape. It is not wrapped again.

## Tests

- `cli/src/migrate.rs` unit tests for selection and planning. Commit a fixture directory (for example `cli/tests/fixtures/per-module/`) hand-written in pre-part-01 spellings, because the current code can no longer generate it.
- `cli/src/config.rs` path-hint test, plus a host startup-log test.

## Validation

```sh
bun run lint
bun run format:check
bun run test
bun run check:generated
```

## Notes

- Docs to update: [architecture/config.md](../../architecture/config.md) (Migration covers both sources, the selection order, the stale hint in Path command markers; remove the "planned change" note) and README.md (an upgrade note: run `stache config migrate` once). Add a row update in [docs/index.md](../../index.md) once all parts are `done`.
- Rollback: revert the commit. The `.jsonc` sources are untouched, so nothing user-side needs restoring.
