---
branch: feat/bar
---

# Bar rewrite

Read when planning the Bar module. This is a draft, not shipped behavior or permission to implement. Obtain approval before implementing each part.

## Goal

Rebuild the legacy status bar as `stache-bar`: one non-activating panel on the primary display, visually identical to legacy, fed by typed providers. Land the generic SDK published-state channel and the host-owned screen reservation registry that Tiling consumes later.

## Non-goals

- No widgets (permanently removed), Settings UI, tiling integration (Spaces/workspaces/app list/focus/top-gap consumption), per-display bars, new CLI command or visual change of any kind.
- No generic launcher endpoint, Keep Awake mutation endpoint, Keep Awake-owned assertion or SkyLight call without separate approval.
- No fix for the known Cmd-Q/logout coordinated-teardown gap.

## Decisions

Approved by the user; parts must not re-decide them.

1. **Visual identity.** Layout, sizes, spacing, colors, fonts, icons, animations, transitions, hover/active states, media overlay, laptop media query (`width <= 2036px`) and empty states equal legacy. A visual-parity gate ([04](04-ui-shell-parity.md)) blocks merge; any deviation needs user approval.
2. **Module.** Crate `src/crates/modules/bar`, package `stache-bar`, catalog ID/file `bar`/`bar.jsonc`. `height`/`padding` default 28/12; weather keys kept. Typed migration through noTunes' catalog-driven migration.
3. **Window.** Primary display only. `WindowKind::NonActivatingPanel` gets native semantics in the host/`stache-macos`, surface `bar`, all Spaces + stationary, level relative to the menu bar. Private SkyLight only if the part 03 gate proves public APIs insufficient, then separately approved and wrapped in `stache-macos`.
4. **Menu-bar auto-hide** kept, event-driven where possible, polling only while needed, best effort.
5. **Published-state channel** (generic SDK): one typed, versioned, read-only DTO per provider with change notification. Keep Awake becomes the first provider in part 08 (blocked until Keep Awake lands); Tiling reuses it. Optional `observes` capabilities are distinct from lifecycle `requires`: absent/disabled Keep Awake never stops Bar.
6. **Screen reservation registry** (host-owned): per-display edge insets per owner, revisioned, cleared on stop/disable/pause/failure. Bar publishes a constant top inset `height + padding` that does not change when the bar slides away (avoids a relayout on every menu reveal).
7. **Keep Awake indicator** shows Keep Awake's published state; click issues the generic session Pause/Resume through the host ([08](08-keep-awake-indicator.md)). Disabled means inert, never enable/config mutation. Preserve exactly two legacy visual states: confirmed active versus otherwise, including locked inactive.
8. **Click actions** fixed per component: CPU→Activity Monitor, Wi-Fi→`x-apple.systempreferences:com.apple.wifi-settings-extension`, Clock→Calendar, Battery→`x-apple.systempreferences:com.apple.Battery-Settings.extension`, Weather→Weather, Media→its fixed bundle map. Typed outcome via the `stache-macos` open adapter.
9. **Wi-Fi** native where no permission is needed; SSID via `ipconfig getsummary` behind a gate; never a Location prompt.
10. **Media** keeps the private MediaRemote adapter as an owned, bounded, gated child process; hidden when unavailable.
11. **Weather** keeps providers, geolocation → ipapi.co → ipinfo.io → `defaultLocation`, env-file key, CSP pinning, 20 min. User approved possible geolocation consent; do not promise a single prompt or prompt-free weather.
12. **CPU temperature** SMC then `ismc`/`smctemp`; battery via `starship-battery` or IOKit.
13. **Font** system-installed Maple Mono NF with the legacy fallback stack; not bundled.
14. **Module→UI data** through tauri-specta generated commands/events, delivered by a host bridge (the SDK bus is pull-only).
15. **Tests** never open real windows/panels, spawn the media adapter, hit the network or prompt permissions; manual native gates need per-run approval.

## Parts

