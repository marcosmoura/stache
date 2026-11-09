---
status: draft
---

# 08 — Native check, config extension, production switch and JankyBorders removal

Read when implementing after parts 01–07 and separate approval. The native check needs its own per-run approval under [validation](../2026-11-03-tiling/validation.md#per-run-native-approval). Paths are under `src/crates/`.

## Scope

Remove the `native-borders` feature (native engine always built), `app/src/host.rs` (compose the overlay adapter, drop `BordersParts` at `host.rs:749–762`), delete `macos/src/borders/` (Mach, CLI, encode, transport, fakes) and from `modules/tiling/src/borders/` the runner `runtime.rs`, the argv parts of `policy.rs` (`base_args`, `focus_command`, `gradient_to_janky` strings, `ANIMATION_FPS`/`FRAME_PERIOD`), their fakes and tests; module stop stage and diagnostics that name JankyBorders (`module.rs:528–546`, `module.rs:707–710`, `status.rs:80–81`); exports in `lib.rs:80–86`; the `enabled` schema description in `config/borders.rs:126–131`, the approved OQ-A fields (`BordersConfig::transition`, `BorderAnimationConfig::style`) and regenerated schemas and bindings. Existing config types, validation and defaults stay equivalent in meaning; the only additions are those fields.

## Requirements

- **Approved native check first.** On the user's machine, with approval naming exact test instances (Ghostty and Spotify at least), a temporary Next config, displays and cleanup: run parts 01–07 gate scenarios in one build with `native-borders`; pass requires no border blink on workspace switch or ⌘H/Cmd-Tab in either application, part 05's pass (zero visible borders in Mission Control, Exposé, Space switches, fullscreen transitions and on inactive Spaces), borders following drags live (or the fallback working), every style and animation rendering, zero leaked overlays after stop and quit, and the budget recorded. Any unmet item blocks this part; nothing is removed on a failed or unexecuted check.
- **Config extension (approved OQ-A shape).** Add `transition` and `animation.style` exactly as approved, with defaults that leave every existing configuration's meaning unchanged (absent `style` = `pingPong`, absent `transition` = instant); validation messages follow the existing `borders.<state>.<field>` form; a pure mapping turns them into part 03's motion model. The JankyBorders path never sees them because it is removed in the same part.
- **Switch.** Production composes the native engine; no build contains both paths. `borders.enabled: true` means Stache draws borders itself.
- **Removal.** No code path looks up `git.felix.borders`, opens a Mach port, runs a `borders` CLI or names JankyBorders at runtime. Stache never touches a running JankyBorders: no stop, no reset, no settings change. Status gets no JankyBorders diagnostics.
- **Coexistence.** Docs state that users may keep running JankyBorders themselves, independently of Stache, and should disable one of the two (Stache `borders.enabled` or their JankyBorders) to avoid double borders.
- **Schema text.** The `enabled` description says it turns on Stache-drawn borders for Tiling-managed windows; the consent wording is removed. Schemas and bindings are regenerated (`bun run generate-schema`).
- **Lasting docs (shipped behavior only).** Rewrite the Borders section of [features/tiling.md](../../features/tiling.md#borders): what gets a border, appearance rules (single width, exact gradient angle convention and the visual change from JankyBorders' buckets, focused-only overrides, corner radius), animations (`once`/`loop`, `transition`, legacy ping-pong) with a config example, ignore matching by bundle id and name (not process name, unlike JankyBorders), blink-free ordering, the Spaces and Mission Control guarantee and its fail-closed diagnostic, live follow and its fallback, diagnostics, limits (late hide after external ⌘H, unverified items). Update [architecture/overview.md](../../architecture/overview.md) (owner map: main-thread overlays, Spaces owner, live-follow owner), [guides/testing.md](../../guides/testing.md) (fakes, native gates; replace "contact JankyBorders" wording), [guides/performance.md](../../guides/performance.md) (raw samples and method), mark [Tiling 09](../2026-11-03-tiling/09-jankyborders.md) and validation gate 4 as superseded by this spec, and remove the "planned" line added with this spec.

## Acceptance criteria

- [ ] TB-08-01: The approved native check is recorded with method, raw samples and cleanup result, and meets every pass item; any unexecuted item is listed unverified and blocks removal.
- [ ] TB-08-02: Production uses native borders; `macos/src/borders/` and the JankyBorders runner, transport, stage, diagnostics and exports are gone; a repository search finds no runtime reference to `git.felix.borders`, Mach borders or the `borders` CLI; existing config parsing, defaults and validation tests are unchanged.
- [ ] TB-08-03: Lasting docs and the schema description describe only shipped native behavior, including coexistence advice and limits; the planned line and superseded notes are updated.
- [ ] TB-08-04: Every existing `tiling.borders` fixture (including legacy `animation: { duration, easing }`) parses to the same config and motion as before; `transition` and `style` parse, default, validate and map to part 03's model; `bun run generate-schema -- --check` and `bun run generate-bindings -- --check` pass with the additions.

## Tests

Full gates from [validation](../2026-11-03-tiling/validation.md#every-part) including `bun run generate-schema -- --check` and `bun run generate-bindings -- --check`. Config tests from `borders/tests.rs` that cover parsing, precedence and colors move to the appearance tests, not deleted.

## Review checklist

- [ ] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed for the whole branch: a final search confirms no JankyBorders code or closely paraphrased structure entered the tree; sources are cited in docs as inspiration only.

## Notes

- Rollback: revert this part; parts 01–07 stay behind the feature and production returns to JankyBorders.
- Validation gate 4 (JankyBorders protocol) is retired by this part, not run.
