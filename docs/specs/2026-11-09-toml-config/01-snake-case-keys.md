---
status: done
---

# 01 — snake_case keys and enum values

Read when implementing the spelling change. Paths are under `src/crates/` unless rooted. The file format stays JSONC and the file names stay `<id>.jsonc` in this part.

## Scope

- Serde spellings of all ten catalog types and everything nested in them: `GlobalConfig`/`LoggingConfig` (`sdk/src/config/global.rs`), `BarConfig`/`WeatherConfig`/`WeatherProvider` (`modules/bar/src/config.rs`), `CommandQuitConfig`/`HudPosition`/`HudAnimation` (`modules/hold-to-quit/src/config.rs`), `KeepAwakeConfig`, `KeybindingsConfig`, `MenuAnywhereConfig`/`MenuAnywhereModifier`/`MenuAnywhereMouseButton`, `NoTunesConfig`, `ProxyAudioConfig`/`AudioDevicePriority`/`AudioDeviceDependency`/`MatchStrategy`, `TilingConfig` and every type under `modules/tiling/src/config/` (`LayoutType`, `EasingType`, `MasterPosition`, `FloatingPosition`, `BorderStyle`, borders, gaps, floating, workspaces), `WallpapersConfig`/`WallpaperMode`, and `LogLevel` (`sdk/src/logging.rs`).
- The schema-driven converter in `cli/src/migrate.rs` (or a new `cli/src/migrate/convert.rs`).
- Regenerated `schemas/*.schema.json` and `src/ui/src/sdk/bindings.gen.ts`.
- Every non-test literal and test fixture that spells a renamed key or value: `app/src/smoke.rs` (`INITIAL_CONFIG`/`RELOAD_CONFIG`), `scripts/measure/config.ts` (`writeNextConfigs` only), `src/ui/src/shell/windows/TestWindow/TestWindow.test.tsx` (`dependsOn` warning text), and the module and CLI tests.
- `LayoutType` consumers outside configuration, as decided under OQ-1: `modules/tiling/src/cli.rs` (`LAYOUTS`, help text, `:746`), `dto.rs`, `ipc.rs` and the render tests.

## Requirements

- Every struct uses `rename_all = "snake_case"`. The kebab override on `WorkspaceConfig::preset_on_open` (`modules/tiling/src/config/workspaces.rs:55`) is removed, so the key is `preset_on_open`. Every unit enum uses `rename_all = "snake_case"`. For enums that are already lowercase single words, this changes no value.
- No serde `alias` for old spellings: the old spelling is an unknown key (warning) or an unknown variant (type error naming the field). The legacy `migrate_legacy_section` hooks keep matching legacy camelCase spellings, because they run before conversion. Only their emitted keys may change if that's simpler, and the converter accepts either form.
- Validation messages and field paths that name keys use the new spellings (for example `workspaces[1].preset_on_open`, `master.ratio`, `floating.presets[0].width`). Doc comments that name keys are updated, because they become schema descriptions and template comments.
- Rust field names are unchanged; only serde spellings change. Specta TypeScript types follow serde, so `bindings.gen.ts` changes field names. UI code compiles against the regenerated types.
- **Converter** (decision 11). It is a pure function over `serde_json::Value` and a root `schemars::Schema`. It resolves `$ref`/`$defs` and handles `properties`, `additionalProperties`, `items`, `prefixItems`, `anyOf`/`oneOf`/`allOf`, and `enum` or `const`-branch string enums (schemars 1 emits documented unit variants as `oneOf` of `const`).
  - Object keys: a key is renamed when its snake form is a declared property at that location and the key itself isn't. An already snake key, an undeclared key and an `additionalProperties` map key are left alone.
  - Untagged unions: the candidate property set is the union over the object-shaped branches, and a string is normalized when any string-enum branch has the snake member. Map values (for example keybindings command strings) are converted against `additionalProperties`, which leaves strings untouched.
  - Collisions: when the input holds both `holdDuration` and `hold_duration`, the converter fails with a message naming both. It never silently drops one.
  - It returns no warnings of its own. Unknown keys are reported by the typed decode as today.
