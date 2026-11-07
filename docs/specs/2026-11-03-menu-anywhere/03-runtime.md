---
status: done
---

# 03 — Runtime and host integration

Read when integrating MenuAnywhere with the shared input service. Approval is required before this part.

## Prerequisites and files

Depends on Parts 01/02 and the **landed** Keybindings shared input service and generic user-prompt-only metadata (Keybindings Parts 02/05) in `main`. Part 02 requires Hold-to-Quit Part 03's landed frontmost observation/identity extension and itself owns the additive exact-instance activation operation in existing `applications`; no activation dependency is fulfilled by Hold-to-Quit. This part is blocked until those capabilities compile against the rebased tree. The service's method names remain planned here; never implement a MenuAnywhere private tap, duplicate SDK service or extra prompt-policy hook to bypass that dependency.

- New `src/crates/modules/menu-anywhere/src/runtime.rs`, `trigger.rs`, `backend.rs`, adjacent tests and fake support; export the planned `MenuAnywhere` module from `src/lib.rs`. Update the crate manifest for SDK/native adapter wiring, keeping module policy testable with fakes and platform APIs inside `stache-macos`.
- `src/crates/app/src/host.rs`: additive `MenuAnywhereFactory`, `ModuleFactories::production`, and production registration tests. Reuse the first landed generic factory context; if it still lacks needed dependencies, extend that one context rather than add per-feature constructors. `src/crates/app/src/lib.rs` supplies the same host-owned services; app Cargo wiring is additive.
- `src/crates/sdk/src/context.rs`, `registry.rs`, `module.rs`, `lib.rs`, their lifecycle tests, and the landed shared input adapter: only narrowly necessary additive acceptance/context capability changes discovered by the compile gate. No new tap owner; preserve existing registry constructors/consumers or migrate all actual callers additively in the owning integration.
- `src/crates/app/src/bindings.rs` and status projection tests: retain ConfigStore-held file diagnostics in generic `ModuleStatusView`; no new command/event/status endpoint. Update `docs/features/menu-anywhere.md` to shipped behavior only when accepted; narrow additions to `docs/architecture/sdk.md`, `docs/guides/new-module.md`, `docs/guides/testing.md`, and additive index/roadmap entries when present. Preserve noTunes/proxyAudio and all later landed registrations.

## Shared input contract

There is exactly one **suppressing** native tap owner in `stache-macos`, host-created and supplied through `ModuleContext` (a separate listen-only observation tap may exist for passive consumers; MenuAnywhere never uses it and can never be passive). Register an owned subscription with a declarative event/button/modifier filter and a pure decision callback. Priority is Hold-to-Quit > MenuAnywhere > Keybindings. Shared-service overlap diagnostics and disabled-tap re-enable remain that service's job; test their integration, do not reimplement them.

Inherit the shared service's finite subscribe/unsubscribe gate: 2 s per-hop acknowledgement and cancellation observed within a 10 ms quantum subject to scheduling. Expired/cancelled queued installation cannot become ready late; logical unsubscribe is immediate, and unacknowledged native removal retains inert cleanup ownership with an explicit failure/pending result. Compile-/fake-test these inherited capabilities before runtime wiring; do not create another wait/policy framework or claim a global shutdown bound.

RightClick requires rightMouseDown **and button 1**; middleClick requires otherMouseDown **and button 2**. Reject leftDown, left/right metadata mismatch, other button numbers including 3+, null/invalid events and all unrelated event kinds. Compare `(flags & 0x1E0000)` exactly to the accepted config's OR mask. Extra supported modifiers reject; unrelated flags do not. Empty modifiers capture bare matching clicks, as explicitly cautioned in Part 01.

The callback reads only an immutable prepublished accepted filter/health snapshot plus nonblocking admission state. No AX, NSWorkspace, app activation, allocation-heavy traversal, lifecycle call, blocking mutex, logging I/O, prompt or synchronous dispatch/wait. It may compare pure event metadata, claim a slot and enqueue. Permission granted, input service healthy, accepted enabled/nonpaused runtime and live generation must all be prepublished before a match can suppress.

Admission rules are explicit:

