---
status: draft
---

# 03 — Panel lifecycle, menu bar and host bridge

Read when implementing native/backend foundations after 02 and separate approval. Production Bar factory stays unregistered until 04 provides the real surface.

## Scope

Add `modules/bar/src/{runtime,window,menubar}.rs`; generic additions in `sdk/src/windows.rs`; safe panel/display/menu adapters in `macos/src/` and `lib.rs`; app `src/{windows,host,bindings,shutdown}.rs` and tests. Define app-owned `BarView` projection and generated bridge contracts now; no forward call to a nonexistent UI route.

## Requirements

### Native evidence gate

- First compile-prove a public AppKit `NSPanel` nonactivating Tauri webview path on macOS 26. All Cocoa objects, tracking areas and observers remain main-thread-owned; safe Send-facing handles carry only opaque identities. No unchecked `Send`/`Sync`, raw pointer transport or legacy `object_setClass` assumption without proof. Only `stache-macos` contains authored unsafe code. Gate must verify that Tauri can own/close the real panel and embedded webview, not merely apply a style mask to an ordinary window. Before 04, any separately approved live panel probe uses an already implemented test surface, never an unknown Bar route or production Bar factory.
- Target legacy transparent/undecorated/no shadow/no effects/dark/accept-first-mouse/nonactivating behavior, all Spaces + stationary, primary only, no forced `fullScreenAuxiliary`. Clicking controls does not activate Stache, steal keyboard focus or disturb frontmost application except the approved target-launch action. Record layering and fullscreen observations; overview source evidence notes that legacy's level was overwritten by always-on-bottom. Do not assert menu-level equivalence from API names. Effective layering/visibility must match the approved legacy evidence; conflict blocks acceptance, not silent redesign.
- Public API gate precedes any private SLS proposal. If insufficient, record exact failed cases and ask separate explicit permission for the smallest `stache-macos` private adapter; no fallback is preauthorized. Read-only probes/compile success do not pass native feasibility. Each live gate requires fresh approval and cleanup evidence.

### Owned lifecycle

- Implement `WindowKind::NonActivatingPanel` semantics additively; `Standard`/test window and unsupported `Launcher` unchanged. Factory construction/configure are inert. SDK session gate currently spans backend calls: backend open/close/release remains enqueue-only, cannot synchronously wait/reenter while holding it. Stage logical identity/reservation then receive async native/page-loaded acknowledgement outside that gate. Distinguish requested, native-created, loaded, failed and cleanup-pending; do not report panel ready/loaded on queue acceptance. Bound acknowledgment waits off-main; page-load failures/timeout clear reservation and expose typed failure.
- Use exact owner/runtime generation/window identity for queued creates, focus, reposition and close. Every native commit rechecks cancellation/topology/config identity; late creation/load callback or old close cannot resurrect or close successor. Nonactivation means Bar reuse must not inherit Standard's `set_focus` path.
- Primary frame uses logical primary bounds/origin and `(padding, padding, width - 2*padding, height)` relative to that display; account for scale and negative origins, no double scaling. Native generic enumeration yields typed display identity for reservations; hotplug/primary/scale change coalesces signals, recomputes one panel and atomically moves reservation. No-primary state is explicitly unavailable with no guessed screen or stale reservation; recover on topology change. Explicit zero remains typed-valid and must not become a default-sized panel. Compile-prove exact zero/large geometry handling in the narrow backend, or report typed unsupported start/reconfigure with no failed-owner reservation; no wrap/clamp or new schema restriction. Every successfully running Bar owner reserves the approved constant sum, including explicit values.
- Reconfigure preflights outside short published locks; use the accepted-config watch for actual committed config/revision, not disk or guessed future revision. Reject keeps old frame/providers. Unchanged reload/tray echo does not duplicate resources. Disabled/paused start no Bar workers/webviews/timers/subprocesses; pause/disable/failure clears reservation immediately. Constant top `height + padding` while menu slides away, never follow the animation with inset updates.
- Stop first invalidates generation and wakes waits, then removes callbacks/closes exact panel and joins owned workers off-main. Bounded native removal acknowledgment returns success only on confirmation; timeout/failure retains inert main-thread owner with exact-ID cleanup retries and truthful cleanup-pending. Never drop Cocoa guards on a worker or forget ownership because a deadline passed. New generation cannot be harmed by retained cleanup. Partial start/install/page-load/spawn failure uses the same rollback. No locks held across native calls, waits or joins.
- Reuse generic factory begin-shutdown; if absent, introduce the minimal hook here: invalidate/wake factory/native/data waits before IPC/worker joins, registry reverse stop before factory finish/drain/drop. A hung background provider must not block main, host commands, bridge reads or shutdown ingress cancellation; native synchronous APIs cannot be promised forcibly interruptible, so quarantine bounded outstanding work with retained ownership and report uncertainty rather than waiting forever. Known Cmd-Q/logout host routing gap remains out of scope.

