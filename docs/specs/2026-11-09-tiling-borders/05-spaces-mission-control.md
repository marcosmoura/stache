---
status: draft
---

# 05 — Spaces and Mission Control

Read when implementing after part 04 and separate approval. Paths are under `src/crates/`. Still behind `native-borders`. Decision 9 (resolved OQ-5): a border visible in Mission Control, App Exposé, Show Desktop, a Space switch animation, a fullscreen transition or on an inactive Space is a **defect**. This part is `done` only after its native check passes (TB-05-04).

## Scope

New pure `modules/tiling/src/borders/spaces.rs` (gate state machine) wired into `borders/engine.rs`; new `macos/src/border_overlay/spaces.rs` (owner thread and native sources) with a fake; `macos/src/ax/observer.rs` (`AxNotification` gains the Dock Exposé notifications); new `macos/src/skylight/space_events.rs` (gated, `dlsym`-resolved; part 06 reuses its registration helper); the overlay collection behavior constant in `macos/src/border_overlay/native.rs` (part 01). Studied for approach only: JankyBorders `src/events.c` (`space_handler`: delayed redraw after a Space change), `src/border.c` and `src/misc/window.h` (per-Space placement with `SLSMoveWindowsToManagedSpace`, private tags, no draw on an invisible Space; no Mission Control detection found); yabai `src/mission_control.c` (Dock Accessibility notifications for Mission Control, App Exposé, Show Desktop and exit). Nothing is copied, translated or closely paraphrased (decision 5).

## Requirements

- **Three layers, all required.** (1) Structural: the window server itself keeps overlays out of Exposé and off other Spaces. (2) Detection: every known signal hides all overlays as early as it arrives. (3) Fail closed: any unknown state hides all overlays. No layer is optional because no single one is proven sufficient.
- **Structural (overlay window).** Collection behavior `transient` (Apple: hidden by Exposé) and `ignoresCycle`, and none of `canJoinAllSpaces`, `moveToActiveSpace`, `stationary`, `managed` or `fullScreenAuxiliary`, so an overlay belongs to one Space, never appears on a fullscreen Space and is not shown by Exposé. The exact set is a constant asserted by tests; the native check confirms each flag's effect on macOS 26, and a flag that does not behave as documented is replaced only by another set that passes the same check.
- **Gate state (pure).** `SpaceGate` holds a set of suspension reasons: `MissionControl` (Mission Control, App Exposé, Show Desktop), `SpaceTransition { display }`, `FullscreenTransition { window }` and `Unknown { cause }`. Entering any reason sends `HideAll { reason }` (part 01) at once and bumps a gate epoch; while any reason is present no `Show` or re-order is sent. Leaving the last reason enters `Resuming`: each bordered window is re-confirmed on the active Space of its display, then goes through part 04's show-after-visible rule plus one display frame. Model updates continue while suspended, so resume shows the current state.
- **Ordering guarantee.** `HideAll` is applied on main ahead of any pending batch (part 01). Every `Show`, `Order` and appearance change carries the gate epoch it was computed under; the main-thread side compares it with the epoch of the latest `HideAll` it applied and drops older ones. A `Show` queued before a suspension can therefore never be applied after it.
- **Detection sources.** All are owned by one thread `stache-tiling-borders-spaces` with its own run loop; callbacks only record a bounded fact and wake the engine (no AX attribute read, no allocation beyond fixed buffers, no lock held while calling out):
  - Dock Accessibility notifications for Mission Control, App Exposé, Show Desktop and their exit, observed on the Dock process through `ax/observer.rs`. A Dock launch or termination (`NSWorkspace` application notifications for `com.apple.dock`) re-registers; while unregistered the reason `Unknown { dock }` holds.
  - The private SkyLight Space-change notification (gated under decision 4, `dlsym`-resolved): enters `SpaceTransition` for the display; it is left only after the public `NSWorkspace.activeSpaceDidChangeNotification` (or the private completion, whichever the native check proves later) and a fresh Space confirmation.
  - Fullscreen: an AX `AXFullScreen` change, a private Space change of the window, or a frame jump to the display bounds for a bordered window enters `FullscreenTransition` for that window and hides its overlay; leaving fullscreen re-confirms like a reveal.
  - A listen-only event observer for Space-switch gestures and shortcuts is a **candidate** enabled only if the native check shows the sources above arrive after the first transition frame; its exact mechanism is decided in that run and recorded here before it is implemented.