| Condition                                                        | Input decision | Runtime outcome                                         |
| ---------------------------------------------------------------- | -------------- | ------------------------------------------------------- |
| Wrong button/flags, unhealthy/not ready, dead generation         | Pass           | No work                                                 |
| Eligible match, free single in-flight reservation and queue slot | Suppress       | One request                                             |
| Eligible match, building/showing/invoking or pending reservation | Suppress       | `Coalesced`, drop newest                                |
| Admission queue unexpectedly full with no busy reservation       | Pass           | `QueueFull`, release own reservation                    |
| Queue disconnected/worker unavailable                            | Pass           | Mark health false; host reports service/runtime failure |

Use one bounded request slot and one atomic busy reservation; queued, reading, presenting and invoking are all busy. A busy matching trigger is swallowed and dropped intentionally (legacy bug fix), never overwrites location, queues another popup or replays the click. Counters are nonblocking and read/logged off callback. Queue-full failure before suppression never claims a popup; queue pressure cannot leak a reservation or suppress unmatched input. An admitted trigger remains consumed if later capture/read fails; do not synthesize a replacement click. A concurrent invalidation after the callback's decision may consume that one already-admitted click while cancelling its work; document the linearization boundary rather than claim retroactive pass-through.

## Acceptance coherence and identity

Each trigger captures one coherent `{typed config/filter, accepted revision, runtime generation, permission/service health, trigger time, Quartz point}`. Do not publish separate atomics that can mix new flags with an old revision. Stage and validate fallible subscription/filter replacement without side effects, then publish at registry acceptance; rejected prepare/reconfigure leaves the previous filter and revision intact. A callback during staging may use the previous accepted tuple but may not use a candidate presented as accepted.

Implementation note (2026-11-07): the shared-service subscription uses a fixed, config-independent prefilter (`MouseDown`, buttons 1 and 2, any modifiers). The published tuple's exact matcher is the filter, so "filter replacement" here means publishing a new tuple; no `Subscription::set_filter` runs on reload.

**Compile/test gate:** inspect the landed registry's `reconfigure` → accepted snapshot/revision publication ordering. Existing `reconfigure` returns before the registry publishes the revision; simply reading `snapshot()` inside it is not acceptance coherence. The canonical contract shared by all rewrites is:

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

This part must reuse that landed contract; if it is still absent, block this part rather than adding a second publisher or guessing a revision. No native work under the acceptance gate. Publication and subscription/filter commit must be ordered so readers only observe complete accepted tuples, including unchanged/rejected reload, disable and paused reload. The gate is an implementation feasibility requirement, not a claim that a particular draft SDK API exists today.

At the earliest owner-worker opportunity after admission, capture frontmost exact identity through `applications`, recording capture time separately from event time. No authoritative identity exists in the callback and no promise of event-time app identity is made. A different app can become frontmost before processing; that app is the observed capture target, not a reconstructed click-time target. Bound the capture hop by the adapter's 2 s window and reject stale queued requests. After capture, never retarget. Check that same instance/frontmost before showing; at action, target the captured instance, even if focus changed during tracking. Use Part 02's exact-instance send guards, self-target exclusion and uncertain outcomes.

Enabled reconfigure changes only admission for **new** triggers. An already-admitted presentation retains its captured revision authority and may finish under that revision; current config equality is not an action cancellation test. Authority is scoped to its unique generation/presentation, invalidated by pause, disable, permission/service loss, stop or shutdown. A stale completion can only release its own busy reservation; it cannot clear successor state. No revision token from the shared applications adapter may accidentally revoke an accepted live popup when the filter changes.

## Lifecycle and diagnostics

Factory construction/registration are inert, including enabled config. `configure` only validates/prepares pure state. `start` acquires owner resources/subscription transactionally and publishes ready only after acknowledged success; start failure rolls back exact partial resources. Repeated lifecycle calls are safe, one subscription per runtime. Native reads start only on a trigger, not during registration/start. No app-main waits for a worker.

