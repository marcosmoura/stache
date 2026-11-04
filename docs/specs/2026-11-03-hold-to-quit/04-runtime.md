---
status: draft
---

# 04 — Runtime, factory and teardown

Read when implementing Hold-to-Quit after explicit approval. Blocked until parts 01–03 and the upcoming Keybindings shared host input service land on `main`. All runtime behavior below is planned.

## Scope and integration gates

- `src/crates/modules/hold-to-quit/src/{runtime,policy,test_support,tests}.rs` and `lib.rs`; production factory in `src/crates/app/src/host.rs`, app dependencies and `src/crates/app/tests/host.rs`.
- Reuse the shared service injected through `ModuleContext`, not the Keybindings module implementation. The host owns the one **suppressing** native tap in `stache-macos` (plus the separate listen-only observation tap used only by passive consumers), fixed priority **Hold-to-Quit > MenuAnywhere > Keybindings**, declarative filters, pure snapshot decisions, interruption reporting and automatic tap re-enable. No private tap or competing input framework. No new CLI, socket endpoint (even read-only), window surface, custom tray item or config store.
- Consumes, and never reimplements, the **yielding reservation** primitive that Keybindings part 03 lands in the shared input registry. Before enabling suppression it must yield the overlapping ordinary Carbon reservation for physical Command-Q so one press cannot execute both the configured binding and Hold-to-Quit; disabled/paused/unready Hold-to-Quit releases the yield so the Carbon binding works again. Preserve other bindings and test registration order and recovery. Whether a suppressing HID tap already prevents Carbon dispatch is measured by Keybindings part 03's gate; do not duplicate that measurement or claim priority before it is recorded. If the primitive is genuinely absent, block this part rather than inventing another hotkey manager.
- Shared host service must own synchronous physical-press pairing and lock-free immediate cancellation state independently of the delayed module worker. The callback remains a pure decision function of immutable event + prepublished snapshot; host machinery applies its declarative pairing transition and nonblocking enqueue. A worker-published `swallow_until_up` alone is insufficient for a fast down/up or Command release while the worker is stalled. Prove this gate against the landed SDK, not an assumed API.

## Requirements

### Lifecycle and readiness

1. Descriptor ID `commandQuit`, no provides/executables/surfaces; require the actual shared input capability through its landed host service/recovery contract. Declare only its required permission (Accessibility per legacy). Reuse Keybindings' generic user-prompt-only input permission metadata: startup, enable, reload, polling and activation may check but never prompt; only the explicit tray permission request may prompt. This is a prerequisite, not current main's automatic-prompt policy or a second Hold-to-Quit permission framework. Factory construction/registration/configure are inert. Disabled starts no observer, panel, input registration or worker. Generic Enable/Pause/Resume/Retry and permission waiting/recovery remain the controls.
2. Start off-main: allocate generation/inbox/cancellation state; install part 03 activation observation before initial confirmed snapshot; acquire part 02 hidden HUD/readiness; establish accepted config publication; start owned worker; register input only once readiness/confirmation and conflict gates are satisfied. Return Running only after acknowledged required HUD installation, never after just enqueueing it. Unknown frontmost may remain Running but new Q presses pass until confirmed. HUD/input prerequisite failure reports `Unavailable` through the existing host readiness/service path, not invisible interception; unexpected lifecycle failures remain typed errors. Roll back acquired resources in reverse, retain any unacknowledged owners, and keep callbacks inert before waiting. Do not pretend current SDK has a special start-result API: rebase/reuse landed readiness reporting, or add the narrow generic host integration with tests here.
3. Publish one coherent generation-tagged atomic/small nonblocking decision snapshot: accepting, actual published accepted revision/config, frontmost ID/epoch/exemption confirmation, HUD readiness and input health. No callback mutex can be held by a worker/native wait. Contention/unconfirmed state fails open for a **new** press. Known stale identity is never eligible. Neither string classification nor application data is queried in callback. Part 03 main refresh produces it before input reads.

