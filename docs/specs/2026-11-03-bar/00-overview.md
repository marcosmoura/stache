---
branch: feat/bar
---

# Bar rewrite

Read when working on the Bar module. All parts (03–08) are implemented and squash-merged into `main` as `98b0c23` (2026-11-10); shipped behavior is documented in [features/bar.md](../../features/bar.md). Remaining live gates are listed in each part's completion record.

## Goal

Rebuild the legacy status bar as `stache-bar`: one non-activating panel on the primary display, visually identical to legacy, fed by typed providers. Parts 01–02 (config, the generic SDK published-state channel and the host-owned screen reservation registry) moved to the [Bar foundation](../2026-11-08-bar-foundation/00-overview.md) spec on 2026-11-08; this spec keeps parts 03–08.

## Non-goals

- No widgets (permanently removed), Settings UI, tiling integration (Spaces/workspaces/app list/focus/top-gap consumption), per-display bars, new CLI command or visual change of any kind.
- No generic launcher endpoint, Keep Awake mutation endpoint, Keep Awake-owned assertion or SkyLight call without separate approval.
- No fix for the known Cmd-Q/logout coordinated-teardown gap.

## Decisions

Approved by the user; parts must not re-decide them.

1. **Visual identity.** Layout, sizes, spacing, colors, fonts, icons, animations, transitions, hover/active states, media overlay, laptop media query (`width <= 2036px`) and empty states equal legacy. A visual-parity gate ([04](04-ui-shell-parity.md)) blocks merge; any deviation needs user approval.
2. **Module.** Crate `src/crates/modules/bar`, package `stache-bar`, catalog ID/file `bar`/`bar.toml` (was `bar.jsonc`; see [config](../../architecture/config.md)). `height`/`padding` default 28/12; weather keys kept. Typed migration through noTunes' catalog-driven migration.
3. **Window.** Primary display only. `WindowKind::NonActivatingPanel` gets native semantics in the host/`stache-macos`, surface `bar`, all Spaces + stationary. Private SkyLight only if the part 03 gate proves public APIs insufficient, then separately approved and wrapped in `stache-macos`.
   - **Level** (confirmed 2026-11-08): legacy's effective level, below normal windows (`BelowNormalWindowLevel`). Legacy set `kCGMainMenuWindowLevelKey` then tao `set_always_on_bottom(true)`, which overrides it (`_legacy/app/native/src/platform/window.rs:147-172`); the creation-time `alwaysOnTop: true` (`_legacy/app/native/tauri.conf.json:31`) is overridden too. Part 03 owns it.
   - **Primary display** (confirmed 2026-11-08): `NSScreen.screens[0]`, the menu-bar display with origin (0,0), matching legacy tao `primary_monitor` and `src/crates/macos/src/desktop.rs:40-41`. Never `mainScreen`. Part 03 owns it.
