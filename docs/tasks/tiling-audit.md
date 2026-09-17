# Tiling audit 6

Date: 2026-09-15

Scope: app/native/src/modules/tiling/, plus directly relevant configuration, native helpers, and dependency implementations.

Baseline: working tree at commit 93f1a03b1c6e9d3c6e26e739f07b20875edba2b8, including pre-existing local dependency/configuration changes.

## Executive summary

The best-supported performance opportunities are reducing native work and removing blocking work from event processing, not replacing the layout algorithms wholesale. The current actor boundary is useful, but native discovery, animation, visibility, and global notification access cross that boundary inconsistently.

This audit identifies 20 correctness, lifecycle, and latency findings, followed by eight performance proposals and a staged architecture proposal. The highest priorities are:

1. Prevent the reproduced split-ratio panic, which can abort release builds.
2. Stop losing lifecycle messages and effect notifications under queue pressure.
3. Repair workspace visibility transitions and distinguish desired geometry from successfully applied geometry.
4. Balance native-object ownership and release locks/update suppression before frame waits.
5. Separate interruptible native execution from the actor and subscriber event loops.

These are independent findings from current code, not conclusions copied from previous audits. No application code was changed for this audit.

### Evidence and severity

- **Reproduced:** a source-derived executable probe demonstrated the failure. This is not a full-app reproduction.
- **Source-confirmed:** the implementation and call sites establish the faulty condition; live macOS behavior was not exercised.
- **Latent:** defective code exists, but the relevant production path is currently unwired.
- **Performance proposal:** unnecessary work is visible in source, but its contribution to real latency and any resulting speedup remain unmeasured.
- **P1:** release crash, loss of required work, or a core workspace operation failing its visibility contract.
- **P2:** conditional incorrect behavior, memory retention, lifecycle risk, or avoidable latency.

The investigation covered the principal actor/state, event ingestion, layout/minimum-size, effect/cache/animation, border, workspace, visibility, tab, startup/shutdown, and native helper paths. It was not exhaustive execution coverage or a line-by-line proof of every function.

### Skills and documentation

The requested find-skills workflow identified actionbook/rust-skills@m10-performance. Its discovery results showed approximately 3,000 installs, and the repository had 1,460 stars when checked. Its full instructions were read and applied without installing a skill or changing the environment. The practical influence was to prioritize redundant native operations, algorithmic work, ownership, and allocation evidence, and to require measurements before claiming speedups. The skill's generic numerical improvement ranges are not estimates for Stache. [Rust performance skill](https://github.com/actionbook/rust-skills/blob/main/skills/m10-performance/SKILL.md)

Tokio documentation was retrieved through Context7 for the worker-boundary proposal. Apple documentation was also checked to reject an apparent observer cleanup issue; see the exclusions below.

## Findings

### A6-01 — P1: short split-ratio vectors can abort the application

**Confidence:** reproduced helper failure; production reachability established by source.

