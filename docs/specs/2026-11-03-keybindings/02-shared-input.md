---
status: done
---

# 02 — Shared input service and native ownership gate

Read when implementing the planned host input capability. Depends on part 01;
this part publishes no production Keybindings subscription.

## Scope and files

Add planned `sdk/src/input.rs` and test fakes; extend `context.rs`, `registry.rs`
service injection/constructors and exports. Add `macos/src/input.rs` (or a focused
directory) and safe exports, target framework dependencies and owner tests.
Wire one inert service through `app/src/host.rs`/`lib.rs` and host fake fixtures.
Reuse any landed generic factory context; do not create another one.

## Requirements

The host creates one service; SDK declares only safe value/trait contracts and
opaque subscription IDs scoped by service identity, generation and revision.
The macOS implementation owns one dedicated thread/run loop, at most one
**suppressing head-insert** HID tap, and at most one additional **listen-only**
observation tap for passive subscribers. The suppressing tap starts lazily on the
first suppressing subscription and closes after the last one/cleanup lease is
acknowledged; the listen-only tap exists only while at least one passive
observation subscriber exists and can never suppress, intercept, inject or change
priority. Passive subscribers receive immutable values and are bounded like any
other callback, but their inability to suppress must be structural, not a
convention. No idle tap when no consumer needs it; no per-module tap.
Native ports, sources and run-loop objects never escape their owner. Publish
readiness only after installation/enable acknowledgement. A cancelled/expired
queued installation cannot install late; partial resources stay owned for cleanup.

Declarative filters include keyboard down/up, `flagsChanged` (including modifier
release), exact key codes, exact mouse-button numbers and down/up kinds, modifier
masks with explicit matching semantics. Button **2** must be representable without
matching all other-button events. Value events expose immutable kind, key/button,
flags, repeat, timestamp, physical-press identity and a consistent cached context
snapshot. No native pointers or event mutation/injection. Define the HID tap's
device-identification limits honestly; where macOS lacks per-keyboard identity,
press identity is stream key/button plus owner epoch, not invented device identity.

Support host-published, versioned immutable frontmost/exemption context snapshots
for future Hold-to-Quit without inline native queries or adding that module now.
Consumer-owned publishers update prebuilt snapshots off the tap; unknown/stale
context is explicitly representable. Modifier-release events cannot be filtered
away merely because their resulting flags no longer match a down filter.

Assign generic ordered priority/tie-break values in host composition so future
Hold-to-Quit > MenuAnywhere > Keybindings. Callback invocation order is stable;
first suppressor wins, lower callbacks cannot dispatch from the same suppressed
event. Diagnose declarative overlaps, including equal-priority ties. Own a minimal
central reservation registry for normalized physical chords and declarative HID
filters, with backend, opaque owner/generation/revision and priority metadata.
Part 03 records ordinary/section Carbon reservations here and updates/removes them
with layout/registration lifetime, and part 03 owns the generic **yielding
reservation** operation: a reservation may be marked yielded to a higher-priority
HID claimant, during which it must not dispatch; yielding is released and the
reservation restored when the claimant pauses/stops/disables/reloads or releases
it, guaranteeing one physical press cannot run both actions. This part supplies
only the registry/seam, not Carbon policy. Detect cross-backend overlaps at
reservation and filter changes; a reservation records a conflict, not HID control
over Carbon. No extra routing framework or consumer-specific SDK API. Until the
yielding operation exists, reject an overlapping higher-priority activation with
an explicit unsupported-overlap diagnostic rather than claim priority is enforced.
First suppression alone does not arbitrate native Carbon; whether a suppressing
HID tap already prevents Carbon dispatch is an unproven measured gate in part 03.

Callbacks receive immutable values and return pass/suppress, possibly reserving
bounded work. Only preallocated per-press state may change inline. No blocking
locks, native queries, command execution, waits, logging I/O or unbounded
allocation. Queue reservations are nonblocking: overflow produces an observable
counter/diagnostic, never a claimed command delivery or deferred late execution.
If first down cannot reserve required work, fail open for that whole press;
deliberate empty-command capture needs no work reservation. Already suppressed
presses keep matched repeat/up handling even if later work cannot enqueue.

Maintain a service-owned bounded physical-press ledger separate from callbacks:
the first-down pass/suppress disposition and suppressing owner persist until up.
Cancellation, reload, priority change and logical unsubscribe inhibit module
callbacks/commands immediately, but inert ledger tombstones consume matching
repeat/up for a suppressed down. A passed down cannot acquire a new suppressor
mid-press. Unknown/orphan releases pass. Bound ledger capacity, reject/fail open
new presses on exhaustion and diagnose; no eviction that mismatches down/up.
Hard bounds are **1,024 press-ledger entries** and **256 native owner control
requests**, with nonblocking rejection/diagnostics on full; new unowned presses
fail open on required reservation/ledger exhaustion. This part defines a generic
non-blocking **work-reservation** contract plus a fake (reserve/release capacity,
reject-on-full, never block in a callback); part 05's dispatcher implements it with
its 16 active / 64 waiting capacity. There is no second independent work queue, and
empty capture still needs a ledger entry but no command slot.