4. **Menu-bar auto-hide** kept, event-driven where possible, polling only while needed, best effort.
5. **Published-state channel** (generic SDK): one typed, versioned, read-only DTO per provider with change notification. Keep Awake becomes the first provider in part 08; Tiling reuses it. Optional `observes` capabilities are distinct from lifecycle `requires`: absent/disabled Keep Awake never stops Bar.
6. **Screen reservation registry** (host-owned): per-display edge insets per owner, revisioned, cleared on stop/disable/pause/failure. Bar publishes a constant top inset `height + padding` that does not change when the bar slides away (avoids a relayout on every menu reveal); none in the zero-size no-panel state (Decision 16).
7. **Keep Awake indicator** shows Keep Awake's published state; click issues the generic session Pause/Resume through the host ([08](08-keep-awake-indicator.md)). Disabled means inert, never enable/config mutation. Preserve exactly two legacy visual states: confirmed active versus otherwise, including locked inactive.
8. **Click actions** fixed per component: CPU→Activity Monitor, Wi-Fi→`x-apple.systempreferences:com.apple.wifi-settings-extension`, Clock→Calendar, Battery→`x-apple.systempreferences:com.apple.Battery-Settings.extension`, Weather→Weather, Media→its fixed bundle map. Typed outcome via the `stache-macos` open adapter.
9. **Wi-Fi** native where no permission is needed; SSID via `ipconfig getsummary`, on by default since the approved feasibility run on macOS 27.0 (2026-11-10, network name shown, no prompt; macOS 27 evidence satisfies the macOS 26+ gate by user decision, macOS 26 itself not separately verified; `STACHE_BAR_WIFI_SSID_SUMMARY=0` is a diagnostic off-switch); never a Location prompt.
10. **Media** keeps the private MediaRemote adapter as an owned, bounded, gated child process; hidden when unavailable.
11. **Weather** (revised 2026-11-10) keeps providers, env-file key, CSP pinning and the 20-minute/reconnect refresh. Location is native: CoreLocation (`CLLocationManager` via `objc2-core-location`) wrapped in `stache-macos` → `default_location` (was `defaultLocation`). No webview `navigator.geolocation`, no IP-based location (ipapi.co/ipinfo.io dropped), no reverse geocoding.
    - **Config:** `[weather] precise_location` (bool, default `false`; legacy has no key, migration materializes the default). `false` requests kilometer-level accuracy and accepts the user's macOS approximate-location choice; `true` requests best accuracy, and temporary full accuracy only where macOS requires it and the user's setting allows, never re-prompting. Takes effect at the next location read without restarting Bar.
    - **Consent:** one when-in-use-style macOS Location prompt is approved; the part determines the authorization call for the Accessory-policy tray app and adds the required usage-description key(s) to the bundle. Denied, restricted, services off, not determined after a bounded timeout (about 10 s), unavailable or error → `default_location` with a typed diagnostic outside Bar visuals; never a bogus coordinate or repeated prompt. Do not promise prompt-free location or tie it to Wi-Fi (Decision 9).
    - **Cadence and delivery:** one read per weather refresh, no continuous updates; zero location workers/observers when Bar is not running or has no panel; late callbacks never republish. Coordinates plus source `coreLocation | defaultLocation` (no address) reach the UI through the Bar bridge as a typed DTO; provider HTTP stays in the scoped UI. Coordinates never appear in logs, generic host status events, persisted query keys or cross-window sync.