Use SDK lifecycle semantics: disabled → `Disabled`; paused → `Paused`; missing Accessibility → `WaitingForPermission`; missing/failed shared input service → `Unavailable`; genuine start/stop failure → `Failed` with operation/category. `Running` means owned, healthy resources, never a thread-spawn optimism flag. Inherit the landed generic user-prompt-only metadata for shared-input consumers: startup, enable, reload, tick and activation check/poll only; only explicit tray request may prompt, never the module or input callback. Current main permits an automatic prompt; that baseline is not the required policy and must not be assumed already changed. Reuse Keybindings' generic policy, preserving other consumers' defaults, with no MenuAnywhere-specific hook. Loss immediately republishes ineligible health, invalidates work, then tears down; recovery restarts only enabled/nonpaused modules. Input re-enable may recover a transient service failure; failed recovery remains unavailable, not Running.

One read/action error records a typed outcome/counter and scoped warning off callback; it does not asynchronously rewrite registry lifecycle to Failed or kill healthy admission. Avoid logging menu titles or contents. Separate read timeout, coalesced, stale, action uncertain, and cleanup failure; success does not erase held infrastructure/file errors. The app's read-only status projection overlays ConfigStore parse/type/I/O/deletion diagnostics without disk access; invalid reload retains accepted revision/runtime and a repaired file clears that diagnostic. No new CLI or Settings surface.

Stop/disable/pause first revoke callback eligibility and atomically invalidate presentation/action authority, independent of any stalled send. Unsubscribe, cancel exact tracking, request owner cleanup, then let registry cancel/join owned workers in its documented order (the registry may already have cancelled the lifecycle token before `stop`). Cleanup acknowledgements use their own fresh bounded wait, never the cancelled token. Report timeout/removal failure and retain inert owner-safe resources/pending state for eventual cleanup/retry. Never report released resources just because cancellation was requested. No synchronous worker/main cycle or detached unowned thread; a native call in flight may finish under its AX timeout before the owner joins, with measured latency rather than an invented instant deadline.

Ensure the host remains dispatch-capable until registry reverse-stop and exact main cleanup acknowledgements/drains complete; reuse landed begin-shutdown/factory finish hooks if any, not a second shutdown framework. Queued jobs recheck authority at native send. An already-issued activation or press is irreversible; uncertain outcomes never retry. Test stop with an open modal menu and a stalled press. The separate Stache Cmd-Q/logout coordinated-host-teardown gap is a documented non-goal, not fixed by this part; use the existing supported shutdown path for acceptance.

## Acceptance criteria

- [x] **MA-03-01** Rebased runtime consumes the landed host input service (including inherited finite subscribe/unsubscribe gates), Hold-to-Quit frontmost identity and Part 02-owned exact activation; production factory adds MenuAnywhere inertly without losing noTunes/proxyAudio or later consumers; one tap owner remains.
- [x] **MA-03-02** Filter tables prove exact event/button/flag matching, including `[]`, mismatched right/left and other buttons >2; priority/overlap/re-enable behavior is exercised with a shared-service fake.
- [x] **MA-03-03** Admission tests prove suppression/pass rules, one reservation, Coalesced busy drops, bounded queue pressure/disconnect handling and callback purity; no queue growth, click replay or stuck busy state.
- [x] **MA-03-04** Barriers prove config/revision acceptance coherence across staged/rejected/unchanged/paused/disabled reload and callbacks racing publication; prior captured presentations remain valid only across enabled reconfigure.
- [x] **MA-03-05** Trigger/capture timing is reported honestly; identity staleness at capture/show/action, self target, PID reuse and delayed dispatch never retarget an existing presentation or issue abandoned work.
- [x] **MA-03-06** Fake host/registry integration proves inherited user-prompt-only metadata (no request on startup/enable/reload/tick/activation; explicit tray request only), correct permission/service/lifecycle states, transactional failed-start rollback, repairable file diagnostics and recovery without overriding disabled/paused intent.
- [x] **MA-03-07** Stop/disable/pause/permission loss invalidate immediately; exact tracking/AX cleanup acknowledges or reports pending failure, selected resources survive in-flight press, and late results cannot mutate a successor.
- [x] **MA-03-08** Supported host shutdown remains responsive, dispatches modal cancellation and joins owners without deadlock; default tests perform no native effects; approved manual gates and lasting behavior docs match observed results.

