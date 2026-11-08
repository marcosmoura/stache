---
status: done
---

# 01 — Config, catalog and migration

Read when implementing the config-only foundation, after rebase and separate approval. No runtime factory, window, provider or UI command ships here.

## Scope

Add `src/crates/modules/bar/{Cargo.toml,src/lib.rs,src/config.rs}` (`stache-bar`), workspace/app dependencies, `app/src/{catalog,bindings}.rs`, catalog tests and generated schema/bindings through existing generators. Reuse main's `ModuleConfig::migrate_legacy_section` (`src/crates/sdk/src/module.rs:78`)/`ConfigCatalog::rewrite_legacy` (`src/crates/sdk/src/config/catalog.rs:130`), the catalog in `src/crates/app/src/catalog.rs:20-31` and CLI catalog dispatch. Remove `"bar"` from `UNPORTED` (`src/crates/cli/src/migrate.rs:21`, keep `"tiling"`) so its arm at `:211-214` no longer matches Bar; add no new CLI policy branch.

## Requirements

- Authoritative `BarConfig`, ID `bar`, `bar.jsonc`, serde camelCase, schemars/Specta. Defaults: `enabled: false`, `height: 28`, `padding: 12`, `weather: { provider: "auto", apiKeys: "", defaultLocation: "" }`. Dimensions retain legacy u16 wire range; preserve explicit zero. Weather enum uses `auto`, `visual-crossing`, `open-meteo`. No widgets, settings or new CLI grammar.
- Structural decode/defaults and native feasibility are separate. Geometry uses checked `height + padding` in a wider type. Preserve explicit zero without substituting defaults; at runtime zero height or nonpositive computed width means Bar runs with no panel ([Bar part 03](../2026-11-03-bar/03-panel-window.md), [Bar overview](../2026-11-03-bar/00-overview.md) Decision 16), never a silent clamp. Padding that leaves no width follows the same no-panel rule; frames beyond backend limits fail start/reconfigure actionably; rejected live candidates retain last-good resources. Do not narrow historical config values merely to fit the current 8192 window validation.
- Migration validates through the catalog. Keep explicit values including 0, materialize omitted height/padding as 28/12 and warn that old derive-default omissions rendered 0. Missing Bar section emits the catalog's disabled template, never enables itself. Retain unknown keys with warnings; widgets still dropped. Immutable source, source-is-destination refusal even with force, collision preflight, file modes and unrelated sections stay unchanged.
- Normalize `weather.apiKeys` by trimming then injected home expansion (`~`/`~/`) and config-directory-relative resolution; never shell/env expansion or process-CWD resolution. Migration retains relative text and warns that the destination config directory may differ from legacy's base/CWD fallback; do not guess historical CWD. Absolute/home-anchored paths retain meaning. No env file I/O during schemas/templates/config validation.
- Main noTunes catalog/migration/bindings and all currently landed modules survive additive edits. Registration is catalog-only, with no production Bar factory, until [Bar part 04](../2026-11-03-bar/04-ui-shell-parity.md); `enabled: true` can validate but must not claim active runtime.

## Acceptance criteria

- [x] BAR-01-01: Authoritative defaults/field shapes/schema/templates/bindings agree, compile and add no runtime activity or command grammar.
- [x] BAR-01-02: Typed migration preserves explicit values, corrects omissions with warnings, preserves source/collision/modes/unknown keys and existing modules' behavior.
- [x] BAR-01-03: Path normalization is deterministic with injected home/config context and ambiguity warning; config-only operations read no secrets or native state.
- [x] BAR-01-04: Generated drift checks and targeted config/CLI tests pass; config/feature docs describe only this slice as shipped.

## Tests

Colocate config tests in the new crate; extend `app/tests/{config_catalog,bindings,generator,cli_binary}.rs` and CLI migration tests. Fixtures: absent/empty object versus empty file, malformed/wrong bool, every omitted nested field, unknown key, all enum spellings, negative/overflow dimensions, explicit 0/max u16, omission warnings, missing Bar section, force/no-force, source alias and same destination, template ordering and noTunes target conversion regression. Temporary home/config/CWD inputs only; env-file access recorder remains unused. Compile-check consumers and regenerate rather than edit generated files.

## Notes

Update config docs' registered/copy lists (`docs/architecture/config.md:321-332`) on implementation, not now. Native geometry and accepted live configuration are owned by Bar part 03; this part does not invent a runtime placeholder that reports Running.
