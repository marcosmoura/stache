---
branch: feat/hold-to-quit
---

# Hold-to-Quit rewrite

Read when planning the independent Hold-to-Quit (`commandQuit`) module. This is a draft, not shipped behavior or permission to implement.

## Goal

Rebuild legacy “hold ⌘Q to quit” as `stache-hold-to-quit`: a default-disabled module that suppresses a Command-only ⌘Q, shows a native progress HUD and asks the exact application instance that was frontmost at key-down to quit gracefully once the hold reaches `holdDuration`. Fix the legacy tap, pause and timer bugs. Ask for approval before implementing each part.

## Non-goals

- No private event tap. Input comes only from the shared SDK input-tap service (see Decisions).
- No exclusion list or any new config key. No `forceTerminate`, no webview/React HUD, no Settings UI, no CLI command and no module-specific mutating endpoint.
- No fix for Stache's own Cmd-Q/logout coordinated-teardown gap, which is separate.
- No changes to proxyAudio, noTunes policy or Keybindings/MenuAnywhere behavior beyond additive adapter and shared-service extensions (part 04) and the shipped Carbon yield to a higher-priority HID claim.

## Decisions

These were approved by the user and are fixed.

1. Crate `src/crates/modules/hold-to-quit`, package `stache-hold-to-quit`, module/catalog ID `commandQuit`, file `commandQuit.jsonc`. Only keys: `enabled` (default `false`) and `holdDuration` (milliseconds, default `1500`). Preserve every `u64`, including zero and `u64::MAX`; no arbitrary bounds or clamping. Checked deadline arithmetic must not overflow; an unrepresentable deadline is a cancellable indefinite wait. Zero is eligible immediately, but only after matching HUD-show acknowledgement and current-state checks. Migration reuses the shipped catalog path.
2. Input uses the shared host input service shipped by [Keybindings](../2026-11-03-keybindings/00-overview.md) ([part 02](../2026-11-03-keybindings/02-shared-input.md), on `main` as `4b8c2b3`): `ModuleContext::input()` returns an `InputCapability`, a handler declares an `InputFilter` and returns `Pass`/`Suppress` from precomputed state, and all work is queued to the module worker or main. One `stache-macos` owner tap, host priorities `input_priority::HOLD_TO_QUIT` (300) > `MENU_ANYWHERE` (200) > `KEYBINDINGS` (100), overlap diagnostics, automatic re-enable, a service-owned press ledger and the reservation registry that yields lower-priority Carbon hotkeys. See [SDK](../../architecture/sdk.md#shared-input-inert-contract).
3. Quit means graceful `terminate` only, never `forceTerminate`. `Refused`, `NotRunning` and `Stale` are reported as such. The instance captured at ⌘Q-down is the only one that can be terminated. If the frontmost application changes during the hold, the press is cancelled: the HUD hides, nothing is terminated, and the rest of that physical press (repeats and key-up) is swallowed.
4. When Finder (`com.apple.finder`) or Stache's own process is frontmost, ⌘Q passes through unswallowed and gets no hold behavior. Stache is never terminated through this path.
5. A native HUD is required before interception: hidden installation/readiness is acknowledged before Running; failure means `Unavailable` and pass-through, never intentional invisible interception. The `stache-macos` HUD is a non-activating, click-through `NSPanel`, shown only from queued down work, with progress and “Hold ⌘Q to quit <App>”. Only a matching successful show acknowledgement arms termination. It hides on release, quit request, cancellation and lifecycle changes. Main owns all Cocoa objects; opaque `Send` handles carry no native object. Placement targets the frontmost app's display when public APIs provide geometry, otherwise reports its fallback. Part 02's minimal native design is an engineering proposal, not user-approved pixel values.
6. The legacy bugs are fixed. The shared service re-enables the tap. Pause, disable and stop invalidate any in-flight press, deadline and HUD immediately, so resume never quits from a stale press. The deadline is an owned, cancellation-wakeable wait, never idle polling.
7. Tests never install taps, terminate real applications, show real panels or prompt for permissions. Manual native gates need explicit approval for each run.
8. The Cmd-Q/logout teardown gap stays out of scope.
9. Match physical keycode 12 and Command-only (no Shift/Control/Option), not layout text. No new socket endpoint, including read-only status, and no new CLI. Callback decisions use only a prepublished generation-tagged nonblocking snapshot; no native work or application query there. Missing, invalidated or unconfirmed frontmost/exemption/HUD readiness fails open. Main revalidates exact frontmost identity before show and terminate; notification lag is an explicit limitation, not perfect OS/input atomicity.
10. The shared host input service must retain physical-press pairing through cancellation and register conflicts with Keybindings' ordinary Carbon hotkeys as well as HID filters. The shipped service already covers most of this. The press ledger keeps inert tombstones that swallow a suppressed press's repeats and up after unsubscribe, reload or invalidation. A HID subscription is registered in the reservation registry, which derives a yield for overlapping lower-priority Carbon reservations, and Carbon dispatch checks `try_dispatchable`. One gap remains. The suppressing tap is removed as soon as the last handler unsubscribes, even with a live tombstone. Part 04 makes the minimal additive fix (deferred tap removal), not a competing framework.
11. Frontmost publication into the shared input context is a host facility, added in part 04. A lease-started host watch over part 03's `watch_frontmost` calls `ContextPublisher::mark_stale` on activation ingestion, `publish_frontmost` after a confirmed refresh and `mark_unknown` otherwise. Host-documented `ExemptionFlags` bits mark Finder and Stache itself. It is reusable by later input consumers and runs only while a consumer holds a lease.
12. The SDK context carries only bundle id and exemption bits, not exact instance identity. Hold-to-Quit therefore keeps its own nonblocking decision state: part 03's `FrontmostWatch` snapshot and epoch, accepted config and HUD readiness. At ⌘Q-down it captures the exact `ApplicationId` and activation epoch, and main revalidates them with `preflight_frontmost` before the show and inside `terminate_gracefully`. Each physical press gets a fresh monotonic `hold_hud::PressId`. The SDK `input::PressId` is `(stream, epoch)` and repeats for successive presses of the same key in one epoch, so it cannot identify a show or authorization.

13. Part 06 (user request, 2026-11-07) supersedes the Part 02 rule "window animation behavior `None`, so no decorative fade" and "omit decorative fades" for Reduce Motion: the HUD now always animates its show and hide with a short fade, plus a slide when `animation` is `slide`; Reduce Motion turns a slide into a fade. The AppKit animation behavior stays `None`.

## Parts

| Part                               | Summary                                                                                                 | Status |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------- | ------ |
| [01](01-config.md)                 | Typed config, catalog, schema, bindings and typed migration                                             | done   |
| [02](02-native-hud.md)             | Native HUD adapter, fakes and ownership feasibility gate                                                | done   |
| [03](03-applications-frontmost.md) | Additive `applications` extension: frontmost observation/query, graceful terminate, self/display lookup | done   |
| [04](04-runtime.md)                | Runtime/factory over the shared input service: hold state machine, status, tray, teardown, race tests   | done   |
| [05](05-release-linger.md)         | Configurable `dismissDelay`: keep the HUD (progress frozen) readable after an early release             | done   |
| [06](06-hud-position-animation.md) | Configurable HUD `position` and `animation` (fade/slide) for show and hide                              | done   |

Order and blocking: `feat/hold-to-quit` is based on current `main` (Keybindings `4b8c2b3`), and parts 01–03 are done on it. Part 04 needs 01–03 and the shipped shared input service. Its two additive shared changes are listed in part 04: a frontmost activation listener and deferred tap removal. Each part builds, tests and commits independently. Only the orchestrator changes status. Parts 05 and 06 were added on 2026-11-07 after manual runs, at the user's request; Part 06 replaces Part 02's no-fade decision (see its Compatibility change). All parts are done; awaiting merge approval. A user-reported manual run on 2026-11-07 (after granting Accessibility to Stache Next) held ⌘Q and quit the app; other native cases remain unverified.

## Integration order

The roadmap order is noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → Bar → Tiling. noTunes, coordinated teardown, [Keep Awake](../2026-11-03-keep-awake/00-overview.md), [Wallpapers](../2026-11-03-wallpapers/00-overview.md) and [Keybindings](../2026-11-03-keybindings/00-overview.md) are on `main`. Reuse the landed helpers, context, accepted-config watch, input service and teardown slots, and keep catalog, bindings, index and roadmap routes additive. Adapter extensions leave noTunes' force API and tests unchanged.

The [consolidated Wallpapers audit](../2026-11-03-wallpapers/review-findings.md) (`git show f13cd3a:docs/specs/2026-11-03-wallpapers/review-findings.md`) informs these contracts: C02 immediate invalidation plus acknowledged retained cleanup; C06 same-cell typed accepted config/revision publication before notification; C07 cancellation/wake before joins and inert registration; C08/C19 additive current-main integration; C15 pre-send expiry versus uncertain issued effects; C18 native callbacks enqueue rather than reenter policy. These are requirements, not certified native behavior.

## Source evidence

- Feature: [`_legacy/docs/features/hold-to-quit.md`](../../../_legacy/docs/features/hold-to-quit.md).
- Runtime, in `_legacy/app/native/src/modules/cmd_q/mod.rs`:
  - Intercepts keycode 12 with Command only (`:271-289`) and swallows both down and up (`:291-302`).
  - Ignores repeats (`:307-316`).
  - Polls every 100/16 ms (`:30-34`, `:174-209`).
  - Calls `terminate`, then `forceTerminate` (`:408-421`).
  - Has an unused hint event (`:426-439`).
  - Pause and resume only toggle the tap (`:481-501`).
  - Uses `unsafe impl Send/Sync` (`:112-119`).
  - Never re-enables a tap that `DisabledByTimeout`/`UserInput` turned off; there is no handler.
- Config:
  - `_legacy/app/native/src/config/types/command_quit.rs:16-35`: defaults `false`/`1500`, no validation.
  - `config/types/root.rs:83-88` and `_legacy/stache.schema.json:22-29`: wrongly say “Enabled by default”.
  - Schema `:467-484`: `holdDuration` minimum 0.
  - Template: `config/template.rs:46-56`.
- Event `stache://cmd-q/alert`: `events.rs:81-87`. Nothing in the UI subscribes to it.
- Tray: legacy `modules/tray/mod.rs:79-106` toggles pause and resume through `LifecycleRegistry`. Next tray: [`tray.rs`](../../../src/crates/app/src/tray.rs) (config-backed Enable, session Pause).
- Applications adapter ([`applications.rs`](../../../src/crates/macos/src/applications.rs)) has:
  - exact `NSRunningApplication` identity through opaque `ApplicationId`;
  - `IssuancePermit` revalidation at the send;
  - `TerminateOutcome { Requested, NotRunning, Refused, Stale }`;
  - a 2 s `ACKNOWLEDGEMENT_BUDGET` and a 10 ms `CANCELLATION_QUANTUM`.

  `ApplicationObservation::terminate` and `ApplicationBackend::request_terminate` still call `forceTerminate` for noTunes. Part 03 added these alongside them:
  - `watch_frontmost` and `FrontmostWatch` (nonblocking `snapshot`, activation `epoch`, `is_epoch_current`, `refresh`) and `preflight_frontmost`, in `applications/frontmost.rs`;
  - graceful `terminate_gracefully` with a one-shot `PressAuthorization` and truthful `GracefulOutcome`, in `applications/graceful.rs`;
  - `display_for`, which returns a `DisplayHint`, in `applications/display.rs`.

  `FrontmostWatch` exposes no change notification; part 04 adds a listener for host publication. A timeout abandons an unissued request, and already-issued effects are uncertain. There is no late-terminate completion callback.

- [`migrate.rs`](../../../src/crates/cli/src/migrate.rs) runs `ConfigCatalog::rewrite_legacy` before typed inspection; part 01 removed `commandQuit` from `UNPORTED`.
- [`registry.rs`](../../../src/crates/sdk/src/registry.rs): `reconfigure` runs before the accepted cell publishes, and `Registry::accepted_watch` returns the same-cell typed `AcceptedWatch` (`read`, `wait_for_change`, `on_change`). A local `ApplicationObservation::accept` token is not the published revision; part 04 binds them as specified there.

## Intentional compatibility changes

1. No `forceTerminate` fallback. A refusal is reported, not escalated.
2. Finder and Stache frontmost pass ⌘Q through.
3. The instance captured at down is terminated, not whatever is frontmost at the deadline. A frontmost change cancels the press.
4. The HUD shows at down, replacing the unused hint event. `stache://cmd-q/alert` is not ported.
5. The tap is re-enabled automatically, and an interruption cancels the in-flight press.
6. Pause, disable, stop and reconfigure clear the in-flight press. No stale quit happens on resume.
7. Lifecycle is generic SDK behavior:
   - config-backed Enable;
   - session Pause/Resume;
   - Accessibility waiting and recovery without a restart;
   - live reload, where legacy read the config once at init.
8. The docs and schema say disabled by default, which matches the legacy struct.
9. There is no 16 ms polling. The deadline is one owned wait and the HUD animates natively.
10. Fn+⌘Q passes through as an ordinary ⌘Q; legacy ignored Fn. The exact Command-only filter keeps the Keybindings Carbon yield to ⌘Q alone.

## Open questions

None. Duration range, physical key matching, no endpoint and required HUD are settled above. Public-API compile proofs and manual display/Space checks remain implementation gates. Revising part 04 against the shipped APIs raised four points, which the user resolved on 2026-11-07:

1. **Unavailable mapping.** The shipped SDK has no module-reported `Unavailable`, and none is added. A HUD or input failure at start is `Failed(Start)`, retried through the tray. A HUD failure while running drops the subscription and passes ⌘Q through. The module stays Running with `detail` set through `ModuleFactory::project_status` and recovers through Pause→Resume.
2. **Fn+⌘Q passes through.** The filter is `ModifierMatch::Exact(COMMAND)` so the Carbon yield covers exactly ⌘Q. Fn+⌘Q is an ordinary ⌘Q; legacy ignored Fn. This is an intentional compatibility change.
3. **Documented limitations.** While Hold-to-Quit is subscribed, a Keybindings ⌘Q binding stays yielded even for presses Hold-to-Quit passes (Finder, self, unknown frontmost). After a tap disable, the remaining repeats and key-up of a claimed press pass through, as the shipped ledger drains. Neither is changed in the service.
4. **Shipped-code fixes stay in part 04.** Deferred suppressing-tap removal in the SDK input service and `ApplicationObservation::watch_frontmost_observed` are additive changes inside part 04, tested there together with Keybindings regressions.

## Validation and safety

Panic containment means preventing unwinding across FFI in unwind-enabled builds. The retained release profile uses `panic = "abort"`: it cannot recover arbitrary Rust panics. Production callbacks must therefore be bounded and nonpanicking; fake unwind tests do not establish release panic recovery.

Default tests use fakes for input, applications and HUD. A real-device check (tap, HUD, quitting a disposable app such as TextEdit with no unsaved document) needs explicit approval for each run, with the exact steps listed in part 04. Docs-only planning ran no build or native code.

## Affected docs

- The [planned feature notice](../../features/hold-to-quit.md) stays marked NOT SHIPPED until part 04.
- The [index](../../index.md) gets an additive route.
- Part 01 updates [config](../../architecture/config.md).
- Part 02 updates [overview](../../architecture/overview.md) (the macOS adapter list).
- Part 03 updates the shipped applications adapter description without changing noTunes' force behavior.
- Part 04 writes the feature doc and narrowly updates [testing](../../guides/testing.md), [new-module](../../guides/new-module.md), [SDK](../../architecture/sdk.md) (deferred tap removal, host frontmost publication), [overview](../../architecture/overview.md) (frontmost listener) and the landed roadmap/index additively.
- Legacy docs stay untouched.