## Tests and validation

Use fake input service, applications/menu adapters, monotonic clock, accepted-publication cell and controllable main dispatcher. Barrier-test before/after subscription install, admission, identity capture, read, presentation, activation, press and cleanup. Inject worker spawn failure, subscription rejection, queue full/disconnect, dispatcher rejection, permission revocation, tap disabled/re-enable failure, cleanup timeout and a native send held indefinitely until the test releases it. Verify immediate logical invalidation while held, honest pending cleanup, exactly-once eventual release and no action/cleanup affecting a new generation. Test selection racing tracking close so the press lease cannot be freed early. Race reconfigure at both acceptance and immediate-send boundaries.

Use actual Registry/ConfigStore/factory/status projection integration with temporary files and fake services, not just a second policy model. Preserve existing production order constraints and all registered IDs. Default tests never install a tap, read real AX, activate, press, show a menu/panel or prompt.

Count fake permission status/request calls for startup, enable, reload, tick, activation and explicit tray request, including absent/granted/revoked trust and paused/disabled recovery; only the explicit request may reach fake `request`. Hold subscribe/install and unsubscribe/removal acknowledgements behind barriers: cancellation/expiry prevents late readiness, removal timeout remains pending/inert, and eventual cleanup cannot remove a successor. These are consumer-integration checks of inherited gates, not duplicate shared-service ownership criteria.

After implementation: `cargo nextest run --workspace --features stache-app/dev-tools`; `cargo check --workspace --all-targets --features stache-app/dev-tools`; `bun run lint`; `bun run format:check`; `bun run test`; `bun run test:ui:coverage`; `bun run generate-schema -- --check`; `bun run generate-bindings -- --check`; `bun run build`; `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps`. Build/package checks are not approval to launch the enabled feature.

## Per-run approved manual integration checks

After Part 02 gates, ask approval for each run's temporary Next config, exact launched test app PID and effects. Do not edit real user config, run legacy, enable unrelated modules or kill by executable name. Preserve/restore any explicitly approved permission change rather than automate a system prompt.

1. Start isolated Next with MenuAnywhere disabled, then enable through the temp file. Verify a single shared tap/subscription, default exact chord, extra modifier pass-through, middle button 2 only and overlap priority. Test `[]` only with explicit bare-click-capture approval and restore the safer chord before ending the run.
2. Trigger rapidly while reading/tracking: only the first popup appears; busy matches are consumed/dropped. Reconfigure enabled while tracking and choose the harmless instrumented test action: the captured revision may finish, subsequent triggers use the new filter.
3. Pause/disable using the supported host controls during tracking and before an action. Verify menu cancellation during tracking, no unissued press, acknowledged exact cleanup and retained pending diagnostics on injected timeout. Repeat using supported tray Quit; do not claim Cmd-Q/logout coverage.
4. Switch frontmost between trigger processing/read/show and action; terminate/relaunch only the owned test target with approval. Verify capture-time semantics, stale show drop, exact captured-target action and no fallback to the newly frontmost app. Record timestamps/outcomes; stop the launched processes and remove only owned temp files.

No system-changing run occurs during this docs-only task. Each part stays draft until separately approved, implemented and verified by the orchestrator.

## Gate evidence

Runs of 2026-11-07/08 on `feat/menu-anywhere` (after `7fb0ea2`), user present, `bun run tauri:dev` with `XDG_CONFIG_HOME=/tmp/claude/stache-ma-gate` (all other modules disabled; the real user config untouched).

| Check                                                                                              | Result               | Evidence                                                                                                                                                                                                                             |
| -------------------------------------------------------------------------------------------------- | -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 1 enable, chord, pass-through                                                                      | Pass                 | Enabling through the temp file took effect without restart; control+command right-click showed the frontmost app's menu at the click; an extra Shift, a plain right click and a middle click passed through; a harmless item worked. |
| 2–4 busy coalescing, enabled reconfigure, pause/disable during tracking, staleness, supported quit | Pass (user-reported) | The user ran the remaining checks and reported all of them working; individual timings were not recorded.                                                                                                                            |
