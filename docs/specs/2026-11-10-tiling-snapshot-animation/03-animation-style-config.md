---
status: draft
---

# 03 — `animations.style` configuration

Read when adding the `animations.style` key. **DRAFT — NOT SHIPPED.** This part needs separate approval. Paths are under `src/crates/` unless noted.

## Scope

Follow the `EasingType` precedent everywhere it appears:

- `modules/tiling/src/config/layout.rs:49-90`: `EasingType` at 49-67 and `AnimationConfig` with its `Default` at 69-90.
- `modules/tiling/src/config.rs:17` (re-export) and `:41` (the `animations` field).
- `app/src/bindings.rs:369-370` (`.typ::<EasingType>()`, `.typ::<AnimationConfig>()`).
- `schemas/tiling.schema.json` (repository root) and `src/ui/src/sdk/bindings.gen.ts`, regenerated with `bun run generate-schema` and `bun run generate-bindings`.
- Runtime selection: `modules/tiling/src/animation/control.rs:320` (`AnimationControl::configure`) and `status.rs` (diagnostics, next to `animation_diagnostics` at `status.rs:114`).
- Tests:
  - `modules/tiling/src/tests.rs:30-50` (defaults), `:58-92` (easing parse and reject) and `:299-311` (schema, template and default);
  - `app/tests/config_catalog.rs` (for example the easing cases near lines 1756, 1846 and 2050);
  - `app/tests/toml_scenarios.rs:320-323`;
  - `app/tests/fixtures/per-module/tiling.jsonc:5` (the legacy-migration fixture; add the key only if the migration path carries it).

## Requirements

- **Type.** Add `AnimationStyle { Snapshot (default), Live }` with `serde(rename_all = "snake_case")`, deriving the same traits as `EasingType` (including `JsonSchema` and `specta::Type`) and documenting both variants.
- **Field.** Add `AnimationConfig.style: AnimationStyle`. The default is `snapshot`. `enabled = false`, `duration = 200` and `easing = "ease_out"` are unchanged. Re-export the type next to `EasingType` and register it in `bindings.rs`.
- **Strict parsing.** Only `"snapshot"` and `"live"` are accepted. Any other value, including `"Snapshot"` and `"snap-shot"`, is rejected with the key path `animations.style`, as `animations.easing` is today.
- **Ignored while disabled.** With `enabled = false`, `style` has no effect: no diagnostic, no thread, no capability check.
- **Path selection (this part).** The runtime reads the style on every accepted configuration through `AnimationControl::configure`. Until part 04 lands, `snapshot` behaves exactly as `live`, and while animations are enabled and the style is `snapshot`, status `diagnostics` show exactly one line: `animation: snapshot animation is not available yet`. `live` shows no such line. A reload that changes the style takes effect on the next plan and never restarts the animation worker.
- **No permission work here.** This part never preflights or requests Screen Recording.
- Regenerate the schema and bindings so `bun run check:generated` passes.
- **Docs.** In `docs/features/tiling.md` Animation, document the key, its default and that `snapshot` currently behaves as `live` with the diagnostic. Update the `animations` default listed in `docs/architecture/config.md:107-108`.

## Acceptance criteria

- [ ] SA-03-01: `AnimationStyle` and `AnimationConfig.style` exist with default `snapshot`. The default object, schema, template and generated bindings agree, and `bun run check:generated` passes.
- [ ] SA-03-02: `"snapshot"` and `"live"` parse and serialize. Unknown values are rejected at `animations.style`, in both the module tests and the catalog/TOML tests.
- [ ] SA-03-03: With animations enabled and `style = "snapshot"`, status shows exactly `animation: snapshot animation is not available yet` and windows animate live. With `live`, or with animations disabled, no line appears. A style reload never restarts the worker.
- [ ] SA-03-04: The feature and config docs describe the key and the interim behavior.

## Tests

- `modules/tiling/src/tests.rs`: the defaults include `style: Snapshot`, both values parse, `"bounce"` and `"Snapshot"` are rejected, and the schema contains `AnimationStyle`.
- `app/tests/config_catalog.rs`: a reject case at `animations.style`, and the template contains `style`.
- `app/tests/toml_scenarios.rs`: a TOML scenario with `style = "live"` next to the existing `[animations]` block.
- Status test: the exact interim diagnostic, present only when enabled with `snapshot`.
- `module_tests.rs`: a reload from `snapshot` to `live` and back keeps the same worker (`AnimationControl::is_running` and `retired() == 0`).

## Notes

- Part 04 replaces the interim diagnostic with real snapshot behavior and removes it.
- Rollback: remove the field. Configurations that set `style` are then rejected as an unknown key, so record this in the feature doc if the part is ever reverted.