| Part                               | Summary                                                                         | Status |
| ---------------------------------- | ------------------------------------------------------------------------------- | ------ |
| [01](01-config-catalog.md)         | `BarConfig`, catalog, schema/bindings, typed migration                          | draft  |
| [02](02-sdk-state-reservations.md) | Published-state channel, change notifier, screen reservation registry (fakes)   | draft  |
| [03](03-panel-window.md)           | Non-activating panel kind, bar window lifecycle, auto-hide, reservation, bridge | draft  |
| [04](04-ui-shell-parity.md)        | UI shell port, tiling-absent empty state, visual-parity gate                    | draft  |
| [05](05-system-providers.md)       | Clock, CPU, battery, Wi-Fi providers and fixed click actions                    | draft  |
| [06](06-weather.md)                | Weather providers, location chain, CSP, env-file key                            | draft  |
| [07](07-media.md)                  | Media adapter child process and artwork (gated)                                 | draft  |
| [08](08-keep-awake-indicator.md)   | Keep Awake provider + indicator + generic pause/resume (blocked)                | draft  |

Acceptance IDs are `BAR-NN-MM`; each is owned by exactly one part. Every part builds, tests and commits alone; unfinished parts block merge.

## Integration order

Roadmap: noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → **Bar** → Tiling. Worktree baseline is `e0b4648`; inspected main `ad34b46` already ships noTunes (`ccae030`), `ApplicationControl` with exact-instance identity/issuance permit, and catalog-driven migration. Do not recreate them from stale worktree docs. [Keep Awake](../2026-11-03-keep-awake/00-overview.md) (`2ddd0e3`, `feat/keep-awake`) and [Wallpapers](../2026-11-03-wallpapers/00-overview.md) (`f13cd3a`, `feat/wallpapers`) are drafts now on `main`, inspected with `git show`, not shipped APIs or module dependencies.

Before each implementation part, rebase on current main and reuse landed generic factory context (C10), accepted-config watch (C06), factory begin-shutdown (C07), and input modules' command runner. If still absent, introduce only the minimal generic facility once in its owning part, compile-prove it and preserve existing consumers. Catalog, bindings, router surfaces, capabilities and index entries are additive. No competing lifecycle, service framework or module-crate imports in Bar.

Dependency chain: 01 → 02 → 03 → 04 → 05 → 06 → 07 → 08; 08 additionally waits for Keep Awake merge. Part 03 tests a staged backend with fakes but registers no production Bar factory/unknown UI route. Part 04 supplies the real static surface and only then activates the inert-by-default production factory. Later providers replace fixture seams, never forward-call unimplemented commands.

## Wallpapers review lessons applied

The mandatory C01–C20 applicability audit is in [review lessons](review-lessons.md). It distinguishes reused invariants from Wallpapers-only commands/cache rules rather than pretending Bar implements them.

## Intentional compatibility changes

1. `height`/`padding` defaults become 28/12. Legacy `#[derive(Default)]` produced 0 (`_legacy/app/native/src/config/types/bar.rs:88-102`, `_legacy/stache.schema.json:209-221`); legacy files that omitted them rendered 0. Migration keeps every explicit value, including 0, materializes omitted defaults and warns about the corrected omission behavior. No redesign accompanies this approved default correction.
2. One file `bar.jsonc` replaces the root `bar` key.
3. Reload reconfigures in place instead of restarting the app (`_legacy/docs/gotchas.md:35-40`).
4. Clock, Battery and Weather clicks open Calendar, Battery settings and Weather instead of removed widgets (`_legacy/app/ui/hooks/useWidgetToggle.ts`). The clickable look is unchanged.
5. No generic `open_app` allowlist command (`_legacy/app/native/src/modules/bar/components/apps.rs:42-55`); actions are fixed typed commands.
6. Keep Awake click is session Pause/Resume of the separate `keepAwake` module instead of `toggle_system_awake` (`keepawake.rs:137-148`). It cannot enable a disabled module.
7. Event names come from generated bindings, not `stache://…` strings (`_legacy/app/native/src/events.rs:25-41`).
8. Menu-bar polling runs only while needed instead of a constant 100 ms loop (`menubar.rs:33`, `watcher.rs:28-38`).
9. The media child is restarted with backoff and terminated on stop; legacy spawned once and never restarted (`media.rs:343-395`).
10. The window is created on demand by the module; legacy declared a fixed `bar` window in `tauri.conf.json:28-53`.
11. The SkyLight private space (`platform/window.rs:117-141`) is dropped unless the part 03 gate requires it.
12. Bar is a lifecycle module with tray Enable/Pause/Retry; legacy had no lifecycle (`bar/mod.rs:12-17`).

## Source evidence