- **Inactive Spaces.** An overlay may be shown only after its target is confirmed on the active Space of its display: the gated private window-Space and current-Space queries, else the public `CGWindowListCopyWindowInfo` on-screen listing for the target id (no Screen Recording needed for that key; gate item). Confirmation runs off main, only on show and resume, never periodically.
- **Fail closed.** `Unknown { cause }` holds, and every overlay stays hidden with one diagnostic `borders: Spaces or Mission Control state cannot be determined (<cause>); borders hidden`, whenever: the Dock observer is not registered or its mailbox overflowed, a required private symbol is missing or registration failed, a Space query fails or is ambiguous, a display or Space layout change is in progress, or the owner thread is not running. It clears only after the cause is fixed and a full re-confirmation. A permanently missing capability keeps borders hidden (borders degrade, Tiling does not).
- **Lifecycle.** The owner starts with the engine and stops before `release_all` ([part 07](07-lifecycle.md) stop order); stop guarantees no callback after release. Disabled, nothing is registered.

## Acceptance criteria

- [ ] TB-05-01: Pure `SpaceGate` tests: each reason enters suspension with one `HideAll` and an epoch bump; overlapping reasons resume only after the last clears; resume re-confirms then applies part 04's show rule and one fake frame, in that order; model changes during suspension are reflected on resume.
- [ ] TB-05-02: With the fake overlay backend and executor, a `Show` computed before a `HideAll` and applied after it is dropped; `HideAll` overtakes pending batches; the collection behavior constant equals the specified set.
- [ ] TB-05-03: With fake sources, every fail-closed cause (Dock absent, Dock relaunch, observer overflow, missing symbol, registration failure, query failure, display change, owner stopped) keeps all overlays hidden with exactly one diagnostic, and clears only after re-registration and re-confirmation; fullscreen enter/exit for one window hides only that border.
- [ ] TB-05-04: The native check below was run with per-run approval and passed; its method, raw evidence summary and any changed mechanism are recorded in this part.

## Tests

`borders/spaces.rs` pure tests on a fake clock; engine tests combining `SpaceGate` with part 04's fakes (reveal during suspension, Space change during a workspace switch, Mission Control during an animation frame stream); `macos/src/border_overlay/tests.rs` for the epoch drop and `HideAll` priority; a fake Dock/SkyLight source injecting enter, exit, duplicate, out-of-order, overflow and relaunch. Default tests register no observer, resolve no symbol and create no overlay.

## Native gate (per-run approval)

On macOS 26 with owned test instances of Ghostty and one more application named in the approval, at least two Spaces, and one and two displays (the "Displays have separate Spaces" setting recorded). Repeat each scenario 20 times: Mission Control by keyboard and trackpad, App Exposé, Show Desktop, Space switch left/right by keyboard and by trackpad swipe, Space switch by clicking a Space in Mission Control, fullscreen enter/exit by green button and shortcut, and a bordered window left on an inactive Space. Evidence: a 60 fps screen recording made by the user with a system tool (Stache gets no Screen Recording permission), reviewed frame by frame, plus the overlay command log with timestamps and gate epochs. Then repeat Mission Control and Space switches with the Dock observer and private symbols forced unavailable. **Pass:** zero frames with a visible Stache border in every scenario and run; borders return after each transition ends; with sources forced unavailable, all borders stay hidden with the diagnostic. Any visible border fails the run (overview OQ-B).

## Review checklist

- [ ] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: Space handling inspired by, not derived from, JankyBorders `events.c`/`border.c`/`window.h`; yabai studied for approach only; private declarations written from observed signatures.

## Notes

- Rollback: none in production (feature-gated); a failing part blocks part 08.
- Unverified until the gate: the effect of `transient` on macOS 26, Dock notification names and timing, private Space-change timing relative to the first transition frame, and fullscreen detection latency.
- Docs: none lasting until part 08 (feature doc states the guarantee and the fail-closed diagnostic).
