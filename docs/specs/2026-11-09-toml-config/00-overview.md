---
branch: feat/toml-config
---

# TOML configuration

Read when planning or implementing the switch of Stache Next's configuration files from JSONC to TOML. Shipped behavior is documented in [architecture/config.md](../../architecture/config.md).

## Goal

Make the configuration simpler to write and faster to load. Each catalog file becomes `<id>.toml` with snake_case keys and enum values, editors find its JSON Schema through a first-line `#:schema <url>` comment, and `config migrate` converts both the legacy single file and the current per-module `<id>.jsonc` files. The catalog, store, watcher, per-file reload, acceptance and diagnostics keep their model. Only the file names, the syntax and the spellings change.

## Non-goals

- No single-file layout. One file per catalog id stays.
- No JSONC (or JSON) fallback at runtime. The loader reads only `<id>.toml`.
- No YAML or other format.
- No change to `_legacy/` or to the legacy configuration format. `scripts/measure` keeps writing legacy JSONC for the legacy app.
- No new configuration keys, defaults or semantics apart from the spelling changes and the i64 bound in [Decisions](#decisions) 8.
- No rewrite of user data: keybinding shortcut keys, command strings, bundle ids, paths, colors, device names and percentage strings such as `"50%"` stay as written.
- No preservation of comments from the legacy or per-module JSONC sources during migration. Today's migration doesn't preserve them either.
- No new benchmark and no promised speedup numbers.
- No edits to historical specs. Planned specs are covered in [Resolved questions](#resolved-questions).

## Decisions

Approved by the user on 2026-11-09 (1–5) and fixed by the orchestrator (6–13).

1. **Layout unchanged.** Files are `<catalog id>.toml` in the same directory, and the ids don't change: `global.toml`, `bar.toml`, `commandQuit.toml`, `keepAwake.toml`, `keybindings.toml`, `menuAnywhere.toml`, `notunes.toml`, `proxyAudio.toml`, `tiling.toml`, `wallpapers.toml`. `config path`, `core.status`, `core.reload`, watcher batching and per-file acceptance keep their model.
2. **TOML only at runtime.** The loader, store, watcher, tray edit, the Wallpapers CLI loader and `config path` validation read `.toml` only. A leftover `<id>.jsonc` is ignored apart from the migration hint (part 03).
3. **`config migrate` converts both sources to TOML:** (a) the legacy single `config.jsonc`/`config.json` with today's discovery and `migrate_legacy_section` hooks, and (b) the per-module `<id>.jsonc` files. Sources are never modified. Collision and `--force` rules are the same as today.
4. **snake_case everywhere in configuration.** Every declared key becomes snake_case (`exec_on_startup`, `default_layout`, `hold_duration`, `dismiss_delay`, `api_keys`, `default_location`, `depends_on`, `mouse_button`, `app_id`, `preset_on_open`; the kebab exception goes away). Enum values become snake_case too (`split_vertical`, `ease_out`, `visual_crossing`, `open_meteo`, `starts_with`, `right_click`, `middle_click`, …). Values that are already a single lowercase word don't change. User data is never rewritten (see Non-goals).
5. **Schema association** uses a first-line `#:schema <url>` comment (the Taplo/Tombi convention). There is no `$schema` data key; a `$schema` key in a TOML file is an ordinary unknown key and warns. Schemas are still generated as `schemas/<id>.schema.json` with the same `$id` base URL (`SCHEMA_BASE_URL`), now with snake_case names and values.
6. **Crates.** `toml` 1.x (serde) for parsing and serialization (already in `Cargo.lock` transitively as `toml 1.2.0`). `toml_edit` for the comment-preserving tray `enabled` edit. `jsonc-parser` is used only to read migration input and leaves every runtime path.
7. **Diagnostics** keep `path:line:col: message (at \`field\`)`. Parse line and column come from the TOML error span. The field path comes from `serde_path_to_error`as today. The message is the parser's single-line reason, not its multi-line snippet. Unknown keys still warn through`serde_ignored`. A whitespace-only file is still rejected, so a truncated save never resets a module to its defaults. A file that holds only comments, such as just the `#:schema` line, is a valid empty table that uses the defaults.
8. **TOML integers are i64.** `hold_duration`, `dismiss_delay` (Hold-to-Quit) and wallpapers `interval` keep their `u64` Rust type but reject values above `i64::MAX` with a typed diagnostic that names the field. Their schemas declare `maximum: 9223372036854775807`. Migration rejects a larger JSON value the same way instead of writing a file TOML cannot hold.
9. **Templates** (`config init` and the missing-section templates in migrate) come from a TOML emitter. Line 1 is `#:schema <url>`. `#` comments come from the schemars descriptions, and keys follow struct declaration order (not alphabetical). Scalars come before sub-tables, and non-empty arrays of objects are written as arrays of tables. The output is TOML 1.0 compatible. Defaults never emit null: every `Option` is `skip_serializing_if`. `config init --stdout` separates files with `# file: <name>`.
10. **Untagged enums round-trip through TOML:** `StartupCommands` (string | array), `GapsConfigValue`, `GapValue`, `DimensionValue` (integer | `"50%"`) and `BorderStateConfig` (including a bare `false`). Keybinding map keys are written as quoted TOML keys (`"CapsLock+a"`, `"§"`, `"cmd+shift+."`).
11. **Schema-driven migration conversion.** Walk the JSON value alongside the target type's generated JSON Schema. Rename an object key (camelCase or kebab-case to snake_case) only where the schema declares that snake form as a named property. Leave `additionalProperties` map keys untouched. Normalize a string value only where its schema location is a string enum (`enum` or `const` branches) and the snake form is a member. For the legacy single file the order is: `migrate_legacy_section` hooks (on legacy spellings), then conversion, then typed validation, then TOML output. Per-module `.jsonc` files don't run the hooks.
12. **Stale-file hint.** `config path`, and the host's startup log once, print a hint to run `` `stache config migrate` `` for each `<id>.jsonc` that has no matching `<id>.toml`.
13. **Every non-historical consumer moves with its part.** That covers the smoke harness, tray, host, bindings docs plus the regenerated `src/ui/src/sdk/bindings.gen.ts`, UI fixtures, `scripts/measure/config.ts` (Stache Next configs only), README.md, `docs/architecture/*`, `docs/guides/*` and `docs/features/*`.

## Parts

| Part                             | Summary                                                                                         | Status |
| -------------------------------- | ----------------------------------------------------------------------------------------------- | ------ |
| [01](01-snake-case-keys.md)      | snake_case keys and enum values (still JSONC), schema-driven converter in legacy `migrate`      | done   |
| [02](02-toml-format.md)          | TOML loader, diagnostics, `.toml` names, `#:schema` templates, `toml_edit` tray edit, i64 bound | done   |
| [03](03-migrate-module-files.md) | `config migrate` converts per-module `.jsonc` to `.toml`; stale-`.jsonc` hint                   | done   |

## Sequencing

The order is 01 → 02 → 03, and each part depends on the one before it. Part 01 is split out because renaming is independently testable on the existing JSONC loader, and it lets part 02 change only syntax. Between 01 and 03, a user's existing per-module `.jsonc` files decode with unknown-key warnings. That is acceptable only on the branch: the [merge gate](../../workflow.md#merge-gate) blocks merging until all three parts are `done`, so `main` never ships the intermediate state.

Acceptance IDs `TC-NN-MM` have exactly one owning part.

## Resolved questions

- **OQ-1 (resolved 2026-11-09): Tiling layout spelling outside configuration.** `LayoutType` is also the `tiling.workspace` socket type (`modules/tiling/src/dto.rs:141`) and the `stache tiling --layout` value list (`modules/tiling/src/cli.rs:69–78`, parsed through serde at `:746`). Decision: one serde spelling (`split_vertical`) for configuration, socket and query results. The CLI accepts snake_case as canonical and keeps the old kebab spellings as CLI-only aliases, so existing keybinding commands such as `stache tiling --layout split-vertical` keep working. Help text and the table renderer show snake_case.
- **OQ-2 (resolved 2026-11-09): planned specs.** The unfinished planned specs (`2026-11-03-bar` 03–08, `2026-11-03-release`, `2026-11-08-tiling-bar` 10, `2026-11-09-tiling-animation`, `2026-11-09-tiling-borders`) were refreshed on this branch in a docs-only commit after the three parts, outside the parts themselves. Done specs keep their historical spellings.

## Affected docs

Each part updates only its shipped slice:

- [architecture/config.md](../../architecture/config.md): keys and values (01); Files, Parsing, Tray enable edits, Commands, Schemas (02); Migration (01–03).
- [README.md](../../../README.md): key spellings (01); file names, examples and upgrade note (02, 03).
- [guides/new-module.md](../../guides/new-module.md): `rename_all = "snake_case"` (01); `.toml` names (02).
- [guides/testing.md](../../guides/testing.md) and [architecture/overview.md](../../architecture/overview.md), [ipc.md](../../architecture/ipc.md), [ui.md](../../architecture/ui.md): file names (02).
- `docs/features/*.md`: key and value spellings (01), file names and examples (02).

Until then, [architecture/config.md](../../architecture/config.md) carries one "planned change" note pointing here.