12. **CPU temperature** (revised 2026-11-10): native SMC only, smctemp-style: an own public-IOKit `AppleSMC` adapter in `stache-macos` with per-chip CPU key lists (M1–M5 and Intel), invalid-value retry within a sample and a bounded last-valid value; unknown when SMC has no valid CPU sensor. No `ismc`/`smctemp` subprocesses and no private API; the private HID sensor-hub path (as iSMC uses on Apple Silicon) needs live evidence and separate approval. Battery via `starship-battery` or IOKit.
13. **Font** system-installed Maple Mono NF with the legacy fallback stack; not bundled.
14. **Module→UI data** through tauri-specta generated commands/events, delivered by a host bridge (the SDK bus is pull-only).
15. **Tests** never open real windows/panels, spawn the media adapter, hit the network or prompt permissions; manual native gates need per-run approval.
16. **Zero size** (confirmed 2026-11-08): explicit `height = 0`, or a computed panel width `<= 0`, stays valid config. Bar runs with no panel: typed status "no panel: zero size", no window, no screen reservation. Shared window validation (`src/crates/sdk/src/windows.rs:377-386`, `src/crates/app/src/windows.rs:430-439`) stays unchanged. Part 03 owns it.
17. **Accessibility gate** (confirmed 2026-11-08): Bar waits behind the shipped [Accessibility launch gate](../2026-11-07-accessibility-gate/00-overview.md) like every module but does not depend on it. See [Accessibility gate interaction](#accessibility-gate-interaction).
18. **Screen topology ownership** (confirmed 2026-11-09): a Bar-scoped host feed in the app reports every attached display (via `stache-macos` `displays.rs`, projected with `sdk_display_id`) through `Registry::apply_screen_topology`, only while Bar is running. On Bar stop, pause, disable or failure the feed applies an empty topology, so Tiling returns to "no host topology" (zero host inset) exactly as with Bar off. Tiling never applies topology. Part 03 owns it, including an end-to-end scenario test with Tiling and Bar running across display hotplug in which no attached display is left `Absent`.

## Parts

| Part                             | Summary                                                                         | Status |
| -------------------------------- | ------------------------------------------------------------------------------- | ------ |
| [03](03-panel-window.md)         | Non-activating panel kind, bar window lifecycle, auto-hide, reservation, bridge | done   |
| [04](04-ui-shell-parity.md)      | UI shell port, tiling-absent empty state, visual-parity gate                    | done   |
| [05](05-system-providers.md)     | Clock, CPU, battery, Wi-Fi providers and fixed click actions                    | done   |
| [06](06-weather.md)              | Weather providers, CoreLocation location, CSP, env-file key                     | done   |
| [07](07-media.md)                | Media adapter child process and artwork (gated)                                 | done   |
| [08](08-keep-awake-indicator.md) | Keep Awake provider + indicator + generic pause/resume                          | done   |

Parts 01–02 moved to the [Bar foundation](../2026-11-08-bar-foundation/00-overview.md) spec, which must merge first. Acceptance IDs are `BAR-NN-MM`; each is owned by exactly one part. Every part builds, tests and commits alone; unfinished parts block merge.

## Integration order

Roadmap (reordered 2026-11-08): noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → Bar foundation → Tiling 01–09 → **Bar 03–08** → [Tiling–Bar integration](../2026-11-08-tiling-bar/00-overview.md). Bar 03–08 were built on main `ba51f20` (Bar foundation, the Accessibility gate and Tiling 01–09 with its real-desktop fixes merged) and landed on `main` as `98b0c23` on 2026-11-10. The earlier baseline main `f260c6f` shipped noTunes, Keep Awake (`src/crates/modules/keep-awake`), Wallpapers (`af81491`), Keybindings (`4b8c2b3`), Hold-to-Quit (`5abfe55`) and MenuAnywhere (`6cdbcdd`), with `ApplicationControl` exact-instance identity/issuance permit and catalog-driven migration. Keep Awake's native power/lock gate remains unverified; Bar does not rely on it. Rebase on current main before each part.

Reuse these landed facilities; do not recreate them:

- `FactoryContext` (`src/crates/app/src/host.rs:229-295`) and `ModuleFactory::{ipc_handlers, project_status, begin_shutdown, finish_shutdown}` (trait `host.rs:300-358`; stage runner calls `begin_shutdown` at `:1385`, `finish_shutdown` at `:1410`).
- Production registration: `ModuleFactories::production` (`host.rs:962`) holds ProxyAudio/NoTunes/KeepAwake/MenuAnywhere; Wallpapers/Keybindings/HoldToQuit/Tiling/Bar are chained in `src/crates/app/src/lib.rs:503-519`. `TilingFactory` (`host.rs:675`; `register` `:788` via `register_module_observed` `host.rs:387`; `project_status` `:832`; `begin/finish_shutdown` `:848`/`:853`) is the pattern. Part 04 chained Bar next to Tiling (`BarFactory::production` `lib.rs:503`, `.with(bar)` `:519`), default-disabled and inert at construction.
- Accepted-config watch: `Registry::accepted_watch` (`src/crates/sdk/src/registry.rs:1208`) and `AcceptedWatch::wait_for_change` (`src/crates/sdk/src/config/accepted.rs:299`), via `register_module_observed`.
- `CommandExecutor::run_captured`/`run_captured_prepared` (`src/crates/sdk/src/commands.rs:847,864`): one-shot captured only, no streaming mode (part 07 adds a minimal bounded streaming extension).
- `NativeOwner` (`src/crates/app/src/shutdown.rs:336-345`).
- Published state (Bar foundation 02): `src/crates/sdk/src/published_state.rs` (`PublishedState`, `SchemaIdentity`, `StateEnvelope::decode`, `StateSchemas`, `StatePublisher`, `StateObserver`), `ModuleContext::state_publisher` (`src/crates/sdk/src/context.rs:180`), `observe_state` (`:193`), `Registry::observe_state` (`registry.rs:625`); app projection `src/crates/app/src/published_state.rs` (`PublishedEnvelopeView`, `PublishedStateView`). No production publisher exists yet; part 08 makes Keep Awake the first.
- Screen reservations (Bar foundation 02): `src/crates/sdk/src/screen_reservations.rs` (`DisplayId`, `TopologyGeneration`, `EdgeInsets`, `ScreenReservations::replace/remove`, `ScreenReservationObserver`), `Registry::apply_screen_topology` (`registry.rs:647`), `Registry::observe_screen_reservations` (`:634`), `FactoryContext::observe_screen_reservations` (`host.rs:292`); app `sdk_display_id` (`src/crates/app/src/display.rs`). Write capabilities (`state_publisher`, `screen_reservations`) come from `ModuleContext` during the module lifecycle; `FactoryContext` exposes only read observers.
- Displays: `src/crates/macos/src/displays.rs` (`DisplayInfo` with `frame` incl. origin, `visible_frame`, `is_primary` (index 0 = `NSScreen.screens[0]`), `is_main`; `DisplayLayout::primary()`, `primary_height()`; `DisplayControl::query()`, `observe_topology()`), used by Tiling (`DisplayControl::system` at `host.rs:756`).
- `ApplicationControl` (`src/crates/macos/src/applications.rs:676`, `system` `:721`); `open` (`:1478`, `open_tracked` `:1502`) takes `(AcceptedActionToken, &Path, completion)`, a file path only. Settings URLs use the `stache-macos` `settings` opener (`src/crates/macos/src/settings.rs`): `open_accessibility_settings()` (`:70`) and, landed by part 05, the fixed constants `BATTERY_SETTINGS_URL` (`:24`)/`WIFI_SETTINGS_URL` (`:28`) with `open_battery_settings()` (`:80`)/`open_wifi_settings()` (`:88`). The app's `SettingsOpener` has `open_pane(&self, SettingsPane, PaneGuard)` (`src/crates/app/src/access.rs:218-222`; `SettingsPane::{Battery, Wifi}` `:226-232`), never arbitrary URLs.
- Per-module typed status overlay on `ModuleStatusView` (`src/crates/app/src/bindings.rs:111`, overlays `keybindings`, `tiling` and `bar` (`bar: Option<BarRuntimeStatus>` `:152`)); bindings builder `collect_commands![app_info, host_status, request_accessibility, open_accessibility_settings, quit_app, bar_snapshot, bar_action, bar_weather, bar_media_artwork, bar_session_action]`/`collect_events![StatusChanged, BarChanged]` (`bindings.rs:351-363`); `AppUi::status` emits `StatusChanged` (`src/crates/app/src/lib.rs:113`).
- Session pause/resume exists host-side only (`ModuleControl::pause/resume` `host.rs:188-206`, `HostHandle::pause/resume` `host.rs:1171`/`:1174`, tray rows `src/crates/app/src/tray.rs:223-240`, dispatch `:301-304`); not exposed to modules/UI (only Tiling has per-module socket pause/resume, `TilingLifecycle` `host.rs:868-904` with `pause` `:893`/`resume` `:899`, served from `ipc_handlers` `:810`), so part 08's generated session-action command is new.

Landed by 03–05 (reuse, do not recreate):

- `WindowKind::NonActivatingPanel` with native semantics (`src/crates/sdk/src/windows.rs:81`).
- Menu-bar visibility observation: `src/crates/macos/src/menu_bar.rs` and `src/crates/modules/bar/src/menubar.rs` (`src/crates/macos/src/menu/` is MenuAnywhere's unrelated AX reader).
- Bar bridge `src/crates/app/src/bar_bridge.rs`: `BarView` (`:96-125`, with `cpu`/`battery`/`wifi`); `bar_snapshot`/`bar_action` are ACL-gated through the `src/crates/app/build.rs` manifest and granted only by `src/crates/app/capabilities/bar.json` (`bar-window`, windows `stache-bar-*`).
- Fixed actions `src/crates/app/src/bar_actions.rs`: `BarAction::{Cpu, Clock, Battery, Wifi}` (`:77-86`) mapped to `BarTarget::{Application, Pane}` (`:90-95`).
- Providers `src/crates/modules/bar/src/providers/{cpu,battery,wifi}.rs`; native SMC `src/crates/macos/src/system/smc.rs`.
- Measurement script `scripts/measure-bar.ts` (`bun run measure:bar`).

Still absent; the owning part introduces each once, minimally, compile-proven and preserving existing consumers: weather/env-file reader/CSP hosts/CoreLocation adapter/location usage strings (06), MediaRemote provider and `CommandExecutor` streaming (07), session pause/resume exposed to UI (08). Catalog, bindings, router surfaces, capabilities and index entries are additive. No competing lifecycle, service framework or module-crate imports in Bar.

Dependency chain: parts 03–08 start only after [Bar foundation](../2026-11-08-bar-foundation/00-overview.md) and Tiling 01–09 have merged into `main`; then 03 → 04 → 05 → 06 → 07 → 08. Part 03 tests a staged backend with fakes but registers no production Bar factory/unknown UI route. Part 04 supplies the real static surface and only then activates the inert-by-default production factory. Later providers replace fixture seams, never forward-call unimplemented commands.

## Accessibility gate interaction

The [Accessibility launch gate](../2026-11-07-accessibility-gate/00-overview.md) shipped on main (`f0fc70a`): the registry merges a host baseline Accessibility requirement into every module (`Registry::with_baseline_requirements`, `src/crates/sdk/src/registry.rs`). Bar inherits it with no Bar code and needs no Accessibility permission itself. Consequences:

- While gated, Bar creates zero windows/webviews (it is never started).
- Bar's panel teardown is the normal stop path, reachable from the revocation stop (`on_permission_change`).
- Bar's Tauri bridge commands return a typed unavailable while Bar is not running, gated or otherwise (part 03).
- UI surfaces are the `WINDOWS` map in `src/ui/src/shell/Shell.tsx` with one directory per window under `src/ui/src/shell/windows/`; Bar's key `bar` maps to `src/ui/src/shell/windows/Bar/`, next to `access`/`test`.
- Reuse the gate's landed pieces instead of new plumbing: per-surface native chrome `WindowChrome`/`chrome_for` (`src/crates/app/src/windows.rs:46,63`, part 03) and the safe main-thread `x-apple.systempreferences:` opener `stache-macos` `settings` (`src/crates/macos/src/settings.rs`, part 05).

## Wallpapers review lessons applied

The mandatory C01–C20 applicability audit is in [review lessons](review-lessons.md). It distinguishes reused invariants from Wallpapers-only commands/cache rules rather than pretending Bar implements them.

## Intentional compatibility changes

1. `height`/`padding` defaults become 28/12. Legacy `#[derive(Default)]` produced 0 (`_legacy/app/native/src/config/types/bar.rs:88-102`, `_legacy/stache.schema.json:209-221`); legacy files that omitted them rendered 0. Migration keeps every explicit value, including 0, materializes omitted defaults and warns about the corrected omission behavior. No redesign accompanies this approved default correction.
2. One file `bar.toml` (was `bar.jsonc`) replaces the root `bar` key.
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
13. Weather location uses native CoreLocation → `default_location` with the new `precise_location` key instead of browser geolocation → ipapi.co → ipinfo.io → `defaultLocation` (`stores/WeatherStore/location/fetchLocation.ts:7-98`); no IP-based location (Decision 11).

## Source evidence

Shorthand paths: `bar/…` is `_legacy/app/native/src/modules/bar/…` (components in `bar/components/`), `platform/…` is `_legacy/app/native/src/platform/…`, UI files are under `_legacy/app/ui/` (`renderer/bar/…`, `stores/…`, `components/…`).

- Window: `_legacy/app/native/tauri.conf.json:28-53` (transparent, undecorated, no shadow, `focusable: false`, `acceptFirstMouse`, `visibleOnAllWorkspaces`, `theme: Dark`, `windowEffects: null`); CSP `:93-109` (legacy only; its ipapi.co/ipinfo.io hosts are not a target, Decision 11); Info.plist location usage strings `_legacy/app/native/Info.plist:11-14` (wording reference only).
- Native: `bar/mod.rs:12-46`; frame math `bar/window.rs:17-41`; primary monitor size (`get_screen_size` via `primary_monitor()`) `platform/window.rs:179-188`; class swap, nonactivating mask, `canJoinAllSpaces|stationary`, no `fullScreenAuxiliary` `platform/window.rs:190-222`; tracking area `:224-260`; level `kCGMainMenuWindowLevelKey` then `set_always_on_bottom` `:147-172` (tao 0.37 maps that to `BelowNormalWindowLevel`; this effective below-normal level is now the target, Decision 3); display callback `bar/screen.rs:8-30`.
- Menu bar: signals and notifications `bar/menubar.rs:33-43`, resolution `:359-372`, layer-25 heuristic `:245-330`, initial state `:89-117`; UI `renderer/bar/Bar.state.ts:6-17`, `Bar.styles.ts:5-28` (`menuHidden` is true when the menu bar is visible).
- UI: `Bar.tsx:20-29`; `Media.styles.ts:5-35`; `components/Surface/Surface.styles.ts:5-14`, `Surface.motion.ts:7-12`; `design-system/colors.ts`, `motion.ts`; `main.css:41,68-80`; `utils/media-query.ts:1`; `Status/Status.tsx:12-22`; `Spaces/Spaces.tsx:11-46`.
- Components: Clock `Clock.state.ts:7,39`; CPU `cpu.rs:43-180`, `Cpu.state.ts:39,49`; Battery `battery.rs:131-150`, `BatteryStore.ts:6-17`; Wi-Fi `wifi.rs:13-284`, `Wifi.state.ts:28-107`; Weather `weather.rs:36-54`, `stores/WeatherStore/WeatherStore.ts:8`, `stores/WeatherStore/location/fetchLocation.ts:7-98` (legacy browser/IP chain, superseded by Decision 11; providers in `stores/WeatherStore/providers/`); Media `media.rs:148-427`, `renderer/bar/Media/Media.constants.ts:18-62`, `Media.state.ts:60-81`; Keep Awake `KeepAwake.tsx:11-31`, `KeepAwake.state.ts:11-34`; tiling gap `_legacy/docs/features/bar.md:60`.

## Open questions

None. Resolved 2026-11-10: weather location is native CoreLocation → `default_location` with `precise_location`, no browser/IP location (Decision 11). Resolved 2026-11-09: screen topology ownership (Decision 18). Resolved 2026-11-08: window level below normal (Decision 3), primary display `NSScreen.screens[0]` (Decision 3), zero size runs with no panel (Decision 16), Accessibility gate interaction (Decision 17). Earlier: optional observation, weather consent, disabled-click inertness and the exact Edge Dev map. Native feasibility and independent baseline approval remain evidence gates, not permission to alter visuals or use private APIs.

## Validation and safety

FFI panic containment applies to unwind-enabled builds. The retained release profile uses `panic = "abort"`, so production callbacks must be bounded and nonpanicking; tests must not imply arbitrary release panic recovery.

Default tests use fakes and WebKit only: no real panel, input injection, audio/wallpaper/power mutation, app launch, network, consent or media subprocess, including doctests. Manual gates (panel/Spaces/fullscreen/auto-hide, Wi-Fi SSID, SMC, CoreLocation consent on a bundled build, media on macOS 26, idle measurements) each require fresh per-run permission, exact identity/scope, observations and cleanup result. Read-only measurements are not native feasibility proof. Legacy app/build/automation is never launched by tests; baseline process is part 04.

Each part runs targeted fake Rust tests, `bun run test` and `bun run check:generated` for affected surfaces, then relevant `bun run lint`, `bun run format:check`, `bun run build` and compile-only `bun run tauri:build`. No live smoke mode by default. Compile-proof SDK consumers and generated bindings at the part introducing them. Update shipped behavior docs only when that slice lands; preserve plans for later slices.

Performance: disabled Bar owns zero workers/timers/webviews/subprocesses. Active deterministic fake-time tests bound sampling counts and queues (05–07). Part 05 records release-host CPU/wakeups with identical hardware, widths, data cadence, duration, warmup and repeated trials, module-off versus module-on. If an approved independently supplied legacy measurement isolates Bar, compare the attributable delta, not the whole legacy process. Budget: no statistically distinguishable regression beyond the recorded run-to-run noise envelope; publish samples and method before acceptance, never fabricate a numeric baseline from docs. Without a trustworthy legacy sample the relative budget remains unverified, not passed; no launch of legacy automation to obtain it.

## Affected docs

The [feature doc](../../features/bar.md) describes the shipped behavior of parts 03–08, its live evidence (macOS 27.0, accepted for the macOS 26+ gates by user decision 2026-11-10) and the remaining unverified gates. Parts update [sdk](../../architecture/sdk.md), [ui](../../architecture/ui.md), [config](../../architecture/config.md), [overview](../../architecture/overview.md), [new-module](../../guides/new-module.md), [testing](../../guides/testing.md) and the index when behavior ships. Legacy docs stay untouched.