On tap-disabled notification, cancel all armed actions/press states before
re-enable, advance epoch and never synthesize missing releases or resume a hold.
macOS may have delivered events during the gap: exact matching across a disabled
tap is impossible; report the discontinuity and treat orphan events as pass.
Re-enable failure is visible, not silent Running. Retry is explicit/bounded.

Contain unwind panics at FFI in unwind-enabled builds; mark the subscriber inert
and fail open for future unowned presses, while preserving ledger cleanup. Measure
callback elapsed time against a **1 ms soft target**; after a returned slow
callback, inhibit future delivery and diagnose. **Cannot preempt arbitrary code**
or recover a panic in release `panic=abort`; require audited nonpanicking bounded
production callbacks, and never promise hard real-time enforcement. Tests for
slow delivery use fake elapsed time, not a deliberately hung callback.

Unsubscribe linearizes immediate logical invalidation independently of native
cleanup. Tap install/enable/removal acknowledgement uses **2 s**, cancellation-aware
wait quantum at most **10 ms**, off-main. Match shipped
`macos/src/applications.rs::{ACKNOWLEDGEMENT_BUDGET,CANCELLATION_QUANTUM}`.
These are shared engineering constants also used by part 03, not user config or
hard OS scheduling guarantees. Late owner work must recheck cancellation before
mutation; deadlines cannot undo an issued native operation or preempt an
uninterruptible call. Timeout/rejection retains inert resources/IDs for retry/
eventual owner cleanup; no free-before-removal or false success. Last unsubscribe retains
only bounded cleanup ownership, and reports why the tap has not physically closed.

## Acceptance criteria

- [x] **KB-02-01** — One lazy host service/tap, safe owner confinement and exact
      readiness/partial-install cleanup compile with real types, not fake Send.
- [x] **KB-02-02** — Filters/snapshots support flagsChanged/modifier release and
      exact button 2, with explicit unknown context/device limitations.
- [x] **KB-02-03** — Generic priorities, overlap diagnostics, reservation overflow
      and callback restrictions implement deterministic HID first suppression;
      the central cross-backend registry diagnoses/rejects unsupported overlaps.
- [x] **KB-02-04** — Press ownership survives cancellation/reload/unsubscribe and
      priority races; disable epochs cancel actions before re-enable honestly.
- [x] **KB-02-05** — Panic/slow handling states build-policy limits; logical
      invalidation is immediate and cleanup timeouts retain inert owned resources.
- [x] **KB-02-06** — A listen-only observation tap exists only for passive
      subscribers, cannot suppress/intercept/inject or alter priority, and closes
      when the last passive subscriber unsubscribes; the suppressing tap is never
      created for passive-only use.
- [x] **KB-02-07** — The generic non-blocking work-reservation contract and fake
      are proven standalone (part 02 builds and tests without part 05).

## Tests and gates

Fake native owner/executor, tap, clock and subscribers test two registries/services,
first/last subscriber, simultaneous subscribe/unsubscribe, startup cancellation,
late readiness, dropped acknowledgement, dispatch rejection, stale/cross-service
IDs, owner removal failure and successor generation. Barrier-test suppression
down→stop→up, passed down→priority change→up, repeat, modifier release, button 2
versus 3+, overflow/ledger exhaustion, overlap and disabled-tap races. Prove a
passive-only subscription creates exactly one listen-only tap, that its callbacks
cannot suppress or reorder suppression, and that it closes on last unsubscribe even
while the suppressing tap stays live. Prove the standalone work-reservation fake
rejects on full without blocking. Prove no cross-backend priority claim without the
yielding operation; test reservation/filter changes and opaque ownership cleanup.
Fake time checks 2 s/10 ms acknowledgements, returned callbacks over 1 ms, and
exact 1,024/256 capacity boundaries. Prove no callbacks after invalidation, no
deferred action from rejected work, no unsafe cross-thread handles and no
main→worker→main wait cycle.

Compile gate: real macOS framework linkage/owner types, release build and test
fakes, warnings-denied SDK/macOS/app tests and rustdoc; run root checks listed in
part 01. No live taps in default tests or this planning task. Any later manual
permission/tap run needs per-run approval and a cleanup record. Update SDK,
architecture/testing docs only for the shipped inert service and its limitations.