**Evidence:** [minimum_size.rs:158](../../app/native/src/modules/tiling/actor/minimum_size.rs#L158), [split.rs:52](../../app/native/src/modules/tiling/layout/split.rs#L52), [release profile](../../Cargo.toml#L31).

The ordinary split layout falls back to equal spacing when ratio count does not match window count. Minimum-size enforcement instead passes a nonempty ratio vector to compute_adjusted_ratios, which indexes cumulative_ratios[i - 1] without verifying its length. Membership changes do not consistently rebuild those ratios.

**Trigger and impact:** customize a two-window split, then introduce a third layoutable window whose minimum size requires enforcement. With cumulative ratios [0.5] and three windows, the helper reads past the vector. A probe compiled from the actual helper reproduced this panic. The release profile uses panic = "abort", so actor catch_unwind cannot recover in that profile.

**Correction:** normalize ratios against layoutable membership before both layout and enforcement. Use the same equal-spacing fallback for invalid lengths; also handle non-finite or invalid ratios at their admission boundary.

**Regression:** add, restore, move, float/unfloat, and remove windows after custom resizing; exercise nonempty short ratios and minimum-size violations together. Assert finite frames and no panic.

### A6-02 — P1: channel saturation discards required state and effects

**Confidence:** source-confirmed, conditional on queue saturation.

**Evidence:** [actor/handle.rs:76](../../app/native/src/modules/tiling/actor/handle.rs#L76), [processor.rs:249](../../app/native/src/modules/tiling/events/processor.rs#L249), [processor.rs:420](../../app/native/src/modules/tiling/events/processor.rs#L420), [subscriber.rs:250](../../app/native/src/modules/tiling/effects/subscriber.rs#L250).

The actor's 1,024-slot channel reports both Full and Closed as SendFailed. Many event producers ignore that result. Geometry batches are drained before sending; destruction processing removes local routing/tracking before knowing whether the actor accepted the destruction. The subscriber's 256-slot queue drops layout, focus, visibility, destruction, and even shutdown notifications after logging.

**Trigger and impact:** bursts during expensive native work or animation can leave stale managed windows, unapplied layouts, or missing cleanup after the queue recovers. Logging a dropped message does not restore convergence.

**Correction:** classify messages by semantics. Preserve reliable lifecycle/commands; retain the newest geometry per exact target; keep dirty-workspace intent until consumed; merge user-triggered intent with logical OR. Distinguish Full from Closed and make overflow schedule authoritative reconciliation. Shutdown needs an independent cancellation/wakeup path. Do not solve this by making every queue unbounded.

**Regression:** saturate tiny test channels while emitting create/destroy, final geometry, focus, and shutdown. Verify eventual state convergence, latest geometry, and termination without requiring another unrelated event.

### A6-03 — P2: synchronous animation prevents timely replacement and shutdown

**Confidence:** source-confirmed; animated paths require animations to be enabled.

**Evidence:** [subscriber.rs:398](../../app/native/src/modules/tiling/effects/subscriber.rs#L398), [subscriber.rs:425](../../app/native/src/modules/tiling/effects/subscriber.rs#L425), [executor.rs:172](../../app/native/src/modules/tiling/effects/executor.rs#L172), [animation/mod.rs:268](../../app/native/src/modules/tiling/effects/animation/mod.rs#L268), [preset.rs:94](../../app/native/src/modules/tiling/actor/handlers/preset.rs#L94), [init.rs:754](../../app/native/src/modules/tiling/init.rs#L754).

The async subscriber executes animation synchronously before receiving its next notification. Its cancel_animation/begin_animation pair runs only after the replacement notification is dequeued, so it cannot interrupt an earlier animation running on that same loop. Presets invoke animation from the actor itself, blocking state-message processing. Shutdown calls cancel_animation and immediately reset_transient_state, clearing the cancellation condition before the subscriber has stopped.

**Impact:** stale animation targets consume native calls and delay newer intent; actor/subscriber queues grow; pause can wait for obsolete native work. Window animation is disabled by default, so this is not a claim that every installation continuously suffers animation stalls.

**Correction:** use a long-lived native worker with revision-based replacement and cancellation observable independently of queue consumption. Preserve cancellation through worker acknowledgement; never reset it while an old generation can still execute. Retarget from the last successfully applied/observed frame instead of snapping obsolete destinations.

**Regression:** replace a layout mid-animation, rapidly apply floating presets, and pause/resume during animation. Assert no old-generation writes after stop acknowledgement and continued actor responsiveness.

### A6-04 — P2: native cache ownership leaks retained objects

**Confidence:** source-confirmed retain/release imbalance; memory growth not profiled.

**Evidence:** [window_cache.rs:117](../../app/native/src/modules/tiling/effects/window_cache.rs#L117), [window_cache.rs:238](../../app/native/src/modules/tiling/effects/window_cache.rs#L238), [window_cache.rs:262](../../app/native/src/modules/tiling/effects/window_cache.rs#L262), [window_cache.rs:334](../../app/native/src/modules/tiling/effects/window_cache.rs#L334), [window_cache.rs:391](../../app/native/src/modules/tiling/effects/window_cache.rs#L391).

Four ownership paths need correction:

- Cold resolution receives a retained window element, retains again for the cache, and retains again for the caller. Caller release and cache eviction leave the original retain outstanding.
- Application creation returns an owned element, and the cache wrapper retains it again without relinquishing creation ownership.
- Batch enumeration returns retained elements for all windows, but releases enumeration ownership only for requested matches.
- A successful resolve during find_invalid_windows is reduced to is_none(), discarding the caller-owned reference.

**Trigger and impact:** cache churn, partial-app batches, and uncached validation accumulate native objects. This is a long-session memory/resource issue, not a measured resident-memory estimate.

**Correction:** encode owned versus borrowed references in RAII wrappers and audit all enumeration exits. Keep application elements owned or guarded for the full use interval: simply releasing today's leaked creation reference would expose the raw pointer returned after the map guard is dropped to concurrent invalidation.

**Regression:** instrument retain/release accounting for hit, miss, eviction, invalidation, unmatched enumeration, and failed lookup. Then run repeated create/destroy and animation cycles under macOS allocation tooling.

### A6-05 — P2: border animation holds the send lock through a 125 ms wait

**Confidence:** source-confirmed lock lifetime; 125 ms is the configured frame interval, not an end-to-end latency measurement.

**Evidence:** [borders.rs:181](../../app/native/src/modules/tiling/borders.rs#L181), [borders.rs:193](../../app/native/src/modules/tiling/borders.rs#L193), [borders.rs:486](../../app/native/src/modules/tiling/borders.rs#L486), [borders.rs:713](../../app/native/src/modules/tiling/borders.rs#L713).

The animated-border loop holds the send mutex while waiting for the next frame/command. Focus preparation acquires this mutex before checking whether the command is redundant. A focus update can therefore wait through a frame interval just to deduplicate. CLI fallback uses Command::output without a timeout and can hold the same serialization path indefinitely if the child hangs.

**Correction:** scope locking to validation/send/publication, dropping it before channel waits. Let the border worker own sending and consume latest replacement commands. Bound CLI fallback execution and reap a timed-out child.

**Regression:** inject a blocked frame wait and verify focus preparation is not blocked by it; test stale epochs, identical commands, fallback failure, and a child that never exits.

### A6-06 — P2: screen-update suppression remains active during animation waits

**Confidence:** source-confirmed scope; visual consequences require live validation.

**Evidence:** [animation/mod.rs:322](../../app/native/src/modules/tiling/effects/animation/mod.rs#L322), [animation/mod.rs:410](../../app/native/src/modules/tiling/effects/animation/mod.rs#L410), [skylight.rs:255](../../app/native/src/modules/tiling/ffi/skylight.rs#L255).

Both animation loops create UpdateGuard before writing frames and retain it through wait_for_next_frame. When disabling updates succeeds, re-enabling occurs only when the loop body's guard is dropped after the wait. Thus suppression covers the idle interval as well as the batch of writes. The spring finalization path also nests another update guard.

**Correction:** use one explicit per-frame scope containing only the update guard, transaction, and writes. Release suppression before sleeping, waiting, cleanup, or cancellation acknowledgement.

**Regression:** inject update-control and clock hooks and assert re-enable precedes every wait and is balanced on every return. Measure presentation smoothness on supported macOS versions; do not infer a precise compositor penalty from these private APIs.

### A6-07 — P1: cycling workspaces skips visibility reconciliation

**Confidence:** source-confirmed.

**Evidence:** [workspace.rs:21](../../app/native/src/modules/tiling/actor/handlers/workspace.rs#L21), [workspace.rs:168](../../app/native/src/modules/tiling/actor/handlers/workspace.rs#L168), [actor/mod.rs:656](../../app/native/src/modules/tiling/actor/mod.rs#L656).

Named switching returns a VisibilityDelta, which the actor applies. Cycling independently changes visibility/focus flags and emits notifications without performing that reconciliation. Subscriber visibility handling updates layout/border state; it does not substitute for hiding and unhiding applications.

**Trigger and impact:** cycle between two workspaces containing distinct applications, with the destination previously hidden. Previous-workspace applications can remain shown and destination visibility can disagree with state.

**Correction:** resolve the next workspace, then execute the same transition as named switching, including visibility, focus history, focus, and notifications.

**Regression:** cycle in both directions between populated workspaces and compare state plus emitted native visibility actions against an equivalent named switch. A single-workspace/no-focus no-op test is insufficient.

### A6-08 — P2: moving a window does not reconcile application visibility

**Confidence:** source-confirmed.

**Evidence:** [window_move.rs:19](../../app/native/src/modules/tiling/actor/handlers/window_move.rs#L19), [window.rs:430](../../app/native/src/modules/tiling/actor/handlers/window.rs#L430).

Moving a window changes workspace membership and requests layouts for both workspaces, but does not recalculate whether its application should be hidden. Moving an application's last visible window into a hidden workspace can leave it visible; moving a hidden application's window into a visible workspace can leave it hidden.

**Correction:** validate the destination before mutation and derive identity-scoped visibility actions from the completed membership transition.

**Regression:** move windows both ways, with and without another visible window belonging to the same application. Preserve the current application-level hiding model; a visible sibling must keep its application unhidden.

### A6-09 — P2: multi-display transitions violate visibility/focus invariants

**Confidence:** source-confirmed.

**Evidence:** [workspace.rs:58](../../app/native/src/modules/tiling/actor/handlers/workspace.rs#L58), [screen.rs:128](../../app/native/src/modules/tiling/actor/handlers/screen.rs#L128), [screen.rs:258](../../app/native/src/modules/tiling/actor/handlers/screen.rs#L258), [screen.rs:364](../../app/native/src/modules/tiling/actor/handlers/screen.rs#L364).

Named switching clears focused flags only on the target screen, so switching focus across screens can leave two workspaces marked focused. Screen removal reassigns workspaces without resolving competing visible flags. Adding a new screen creates an invisible fallback workspace but skips initial visibility setup when a focused workspace already exists.

**Impact:** state can contain multiple visible workspaces on one display, no visible workspace on a new display, or multiple focused workspaces. Downstream selection and layout then depend on inconsistent flags.

**Correction:** reconcile after every topology/workspace transition: one visible workspace per active screen, one globally focused workspace when screens exist, and focus IDs matching flags. Preserve the existing destination workspace where possible; show a fallback only when needed. Apply visibility changes after this choice.

**Regression:** cross-display named switches, unplug with both displays populated, reconnect configured workspaces, add an unconfigured display, and remove the focused display.

### A6-10 — P2: coalescing can forget that a resize occurred

**Confidence:** source-confirmed.

**Evidence:** [processor.rs:560](../../app/native/src/modules/tiling/events/processor.rs#L560), [processor.rs:601](../../app/native/src/modules/tiling/events/processor.rs#L601), [window.rs:695](../../app/native/src/modules/tiling/actor/handlers/window.rs#L695).

Move after Resize produces MoveResize, but a subsequent Move converts MoveResize back to Move. The resize path has the symmetric downgrade. Batch handling uses the accumulated type to distinguish resize from move behavior.

**Correction:** make MoveResize absorbing, or accumulate independent move/resize bits while retaining the newest frame.

**Regression:** cover Resize→Move→Move, Move→Resize→Resize, repeated alternating events, and same-type-only sequences. Verify final frame and classification, not merely event count.

### A6-11 — P2: minimum-size enforcement uses inconsistent axes and stale iteration data

**Confidence:** source-confirmed; dwindle orientation helper also checked by source-derived probe.

**Evidence:** [minimum_size.rs:50](../../app/native/src/modules/tiling/actor/minimum_size.rs#L50), [minimum_size.rs:327](../../app/native/src/modules/tiling/actor/minimum_size.rs#L327), [minimum_size.rs:427](../../app/native/src/modules/tiling/actor/minimum_size.rs#L427), [minimum_size.rs:488](../../app/native/src/modules/tiling/actor/minimum_size.rs#L488), [dwindle.rs:125](../../app/native/src/modules/tiling/layout/dwindle.rs#L125).

- The matches! guard applies to both SplitHorizontal and automatic Split, so explicit horizontal layout on a portrait display enforces the vertical dimension.
- The first-window dwindle adjustment asks whether split index zero is horizontal; that helper returns false for landscape, contrary to the layout's first split.
- Dwindle/grid iterations keep deriving adjustments from the original frames and violations. Newly computed violations only control termination, not the next adjustment.
- Grid adjustment logic covers only specific special window counts. Other layouts can repeat calculations without a supported adjustment.

**Correction:** share split orientation/topology logic with layout generation, use usable dimensions, and iterate from the latest frames/violations. Stop on no progress and explicitly distinguish infeasible minimums from solver failure. Avoid redundant final recomputation.

**Regression:** portrait explicit-horizontal layouts, first-window width deficits in landscape dwindle, secondary violations introduced by an adjustment, regular/special grid counts, and infeasible total minimums.

### A6-12 — P2: per-display batching is not connected to production assignments

**Confidence:** source-confirmed call-site absence and timer behavior.

**Evidence:** [processor.rs:150](../../app/native/src/modules/tiling/events/processor.rs#L150), [processor.rs:222](../../app/native/src/modules/tiling/events/processor.rs#L222), [processor.rs:249](../../app/native/src/modules/tiling/events/processor.rs#L249), [screen_monitor.rs:208](../../app/native/src/modules/tiling/events/screen_monitor.rs#L208), [screen_monitor.rs:245](../../app/native/src/modules/tiling/events/screen_monitor.rs#L245).

set_window_screen has test callers but no production callers, so real geometry falls back to the default screen batch. The monitor registers current displays but does not unregister removed ones. Updating a registered display's refresh rate does not replace the interval captured by its running timer. The reconfiguration callback filters out changes without add/remove flags.

**Impact:** the advertised per-display batching cannot correctly follow mixed-refresh windows; obsolete timers remain, and standalone mode/rotation changes can leave layout or timing stale.

**Correction:** publish authoritative window assignments on admission, moves, and topology changes. Reconcile removed screens and rearm timing when modes change; debounce reconfiguration into one current topology snapshot. Fix A6-13 before wiring removal.

**Regression:** verify production integration with two displays, moving a window between them, refresh-rate changes without unplugging, rotation, removal, and reconnect. Confirm no timer remains for a removed display.

### A6-13 — P2, latent: unregistering an empty screen batch deadlocks

**Confidence:** source-confirmed; unregister_screen currently has no production caller.

**Evidence:** [processor.rs:193](../../app/native/src/modules/tiling/events/processor.rs#L193).

unregister_screen drops the first screen_batches guard only when a removed batch contains pending updates. With an empty batch or unknown screen, it reaches a second lock acquisition while the first guard is still alive. Shadowing the variable does not release the first guard before acquiring the second.

**Correction:** remove/update the batch and default-screen selection inside one lock scope, then deliver any flushed updates outside it.

**Regression:** bounded-time tests for empty, absent, and nonempty screen IDs. The existing nonempty flush test exercises the path that explicitly releases the lock and does not cover the deadlock.

### A6-14 — P2: fullscreen state is not refreshed by production events

**Confidence:** source-confirmed call-site absence.

**Evidence:** [ax_observer.rs:242](../../app/native/src/modules/tiling/events/ax_observer.rs#L242), [ax_observer.rs:331](../../app/native/src/modules/tiling/events/ax_observer.rs#L331), [processor.rs:521](../../app/native/src/modules/tiling/events/processor.rs#L521).

Fullscreen is read during window creation, but ordinary move/resize callbacks fetch geometry without refreshing it. The processor exposes a fullscreen-change method that no production producer calls. A tracked window can therefore retain its admission-time fullscreen flag after entering or leaving native fullscreen.

**Correction:** reconcile fullscreen from relevant supported AX/native transitions and the authoritative snapshot path. Do not invent or assume an AX notification exists merely because a state handler does.

**Regression:** enter/exit fullscreen on an already tracked window, including windows admitted while fullscreen; verify layout eligibility and restoration without requiring recreation.

### A6-15 — P2: configured ignore rules are not applied to window admission

**Confidence:** source-confirmed.

**Evidence:** [tiling configuration](../../app/native/src/config/types/tiling.rs#L212), [borders.rs:564](../../app/native/src/modules/tiling/borders.rs#L564), [init.rs:900](../../app/native/src/modules/tiling/init.rs#L900), [ax_observer.rs:183](../../app/native/src/modules/tiling/events/ax_observer.rs#L183), [window.rs:52](../../app/native/src/modules/tiling/actor/handlers/window.rs#L52).

The configuration describes ignored applications/windows as never managed by tiling. Reads of tiling.ignore are limited to configuration preparation and border exclusion; startup uses built-in eligibility rules, while event admission applies structural checks without the configured ignore rules.

**Impact:** a configured ignored window may still be tracked, tiled, moved, or included in visibility management even if its border is excluded.

**Correction:** centralize admission policy and apply existing rule matching semantics in both initial enumeration and later discovery. Apply application-only exclusions before unnecessary native window extraction where possible.

**Regression:** ignored application, title-specific rule, combined application/title rule, and nonmatching sibling windows, through both startup and live creation.

### A6-16 — P2: reused window IDs do not clean up the old identity

**Confidence:** source-confirmed.

**Evidence:** [window.rs:60](../../app/native/src/modules/tiling/actor/handlers/window.rs#L60), [window.rs:199](../../app/native/src/modules/tiling/actor/handlers/window.rs#L199).

When an ID belongs to an existing identity different from the incoming identity, creation invokes on_window_destroyed with the incoming identity. Destruction correctly rejects that call because it does not match the stored identity. Creation then continues, leaving old membership/cache references while replacing the window entry.

**Correction:** destroy the existing exact target, clean its membership/focus/cache/tab references, then admit the new target. Later delayed events for the old identity must remain harmless.

**Regression:** reuse one window ID across different application identities and workspaces, then deliver delayed old-identity destruction/geometry. Assert exactly one membership and no surviving old cache entry.

### A6-17 — P2: focused indices drift during membership changes and swaps

**Confidence:** source-confirmed.

**Evidence:** [window_move.rs:31](../../app/native/src/modules/tiling/actor/handlers/window_move.rs#L31), [window_move.rs:97](../../app/native/src/modules/tiling/actor/handlers/window_move.rs#L97), [window.rs:236](../../app/native/src/modules/tiling/actor/handlers/window.rs#L236), [preset.rs:52](../../app/native/src/modules/tiling/actor/handlers/preset.rs#L52).

The move handler removes a window before searching for its position, so the index-adjustment branch cannot run. Destruction only clamps an out-of-bounds focused index, missing removal before a still-in-bounds index. Direct swaps change order without preserving focus by identity. Floating presets consume this index rather than the authoritative focused window ID.

**Impact:** a preset can target a different window, or focus-dependent commands can silently do nothing.

**Correction:** preserve focused identity across membership/reordering and derive any needed index afterward. Prefer one authoritative focused target; validate destination existence before membership changes.

**Regression:** remove/move before, at, and after the focused position; swap focused/nonfocused windows; apply a floating preset afterward and assert its exact target.

### A6-18 — P2: application activation happens before exact target validation

**Confidence:** source-confirmed ordering; impact requires stale identity/PID reuse.

**Evidence:** [window_ops.rs:657](../../app/native/src/modules/tiling/effects/window_ops.rs#L657), [window_ops.rs:668](../../app/native/src/modules/tiling/effects/window_ops.rs#L668).

focus_window dispatches asynchronously to the main thread. Its implementation activates target.identity.pid before resolving and validating the target. If the original process exits and its PID is reused, an unrelated application can be activated even though exact window resolution subsequently rejects the stale request.

**Correction:** validate the current application object against AppIdentity before any activation side effect, and use that validated object for activation rather than performing a fresh PID-only lookup.

**Regression:** inject a stale target/PID replacement between scheduling and execution. Assert no activation, AX focus, or raise call for the replacement application.

### A6-19 — P2: desired layouts are cached as applied before execution succeeds

**Confidence:** source-confirmed; failure requires a rejected/skipped native operation.

**Evidence:** [subscriber.rs:85](../../app/native/src/modules/tiling/effects/subscriber.rs#L85), [executor.rs:203](../../app/native/src/modules/tiling/effects/executor.rs#L203), [animation/mod.rs:324](../../app/native/src/modules/tiling/effects/animation/mod.rs#L324), [actor/mod.rs:392](../../app/native/src/modules/tiling/actor/mod.rs#L392).

Subscriber layout tracking advances before effects execute. Animated targets whose starting frame cannot be resolved are silently excluded by filter_map. Animation discards frame-write failures and returns the number of animatable windows rather than successful final writes. Expected-frame handling also writes desired frames into model geometry.

**Impact:** a transient native failure can leave actual windows behind the cache. A later identical non-user-triggered layout is suppressed as unchanged; a user-triggered layout can retry, but no automatic convergence is guaranteed.

**Correction:** separate desired, observed, and successfully applied frames. Return per-target execution outcomes and only advance applied state on success. Retry transient failures with bounds and reconcile actual geometry; remove dead targets through identity-checked lifecycle handling.

**Regression:** fail initial-frame lookup, an intermediate write, and a final write, then request the identical layout. Assert failed targets remain dirty and recover without claiming false success.

### A6-20 — P2: initial layout delivery depends on startup scheduling

**Confidence:** source-confirmed race; not reproduced in the live app.

**Evidence:** [init.rs:524](../../app/native/src/modules/tiling/init.rs#L524), [init.rs:616](../../app/native/src/modules/tiling/init.rs#L616), [actor/mod.rs:774](../../app/native/src/modules/tiling/actor/mod.rs#L774), [subscriber.rs:704](../../app/native/src/modules/tiling/effects/subscriber.rs#L704).

build_runtime starts actor/subscriber work and queues initial enumeration/InitComplete before start_runtime publishes RuntimeSlot::Running. If InitComplete runs first, get_subscriber_handle returns None and initial layout intent is lost. Independently, subscriber initialization can cache computed target layouts without executing them; a subsequent identical non-user initial notification is then considered unchanged.

**Correction:** inject the effect sink instead of looking it up through unpublished global state. Add a bootstrap readiness/acknowledgement barrier and keep initial work dirty until actually applied. Do not populate the applied-layout cache from desired startup state.

**Regression:** deterministically schedule actor-before-publication, subscriber initialization after populated state, and delayed effect startup. Every visible workspace must receive an initial apply exactly as needed, irrespective of interleaving.

## Performance proposals

These proposals identify avoidable work, not measured bottleneck percentages. Use the measurement plan below to prioritize changes within each stage.

### P6-01 — Coalesce before native geometry reads

[AX move/resize callbacks](../../app/native/src/modules/tiling/events/ax_observer.rs#L331) fetch window ID, position, and size before the processor merges events. Therefore batching reduces actor messages but does not reduce those native reads. Programmatic animation echoes can incur this work before later suppression.

Keep callbacks lightweight; retain only the native handle/identity information needed safely, mark the exact target dirty, and obtain one current geometry snapshot per drain. Preserve an explicit final-drag snapshot/barrier and per-target echo reconciliation. Measure AX reads per input burst and callback time before/after; do not discard all geometry globally during animation.

### P6-02 — Scan application tabs once per discovery burst

[Window admission](../../app/native/src/modules/tiling/actor/handlers/window.rs#L98) scans tabs, and its subsequent is_new_window_a_tab path can scan the same application again. Startup also scans applications before batch admission repeats this work. These are native tree traversals inside latency-sensitive paths.

Publish an identity-scoped tab snapshot/revision from one discovery pass and reuse it for all windows in that burst. Keep native identity validation outside the tab registry write lock, then publish under a short lock. Invalidate on relevant events; do not assume tabs are immutable. Benchmark native calls for a many-window, tab-heavy application.

### P6-03 — Actually group native cache misses by application

[batch_resolve](../../app/native/src/modules/tiling/effects/window_cache.rs#L317) builds an identities vector without deduplication. When other unresolved targets remain, a repeated identity can enumerate its application's windows again. Immediate frame updates resolve targets individually, and some drag-completion snapshot paths use uncached per-window resolution.

Group misses into one set per AppIdentity, enumerate each application once, and share that result across requested targets. Combine this with the ownership fix in A6-04. Measure cold-cache enumeration counts with multiple applications and missing targets; the missing-target case prevents a misleading all-resolved fast-path benchmark.

### P6-04 — Build compact layout inputs in workspace order

[compute_layout](../../app/native/src/modules/tiling/actor/mod.rs#L541) obtains cloned Window values through a global filter, then checks workspace membership with nested searches. [compute_layout_targets](../../app/native/src/modules/tiling/actor/mod.rs#L526) performs further lookups. Minimum-size enforcement adds repeated ID searches, and converting small-vector results into Vec gives up inline storage.

Use workspace window IDs plus existing indices to collect borrowed or compact layout inputs in stack order, carrying target identity and effective minimum size together. Avoid cloning titles/application strings for layout. Keep small-vector storage through local stages where practical. The current membership construction includes global O(N) work and workspace O(n²) comparisons; measure allocations and time at realistic small n as well as larger workspaces.

### P6-05 — Simplify actor-owned mutations and batch removal

[State updates](../../app/native/src/modules/tiling/state/tiling_state.rs#L231) replace values via remove/insert. The observable vectors are backed by imbl persistent vectors, not ordinary Vec; no tiling subscriptions were found. Do not claim every edit moves an entire contiguous vector or causes downstream subscriber fan-out.

First compare direct replacement with remove/insert. Then benchmark ordinary actor-owned storage against the persistent container only if it remains material. [Application termination](../../app/native/src/modules/tiling/actor/mod.rs#L921) repeatedly removes windows and repairs indices; [remove_window](../../app/native/src/modules/tiling/state/tiling_state.rs#L322) scans the index map, making a k-window termination potentially O(kN). Remove an application's windows in one pass, repair indices once, and notify each affected workspace once. Consider identity membership/visible counts if profiling confirms repeated visibility scans are significant.

### P6-06 — Eliminate redundant animation setup and writes

[Animation setup](../../app/native/src/modules/tiling/effects/animation/mod.rs#L286) searches transitions for each resolved target. Carry original indices through resolution instead. Each frame writes geometry for every animatable window; spring mode continues writing settled windows and writes exact final frames again at completion.

Track last successfully applied position/size, skip unchanged attributes and settled windows, and preserve an exact final application/verification. Benchmark native write count, frame duration, and final correctness separately. Do not trade fewer writes for leaving fractional or failed final positions uncorrected.

### P6-07 — Make timers and display links demand-driven

[Per-screen timers](../../app/native/src/modules/tiling/events/processor.rs#L249) run while the processor is active even without pending geometry. [Animation sync](../../app/native/src/modules/tiling/effects/animation/sync.rs#L102) caches the main display refresh rate indefinitely; its static display link remains alive after first initialization. Fallback timing includes a spin-wait tail. Animation also raises the executing thread's QoS without restoration, problematic when that thread is a shared runtime worker.

Arm geometry timers only for dirty work, start display synchronization only while needed, and rebuild timing on relevant display changes. Use the target display's timing, not one permanent main-display value. Confine QoS to an owned worker or restore it. Measure idle wakeups and CPU before considering precision spinning; handle missing vsync without repeatedly paying both timeout and a full fallback interval.

### P6-08 — Compare release optimization settings, do not assume them

The [release profile](../../Cargo.toml#L31) uses opt-level = "s", LTO, and one codegen unit. Compare that baseline against speed-oriented optimization using identical release workloads and hardware. Report binary size, startup, CPU, allocation, and tail latency. This is lower priority than avoidable AX work and blocking; neither higher optimization nor SIMD is a demonstrated solution to native IPC latency.

## Proposed architecture

### Preserve ownership; change where native work runs

Keep the single-owner actor for deterministic state, workspace transitions, and pure layout calculation. Do not parallelize small layout calculations or distribute mutable workspace state without evidence that calculation is the bottleneck.

The target flow is:

    Native callbacks
        -> identity-checked lifecycle events + coalesced dirty targets
        -> state actor and pure workspace/layout transitions
        -> revisioned desired native effects
        -> dedicated native execution worker
        -> per-target outcomes and observed-state reconciliation
        -> actor

The border worker remains separate so border animation or CLI fallback cannot delay window execution.

Blocking native calls and frame waits should not occupy async event-processing tasks. Tokio recommends dedicated threads for long-lived blocking work rather than treating it as ordinary async execution; bounded spawn_blocking is appropriate for finite blocking jobs, not an unbounded task per AX event. [Tokio spawn_blocking documentation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)

### Internal interfaces and delivery semantics

- Keep existing exact WindowTarget/AppIdentity ownership and attach runtime generation plus layout revision to native intent/results.
- Deliver lifecycle/commands reliably, with observable overflow reconciliation when callbacks cannot wait. Coalesce geometry by exact target and layout intent by workspace. Latest focus intent supersedes older focus intent; user-triggered layout intent must not be lost during merging.
- Use a dedicated, serialized native worker initially. It checks replacement/cancellation between frames and operations, with bounded native-call timeouts where supported. Main-thread-only work remains a narrow dispatch using validated application objects.
- Worker isolation keeps the actor responsive, but cannot interrupt an already blocked native call. If measurements show cross-application head-of-line blocking after timeouts/batching, evaluate a bounded per-application execution pool while retaining one writer per target. Do not introduce unlimited concurrency.
- Separate desired geometry, observed geometry, and last successfully applied geometry. Acknowledgements describe per-target success/failure, not merely scheduling. Use bounded retries, backoff, and authoritative reconciliation.
- Publish compact actor snapshots/effect plans directly instead of having the subscriber repeatedly query the actor for facts the actor just changed.
- Use one transition implementation for named/cycle workspace switching, moves, and topology reconciliation. Its output contains visibility, focus, and dirty-layout intent after state invariants hold.
- Make startup readiness and shutdown completion explicit. Do not depend on global handle publication timing; do not reset cancellation until workers acknowledge the old generation has stopped.

No public IPC, configuration, or schema changes are necessary for these internal changes. Preserve the current application-level hiding limitation: windows of the same application on different workspaces cannot be independently hidden by an application-hide operation. This proposal does not authorize a new private-API hiding mechanism.

### Staged implementation order

1. **Correctness and narrow fixes:** A6-01, ownership accounting, border/update-guard scopes, geometry merging, identity cleanup, focused-index handling, and the latent unregistration deadlock. Add focused regressions.
2. **Convergence:** reliable/coalesced delivery, unified visibility transitions, fullscreen/ignore admission reconciliation, desired/applied separation, and startup acknowledgement. Add fault-injected actor/effect integration tests.
3. **Native execution boundary:** remove synchronous animation/discovery from actor/subscriber processing, introduce revisioned outcomes and durable cancellation, then correct per-display scheduling.
4. **Measured optimization:** batch discovery/cache misses, compact layout inputs, batch removal, unchanged-write suppression, and idle scheduling. Consider container/compiler changes only against the new profile.

Record baseline metrics before these stages and compare after each. Gate each stage on relevant correctness tests; retain a nonanimated execution path using the same delivery and outcome semantics. Do not bundle all fixes into an unreviewable architectural rewrite.

## Validation performed

The following existing tests passed against the audited working tree:

| Filter                               | Passed | Failed |
| ------------------------------------ | -----: | -----: |
| modules::tiling::layout              |    106 |      0 |
| modules::tiling::actor::minimum_size |     16 |      0 |
| modules::tiling::state               |     28 |      0 |
| modules::tiling::events::processor   |     12 |      0 |
| Total                                |    162 |      0 |

Commands used:

    cargo test --locked --offline -p stache --lib modules::tiling::layout -- --test-threads=1
    cargo test --locked --offline -p stache --lib modules::tiling::actor::minimum_size -- --test-threads=1
    cargo test --locked --offline -p stache --lib modules::tiling::state -- --test-threads=1
    cargo test --locked --offline -p stache --lib modules::tiling::events::processor -- --test-threads=1

A separate temporary executable compiled the actual compute_adjusted_ratios and is_dwindle_split_horizontal definitions extracted from minimum_size.rs. Its catch_unwind call to compute_adjusted_ratios(&[0.5], &[0.7, 0.0, 0.0], 3) confirmed a panic. It also confirmed is_dwindle_split_horizontal(0, true) returns false. This tested the source helpers, not a running application or a release-process crash.

Passing existing tests does not validate the untested cases above. No full suite, full-app AX reproduction, release profiling, memory-growth measurement, or live display benchmark is claimed. No native window positions or visibility were intentionally changed for validation.

## Measurement and acceptance plan

### Instrumentation

Measure timestamps and counts at callback, coalescer drain, actor dequeue, layout completion, first native application, and final verified application. Tag records by runtime generation, revision, exact target, workspace, and display. Keep detailed tracing sampled or feature-gated so measurement overhead does not dominate.

Capture:

- p50/p95/p99 event-to-actor and intent-to-first/final-application latency.
- Queue depth/age, coalescing ratio, Full/Closed outcomes, and reconciliation requests.
- AX queries/writes/enumerations and their duration, including failures/timeouts.
- Per-display frame duration, missed deadlines, superseded work, and cancellation latency.
- Rust allocation count/bytes, native object retention, idle wakeups, and process CPU.

Use macOS Time Profiler/Allocations and application counters for live native behavior; use release microbenchmarks for pure layout, coalescing, and state mutation. Debug-test timings are not production performance evidence.

### Workloads

- 1, 4, 8, 16, and 32 windows, distributed across one and multiple applications/workspaces.
- Tab-heavy applications; cold/warm cache; requested and missing native targets.
- One display and mixed-refresh displays, including 60/120/144 Hz where hardware permits.
- Rapid workspace/focus changes, continuous move/resize, floating presets, and animation enabled/disabled.
- An unresponsive application, transient AX failures, queue saturation, and delayed worker startup.
- Display add/remove/mode changes; pause/resume mid-animation; application restart and stale identity events.
- Extended idle and repeated create/destroy cycles to distinguish leaks from stable caches.

For counts beyond a layout's supported capacity, benchmark state/event handling separately from layout placement; do not silently interpret the current grid limit as support for arbitrary counts.

### Acceptance

Every correctness finding needs a focused regression demonstrating the old failure and new behavior. Require eventual convergence after transient failures, no lost required lifecycle state, no stale-generation effects after acknowledged shutdown, balanced ownership, and consistent workspace/focus membership.

For performance changes, compare identical release workloads with repeated runs and recorded hardware/display/configuration. Require a demonstrated improvement in the intended metric without a meaningful regression in tail latency, memory, idle CPU, or correctness. Establish numerical targets from the baseline; this audit does not invent an unsupported universal speedup or frame budget.

## Exclusions and cautions

- Missing explicit CFRunLoopRemoveSource during observer teardown is not itself a leak: Apple documents that releasing AXObserver automatically removes its run-loop source. [AXObserverGetRunLoopSource](https://developer.apple.com/documentation/applicationservices/1459139-axobservergetrunloopsource)
- get_config returns a reference from OnceLock; it is not an expensive configuration clone or contended lock on each call.
- The observable vector uses persistent imbl storage. Claims about Vec-wide shifting or active subscriber fan-out would mischaracterize this implementation.
- The grid's bounded window-count behavior is documented/tested; expanding capacity is a product change, not treated here as an accidental defect.
- Private SkyLight behavior and native thread-affinity constraints must be verified on supported macOS versions before changing their execution placement.
- Larger queues, more threads, blanket lock-free structures, unsafe shortcuts, and a faster release optimization level are not substitutes for correct ownership, delivery, and measurements.

## Consolidated findings from earlier audits

This document supersedes `tiling-audit-1.md` through `tiling-audit-5.md`. The detailed findings above are the current, deduplicated assessment. The following distinct items from the earlier reviews remain actionable or worth validating; duplicated claims are intentionally not repeated.

### C-01 — Medium: state collections can bypass index maintenance

**Evidence:** [tiling_state.rs](../../app/native/src/modules/tiling/state/tiling_state.rs).

Entity collections are publicly reachable while their side indexes are maintained by dedicated upsert/remove methods. A handler can mutate a collection directly and leave its index map stale; consistency checks are debug assertions rather than production recovery.

**Correction:** make backing collections private and expose invariant-preserving operations only. If actor state is later simplified, choose a representation that makes keyed lookup and ordered workspace membership explicit rather than duplicating mutable indexes.

### C-02 — Low: removed workspaces retain focus history

**Evidence:** [tiling_state.rs](../../app/native/src/modules/tiling/state/tiling_state.rs).

Workspace removal does not remove its `focus_history` key. This is bounded by workspace churn but leaves obsolete UUID entries indefinitely.

**Correction:** remove the history entry as part of the same workspace-removal transaction and assert no history exists for absent workspaces.

### C-03 — Medium: transaction drop can apply partial native work

**Evidence:** [transaction.rs](../../app/native/src/modules/tiling/ffi/transaction.rs).

An uncommitted FFI transaction commits in `Drop`. An error path can therefore apply a partial batch synchronously and on the dropping thread, contrary to an explicit-commit model.

**Correction:** make drop abort/release only; make commit the sole operation that can send a transaction. Test early returns and panics without treating drop as a native side effect.

### C-04 — Medium: mouse-up callback executes while its mutex is held

**Evidence:** [mouse_monitor.rs](../../app/native/src/modules/tiling/events/mouse_monitor.rs).

The mouse-up callback is invoked under the global callback mutex. It can deadlock if it changes callback registration and extends the CGEventTap callback with arbitrary work.

**Correction:** copy the callback reference under the lock, release it, then invoke. Keep the callback itself nonblocking and queue follow-up state work.

### C-05 — Low: native API contracts need defensive validation

**Evidence:** [accessibility.rs](../../app/native/src/modules/tiling/ffi/accessibility.rs), [skylight.rs](../../app/native/src/modules/tiling/ffi/skylight.rs), [window.rs](../../app/native/src/modules/tiling/window.rs).

The accessibility boolean path wraps returned values without first confirming the Core Foundation type. Some geometry inputs are accepted without finite/non-negative validation. A safe focused-window helper exposes a routine documented as main-thread-only without enforcing that precondition.

**Correction:** type-check and release mismatches, validate geometry at every FFI ingress, and either dispatch the focused-window operation to the main thread or express its precondition in the type/API. Add malformed-value test seams where FFI can be injected.

### C-06 — Low: display events and unrouted geometry need convergence semantics

**Evidence:** [screen_monitor.rs](../../app/native/src/modules/tiling/events/screen_monitor.rs), [processor.rs](../../app/native/src/modules/tiling/events/processor.rs).

The monitor's processing gate can discard a second display change while a delayed refresh is pending. Geometry whose window-to-screen mapping is absent falls back to the first hash-map key, which is nondeterministic. These reinforce A6-12's incomplete display lifecycle.

**Correction:** use a dirty/coalesced topology refresh that always reruns after an overlapping event; route unknown windows to a stable default screen or defer them until an assignment is known.

### C-07 — Low: layout identity strings have more than one source of truth

**Evidence:** [commands.rs](../../app/native/src/modules/tiling/commands.rs), [types.rs](../../app/native/src/modules/tiling/state/types.rs).

The command-facing formatter maps at least one layout differently from the canonical `LayoutType::as_str` representation. This can make IPC/frontend names disagree with configuration and serialization.

**Correction:** delegate all external string formatting to the canonical layout-name method and add an exhaustive enum-to-string consistency test.

### C-08 — Medium: blocking AX work still occurs before coalescing

**Evidence:** [ax_observer.rs](../../app/native/src/modules/tiling/events/ax_observer.rs), [window.rs](../../app/native/src/modules/tiling/actor/handlers/window.rs), [processor.rs](../../app/native/src/modules/tiling/events/processor.rs).

Application visibility checks, tab scans, destruction validation, and callback geometry collection can perform cross-process Accessibility or Objective-C work on actor or main-run-loop paths. A nonresponsive target application can therefore amplify the queue-loss and input-latency risks identified above.

**Correction:** include discovery and validity checks in the revisioned native-work boundary, preserve only lightweight identity/event information in callbacks, and use bounded reconciliation rather than blocking the actor or main callback.

### C-09 — Performance: unify and measure AX resolution paths

**Evidence:** [window_cache.rs](../../app/native/src/modules/tiling/effects/window_cache.rs), [window_ops.rs](../../app/native/src/modules/tiling/effects/window_ops.rs).

Cached/batched resolution and uncached window-operations resolution coexist. A cache hit still validates via an AX call, which is a correctness tradeoff rather than a free hit. Several immediate operations therefore enumerate or validate separately.

**Correction:** route normal native effects through one ownership-safe batch resolver with a narrowly scoped uncached fallback. Keep a short validity epoch only if profiling proves it safe and worthwhile; identity/lifecycle invalidation must remain authoritative.

### C-10 — Performance: reduce actor query round trips and lock contention

**Evidence:** [subscriber.rs](../../app/native/src/modules/tiling/effects/subscriber.rs), [processor.rs](../../app/native/src/modules/tiling/events/processor.rs), [init.rs](../../app/native/src/modules/tiling/init.rs).

Layout handling performs a query-back round trip and expected-frame update as separate operations. Geometry callbacks contend on one screen-batch mutex. Some IPC helpers construct a current-thread runtime per query. These costs are candidates for profiling after delivery correctness is fixed.

**Correction:** emit revisioned layout snapshots/effect plans directly from actor transitions, move expected-frame bookkeeping into that revision, use per-screen or short-lived dirty queues only when profiling shows mutex contention, and reuse the application's existing async runtime for IPC bridging.

### C-11 — Product limitation: grid layout capacity is explicit but should be visible

**Evidence:** [grid.rs](../../app/native/src/modules/tiling/layout/grid.rs).

The grid implementation has a bounded placement capacity. The current behavior is documented and tested, so it is not recorded as an accidental correctness defect. It can still surprise users when a workspace exceeds that capacity.

**Recommendation:** choose and document a product policy: warn/emit a frontend event, use a defined overflow layout, or reject the selection. Add coverage for the selected behavior before changing capacity.

### C-12 — Documentation is stale

**Evidence:** [tiling guide](../agents/tiling.md).

The agent-facing tiling guide names legacy manager/observer-oriented paths rather than the current actor/processor/subscriber architecture. This makes future changes more likely to target obsolete seams.

**Correction:** update the architecture and common-task sections after the delivery/native-worker design is settled; include the runtime generations, message classes, and visibility constraints described in this report.

## Rejected or downgraded historical claims

- Releasing an AXObserver without explicitly removing its run-loop source is not independently a leak: Apple documents that releasing the observer removes its source. Do not carry the old claim forward without contradictory platform evidence. [AXObserverGetRunLoopSource](https://developer.apple.com/documentation/applicationservices/1459139-axobservergetrunloopsource)
- `get_config()` returns a `OnceLock`-backed shared reference, so it is not an allocation or mutex hot path by itself.
- Persistent observable vectors are not ordinary `Vec`s, and no tiling subscriber use was found. Their replacement remains a benchmarked design decision, not a presumed win.
- The original "grid silently drops windows" concern is now classified as the documented capacity limitation in C-11.