- **Legacy migrate**: for each registered section the order is: hook, convert against `catalog.schema(id)`, inspect, write. `execOnStartup` and `logging` at the legacy root are converted to `exec_on_startup` and `logging` in `global.jsonc`. Legacy root section names are catalog ids and are never renamed. The output is still pretty JSONC with `.jsonc` names. Report texts use the new key names (`converted exec_on_startup -> global.jsonc`).
- A catalog-level guard enforces the spelling rules: every property name and every string enum value in every registered schema matches `^[a-z][a-z0-9]*(_[a-z0-9]+)*$`, ignoring `additionalProperties` maps and `$schema`. It runs over the composed app catalog (`app/src/catalog.rs`), so a future module that forgets the rule fails tests.

## Acceptance criteria

- [ ] TC-01-01: All ten schemas and `bindings.gen.ts` are regenerated with snake_case names and values only. `bun run check:generated` passes, and the spelling guard test passes over the composed catalog.
- [ ] TC-01-02: Every renamed key and value decodes in its new spelling, and the old spelling warns (key) or fails with a field-path type error (enum value). There's one test per type family, including `preset_on_open`, `split_vertical`, `ease_out`, `visual_crossing`, `starts_with`, `right_click`.
- [ ] TC-01-03: `config init` templates for every registered id decode to their `Default` with zero warnings.
- [ ] TC-01-04: Converter unit tests cover nested renames through `$ref`, arrays, every untagged union listed in decision 10, enum normalization in `oneOf`/`const` form, keybinding map keys left untouched (`"CapsLock+a"`, `"§"`, a key literally named `enabledApps`), unknown keys left as written, already snake input left unchanged (idempotent), and a camel/snake collision failing.
- [ ] TC-01-05: Migrating a realistic legacy file that has every module section, `execOnStartup`, `logging` and `widgets` produces files that `catalog.inspect` accepts with **zero** warnings. The hook-driven cases from today's tests still hold (notunes `targetApp`, keybindings wrap, bar height/padding, wallpapers relative path, tiling warnings).
- [ ] TC-01-06: The OQ-1 outcome for `LayoutType` is implemented and covered by `cli_tests.rs`/`ipc_tests.rs`/`render_tests.rs`.
- [ ] TC-01-07: The smoke harness configs and `scripts/measure/config.ts` (next-gen only) use the new spellings. `bun run test:scripts` passes.

## Tests

- Per-module config tests are updated to snake spellings, and each module gets a test that the old spelling is rejected or warns.
- `cli/src/migrate.rs` tests: converter unit tests (TC-01-04), plus a committed realistic legacy fixture (for example `cli/tests/fixtures/legacy-full.jsonc`) with every section in legacy spellings, including kebab enum values, `preset-on-open`, `dependsOn`, `mouseButton`, gradient/glow borders, per-display gaps and percentage presets. Migrate it into a temp dir, then decode every output with the catalog and assert zero warnings plus a few typed values.
- Composed-catalog tests in `app`: the spelling guard and the template round-trip for every id.
- UI: `bun run test:ui` passes against the regenerated bindings.

## Validation

```sh
bun run generate-schema
bun run generate-bindings
bun run check:generated
bun run lint
bun run format:check
bun run test
```

## Notes

- Docs to update: [architecture/config.md](../../architecture/config.md) (every key and value spelling, migration texts), README.md key spellings, [guides/new-module.md](../../guides/new-module.md) (`rename_all = "snake_case"`, the spelling guard), and the key spellings in every `docs/features/*.md`, including [tiling.md](../../features/tiling.md) CLI layout names per OQ-1. File names stay `.jsonc` in this part.
- Existing per-module `.jsonc` files with camelCase keys now load with unknown-key warnings. Part 03 migrates them, and the branch doesn't merge before part 03.
- Rollback: revert the commit; no persisted state.