### Pure input and physical pairing

4. Match **physical keycode 12**, Command present, Shift/Control/Option absent; ignore Caps Lock/Fn and left/right distinction as legacy. Filter Q down/up plus modifier-change and interruption signals. The callback does no logging, allocation-heavy work, AppKit/AX lookup, application query, native action or blocking wait; it reads snapshots, returns Pass/Suppress, and host machinery nonblockingly enqueues a pure tagged event. All application/HUD/worker work is asynchronous.
5. At a fresh nonrepeat down, confirmed eligible nonexempt frontmost + required HUD/input readiness + accepting published revision means suppress and claim that **physical press**; otherwise pass and remember it as passed until Q-up. Finder, exact Stache self and unknown/unconfirmed classification pass entirely without HUD/terminate. Do not start interception halfway through a passed press when focus/readiness changes. A repeat never claims a new press or restarts timing.
6. Claimed repeats and Q-up remain suppressed regardless of changed modifiers/frontmost/readiness. Loss of Command-only eligibility (including flagsChanged before Q-up) immediately invalidates press authorization and wakes/cancels worker; modifiers themselves pass. Frontmost epoch change, reconfigure, HUD failure and pause/stop cancel authorization immediately. Remaining events of a claimed physical press are swallowed through Q-up by an inert pairing tombstone in the shared host service, even after module registration retires. No native HUD/application ownership remains in that tombstone. Tap disable/drop cancels and quarantines the press; if Q-up was lost, use the service's resynchronization/fresh-down boundary, not a sticky suppression flag or treating repeats as fresh. These are integration requirements, not assumed existing SDK behavior.
7. Bounded enqueue failure invalidates authorization immediately, marks snapshot unready and wakes an out-of-band/coalesced interruption path; a full queue cannot lose the only cancellation signal or authorize a quit. Host pairing still suppresses an already claimed press; new presses fail open. Re-enable/recovery needs fresh confirmation and a new physical press, never resumes an old hold.

### Hold, HUD and actual issuance

8. Worker starts `AwaitingShow` only for a claimed nonrepeat down. Capture fresh `PressId`, exact `ApplicationId`, activation epoch, generation, accepted config/revision, monotonic down timestamp and checked `down + Duration::from_millis(holdDuration)`. Unrepresentable deadline means a cancellable indefinite wait, not rejection, wrap or immediate quit. Queue main revalidation and show immediately; a geometry hint may be missing but identity may not be guessed.
9. Only matching successful `Shown` acknowledgement for that still-live press enters `Tracking` and arms the owned deadline. Down timestamp stays the origin (ack latency does not restart the hold); zero/already elapsed duration can proceed only after show success and current-state checks. Show nil/refusal/error/timeout invalidates press and HUD readiness, cancels the deadline, requests nothing and reports HUD `Unavailable` through generic status; no intentional HUD-less mode. A down suppressed before an unexpected show failure cannot be retroactively replayed: swallow its remaining pair, fail open on new presses. Late success is hidden/inert, never starts a timer.
10. Worker sleeps on one wakeable wait for input/control/show results/deadline/cancellation. No polling, idle timer, detached task or Rust animation tick. Release/modifier loss/cancel invalidates the shared one-shot press authorization **before** worker processing, so a queued main action is refused even if the worker is awaiting an acknowledgement. At deadline, drain already queued events before requesting issuance; a release/cancel timestamp ≤ deadline wins ties. Input not yet delivered cannot be known; document delivery latency and the part 03 notification race, not a promise that draining closes them.
11. Quit only once: validate same accepted typed config/revision/generation, matching acknowledged show, live press and elapsed deadline; queue main hide and graceful request against the captured instance. Main revalidates exact running/frontmost/nonexempt identity and epoch, then part 03 permit rechecks accepted token, acknowledgement deadline and one-shot press authorization at the actual `terminate` send. Never substitute current app, use PID to kill, force, retry or issue from callback. Release before the issuance commit prevents send; release after the irreversible commit cannot undo it. Outcome/late diagnostics are tagged and cannot mutate a successor. `Requested` ≠ exited; timeout after commit is uncertain, not `Refused`/proof of no effect. Hide on request/cancel/release; the rest of the physical press stays paired.

