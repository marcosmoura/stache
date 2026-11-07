---
status: draft
---

# 01 — Typed configuration and inert catalog

Read when implementing after the overview's sequencing gate (Bar foundation merged) and separate approval.

## Scope

Add `src/crates/modules/tiling/{Cargo.toml,src/lib.rs,src/config.rs,src/config/}` with `TilingConfig: ModuleConfig`; package `stache-tiling`, ID `tiling`. Add workspace/app manifest wiring, app `catalog.rs`, `bindings.rs`, generated schema/bindings through generators, catalog/template/migrate tests. No runtime factory/observer/worker/effect starts here. Legacy mapping: inventory config row.

## Requirements

- Preserve `enabled=false`, `defaultLayout=dwindle`, empty workspaces/ignore, animations `{enabled:false,duration:200,easing: "ease-out"}`, floating `{defaultPosition:"center",presets:[]}`, master `{ratio:60,position:"auto"}`, zero gaps and disabled borders. Preserve all six easing variants and all eight layout names; one authoritative layout enum/default across config/state/CLI projections.
- Preserve workspace name, optional layout, screen default `main`, rules and exact `preset-on-open` spelling; aliases main/primary/secondary/named display. Preserve gap uniform/per-axis/per-side and global/per-display forms, pixel/percentage preset dimensions, x/y/center precedence, full border state/layout override/gradient/glow/color syntax. Serialize user shapes, never lowercase caches/native tokens.
- Authoritative typed validation covers duplicate workspace/preset names, missing referenced presets, no-field rules, malformed/nonfinite dimensions/colors/ratios, arithmetic overflow, unsupported style and unusable geometry. Preserve valid legacy values; do not silently invent defaults for supplied invalid values. Freeze accepted/rejected fixtures against legacy types; any additional rejection of formerly accepted input must be documented and approved before acceptance, not silently broadened here. Empty rule remains nonmatching rather than match-all.
- Catalog-driven `migrate_legacy_section` is already shipped: reuse it and register `tiling`, replacing only its pass-through migration. Retain valid explicit values/default semantics; warn about intentional grid/position/default corrections. Source untouched, collisions/force/unknown warnings follow SDK. No custom parallel migrator.
- Typed config/schema/template/bindings/defaults agree. Catalog registration is inert even if config says enabled. Rebase preserves all earlier module registrations. No Settings UI/new user settings.

## Acceptance criteria

- [ ] TI-01-01: All legacy config shapes/defaults round-trip with one dwindle enum and explicit validation/compatibility fixtures; generated schema and TypeScript agree.
- [ ] TI-01-02: Init/path/schema/migrate use the authoritative catalog hook, preserve preceding modules, handle invalid/missing/duplicate/collision cases and start zero native resources.
- [ ] TI-01-03: Configuration/feature/index docs describe catalog support as inert, with corrections and later runtime behavior still planned.

## Tests

Temporary injected config directories; missing/empty/invalid/unknown fields, percentage/overflow/border variants, prepared/unprepared rule shape, migration omitted/explicit defaults and every existing registered module. Compile crate/export/binding consumers. Run relevant fake checks in [validation](validation.md).

## Rollback and failure gates

Rejected input leaves last accepted state/revision; no fallback to enabled defaults. Remove only this part's inert registration to roll back; never delete user's files. No native tests. A changed legacy valid-input decision blocks acceptance for approval.

## Affected docs

`docs/architecture/config.md`, `docs/architecture/ui.md`, `docs/features/tiling.md`, index; state clearly no Tiling runtime yet.
