---
status: draft
---

# 02 — Native HUD adapter

Read when implementing the required Hold-to-Quit HUD, after explicit approval. This part installs no runtime, tap or application observer and shows nothing by default.

## Scope

Add `src/crates/macos/src/hold_hud.rs`, `hold_hud/{testing,tests}.rs`, exports in `src/lib.rs` and only proven native binding features in `src/crates/macos/Cargo.toml`. Independently implementable without parts 01/03 or the input service. Proposed type names below are contracts, not shipped symbols.

## Requirements

### Ownership, readiness and acknowledgements

1. `HudControl: Send + Sync + Clone` holds executor/backend references, opaque `HudHandle`/generation and plain synchronization state only. A main-thread-local owner map retains every `NSPanel`, view, layer and animation under `MainThreadMarker`. No Cocoa value crosses threads, no unsafe `Send/Sync` shim, static Cocoa singleton or forgotten owner. Document the ownership diagram and late-cleanup owner.
2. Installation/readiness creates and configures one hidden panel and verifies usable screen/layout resources on main before acknowledging `Ready`. Nil construction, missing screen, native failure, dispatch rejection, cancellation and timeout are distinct typed results, never readiness success. Acquisition is off-main with bounded acknowledgements. Repeated installation reuses the owned reservation; retained failed cleanup prevents a fresh duplicate panel.
3. `show(HudRequest)` and `hide(PressId)` enqueue without waiting. A request carries owner generation, press ID, app label, optional opaque display hint, duration/down time and a pure authorization token; no native pointer or arbitrary module callback. Part 04 supplies an application-revalidated authorized main job before ordering front. Part 02 fake-tests the authorization boundary without depending on part 03.
4. Every show emits a generation/press-tagged asynchronous result: `Shown` only after native ordering succeeded for that still-authorized press; otherwise typed `Stale`, `Unavailable` or failure. A missing acknowledgement times out within 2 s and invalidates the request. A late result cannot arm a timer or resurrect a cancelled press. Native accepted ordering is not proof of visible pixels in every Space; manual coverage remains explicit. An intentional no-op is never `Shown`.
5. Logical hide/release invalidates immediately before queued native work. Every install/show/hide/release job rechecks handle/generation/press at commit; an older hide cannot hide a successor press. Release orders out, removes animations and drops native objects on main before acknowledging `Released`. Timeout, removal error or cancelled wait reports `Retained` with exact ownership and the control retired. Queued cleanup may later finish exactly once; retry is idempotent. It never reports success or frees off-main on failed cleanup.
6. Use shipped applications constants `ACKNOWLEDGEMENT_BUDGET` (2 s) and `CANCELLATION_QUANTUM` (10 ms) for acquisition cancellation checks; these are per-hop budgets, not an invented global shutdown SLA. Cleanup must still run after lifecycle cancellation: do not abort physical release merely because that token is already cancelled. No acknowledgement wait on main; no main-thread wait for a worker awaiting main. Native callbacks/jobs contain panics; completion only enqueues pure results, never invokes module policy inline.

### Panel and display behavior

7. Native borderless, nonactivating `NSPanel`: click-through (`ignoresMouseEvents`), clear/nonopaque, not hidden on deactivate, status-window level, all-Spaces/full-screen auxiliary/stationary/ignore-cycle behavior where public APIs support it. Order front without making key/main or activating Stache. Install hidden, show only for down work, reuse for subsequent presses. Hide on release, cancellation, quit request and stop.
8. Recompute placement at each show: attached requested display → screen containing mouse → main screen → first screen. No screen is typed `NoScreen`, not successful show. Part 03 owns public frontmost-window/display geometry; absence is explicit fallback, not a claim that mouse/main equals the app's display. Report the selected display and placement source as plain diagnostic data. Never request Screen Recording or AX window metadata for placement.
9. Animate a linear progress fill with native Core Animation/display refresh, not a Rust 16 ms loop. No animation/timer while hidden. Zero or elapsed ≥ duration means full progress. Full `u64` range and unrepresentable native animation durations must remain safe: bounded fraction calculation, no overflow/panic and no long-duration animation interpreted as immediate completion. Such a duration may render a static safe fraction; the owned runtime deadline remains authoritative.

### Minimal native visual plan

Use a compact rounded system-material surface, one accessible static-text line “Hold ⌘Q to quit <App>” and a high-contrast progress track beneath. Missing name uses “the app”; long names truncate without moving the progress track off-screen. Center horizontally in the chosen visible frame near its lower edge, with margins and width constrained to that frame. Use system typography/colors and respect increased contrast/Reduce Motion (omit decorative fades, keep meaningful progress). No icon, actions, webview or focus. This is a concrete minimal engineering proposal; no pixel sizes or legacy theme tokens are user-approved requirements.

### Compile/ownership gate

Before part 04, compile-prove panel creation/configuration/ordering, layer animation, screen-number mapping and `HudControl: Send + Sync` against the installed bindings. Record exact dependency features, nil/error behavior, ownership and unsupported Space/display cases in rustdoc. Compile proofs/doctests never create a real panel. If public APIs or safe ownership cannot meet the contract, stop/report rather than add private APIs or unsafe trait shims. Runtime remains blocked; do not silently drop the required HUD.

## Acceptance criteria

- [ ] HQ-02-01: Compile proof and rustdoc establish safe main-only Cocoa ownership and opaque Send handles without unsafe trait impls.
- [ ] HQ-02-02: Hidden readiness is truthful for nil/error/dispatch/cancel/timeout. Async show acknowledges only matching authorized ordering; stale, failed or late shows are inert.
- [ ] HQ-02-03: Hide/release invalidate before dispatch; bounded acknowledged release keeps exact inert ownership on failure, supports eventual cleanup/retry and prevents fresh duplicate panels.
- [ ] HQ-02-04: Fake-tested layout/placement diagnostics, no-screen failure, fallback, accessible text, zero/full-u64 progress and idle animation behavior match the design without Rust polling.
- [ ] HQ-02-05: Exported fakes provide queued/never-draining executors, pre-order barriers and injectable install/show/hide/release failures. Default tests create no native panels.

## Tests

- Install nil/refusal, screen absence, dispatch failure and timeout, then late drain; no falsely ready handle or duplicate panel on Retry.
- Show → hide; hide/release before show commit; cancellation after native show before ack delivery; late success/failure for old press; old hide versus successor; interleaved owners; panic containment.
- Release timeout/error/cancellation with exact retained ownership, repeated release and eventual single cleanup. Already-cancelled lifecycle still attempts cleanup. A blocked main queue never blocks main on a worker.
- Pure layout for small visible frames, long/missing label, detached requested display, all fallbacks and no screen. Fraction math at 0, normal duration and `u64::MAX` without overflow.
- Manual only with approval **for each run**: an ignored smoke harness shows/releases on each attached display; observe focus, click-through, full-screen/Spaces, contrast and Reduce Motion. No app termination. Record skipped coverage as unverified, not inferred from compilation.

## Notes

After implementation: `cargo nextest run -p stache-macos`, `bun run lint`, `bun run format:check`, `bun run test`, and `RUSTDOCFLAGS='-D warnings' cargo doc -p stache-macos --no-deps`. Add the independently shipped adapter to [architecture](../../architecture/overview.md), explicitly without a Hold-to-Quit runtime. Manual checks require separate approval even after these pass.
