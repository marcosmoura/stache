---
status: done
---

# 08 — Native tabs, passive drag and opt-in animation

Read when implementing after 07 and separate approval. Adapter compile gates from 03 must already pass.

## Scope

Tiling `src/{tabs.rs,drag.rs,animation/}` extending 05's frame-effect seam (`actor/`, `state::Effect`, `backend.rs`). No SDK/macOS input change: landed `InputEvent::location()` (`sdk/src/input/event.rs`) is set for mouse down/up (`macos/src/input/convert.rs`), observers subscribe via `InputCapability::subscribe_observer` and implement `InputObserver::on_event`/`on_discontinuity` (`sdk/src/input/{capability,subscribers}.rs`), and the listen-only tap exists only while observers exist and always passes (`sdk/src/input/service.rs`). MouseMoved/dragged events are deliberately masked and not needed: legacy `events/mouse_monitor.rs:175–183,251–254` used dragged events only to keep the down flag set, so drag uses down/up with location plus window frame snapshots. App injects same host service; no Keybindings module import. Map inventory tabs/drag/animation files. Public command contracts remain 07's.

## Requirements

- Native tab snapshots are exact-app/window-token scoped, replace atomically, scan once per app initially and reconcile on tab group changes. Inactive tabs are tracked, not extra tiles; tab-only create/destroy must not trigger whole layout; active-member/group changes invalidate affected geometry safely. No global TabRegistry or CG ID-only cache. Missing/unreliable tab capability is explicit; do not arbitrarily exclude ordinary windows or claim working tab suppression before native evidence.
- Reuse one host Keybindings input service. Passive mouse down/up observation uses the service's **listen-only tap** (user-approved exception): the service owns at most one suppressing tap plus one listen-only observation tap that exists only while passive subscribers exist. Tiling always passes and can never suppress, intercept, inject, or change HQ > MA > KB priority; observation must still see the physical down/up which an interception consumer suppressed, via the separate listen-only path, with bounded queueing and explicit discontinuity reporting. No second suppressing tap, no private tap, and no event injection or change to suppressor callback semantics. Listen-only tap permission (Input Monitoring) is handled by the existing input service and is not a typed `Permission`; add no new `Permission` variant. If the listen-only tap cannot be installed, degrade only drag features with a diagnostic, never by reusing the suppressing tap opportunistically.
- Callback only enqueues values; off-callback actor captures exact window snapshots/frames and accepts external geometry while a drag is active. No native AX walks in mouse-up callback. Drag end computes ratio updates for resize; floating movement remains user-owned. Swap: among non-floating snapshot windows, select the one with the **greatest centre travel** whose distance is strictly **>50 logical px** as the dragged window (`init.rs:1894–1922`); then pick the **first** snapshot, in stable workspace order, whose original frame **inclusively** contains the dropped centre (`init.rs:1938–1941`), ignoring the dragged window and floating windows. Otherwise one snap-back plan. Boundary exactly 50 does not swap. Revalidate both tokens/workspace/plan at commit.
- Input overflow/disabled tap/permission loss/stop cancels gesture, marks lost observation and reconciles once; never invent missing up or replay click. Optional drag service failure degrades only drag with status warning; ordinary tiling/other modules remain healthy. Distinguish user geometry from self-write echoes without suppressing genuine user changes indefinitely.
- Animations remain off by default with the defaults and easings already in `config/layout.rs` (`AnimationConfig`, `EasingType`); duration is distance-scaled below 500 px as legacy. Port legacy pure easing/spring/transition semantics, not global worker. One owned worker with latest-plan bounded mailbox; retarget from confirmed interrupted position, not assumed destination. Each intermediate/final write rechecks authority; partial/refused/unresizable stops that target, no infinite final snap loop.
- Preserve bounded animation-active/settling echo suppression; after settle read actual geometry once. Completion acknowledges exact plan sequence and actual frames only; interrupted/superseded jobs cannot mark successor applied (A–B–A). Stop seals/cancels/wakes animation, drains issued writes before 06 restore, retains quarantine if unable to drain. No restoration followed by a late frame animation or native handle cache reuse.

## Acceptance criteria

- [x] TI-08-01: Exact native tab grouping/lifecycle and unavailable capability tests preserve one visible tile without tab-only relayout or stale-token leakage.
- [x] TI-08-02: Passive observation uses only the service's listen-only tap, cannot suppress/intercept/inject or alter priority, closes with the last passive subscriber, and gestures cancel across discontinuities and bounded drops.
- [x] TI-08-03: Drag-resize ratios and the max-distance >50px dragged-window selection with inclusive first-matching target containment pass exact identity/order/threshold fixtures, including two windows both moved beyond 50px.
- [x] TI-08-04: Animation defaults/easing/pacing/retarget/outcomes are bounded, truthful and generation-safe, draining before owned visibility restoration without retry fights.

## Tests, rollback and affected docs

Fake tab AX trees/attributes, multi-tab app, same CG ID reused, group switch/delete; fake input HQ/MA/KB+passive Tiling overlapping events, suppressed down/up, drop/disabled epoch, 49/50/51px and target edge cases; fake clock/native writer refusal/partial/late/cancel/reload/A–B–A. Default tests never install a tap or move windows. Native tab/drag/animation gate requires each-run approval under [validation](validation.md).

Rollback cancels/drains optional resources and leaves core/commands intact; preserve shared input's other consumers. Update feature, SDK input limitations, testing/performance docs for landed optional behavior only.
