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
3. **Window.** Primary display only. `WindowKind::NonActivatingPanel` gets native semantics in the host/`stache-macos`, surface `bar`, all Spaces + stationary. Private SkyLight only if the part 03 gate proves public APIs insufficient, then separately approved and wrapped in `stache-macos`.
   - **Level** (confirmed 2026-11-08): legacy's effective level, below normal windows (`BelowNormalWindowLevel`). Legacy set `kCGMainMenuWindowLevelKey` then tao `set_always_on_bottom(true)`, which overrides it (`_legacy/app/native/src/platform/window.rs:147-172`); the creation-time `alwaysOnTop: true` (`_legacy/app/native/tauri.conf.json:31`) is overridden too. Part 03 owns it.
   - **Primary display** (confirmed 2026-11-08): `NSScreen.screens[0]`, the menu-bar display with origin (0,0), matching legacy tao `primary_monitor` and `src/crates/macos/src/desktop.rs:40-41`. Never `mainScreen`. Part 03 owns it.
4. **Menu-bar auto-hide** kept, event-driven where possible, polling only while needed, best effort.
5. **Published-state channel** (generic SDK): one typed, versioned, read-only DTO per provider with change notification. Keep Awake becomes the first provider in part 08; Tiling reuses it. Optional `observes` capabilities are distinct from lifecycle `requires`: absent/disabled Keep Awake never stops Bar.
6. **Screen reservation registry** (host-owned): per-display edge insets per owner, revisioned, cleared on stop/disable/pause/failure. Bar publishes a constant top inset `height + padding` that does not change when the bar slides away (avoids a relayout on every menu reveal); none in the zero-size no-panel state (Decision 16).
7. **Keep Awake indicator** shows Keep Awake's published state; click issues the generic session Pause/Resume through the host ([08](08-keep-awake-indicator.md)). Disabled means inert, never enable/config mutation. Preserve exactly two legacy visual states: confirmed active versus otherwise, including locked inactive.
8. **Click actions** fixed per component: CPU→Activity Monitor, Wi-Fi→`x-apple.systempreferences:com.apple.wifi-settings-extension`, Clock→Calendar, Battery→`x-apple.systempreferences:com.apple.Battery-Settings.extension`, Weather→Weather, Media→its fixed bundle map. Typed outcome via the `stache-macos` open adapter.
9. **Wi-Fi** native where no permission is needed; SSID via `ipconfig getsummary` behind a gate; never a Location prompt.
10. **Media** keeps the private MediaRemote adapter as an owned, bounded, gated child process; hidden when unavailable.
11. **Weather** keeps providers, geolocation → ipapi.co → ipinfo.io → `defaultLocation`, env-file key, CSP pinning, 20 min. User approved possible geolocation consent; do not promise a single prompt or prompt-free weather.
12. **CPU temperature** SMC then `ismc`/`smctemp`; battery via `starship-battery` or IOKit.
13. **Font** system-installed Maple Mono NF with the legacy fallback stack; not bundled.
14. **Module→UI data** through tauri-specta generated commands/events, delivered by a host bridge (the SDK bus is pull-only).
15. **Tests** never open real windows/panels, spawn the media adapter, hit the network or prompt permissions; manual native gates need per-run approval.
16. **Zero size** (confirmed 2026-11-08): explicit `height: 0`, or a computed panel width `<= 0`, stays valid config. Bar runs with no panel: typed status "no panel: zero size", no window, no screen reservation. Shared window validation (`src/crates/sdk/src/windows.rs:372-381`, `src/crates/app/src/windows.rs:361-368`) stays unchanged. Part 03 owns it.
17. **Accessibility gate** (confirmed 2026-11-08): Bar waits behind the planned [Accessibility launch gate](../2026-11-07-accessibility-gate/00-overview.md) like every module but does not depend on it. See [Accessibility gate interaction](#accessibility-gate-interaction).

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
| [08](08-keep-awake-indicator.md)   | Keep Awake provider + indicator + generic pause/resume                          | draft  |

Acceptance IDs are `BAR-NN-MM`; each is owned by exactly one part. Every part builds, tests and commits alone; unfinished parts block merge.

## Integration order

Roadmap: noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → **Bar** → Tiling. Baseline is main `f260c6f`: noTunes, Keep Awake (`src/crates/modules/keep-awake`), Wallpapers (`af81491`), Keybindings (`4b8c2b3`), Hold-to-Quit (`5abfe55`) and MenuAnywhere (`6cdbcdd`) are shipped, with `ApplicationControl` exact-instance identity/issuance permit and catalog-driven migration. Keep Awake's native power/lock gate remains unverified; Bar does not rely on it. Rebase on current main before each part.

Reuse these landed facilities; do not recreate them:

- `FactoryContext` and `ModuleFactory::{ipc_handlers, project_status, begin_shutdown, finish_shutdown}` (`src/crates/app/src/host.rs`, trait ~230-281, stage runner ~1037-1060).
- Accepted-config watch: `Registry::accepted_watch` (`src/crates/sdk/src/registry.rs:1026`) and `AcceptedWatch::wait_for_change` (`src/crates/sdk/src/config/accepted.rs:299`), via `register_module_observed`.
- `CommandExecutor::run_captured`/`run_captured_prepared` (`src/crates/sdk/src/commands.rs:847,864`): one-shot captured only, no streaming mode (part 07 adds a minimal bounded streaming extension).
- `NativeOwner` (`src/crates/app/src/shutdown.rs:336-345`).
- `ApplicationControl` open (`src/crates/macos/src/applications.rs:588`); `open` takes only a file `Path` (`:1337-1341`), so part 05's fixed settings-URL operation is new.
- Per-module typed status overlay on `ModuleStatusView` (`src/crates/app/src/bindings.rs` ~97-128, e.g. `keybindings: Option<KeybindingsRuntimeStatus>`); bindings builder `collect_commands![app_info, host_status]`/`collect_events![StatusChanged]` (`bindings.rs:258-291`); `AppUi::status` emits `StatusChanged` (`src/crates/app/src/lib.rs:90-96`).
- Session pause/resume exists host-side only (`ModuleControl::pause/resume` `host.rs:150-172`, `HostHandle::pause/resume` `host.rs:844-847`, tray rows `tray.rs:206-214`); not exposed to modules/UI, so part 08's generated session-action command is new.

Still absent; the owning part introduces each once, minimally, compile-proven and preserving existing consumers: generic published-state channel with revision/notify and `observes` declaration (02), screen reservation registry (02; distinct name from the unrelated keyboard-chord `src/crates/sdk/src/input/reservations.rs`), SDK-side typed display ID (02), panel window kind, menu-bar visibility observation (03), Wi-Fi/battery/SMC providers (05), weather/env-file reader/CSP hosts (06), MediaRemote provider and streaming runner (07). Catalog, bindings, router surfaces, capabilities and index entries are additive. No competing lifecycle, service framework or module-crate imports in Bar.

Dependency chain: 01 → 02 → 03 → 04 → 05 → 06 → 07 → 08. Part 03 tests a staged backend with fakes but registers no production Bar factory/unknown UI route. Part 04 supplies the real static surface and only then activates the inert-by-default production factory. Later providers replace fixture seams, never forward-call unimplemented commands.

## Accessibility gate interaction

The [Accessibility launch gate](../2026-11-07-accessibility-gate/00-overview.md) is specified, not implemented; either may land first and Bar does not depend on it. Bar needs no Accessibility permission itself. Whenever the gate exists:

- While gated, Bar creates zero windows/webviews (it is never started).
- Bar's panel teardown is the normal stop path, reachable from the revocation stop (`on_permission_change`).
- Bar's Tauri bridge commands return a typed unavailable while Bar is not running, gated or otherwise (part 03).
- Both specs add to `SURFACES` (`src/ui/src/shell/components/WindowRouter/surface.ts:1`); the change is additive and whichever lands second rebases.

## Wallpapers review lessons applied

The mandatory C01–C20 applicability audit is in [review lessons](review-lessons.md). It distinguishes reused invariants from Wallpapers-only commands/cache rules rather than pretending Bar implements them.

## Intentional compatibility changes

1. `height`/`padding` defaults become 28/12. Legacy `#[derive(Default)]` produced 0 (`_legacy/app/native/src/config/types/bar.rs:88-102`, `_legacy/stache.schema.json:209-221`); legacy files that omitted them rendered 0. Migration keeps every explicit value, including 0, materializes omitted defaults and warns about the corrected omission behavior. No redesign accompanies this approved default correction.
2. One file `bar.jsonc` replaces the root `bar` key.
3. Reload reconfigures in place instead of restarting the app (`_legacy/docs/gotchas.md:35-40`).
4. Clock, Battery and Weather clicks open Calendar, Battery settings and Weather instead of removed widgets (`_legacy/app/ui/hooks/useWidgetToggle.ts`). The clickable look is unchanged.
5. No generic `open_app` allowlist command (`_legacy/app/native/src/modules/bar/components/apps.rs:42-55`); actions are fixed typed commands.
6. Keep Awake click is session Pause/Resume of the separate `keepAwake` module instead of `toggle_system_awake` (`_legacy/app/native/src/modules/bar/components/keepawake.rs:137-148`). It cannot enable a disabled module.
7. Event names come from generated bindings, not `stache://…` strings (`_legacy/app/native/src/events.rs:25-41`).
8. Menu-bar polling runs only while needed instead of a constant 100 ms loop (`_legacy/app/native/src/modules/bar/menubar.rs:33`, `_legacy/app/native/src/modules/bar/watcher.rs:28-38`).
9. The media child is restarted with backoff and terminated on stop; legacy spawned once and never restarted (`_legacy/app/native/src/modules/bar/components/media.rs:343-395`).
10. The window is created on demand by the module; legacy declared a fixed `bar` window in `_legacy/app/native/tauri.conf.json:28-53`.
11. The SkyLight private space (`_legacy/app/native/src/platform/window.rs:117-141`) is dropped unless the part 03 gate requires it.
12. Bar is a lifecycle module with tray Enable/Pause/Retry; legacy had no lifecycle (`_legacy/app/native/src/modules/bar/mod.rs:12-17`).

## Source evidence

Shorthand paths: `bar/…` is `_legacy/app/native/src/modules/bar/…` (components in `bar/components/`), `platform/…` is `_legacy/app/native/src/platform/…`, UI files are under `_legacy/app/ui/` (`renderer/bar/…`, `stores/…`, `components/…`).

- Window: `_legacy/app/native/tauri.conf.json:28-53` (transparent, undecorated, no shadow, `focusable: false`, `acceptFirstMouse`, `visibleOnAllWorkspaces`, `theme: Dark`, `windowEffects: null`); CSP `:93-109`; Info.plist location keys `_legacy/app/native/Info.plist:11-14`.
- Native: `bar/mod.rs:12-46`; frame math `bar/window.rs:17-41`; primary monitor `platform/window.rs:179-188`; class swap, nonactivating mask, `canJoinAllSpaces|stationary`, no `fullScreenAuxiliary` `platform/window.rs:190-222`; tracking area `:224-260`; level `kCGMainMenuWindowLevelKey` then `set_always_on_bottom` `:147-172` (tao 0.37 maps that to `BelowNormalWindowLevel`; this effective below-normal level is now the target, Decision 3); display callback `bar/screen.rs:8-30`.
- Menu bar: signals and notifications `bar/menubar.rs:33-43`, resolution `:359-372`, layer-25 heuristic `:245-330`, initial state `:89-117`; UI `renderer/bar/Bar.state.ts:6-17`, `Bar.styles.ts:5-28` (`menuHidden` is true when the menu bar is visible).
- UI: `Bar.tsx:20-29`; `Media.styles.ts:5-35`; `components/Surface/Surface.styles.ts:5-14`, `Surface.motion.ts:7-12`; `design-system/colors.ts`, `motion.ts`; `main.css:41,68-80`; `utils/media-query.ts:1`; `Status/Status.tsx:12-22`; `Spaces/Spaces.tsx:11-46`.
- Components: Clock `Clock.state.ts:7,39`; CPU `cpu.rs:43-180`, `Cpu.state.ts:39,49`; Battery `battery.rs:131-150`, `BatteryStore.ts:6-17`; Wi-Fi `wifi.rs:13-284`, `Wifi.state.ts:28-107`; Weather `weather.rs:36-54`, `WeatherStore.ts:8`, `fetchLocation.ts:7-98`; Media `media.rs:148-427`, `Media.constants.ts:18-62`, `Media.state.ts:60-81`; Keep Awake `KeepAwake.tsx:11-31`, `KeepAwake.state.ts:11-34`; tiling gap `_legacy/docs/features/bar.md:60`.

## Open questions

None. Resolved 2026-11-08: window level below normal (Decision 3), primary display `NSScreen.screens[0]` (Decision 3), zero size runs with no panel (Decision 16), Accessibility gate interaction (Decision 17). Earlier: optional observation, weather consent, disabled-click inertness and the exact Edge Dev map. Native feasibility and independent baseline approval remain evidence gates, not permission to alter visuals or use private APIs.

## Validation and safety

FFI panic containment applies to unwind-enabled builds. The retained release profile uses `panic = "abort"`, so production callbacks must be bounded and nonpanicking; tests must not imply arbitrary release panic recovery.

Default tests use fakes and WebKit only: no real panel, input injection, audio/wallpaper/power mutation, app launch, network, consent or media subprocess, including doctests. Manual gates (panel/Spaces/fullscreen/auto-hide, Wi-Fi SSID, SMC, media on macOS 26, idle measurements) each require fresh per-run permission, exact identity/scope, observations and cleanup result. Read-only measurements are not native feasibility proof. Legacy app/build/automation is never launched by tests; baseline process is part 04.

Each part runs targeted fake Rust tests, `bun run test` and `bun run check:generated` for affected surfaces, then relevant `bun run lint`, `bun run format:check`, `bun run build` and compile-only `bun run tauri:build`. No live smoke mode by default. Compile-proof SDK consumers and generated bindings at the part introducing them. Update shipped behavior docs only when that slice lands; preserve plans for later slices.

Performance: disabled Bar owns zero workers/timers/webviews/subprocesses. Active deterministic fake-time tests bound sampling counts and queues (05–07). Part 05 records release-host CPU/wakeups with identical hardware, widths, data cadence, duration, warmup and repeated trials, module-off versus module-on. If an approved independently supplied legacy measurement isolates Bar, compare the attributable delta, not the whole legacy process. Budget: no statistically distinguishable regression beyond the recorded run-to-run noise envelope; publish samples and method before acceptance, never fabricate a numeric baseline from docs. Without a trustworthy legacy sample the relative budget remains unverified, not passed; no launch of legacy automation to obtain it.

## Affected docs

[Planned feature notice](../../features/bar.md) stays NOT SHIPPED until parts land. Parts update [sdk](../../architecture/sdk.md), [ui](../../architecture/ui.md), [config](../../architecture/config.md), [overview](../../architecture/overview.md), [new-module](../../guides/new-module.md), [testing](../../guides/testing.md) and the index when behavior ships. Legacy docs stay untouched.