### Transactional reconfigure and publication

12. Rejected candidates leave old accepted config/revision, readiness and press coherent. Prepare all fallible work before acceptance; bounded acceptance failure must not publish a candidate or invalidate old accepted native tokens. On a successful changed reconfigure, cancel the press, stage the candidate and suppress no **new** presses until the **same typed value and actual accepted revision** are published; unchanged config is a no-op. Preserve the input registration when enabled→enabled, including pairing tombstones. Paused reload stages only next start; disabling uses stop.
13. Reuse the canonical accepted-config publication contract that all rewrites share, quoting it here so the shape cannot drift:

> Accepted-config publication contract (canonical, shared by all rewrites): the
> registry publishes the accepted typed config value and its actual revision as one
> coherent cell, before notifying waiters. A rejected candidate publishes nothing
> and leaves the previous accepted cell, resources and revision untouched. A module
> stages fallible resources from the candidate before acceptance, then observes the
> published accepted revision through a bounded, cancellation-wakeable watch; only
> after observing that same revision may it flip staged resources or issue
> candidate-derived effects. No callback into module code runs while the
> registry/config-store lock or the publication step is held. Publication is the
> single source of truth: no module-local revision counters, no `revision + 1`
> guessing, no metadata-only equality. The first landing consumer (Wallpapers part
> 03, else Keybindings part 05) implements this shape generically in the SDK; every
> later consumer reuses it and must not introduce a second shape.

This part does not own that SDK contract. It must use the landed same-cell typed
accepted-config watch/read handle; if it is still absent, block this part rather
than adding a second publisher, callback framework, store or revision guess. For
this runtime, from the moment a changed candidate is accepted until the module has
observed that same revision and flipped its staged runtime, **no new press may be
suppressed**: existing claimed presses may complete or be cancelled per requirement
12, but nothing may arm under stale or half-applied config. Once a newer revision
is observed, work derived from the old value must not arm. Failed reconfigure
publishes no accepted candidate; no lost wake between check/wait; stop wakes pending
publication waits before joins. At inspected `ad34b46`, `Registry::reload_inner`
assigns config/revision after `reconfigure`, and `ApplicationObservation::accept`
is only a local token; neither proves registry publication. Confirm the invariant
by test rather than assumption.

### Stop and ownership

14. Token cancellation precedes `stop` per SDK. Immediately make snapshot nonaccepting, invalidate generation/press/HUD authorization and cancel/wake timer/publication/input waits. Preserve only inert host pairing tombstones for a swallowed press. Retire/unregister input with acknowledgement; logically hide then release HUD; stop exact applications observation; only then return for registry worker joins. Never wait on main for a worker that awaits main. Off-main cleanup uses finite per-hop budgets; already-cancelled lifecycle must not skip physical cleanup. Preserve first failure and additional diagnostics; retained panel/observer/control ownership remains inert and blocks duplicate successor acquisition until actual cleanup. Retry/stop is idempotent and never reports success on timeout. A stalled native send already committed may finish; stop cannot revoke it. No global bounded-shutdown claim beyond those per-hop waits.

## Acceptance criteria

