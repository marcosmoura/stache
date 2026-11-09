---
status: done
---

# 02 — Pure layouts, rules and geometry policy

Read when implementing after 01 and separate approval. No native actions/production registration.

## Scope

Add Tiling `src/{geometry.rs,rules.rs,layout/,presets.rs,minimum_size.rs}` and pure tests. Source mapping: inventory layout/rules/state rows. Expose pure `LayoutPlan` containing desired frames, overflow identities and capacity/geometry diagnostics, not an untyped vector that silently loses windows.

## Requirements

- Preserve dwindle spiral/node ratios; split auto/vertical/horizontal cumulative boundaries; master ratio/position (auto left landscape/top portrait); monocle full usable frame; floating writes nothing. Geometry is finite logical points; checked math handles zero usable area, negative origins, excessive gaps and overflow explicitly. Preserve actual gap resolution: first configured entry matching case-insensitive display name OR applicable main/primary/secondary alias, then first entry, then zero; a later exact name does not outrank an earlier matching alias (`_legacy/app/native/src/modules/tiling/layout/gaps.rs:142–169`). Inner per-side averages, outer applied once. Accept pure full logical display bounds, native visible-frame insets and generic host reservation insets (absent host = zero). Each base edge exclusion is max(native inset, host aggregated reservation), then add own outer gap once. Do not add reservations to an already inset visible frame. Part 05 owns subscription/normalization before any frame writes; 10 only final integration. Full top0/native24/reservation40/own6 yields top46, not70.
- Preserve AND matching and first matching workspace/rule order. Exact case-insensitive appId; case-insensitive appName/title substrings; empty rule never matches; global ignore/system blacklist/current self/PiP exclusions. Fix `_legacy/app/native/src/modules/tiling/rules/mod.rs:52–54`: fallback compares rule appId against **window** appId. Prepared and unprepared results must be equivalent, including Unicode title/name and missing metadata.
- Preserve non-Grid creation order from `_legacy/app/native/src/modules/tiling/actor/handlers/window.rs:147–164`: insert after the focused window in the assigned workspace, else append. **Grid exception (approved stable-cap correction):** append new eligible windows at end rather than displacing existing tiles after focus. First ten otherwise eligible entries in actor-maintained stable workspace layout order tile. Eligibility excludes explicit floating, native inactive tabs, fullscreen, minimized, hidden/nonvisible tiles. Extras remain tracked with `GridOverflow`, preserving observed/app-provided frames (no automatic center/preset/frame write); no focus-based replacement. At eligibility removal promote earliest remaining eligible extra; explicit swap/order commands may change order, focus cannot. Recompute from ordered eligibility, never mutate explicit user-floating choice.
- Leaving grid removes only overflow classification; ordinary eligible windows tile in the selected nonfloating layout without a ten cap. Explicit user-floating windows remain floating. Entering a floating layout is not an explicit user toggle and preserves flags/order; returning restores eligibility. A window newly seen while overflowed receives no automatic open preset/center later merely because a refresh occurs. Promotion tiles it, not floats/centers it.
- Floating event policy: initial adoption is not a new-window event. New ordinary floating windows (including new windows in floating layout) and explicit tiled→user-floating toggle get defaultPosition once. `center` preserves observed size and centers on owning display; `default` leaves frame. Explicit preset in a Floating workspace outranks newly floating defaultPosition; outside that workspace layout it is a silent admitted no-op, including individually floating windows (`_legacy/app/native/src/modules/tiling/actor/handlers/preset.rs:21–49,166–195`). No fictitious Applied frame; internal NoOp reason may diagnose eligibility. `preset-on-open` is declared only at `_legacy/app/native/src/config/types/workspaces.rs:109–115`, with no runtime reads anywhere in legacy native source; preserve it inert pending separate approval, never auto-apply it. Preset `center=true` ignores x/y; percentages resolve against normalized display usable geometry. Preserve case-insensitive preset lookup, special50% width/height and50% x/y inner-gap correction, size/position clamp from `_legacy/app/native/src/modules/tiling/layout/floating.rs:63–125`. Grid overflow bypasses automatic preset/centering; a later eligible **explicit** preset/toggle is a user action, not overflow automation. Layout change/reload/focus/reconcile never repeats positioning.
- Respect reported/inferred minimums when feasible; preserve ratio redistribution, user master override, balance clearing inferred constraints, stale split boundary invalidation on membership change, unchanged-layout/balance no-op semantics. If geometry cannot satisfy constraints or app refuses resize, preserve actual frame/capability with a diagnostic, not fictitious min-size compliance. Bound pure solving work; no retry-to-convergence loop.

## Acceptance criteria

- [x] TI-02-01: All eight pure algorithms/gaps/master/ratio/preset cases preserve intended legacy geometry with deterministic finite plans and explicit infeasible outcomes.
- [x] TI-02-02: Grid 0/1/10/11/large cases retain every identity; stable overflow/promotion/layout switching never center, auto-preset, focus-displace or overwrite explicit floating.
- [x] TI-02-03: Prepared/unprepared rule tests catch the appId bug, preserve AND/first-match/exclusions and missing/Unicode metadata behavior.
- [x] TI-02-04: One-shot floating-position/preset precedence and bounded minimum-size handling cover unresizable and impossible geometry without native assumptions.

## Tests

Golden/property geometry fixtures for portrait/landscape/scales1/2/negative origins, all four normalized edges (including top46 example), invalid/infeasible geometry, all grid counts 0–10 plus 11/1000, hide/minimize/tab eligibility changes, swap/focus and explicit-floating overflow transitions. Contrast non-Grid insert-after-focus/no-focus append with Grid eleventh-window append (first ten unchanged), stable promotion and explicit swap. Test preset in Floating workspace versus individually floating in tiled workspace, explicit precedence, inert preset-on-open and overflow no-autopreset. Fake observed fixed-size windows and refusal/partial capability inputs. Compare pure fixture outputs, never run legacy code/tests. [Validation](validation.md) applies.

## Rollback and affected docs

No production activation to undo. Keep pure API compiling when rolling back consumers. Update feature planned geometry/correction documentation and testing guide only for landed pure APIs, never claim native resizing works.