- Window: `_legacy/app/native/tauri.conf.json:28-53` (transparent, undecorated, no shadow, `focusable: false`, `acceptFirstMouse`, `visibleOnAllWorkspaces`, `theme: Dark`, `windowEffects: null`); CSP `:93-109`; Info.plist location keys `_legacy/app/native/Info.plist:11-14`.
- Native: `bar/mod.rs:12-46`; frame math `bar/window.rs:17-41`; primary monitor `platform/window.rs:179-188`; class swap, nonactivating mask, `canJoinAllSpaces|stationary`, no `fullScreenAuxiliary` `platform/window.rs:190-222`; tracking area `:224-260`; level `kCGMainMenuWindowLevelKey` then `set_always_on_bottom` `:147-172` (tao 0.37 maps that to `BelowNormalWindowLevel`, so legacy's effective level is below normal, not menu level); display callback `bar/screen.rs:8-30`.
- Menu bar: signals and notifications `bar/menubar.rs:33-43`, resolution `:359-372`, layer-25 heuristic `:245-330`, initial state `:89-117`; UI `renderer/bar/Bar.state.ts:6-17`, `Bar.styles.ts:5-28` (`menuHidden` is true when the menu bar is visible).
- UI: `Bar.tsx:20-29`; `Media.styles.ts:5-35`; `components/Surface/Surface.styles.ts:5-14`, `Surface.motion.ts:7-12`; `design-system/colors.ts`, `motion.ts`; `main.css:41,68-80`; `utils/media-query.ts:1`; `Status/Status.tsx:12-22`; `Spaces/Spaces.tsx:11-46`.
- Components: Clock `Clock.state.ts:7,39`; CPU `cpu.rs:43-180`, `Cpu.state.ts:39,49`; Battery `battery.rs:131-150`, `BatteryStore.ts:6-17`; Wi-Fi `wifi.rs:13-284`, `Wifi.state.ts:28-107`; Weather `weather.rs:36-54`, `WeatherStore.ts:8`, `fetchLocation.ts:7-98`; Media `media.rs:148-427`, `Media.constants.ts:18-62`, `Media.state.ts:60-81`; Keep Awake `KeepAwake.tsx:11-31`, `KeepAwake.state.ts:11-34`; tiling gap `_legacy/docs/features/bar.md:60`.

## Open questions

None requiring a new product decision. Optional observation, weather consent, disabled-click inertness and the exact Edge Dev map are resolved. Native feasibility and independent baseline approval remain evidence gates, not permission to alter visuals or use private APIs.

## Validation and safety

FFI panic containment applies to unwind-enabled builds. The retained release profile uses `panic = "abort"`, so production callbacks must be bounded and nonpanicking; tests must not imply arbitrary release panic recovery.

Default tests use fakes and WebKit only: no real panel, input injection, audio/wallpaper/power mutation, app launch, network, consent or media subprocess, including doctests. Manual gates (panel/Spaces/fullscreen/auto-hide, Wi-Fi SSID, SMC, media on macOS 26, idle measurements) each require fresh per-run permission, exact identity/scope, observations and cleanup result. Read-only measurements are not native feasibility proof. Legacy app/build/automation is never launched by tests; baseline process is part 04.

Each part runs targeted fake Rust tests, `bun run test` and `bun run check:generated` for affected surfaces, then relevant `bun run lint`, `bun run format:check`, `bun run build` and compile-only `bun run tauri:build`. No live smoke mode by default. Compile-proof SDK consumers and generated bindings at the part introducing them. Update shipped behavior docs only when that slice lands; preserve plans for later slices.

Performance: disabled Bar owns zero workers/timers/webviews/subprocesses. Active deterministic fake-time tests bound sampling counts and queues (05–07). Part 05 records release-host CPU/wakeups with identical hardware, widths, data cadence, duration, warmup and repeated trials, module-off versus module-on. If an approved independently supplied legacy measurement isolates Bar, compare the attributable delta, not the whole legacy process. Budget: no statistically distinguishable regression beyond the recorded run-to-run noise envelope; publish samples and method before acceptance, never fabricate a numeric baseline from docs. Without a trustworthy legacy sample the relative budget remains unverified, not passed; no launch of legacy automation to obtain it.

## Affected docs

[Planned feature notice](../../features/bar.md) stays NOT SHIPPED until parts land. Parts update [sdk](../../architecture/sdk.md), [ui](../../architecture/ui.md), [config](../../architecture/config.md), [overview](../../architecture/overview.md), [new-module](../../guides/new-module.md), [testing](../../guides/testing.md) and the index when behavior ships. Legacy docs stay untouched.