- [ ] HQ-04-01: Inert default-disabled factory and generic lifecycle/permission/readiness recovery work; required HUD failure is Unavailable/pass-through, with no extra CLI/IPC/UI.
- [ ] HQ-04-02: Pure nonblocking snapshot decisions, physical modifiers/keycode, exemptions and whole-press pairing work under worker delay, drop, tap interruption and modifier release before Q-up.
- [ ] HQ-04-03: Matching show acknowledgement gates checked full-u64 deadlines and at-most-once exact-instance graceful issuance; release/deadline/queued-send barriers prove cancellation and truthful uncertain outcomes.
- [ ] HQ-04-04: Frontmost change/burst, reconfigure, pause/stop and late HUD/native results cannot revive or retarget a press. Rejected revisions preserve old coherent state; accepted values arm only after matching actual publication without lost wakes.
- [ ] HQ-04-05: Timer/input/HUD/publication cancellation precedes joins; blocked main/issuance cleanup returns bounded truthful errors with inert retained ownership, no duplicate resources or successor mutation. Idle wait does no polling.
- [ ] HQ-04-06: Shared HID priority and ordinary Carbon conflict registration prevent duplicate actions in both registration orders; no private tap/framework/module dependency is introduced.
- [ ] HQ-04-07: Feature/testing/new-module/index updates describe verified shipped behavior, limitations and manual coverage honestly, without claiming the separate own-Cmd-Q/logout host gap is fixed.

## Tests

All automatic tests use shared-input fake, fake clock, `FakeApplicationBackend`, `FakeHudBackend` and controllable/never-draining main executor. No real taps, panel, app termination or prompts.

- Modifier matrix; repeat; fast down/up before worker; flagsChanged Command release then Q-up; passed press becoming eligible; Finder/self/unknown metadata; exact self versus another `stache`; full-u64/zero/checked-deadline overflow.
- Release before/equal/after deadline and before/after native issuance commit, with worker/main barriers; show pending at deadline; late/refused/nil/timeout/cancelled show; no termination unless matching show succeeded. Queue overflow before release cannot leave an armed timer.
- A→B, A→B→A and actual main focus change before notification delivery; exempt app at show/send preflight; captured app exits/PID reused. Never kill replacement/current app. Late diagnostics after successor remain inert.
- Reconfigure rejection and native acceptance timeout preserve old value; barrier between successful reconfigure and actual registry publication; same-value different revision, rapid accepted changes, unchanged reload, lost-wake boundary, stop during publication wait. Extend generic SDK tests only when adding the watch.
- Stop while show/native issuance or cleanup is blocked, before and after commit; main queue never drains; observer/HUD cleanup error/timeout then eventual drain/retry; no fresh panel duplicates; immediate inert state and cancellation before all joins; no idle wakeups.
- Host Enable writes JSONC; Pause writes nothing; malformed/deleted reload holds last-good plus diagnostics; permission denied→granted; automatic triggers never prompt, explicit tray request alone may request trust; HUD/input failure→Unavailable→Retry; unrelated modules remain functional. Shared Carbon/HID overlap tests cover HQ priority, disabled/paused/unready reservation release, registration order, MenuAnywhere and Keybindings coexistence.
- Manual, approval **per run**, only after compile/fake gates: use Next identity and a disposable app with no unsaved work (e.g. TextEdit). Tap versus hold; repeat/Command-release; switch focus mid-hold; Finder pass-through; pause mid-hold; display/Space/focus/click-through. Quit only the disposable app; never target Stache or unsaved work. Record skipped cases as unverified. Builds do not authorize launching, granting permissions or native smoke.

## Notes

After implementation run `cargo nextest run -p stache-hold-to-quit -p stache-macos -p stache-sdk -p stache-notunes`, `cargo nextest run -p stache-app --features dev-tools`, `bun run lint`, `bun run format:check`, `bun run test`, `bun run check:generated`, `bun run build`, `bun run tauri:build` and `RUSTDOCFLAGS='-D warnings' cargo doc -p stache-hold-to-quit -p stache-macos -p stache-app --no-deps`. These are future gates, not planning results.

Replace the [feature notice](../../features/hold-to-quit.md) with verified behavior; narrowly update [testing](../../guides/testing.md), [new-module](../../guides/new-module.md), [SDK](../../architecture/sdk.md) if its read/watch contract changes, and the index/landed roadmap additively. Do not claim to fix the known own-Cmd-Q/logout coordinated-teardown gap.
