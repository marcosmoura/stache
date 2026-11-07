---
status: done
---

# 01 — Configuration, catalog and migration

Read when implementing the first Hold-to-Quit part, after explicit approval. This part ships no runtime, factory, tap or HUD.

## Scope

Create the config-only crate through existing catalog/bindings/migration mechanisms. Current `main` `ad34b46` ships noTunes (`ccae030`), catalog-driven typed migration/template emission and `ModuleConfig::migrate_legacy_section`; rebase this older worktree first and reuse them. No new migration framework or legacy rewrite is needed for the unchanged keys.

Files: root `Cargo.toml`/`Cargo.lock`; new `src/crates/modules/hold-to-quit/{Cargo.toml,src/lib.rs,src/config.rs}`; `src/crates/app/src/{catalog.rs,bindings.rs}` and `Cargo.toml`; generated `schemas/commandQuit.schema.json` and `src/ui/src/sdk/bindings.gen.ts`; `src/crates/cli/src/migrate.rs` (remove `commandQuit` from `UNPORTED`) and its tests; `src/crates/app/tests/config_catalog.rs`.

## Requirements

1. Export `CommandQuitConfig { enabled: bool, hold_duration: u64 }`. It uses camelCase serde (`enabled`, `holdDuration`) and `Default` `false`/`1500`. It derives schemars with accurate descriptions (disabled by default, milliseconds) plus Specta `Type`, `Clone`/`Debug`/`PartialEq`/`Eq`, and implements `ModuleConfig`. Use one `CommandQuitConfig::ID = ModuleId::new("commandQuit")` everywhere. There is no second default or template definition.
   1a. TypeScript binding representation: Specta's default TS `number` cannot represent every `u64`, and changing the generator globally would lose precision across modules. Because this module has no status endpoint, settings UI or other TS consumer, `holdDuration` is **excluded from the generated TypeScript surface** (field-level Specta skip/override, or exclusion of this config type from the bindings collection); the JSON Schema keeps the integer `u64` range. A generator test must prove bindings and schema generation succeed and that no TS consumer claims precision. If a future UI needs this value, it must approve an explicit representation (opaque decimal string, or a documented bounded number) rather than a silent lossy number.
2. `validate` accepts every `u64`, including 0 and `u64::MAX`, matching the approved legacy range. No arbitrary bounds or clamping. Schema numeric limits reflect `u64`, not an invented UX limit. Negative, fractional, string and overflowing values fail as type errors with the field path. Runtime checked arithmetic belongs to part 04, not validation.
3. A missing file or `{}` gives the defaults. Malformed, empty or mistyped files stay invalid with diagnostics. Unknown keys get the generic warning, and no exclusion or force key is recognized.
4. Register in the app catalog and the bindings collection. `config init`, `config path`, `schema commandQuit` and both generators pick it up through the existing mechanisms. Catalog-only files are reported as neither bound nor active.
5. Migration decodes legacy `commandQuit` through the catalog's type. A valid section becomes `commandQuit.jsonc` with its values. An invalid section fails migration as other typed sections do. An absent section emits the commented default template. The source, collision, `--force` and atomic-write rules stay unchanged, as do the other unported copies. The report prints the conversion.
6. Nothing in config loading, migration or generation creates a factory, tap, observer, panel or worker.

## Acceptance criteria

- [x] HQ-01-01: Config defaults, serde casing, type rejection, `ModuleConfig`, schema and Specta derive from one type. The schema says disabled by default, the generated TypeScript does not export the lossy `holdDuration`, and generated checks are deterministic.
- [x] HQ-01-02: Every `u64`, including 0 and `u64::MAX`, is accepted without clamping or arbitrary limits; wrong types and overflow fail with a field path.
- [x] HQ-01-03: `config init`/`path`/`schema commandQuit` include the file. Migration of a valid, invalid or absent legacy `commandQuit` is typed, and the other unported sections are still copied verbatim.
- [x] HQ-01-04: No runtime, factory or native side effect is introduced. Catalog-only state stays inactive; proxyAudio, noTunes and subsequently landed catalog consumers pass unchanged regression tests.

## Tests

- Config: missing, `{}`, explicit values, `holdDuration` of 0 and `u64::MAX`, negative/float/string/overflow rejection, unknown-key warning, the template golden and the schema golden. A generator test asserts bindings build without a lossy `holdDuration` export and that `check:generated` is clean.
- Migration (CLI tests use the injected catalog): legacy `{enabled: true, holdDuration: 800}`, an invalid `holdDuration`, an absent section, an existing destination with and without `--force`, a source that is never overwritten, and unchanged copies of the other unported sections.

## Notes

Validation, run after implementation and not during planning:

```sh
cargo nextest run -p stache-hold-to-quit -p stache-cli -p stache-sdk
cargo nextest run -p stache-app --features dev-tools
bun run lint
bun run format:check
bun run test
bun run check:generated
RUSTDOCFLAGS='-D warnings' cargo doc -p stache-hold-to-quit -p stache-app --no-deps
```

Regenerate with `bun run generate-schema` and `bun run generate-bindings`. In [config.md](../../architecture/config.md), add `commandQuit.jsonc`, the schema ID and the typed migration rule. Keep the [feature notice](../../features/hold-to-quit.md) marked config-only, not a runtime.
