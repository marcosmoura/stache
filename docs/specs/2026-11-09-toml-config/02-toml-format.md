---
status: done
---

# 02 — TOML file format

Read when implementing the syntax switch after part 01. Paths are under `src/crates/` unless rooted. This is the largest part. Loader, templates, tray edit and consumers change together because every file the app writes must load.

## Scope

- `sdk/Cargo.toml`: add `toml` 1.x and `toml_edit` (versions compatible with the `toml 1.2.0` already in `Cargo.lock`). Move `jsonc-parser` off the runtime crates: it stays only where migration input is read (proposed: `stache-cli` only).
- `sdk/src/config/loader.rs` (TOML parse, empty-file rule), `diagnostics.rs` (`file_name` → `<id>.toml`, `ConfigDiagnostic::parse` from a TOML error, `DiagnosticKind::Parse` docs), `paths.rs` (`module_file`), `schema.rs` (`schema_url`/`file_name` derive from the id, not by trimming an extension), `catalog.rs` (remove the `$schema` skip at `decode_value`), `template.rs` (TOML emitter), `edit.rs` (`toml_edit`), `store.rs`/`watcher.rs` (names only), `global.rs`/`logging.rs` doc names.
- i64 bound: `modules/hold-to-quit/src/config.rs` (`hold_duration`, `dismiss_delay`) and `modules/wallpapers/src/config.rs` (`interval`).
- `cli/src/migrate.rs`: read the legacy file through the migration-only JSONC parser, write `.toml`. `cli/src/config.rs`: `--stdout` separator `# file: <name>`.
- Consumers: `app/src/config_edit.rs`, `tray.rs`, `host.rs`, `smoke.rs`, `bindings.rs` docs and regenerated `src/ui/src/sdk/bindings.gen.ts`, `modules/wallpapers/src/cli/{mod.rs,adapters.rs}` message text, `src/ui/tests/hostFixtures.ts`, `TestWindow.test.tsx`, `scripts/measure/config.ts` (`writeNextConfigs` writes `global.toml`/`proxyAudio.toml`; `writeLegacyConfig` stays JSONC), and every test using `.jsonc` names or JSON text for runtime files.

## Requirements

### Parsing and diagnostics

- `loader::parse_text` parses TOML. The root is always a table. Type decoding keeps today's pipeline (`serde_path_to_error` + `serde_ignored`, field paths, unknown-key warnings) and the catalog's `fn(serde_json::Value)` decode boundary. Converting the parsed TOML to `serde_json::Value` first is acceptable.
- Parse errors give `DiagnosticKind::Parse` with a 1-based line and column (character column) from the error span. The message is the single-line reason. Duplicate keys, redefined tables, invalid escapes and unterminated strings are parse errors.
- Values JSON cannot hold are rejected with a typed diagnostic that names the field: TOML date/time values (no config field accepts them) and `nan`/`inf` floats. They must not reach serde as private datetime maps or nulls.
- A whitespace-only file is rejected with ``file is empty; add a `#` comment or a key to use defaults``. A comment-only file decodes as `{}` (defaults).
- `$schema` is no longer special: as a TOML key it warns like any unknown key. A `#:schema` comment is just a comment.
- i64 bound: values above `i64::MAX` in the three fields are rejected with a typed diagnostic naming the field (reachable from JSON migration input and from direct Rust construction checked in `validate`). Their schemas declare `maximum: 9223372036854775807`. Specta treatment is unchanged (`#[specta(skip)]` stays where present).

### Names

- Every catalog file is `<id>.toml`: `ConfigFileId::file_name`, `ConfigPaths::module_file`/`file`, watcher matching, `core.status` file names, `config path` output, `config init`/`migrate` destinations. Schema files and `$id`s are unchanged (`schemas/<id>.schema.json`).
- The watcher ignores `.jsonc` changes.

### Templates

- `catalog.template(id)` returns TOML (decision 9). Line 1 is `#:schema <schema_url>`. Each key has its schemars description as `#` comments above it (multi-line kept, a blank description line becomes `#`). Keys follow struct declaration order. Don't enable `serde_json/preserve_order` workspace-wide if it would reorder committed schemas or other JSON output; serializing the typed default directly to TOML is the expected route.
- Root and sub-table scalars come before sub-tables. An empty array is `key = []`, an empty map is an empty table or `{}`, and a non-empty array of objects is an array of tables. Strings and map keys that aren't bare-key safe are quoted. No null is ever emitted, and an `Option` field without `skip_serializing_if` fails a test.
- Output is TOML 1.0 compatible: no multi-line inline tables and no trailing commas in inline tables.
- Migrated files (validated legacy sections and the converted global) also start with the `#:schema` line. Their key order isn't significant. Unknown keys inside a section are preserved as today. A null inside an unknown key can't be written: that key is dropped with a warning naming its path.

### Tray `enabled` edit