### Menu bar and data bridge

- Audit legacy `menubar.rs`/`watcher.rs` and `Bar.state.ts`: `menuHidden` means menu visible, causing Bar translateY(100%)/opacity 0. Seed initial state and subscribe before refresh, reconcile dropped/coalesced signals. Event-driven display/menu/activation observations where feasible, smallest bounded poll only while needed; document macOS heuristic limits, unknown/error behavior and deduplicated diagnostics. Preserve actual reveal/hide timing and animation in 04, no polling-derived visual redesign.
- SDK EventBus is pull-only/lossy. Host owns bounded pull workers/state subscriptions, recovery snapshots and concrete tauri-specta `collect_commands!`/`collect_events!` entries in `bindings::builder`. App `BarView` reads published cells only, not native/module locks; exact provider revision/generation/sequence and held file-first diagnostics survive projection. Commands/events have concrete generated DTOs; no handwritten TS names or generic JSON facade. Future providers fill typed optional fields, not forward-invoked functions.
- Least-privilege route-bound authorization validates actual window label/owner/surface, not a caller-supplied surface argument. Expose only current read feeds; no filesystem/shell/plugin HTTP/general launch/config writes. Part 04 adds the capability when the route exists; part 08 adds only authorized generic session actions.

## Acceptance criteria

- [ ] BAR-03-01: Public-panel feasibility is recorded with compile-proof and separately approved native observations, or remains explicitly blocked pending evidence/private permission.
- [ ] BAR-03-02: Fake-backed lifecycle/ack/load/cleanup state is truthful and bounded, generation-safe, main-owned and nonblocking across SDK session gate/shutdown order.
- [ ] BAR-03-03: Primary topology/frame and constant reservation follow accepted config/hotplug, with zero resources disabled and rollback on invalid/failed native work.
- [ ] BAR-03-04: Menu state scheduling and host generated bridge recover loss/races without locks/native reads; current test surfaces/modules remain intact and production Bar remains staged.

## Tests

ManualExecutor/fake panel/display/menu/clock and app integration: queue open then stop before drain; load timeout; native-create failure after queue acceptance; stalled main queue; canceled start; synchronous callback reentrancy; close failure/late retry versus successor; stale topology/ID reuse; primary unplug/scale/negative origin; zero/max dimensions and impossible frame; accepted/rejected reconfigure barrier; constant inset across all menu states. Fake shutdown barrier proves begin-shutdown precedes IPC join and no worker waits for main while main joins. Bridge bounded overflow/resnapshot, subscribe-before-read, stale initial response after newer event, teardown during subscription installation, unauthorized route/label, and provider hangs all remain prompt. No live panel tests by default.

## Notes

Update SDK/UI/overview docs only for landed backend contracts and limitations. Part 04 owns production surface activation and visual acceptance; this part is independently compilable/testable without ever opening an unknown surface.
