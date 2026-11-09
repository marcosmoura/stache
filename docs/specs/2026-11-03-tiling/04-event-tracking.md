---
status: done
---

# 04 — Event pipeline and read-only actor tracking

Read when implementing after 03 and separate approval. This part has no production mutation/factory registration.

## Scope

Tiling `src/{state/,actor/,events/,tracking.rs}` and fake runtime harness. Map all legacy state/actor/event files in [inventory](inventory.md); actor handlers become pure transitions, native refresh stays adapter-owned. Internal typed commands/outcomes compile here for later consumers, but no public unimplemented command is exposed.

## Requirements

- One actor writer for screen/workspace/window/focus state and future visibility ledger. All inputs/effect completions are pure owned values tagged exact identity/runtime epoch/revision/sequence; no global statics, native calls, locks awaiting main or direct mutations from processor/subscribers.
- Bound semantic queue at 1024 messages and bound geometry/mailbox cardinality with explicit admission accounting. Create/destroy/focus preserve accepted FIFO order; flush/invalidate older coalesced geometry across identity/membership barriers. Geometry uses latest-per-exact-token per display at refresh clamp 30–360 Hz (unknown 60). Retiming/hotplug cancels old timer and remaps last observed frames, not bare IDs.
- Full semantic queue cannot silently lose destroy/focus then continue acting on stale identity. Increment diagnostics, mark affected scope unconfirmed, invalidate unsafe issuance and schedule one bounded authoritative reconciliation; no unbounded overflow list. Geometry overflow may discard superseded samples but requests refresh. Work reservation failure never becomes command success. A permanently overloaded stream must stay bounded/ineligible with a visible diagnostic.
- Install observers before asynchronous, chunked initial enumeration. Epoch/checkpoint replay or final resnapshot closes enumeration/event races, including create/destroy while scanning. No per-window synchronous startup wait chain. Read-only publication reports initializing/reconciling/complete/incomplete facts, separate desired state from observed state.
- Preserve app activate/hide/show/terminate, window create/destroy/focus/unfocus/title/geometry/minimize/fullscreen, display changes and unknown metadata. Fullscreen/minimized/hidden/native-tab windows remain tracked but excluded from writes appropriately. External user-hidden state is not runtime-owned. Ignore/PiP filters apply consistently to startup and events.
- Queries read one atomic actor snapshot (screens/workspaces/windows/focus/sequence), not independent sequential samples. Pure policy retains focus history and stable layout order; no numeric identity reuse leaks caches. Reconcile only on startup/overflow/observer discontinuity/topology or proven missed events; any periodic safety pass needs measured justification, finite cadence and cancellation-wakeable sleep. No constant polling assumption.
- Cancellation wakes actor/timers/reconcile reads; disconnects and recoverable callback panics in unwind-enabled builds become explicit health facts and cancel unsafe actions. Do not catch a panic then continue with possibly corrupted state; preserve last published safe snapshot and require reconciliation/restart as appropriate. Release panic=abort cannot recover arbitrary Rust panics; audit bounded nonpanicking callbacks rather than claim fake unwind recovery proves release resilience.

## Acceptance criteria

- [x] TI-04-01: Single-writer pure actor preserves all inventory event/state semantics with exact-token cache pruning and atomic read-only snapshots.
- [x] TI-04-02: Bounded FIFO/coalesced queues/overflow reconciliation pass create-focus-destroy/geometry ordering and overload tests without stale action authority or memory growth.
- [x] TI-04-03: Async initial enumeration/reconciliation, hotplug/retiming and lifecycle cancellation remain responsive under barriers and report completeness truthfully.
- [x] TI-04-04: Read-only harness has zero native effects, deterministic diagnostics, no global runtime and no public placeholder handlers; docs distinguish tracking from active tiling.

## Tests

Fake AX/apps/displays/clock; enumeration blocked while event stream advances; destroy+same-app ID reuse; title/rule exclusions; fullscreen/minimize/hide; 30/60/120/360/out-of-range/NaN refresh; queue full/disconnect, mailbox saturation and slow actor; cancel between check/sleep; no-screen/hotplug; two registries and late old completions. Run [validation](validation.md), without real windows.

## Rollback and affected docs

No production registration to undo. Keep state/effect seams compiling for 05. Failure blocks mutation until confirmed reconciliation; never fabricate empty successful queries. Update feature/testing and SDK integration notes for read-only harness only.