- `sdk/src/config/edit.rs::set_enabled` uses `toml_edit`. It replaces only a root `enabled` boolean, keeping its trailing comment, whitespace and every other byte. If `enabled` is absent, it inserts `enabled = <bool>` into the root key/value block. The insertion goes after any leading comment lines, so `#:schema` stays line 1, and before the first table header. A root `enabled` that isn't a boolean (including `[enabled]`) gives `NotBoolean`. An unparsable file gives `Parse` with the TOML message (`cannot edit invalid TOML: …`).
- `app/src/config_edit.rs::plan_enabled` keeps every safety rule in [config.md](../../architecture/config.md#tray-enable-edits): symlink refusal, validation of the full replacement, no-clobber create, re-read before rename and permission bits. A missing file is created from the TOML template.

### Migration (legacy single file)

- Discovery, hooks, conversion (part 01), collision and `--force` are unchanged. Destinations become `.toml`. The unknown-key report says `preserved in <id>.toml`. Values above `i64::MAX` fail migration with the field path, and nothing is written.

## Acceptance criteria

- [ ] TC-02-01: Every registered template parses, decodes to `Default` with zero warnings and has `#:schema <url>` as line 1. Key order matches declaration order (snapshot test for `global`, `commandQuit`, `tiling`, `proxyAudio`).
- [ ] TC-02-02: The untagged unions round-trip through TOML text (string and array `exec_on_startup`, all gap forms, integer and `"50%"` dimensions, `false`/solid/gradient/glow border states). Keybinding keys `"CapsLock+a"`, `"§"`, `"cmd+shift+."` and a key with spaces survive a serialize → parse round-trip.
- [ ] TC-02-03: Diagnostics: parse errors give `path:line:col: message`; type errors give `(at \`field\`)`; datetime, `nan`/`inf`, duplicate key, whitespace-only and comment-only files behave as specified; `$schema` warns.
- [ ] TC-02-04: `i64::MAX` is accepted and `i64::MAX + 1` is rejected for all three fields (decode from migration JSON and validate). The schemas declare the maximum.
- [ ] TC-02-05: Tray edit preserves comments, the `#:schema` line, key order, inline/trailing comments, tables and arrays of tables. Inserting into a file that has only `#:schema` plus `[table]` sections yields a valid file with `enabled` at root. All existing `config_edit.rs` safety tests pass with TOML text.
- [ ] TC-02-06: End-to-end in a temp dir: `config init` → edit `proxyAudio.toml` and `tiling.toml` → `ConfigStore` reload applies → `plan_enabled(..., true)` commit → `core.status` (router test) shows `<id>.toml`, accepted, no diagnostics or warnings. Then write a malformed TOML file and get a parse diagnostic while the last good revision stays active.
- [ ] TC-02-07: The realistic legacy fixture from part 01 migrates to `.toml` files that load with zero warnings. `config path` on the result shows every file `(exists, offline)`.
- [ ] TC-02-08: `cargo tree -p stache-sdk -e normal` does not contain `jsonc-parser` (or, if the worker keeps a sdk migration helper, the loader/store/watcher/edit modules don't import it and the overview's decision 6 note is updated).
- [ ] TC-02-09: The smoke harness writes TOML and its status lookups use `proxyAudio.toml`. `bun run test:scripts` covers TOML from `writeNextConfigs`. The orchestrator confirms the release launch measurement in [guides/performance.md](../../guides/performance.md) still runs end to end (no numeric target).

## Tests

- `loader.rs`, `diagnostics.rs`, `catalog.rs`, `template.rs`, `edit.rs`, `paths.rs`, `schema.rs` unit tests rewritten for TOML. Add a template property test over the composed app catalog (TC-02-01) in `app`.
- `cli/src/config.rs` (`init`, `--stdout` separator, `path` markers with `.toml`), `cli/src/migrate.rs` (TOML outputs, i64 rejection, null-in-unknown-key drop).
- `app/src/config_edit.rs`, host and router status tests, Wallpapers CLI tests (`wallpapers.toml`).
- UI: `hostFixtures.ts` and `TestWindow.test.tsx` use `.toml` names.

## Validation

```sh
bun run generate-schema -- --check
bun run check:generated
bun run lint
bun run format:check
bun run test
```

## Notes

- Docs to update: [architecture/config.md](../../architecture/config.md) (Files and paths, Parsing, Tray enable edits, Commands `--stdout`, Schemas with `#:schema`, Migration destinations; narrow the "planned change" note to part 03), README.md (file names, TOML examples), [architecture/overview.md](../../architecture/overview.md), [ipc.md](../../architecture/ipc.md), [ui.md](../../architecture/ui.md), [guides/new-module.md](../../guides/new-module.md), [guides/testing.md](../../guides/testing.md), and file names and examples in every `docs/features/*.md`. Leave the [roadmap](../../roadmap.md) "Finished" rows, which describe history.
- Editor support depends on Taplo/Tombi reading `#:schema`. No editor is verified by tests.
- Until part 03, an existing per-module `.jsonc` is silently ignored on this branch.
