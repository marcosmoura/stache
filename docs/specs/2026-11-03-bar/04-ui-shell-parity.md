---
status: done
---

# 04 — Static UI and visual-parity gate

Read when porting the Bar surface after 03 and separate approval. **No visual changes at all**; a new design is not an acceptable interpretation of porting.

## Scope

Add the `src/ui/src/shell/windows/Bar/` window with component directories for Media, Status, Clock, Cpu, Battery, KeepAwake, Wifi, Weather under it; each owns implementation, `.styles.tsx`, colocated tests and `index.ts`. Shared primitives go in `src/ui/src/shell/components/` (created by this part) with the same directory convention. Support/fixtures only in `src/ui/tests`. Extend the `Shell` window router (add a `bar` entry to the `WINDOWS` table in `src/ui/src/shell/Shell.tsx`, next to the Accessibility gate's `access` surface), the existing `src/ui/src/sdk/{client,queries,statusFeed}.ts`, a new `src/crates/app/capabilities/bar.json` plus the `security.capabilities` list (`src/crates/app/tauri.conf.json:18`), and production factory registration (a `BarFactory` modeled on `TilingFactory`, `app/src/host.rs:667`, chained in `app/src/lib.rs:453-471` next to Tiling, default-disabled and inert at construction). Static fixtures/read-only unavailable data work before later providers; never invoke unimplemented commands. This part activates the config-disabled-by-default factory only after the route and capability exist.

## Requirements

- Port actual `_legacy/app/ui/renderer/bar/**`, its stores, `design-system/{colors,motion}.ts`, `main.css` and transitive `components/{Surface,Button,Stack,Icon,ScrollingLabel,QueryErrorFallback}`. Audit DOM, measurements, icon SVG paths/strokes, every style and data-dependent state; do not infer from screenshots alone. No aesthetic skill/redesign, new status badges, loading skeletons, tooltips, adaptive layout or clickable-state styling.
- Preserve root `'Maple Mono NF', 'SF Mono', monospace`, 13px/500/tabular numbers/1.6em and smoothing/reset/reduced-motion rules. Maple Mono NF is system-installed, never bundled. Scope legacy root rules to Bar as necessary without altering computed values or existing shell/test styles; missing font is a recorded environment failure/fallback case, never permission to pick a substitute font.
- Preserve grid/space-between layout with first empty `<div />` slot from `Spaces.tsx` when tiling absent. Also reproduce the legacy `SpacesFallback`: `<Stack><Surface className={styles.fallback}>Loading...</Surface></Stack>` with `display:inline-flex; align-items:center; padding:0 10px`, shown while `useSpaces` is suspended. It is a real legacy startup state, not permission for a new loading treatment; if the new data seam can never suspend, record that state as unreachable and why instead of silently dropping it. No workspace/app strip, icon maps, focus or navigation port until final Tiling spec. Do not delete the reserved slot or redistribute status/media to occupy it. Status order is Weather, CPU, Battery, Keep Awake, Wi-Fi, Clock. Media stays a fixed-centered overlay (`left:50%`, translateX(-50%)), not a third flex column; height 100%, padding-left 1px, label max 560px/400px at `(width <= 2036px)`, artwork 24×24/radius10. Preserve overlap/clipping rather than introducing an unapproved collision layout.
- Surface radius12/crust/text, Button grid gap6/padding0 10px/border1px and hover/active colors match source. Preserve 150ms cubic-bezier transitions; Surface uses the **actual** `motionRaw.durationSlow` 0.275s, not similarly named CSS 400ms. Preserve scrolling labels and icon sizing. Menu-visible hide is translateY(100%) + opacity0 with original timing. Check keyframes and intermediate samples as well as settled screenshots.
- Static fixture seam reproduces all shipped legacy visual branches before live data: media absent/paused/artwork load/error/long title, battery absent/full/unknown/compact/charging, CPU no temperature/thresholds, Wi-Fi unknown/off/disconnected/missing SSID/signal, weather loading/unconfigured/error, Keep Awake pending/active/inactive, global Suspense/error fallback. No fabricated native readings. Two Keep Awake visual states only; unavailable maps to inactive once resolved, initial unresolved follows legacy null. Later parts change data/actions, not these styles.
- React Query owns canonical server snapshots; subscribe-before-fetch and sequence/generation checks reject older initial responses. Zustand is UI-local only (animation/hover local state), never duplicate native/config/provider state. Dispose late subscriptions under StrictMode. Existing shell/test routing remains unchanged; unknown surfaces remain inert. Generated names only; compile/check binding additions.

## Independent reference process

Do not implement the baseline in this documentation task. Implementation must choose one feasible reference path and record user approval of its provenance before accepting parity:

1. Read-only isolated reference markup/CSS/SVG fixtures extracted from the pinned legacy sources, reviewed against originals. Render in the existing pinned WebKit harness with no legacy runtime imports, legacy pnpm install, application build or automation. Resolve Linaria tokens/styles to reference CSS with a small auditable extraction/fixture step; it may use current tooling, not the entire legacy dependency graph. Pin external icon package versions/SVG hashes from legacy lock/assets, with license/provenance. Reference fixtures are not produced by the new Bar components or shared new styling helpers.
2. User-supplied approved legacy screenshots plus environment/data/style measurement records matching the matrix below. If an image lacks enough provenance/computed-style evidence, it cannot independently pass all gates; request missing evidence, not synthesize a reference from new UI.

Pinned source commit: `e0b4648ed22891d3ba29abf3d8fa8f56f949c657`. Git object IDs (tree IDs recursively pin child paths/content; main.css is a blob):

| Legacy path under `_legacy/app/` | Git object                                 |
| -------------------------------- | ------------------------------------------ |
| `ui/renderer/bar`                | `57d4212a9f591d3230076918a2f6527b7baf5a67` |
| `ui/stores`                      | `2dc990c01a99673cdce2260f577896283f1d909a` |
| `ui/design-system`               | `b8af6c5b4cdd411d5a4c752b89c18dcff772d050` |
| `ui/components`                  | `f935047c48d6cb595f12f0ea24a98327bb41bc55` |
| `ui/main.css`                    | `d9080d2880fc419ada4133530a2eec31ec4d18b8` |
| `native/src/modules/bar`         | `093dc10ac555e036f70eb71c520653bcea6f9539` |

Reference manifest additionally records SHA-256 of extracted markup/styles/icons/images, transitive hooks/utilities, legacy lockfile and screenshot inputs; font name/version/file hash (measurement only, not redistribution), OS/browser build, viewport/device scale and color profile. Verify original object IDs and extraction hashes before comparison. An extraction correction needs review and provenance update, not silent baseline regeneration. Fail if reference points to new component/style output or two copies of the new implementation. Approved source updates require user review.

## Acceptance criteria

- [x] BAR-04-01: Static surface, component structure and scoped primitives compile with only generated contracts; factory activates only with implemented route/capability and disabled owns no webview.
- [x] BAR-04-02: Independent legacy reference provenance is verified; exact pixel/computed-style/geometry/icon/animation parity matrix passes with no unapproved deviation.
- [x] BAR-04-03: Tiling-absent slot, the legacy Spaces Suspense "Loading..." fallback (or a recorded unreachable-state justification), centered media, status order, all empty/error/hover states and breakpoint behavior match sources without affecting existing UI.
- [x] BAR-04-04: Query feed races/disposal/authorization pass; UI/feature/testing docs accurately distinguish static surface from later providers and native gates.

## Tests

Colocated WebKit visual tests freeze data, wall clock/timezone/locale, random seed, browser/OS, color scheme/profile, font loading (`document.fonts.ready` plus metrics/hash check), device scale and animation timeline. Compare laptop 1440 and external 2560 logical-display fixtures, widths 2035/2036/2037 at actual CSS viewport, scale1/2, default and explicit custom dimensions. Record display width versus panel viewport (`display - 2*padding`) to avoid testing the wrong breakpoint. Cover normal/reduced motion, hover/active, hidden/reveal at t=0/midpoint/end, scrolling overflow and each data branch above.

Pixel screenshots require exact equality in the pinned environment; no blanket threshold, masking of Bar regions or auto-update. If rasterization noise is proven, a narrowly documented tolerance needs user approval; geometry/computed-style/SVG path checks remain exact. Compare bounding rects, grid placement, padding/gaps, font metrics, colors, borders/radii, transforms, transitions and visibility to independent reference. Intentionally perturb font/spacing/icon/centering and provenance in negative tests to prove the gate catches them. Screenshot-only similarity is insufficient.

## Notes

No test launches legacy app/build/automation or native Bar. Missing approved baseline/font/native gate is **blocked/unverified**, never a pass. Any visual deviation, even an apparent improvement or dependency-induced icon change, requires approval before acceptance. Later parts extend this same gate; they do not reset baselines.

Completion record (2026-11-10):

- Reference path: option 1. Provenance (pinned `e0b4648`, six verified objects, legacy-lock icon packages with integrity, rendered by running the pinned legacy components) and the harness accounting in `src/ui/tests/bar-parity` (reference `<html lang="en">`, ignored Stylis-only `-webkit-box-align/-pack`, normalized hashed keyframe name, React's two CPU text nodes, legacy scroll measurement re-applied on the reference layout, forced relayout before capture) approved by the user. The `motion` fade mid-samples use a 5e-4 numeric bound against the exact legacy bezier (approved); pixels and geometry are exact.
- Matrix: 65 scenarios, panel viewports 1416/2536 (padding 12), 1412/2532 (padding 14), 2035/2036/2037, heights 28/32, scale 1 and 2, reduced motion, hover/active, hide/reveal samples, scrolling labels, negative perturbations; all pixel-identical with no new-UI change. Runs as `bun run test:parity` (user decision: separate from `test:ui`).
- `SpacesFallback` is ported and tested but unreachable in production: no Bar data source suspends until Tiling–Bar part 10.
- Live run (user-observed, macOS 27.0, `bun run tauri:dev`, migrated test config, Bar 28/14): the bar renders on the built-in display, the clock matches legacy, menu-bar auto-hide hides and reveals the bar (closes part 03's live menu-bar item), Tiling places windows at y = 56 (42 reservation + 14 gap). Click actions are part 05.
- Live investigation (2026-11-10, user-approved runs A–E with temporary diagnostics, since removed): Bar is not implicated in three issues seen during the live check. (1) PosterBoard crash reports (`-[PRPosterDescriptor _initWithPath:]` assertion during its wallpaper store setup) also occurred with no Stache running. (2) Tiling's stop fails with Bar off as well: its restore reports `still owes an unhide (AXHidden write accepted but not confirmed by readback)`. (3) Bursts of identical `NSApplicationDidChangeScreenParametersNotification` re-reads (435 and 1136 in two long sessions) did not reproduce in 3-minute idle runs with Bar off or on; with Bar on, Bar refreshed its topology once and never repositioned. Items 2 and 3 are Tiling follow-ups outside this branch.
