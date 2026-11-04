---
status: draft
---

# 01 — Configuration and migration

Read when implementing the inert MenuAnywhere configuration. Approval is required before this part.

## Scope

Add the authoritative typed configuration, catalog entry, schema and generated type export. No runtime, factory registration, input subscription, AX access or menu presentation. An enabled file still cannot run MenuAnywhere in this part.

## Files and API boundary

- New `src/crates/modules/menu-anywhere/Cargo.toml`, `src/lib.rs`, `src/config.rs` (paths under that crate): package `stache-menu-anywhere`, `MenuAnywhereConfig`, `MenuAnywhereModifier`, `MenuAnywhereMouseButton`; config tests alongside source.
- Root `Cargo.toml`/`Cargo.lock`, `src/crates/app/Cargo.toml`: workspace/package wiring using established dependency versions, not a dependency upgrade.
- `src/crates/app/src/catalog.rs`: additive `catalog()` registration. `src/crates/app/src/bindings.rs`: add config and enum types to the existing builder, preserving all existing exports.
- `src/crates/cli/src/migrate.rs`: remove `menuAnywhere` from `UNPORTED`; extend its fake-catalog and integration tests. Reuse shipped registered-type dispatch, `rewrite_legacy` and `ModuleConfig::migrate_legacy_section`; no second converter or unnecessary SDK hook.
- Generated `schemas/menuAnywhere.schema.json`, `src/ui/src/sdk/bindings.gen.ts`; existing schema/bindings generator sources only if their current catalog/type-driven entry points actually require an additive entry.
- `src/crates/app/tests/` existing config/CLI integration suites: extend production-catalog checks at their rebased paths. Update `docs/architecture/config.md` and the planned feature notice with the narrower shipped-config/inert-runtime distinction when this part is accepted.

All MenuAnywhere types are planned. The catalog/migration infrastructure already ships on current `main`; rebase first, do not recreate noTunes' implementation.

## Requirements

`MenuAnywhereConfig` implements SDK `ModuleConfig`, serde, schemars, Specta, `Default` and typed equality. Exact ID is `menuAnywhere`; JSON fields are `enabled`, `modifiers`, `mouseButton`. Defaults: `false`, `["control", "command"]`, `"rightClick"`. Enums retain `control|option|command|shift` and `rightClick|middleClick` spellings. `{}` uses per-field defaults. No added keys, preset, activation option or configurable budget.

Empty modifiers remain valid: warn in schema field descriptions, generated template comments and behavior docs that `[]` captures unmodified matching clicks system-wide while enabled. Do not reject or silently insert modifiers. Duplicate modifiers remain valid and OR to one bit; preserve vector serialization/order rather than normalize users' files. Matching uses equality of the four supported modifier bits, ignoring unrelated flags such as Caps Lock, as legacy does. Keep native Quartz types out of the config crate; pure normalized mask/filter preparation is allowed, not an input backend.

Use ordinary catalog unknown-key warnings, field-path type failures and last-good configuration behavior. Unknown enums, null/wrong-type fields and nonobject roots fail typed loading/migration. No legacy transformation is needed: keep the default migration hook. Existing generic migration validates before writes, preserves unknown section keys with warnings, emits missing registered sections as commented default templates, and retains existing source/collision/force rules. Migration accepts `[]` and middleClick without rewriting them; describe the runtime middle-button correction separately.

Catalog registration is inert: standalone `schema`, `config init/path/migrate` must never construct a runtime or start a desktop. An unbound registered MenuAnywhere file is not falsely marked active. Preserve shipped noTunes migration spellings, proxyAudio catalog behavior and later landed entries.

## Acceptance criteria

- [ ] **MA-01-01** Defaults, camelCase roundtrip, all enum spellings, empty/duplicate modifiers and normalized four-bit equality match the contract; invalid types/enums retain field diagnostics.
- [ ] **MA-01-02** Production catalog/template/schema/bindings expose the same type and caution; deterministic generation checks pass without removing any prior module exports.
- [ ] **MA-01-03** Migration validates MenuAnywhere generically, preserves unknown keys with warnings, creates the missing-section default, and rejects bad sections before any write; source bytes, collisions and unrelated files are preserved.
- [ ] **MA-01-04** Config commands and registration remain inert, with no MenuAnywhere factory/subscription/native effects and no unbound-file active marker; noTunes/proxyAudio regression tests pass.

## Tests and validation

Pure config/table tests for every modifier subset, duplicates, `[]`, extra/missing flags and malformed fields; migration temp-directory tests for all listed outcomes plus `--force` and source-is-destination refusal. Use injected environments, never real home/config or environment mutation. Test the actual production catalog in app integration, not only a duplicate test catalog.

After implementation: `cargo nextest run -p stache-menu-anywhere -p stache-sdk -p stache-cli -p stache-app --features stache-app/dev-tools`; `bun run generate-schema -- --check`; `bun run generate-bindings -- --check`; `bun run lint`; `bun run format:check`; `bun run test`; `bun run build`; `RUSTDOCFLAGS='-D warnings' cargo doc -p stache-menu-anywhere -p stache-sdk -p stache-cli --no-deps`. Generation writes are explicit implementation tasks, not part of this docs-only run. No native manual check is needed in this inert part.

## Notes

This part is independently committable but not permission to merge unfinished parts. Native rendering belongs to [02](02-native-menu.md); service-dependent behavior to [03](03-runtime.md). Do not add a fake runtime merely to register the config.
