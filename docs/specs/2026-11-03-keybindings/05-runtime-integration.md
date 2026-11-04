---
status: draft
---

# 05 — Runtime, accepted reload and host integration

Read when activating Keybindings after parts 01–04 and their feasibility gates.

## Scope and files

Add `modules/keybindings/src/{runtime.rs,dispatch.rs,status.rs,test_support.rs}`
and lifecycle tests; extend `lib.rs`. Add inert `KeybindingsFactory` in
`app/src/host.rs`, production composition in `lib.rs`, app-owned status projection
in `bindings.rs`, and host/catalog/bindings/CLI regression tests. Extend generic
SDK registry/publication/permission contracts narrowly where required below;
reuse already-landed accepted-config/factory-context helpers after rebase.

## Requirements

Implement `Module<Config = KeybindingsConfig>` with configure/start/reconfigure/
stop. Registration and disabled/paused operation create no native subscription,
hotkey, command worker or remap. Crash-left Caps recovery is **not** part of module
activation: it is the host-owned startup step specified by part 04, which runs once
per desktop start regardless of enabled state. Starts use accepted configuration
only; missing/invalid file, default, failed start and Retry keep existing
ConfigStore/registry semantics.
One bad shortcut is diagnosed/skipped; native failures never masquerade as full
success. Partial success reports the exact active/rejected subset; all requested
usable bindings failing is a lifecycle failure. An intentionally empty map is
valid/inactive, not a registration error.

The runtime owns a lazy bounded dispatcher/task pool: at most **16 active
sequences** plus **64 FIFO waiting sequences**. No eager 16 idle workers; create
owned tasks only as work is admitted and reap completed tasks. Independent accepted
presses, including repeated presses of one shortcut, may run concurrently; only
steps within each sequence are sequential/abort-on-failure. Start waiting sequences
in FIFO admission order as slots free (completion order is not FIFO); new work
cannot bypass waiters. Queue whole immutable sequences with generation/revision;
do not evict accepted work. Reject newest at saturation with an overflow
diagnostic/counter: an intentional safety limit, not legacy global serialization
or unbounded per-press threads. Empty capture bypasses command capacity, including
at saturation. A long-lived child occupies only its own slot until cancellation;
other free slots progress, with no duration timeout. Tap reservation and dispatcher
acceptance must be the same bounded admission decision, not two queues that can disagree.
Carbon cannot guarantee fail-open after OS capture on queue full: diagnose lost
invocation, do not execute it late or claim it was delivered.

Document one known interaction: a binding that runs `stache reload` is terminated
by the accepted Keybindings reload that its own command causes (the reload cancels
queued sequences), so later steps in that same sequence never run. That is
consistent with cancellation ownership, not a bug; the docs must state it so users
do not rely on post-reload steps.

Pause/disable/stop and an **accepted changed** reload immediately invalidate the
old dispatcher generation, discard queued sequences, and cancel/terminate/reap
only directly spawned active children across all slots via part 01. A rejected/
unchanged candidate does not cancel accepted work. The spawn gate serializes final validity checking
with invalidation at creation; no stale process may start after invalidation wins.
Already-created processes/effects are not retroactively unissued. Retained reap
failures are bounded and visible; a full retained-ownership budget blocks new
dispatch, never leaks ownership or prevents all host shutdown indefinitely.

### Accepted configuration and resource transaction

Prove actual registry behavior, not a presumed future API: validate/plan, stage
inert added resources, and reuse unchanged native registrations. The canonical
contract shared by all rewrites is:

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
> 03, else this Keybindings part 05) implements this shape generically in the SDK;
> every later consumer reuses it and must not introduce a second shape.

For this runtime: candidates cannot execute commands before publication. After
publication and before the module observes the new revision, no press may execute
an action derived from either binding set; the tap stays inert for commands and
queued work is admitted only once the runtime has flipped to the observed revision.
Once a revision change is observed, work derived from the old binding set must not
execute. Account for current registry `reconfigure` preceding published revision: a
module-local counter or "old revision + 1" alone is not proof. Reuse a landed typed
accepted read/watch cell; otherwise introduce exactly the contract above in the SDK
with rollback and publication-before-notification. No config-store read from the
tap, speculative generic callback framework or module-ID special case.

A global Caps remap cannot be staged inertly: stage its lease/journal/readiness
preconditions without mutation, then authorize apply only for the committed
value/revision. Until verified, Caps dispatch stays inactive. Post-commit apply
failure/uncertainty is an accepted configuration with failed/degraded activation
and retained recovery, not a rejected candidate or successful registration.

Failed staging/reconfigure retains last-good config/revision/routes/queued work;
cleanup of an inert failed candidate is tracked independently. A Caps-removal
candidate cannot mutate the last-good remap then return rejected: defer destructive
retirement until commit. After commit, restoration failure is accepted config plus
cleanup/degraded outcome, never a fictitious rejection/rollback of native effects.
Likewise retired Carbon resources stay inert/pending on removal failure. Any
minimal generic post-commit/cleanup outcome must distinguish committed from
rejected and remain typed through real host/store adapters. Stop for architectural
approval if the current contract cannot express this safely; do not weaken
`Module::reconfigure`'s rejection guarantee.

### Permissions and lifecycle reporting

Ordinary/section-only configurations require no tap permission. Caps uses fresh
nonprompting trust plus confirmed tap availability. Add only a generic
**accepted-config-dependent requirements** hook if not already landed, preserving
static-descriptor defaults for existing modules. Registry/host must recompute it
for candidate transitions and waiting/recovery; ordinary→Caps without trust is an
accepted Waiting state, not falsely active. All Caps-dependent activity is stopped
on revocation before any reacquisition. Mixed configurations waiting for Caps do
not claim ordinary chords active if the whole runtime is waiting.

Shipped host permission policy currently permits one automatic prompt. Shared
input requirements must be explicitly **user-prompt-only**: startup, enable,
reload, tick and activation may check/poll but not request. Add minimal generic
prompt-policy metadata, defaulting to existing behavior for non-input consumers.
The user-only metadata applies to **every shared-input consumer**, including future
Hold-to-Quit/MenuAnywhere, not merely Caps/Keybindings; record this downstream
contract in shared-input docs rather than granting later consumers auto-prompt.
Explicit tray request may prompt, default tests never do. A trusted but failed
tap is unavailable/platform failure, not proof of a missing permission; preserve
Waiting versus Unavailable/Failed truthfully (a generic unavailable reason only
if needed). No permanent permission cache and no hot retry loop.

### Runtime diagnostics and shutdown

Module owns a separately published read-only runtime snapshot: active counts and
per-binding registration/parse/conflict/layout diagnostics, queue drops, tap state,
remap phase (`inactive`, `applying`, `owned`, `cleanupPending`, `uncertain`,
`ownershipLost`), retained cleanup IDs/child summary and accepted revision. Reads
never lock module code, wait on native work or access disk. Expose it as an
optional typed `keybindings` runtime-details field on app-owned `ModuleStatusView`
(or reuse an equivalent landed typed projection), with Rust-derived DTO exports.
App overlays held ConfigStore diagnostics without overwriting runtime details;
`diagnostic` remains the rejected-file channel, not a runtime error bucket. No
new socket/CLI/Settings surface. IDs and uncertain outcomes survive serde and
generated bindings; opaque revisions are decimal strings on the UI boundary.

Factory construction is inert. Stop invalidates before acknowledged cleanup;
repeated/partial stop is safe. Host begin-shutdown wakes owned waits before joins;
registry reverse-stop precedes shared input final owner cleanup. Main never waits
for a worker that needs main. Retain inert failed cleanup in a bounded host/native
owner independent of joinable module workers, expose pending state and allow
unrelated shutdown to proceed. Do not claim to fix the separate Cmd-Q/logout gap.

## Acceptance criteria

- [ ] **KB-05-01** — Inert production factory/start/enable/pause/resume/Retry and
      partial/all-failed registration report actual active resources correctly.
- [ ] **KB-05-02** — Bounded FIFO dispatcher, overflow/capture behavior and
      generation cancellation/spawn gate honor accepted sequence ownership and
      legacy independent-press concurrency within 16 active / 64 waiting limits.
- [ ] **KB-05-03** — The canonical accepted-config publication contract is reused
      or introduced exactly once, prevents stale/half-applied dispatch, preserves
      rejected candidates and reports committed cleanup.
- [ ] **KB-05-04** — Config-dependent permission/prompt policy keeps ordinary
      shortcuts permission-free, Caps truthful and every shared-input consumer's
      requests explicit-user-only, including downstream HQ/MA.
- [ ] **KB-05-05** — Published typed runtime details and held file diagnostics
      coexist, repair independently and survive generated/serde boundaries.
- [ ] **KB-05-06** — Partial startup, pending restore/reap, shutdown and successor
      generations remain bounded, inert and deadlock-free across actual host paths.
- [ ] **KB-05-07** — Host-owned Caps startup recovery is composed and exercised
      independently of module activation, and a reject/unchanged reload causes no
      cancellation of accepted work.

## Tests, gates and docs

Use fake input/Carbon/layout/hidutil/process/permissions and temp config/identity
roots. Barrier-test acceptance versus callback/enqueue/spawn/layout refresh,
unchanged/rejected reload, Caps addition/removal, staged failure and cleanup
timeout after commit. Prove that between publication and the module observing the
new revision no press executes from either binding set, and that observed revision
change stops old-binding actions. Prove host Caps recovery runs with the module
disabled and does not require config validity. Prove lazy zero-idle-task construction, 16 active plus 64
waiting admission, rejection of submission 81, FIFO promotion without bypass,
and continued progress in another slot while one child stays long-running. Prove
two independent presses overlap but their own steps remain sequential and abort
independently; cancel every active slot and all waiters on accepted invalidation,
with no stale spawn or lost child ownership. Test queued sequence preservation
for rejected/unchanged reload, reserved empty capture at saturation (subject to
part 02's press-ledger capacity), and reap-ownership exhaustion. Test permissions absent/granted/revoked, no requests
on automatic triggers for any shared-input consumer, explicit request,
ordinary→Caps→ordinary and paused edits. Use fake time for all engineering limits.
Exercise actual ConfigStore/Registry/HostDriver, source repair/deletion/invalid
files, status serialization, stale IDs, failed-start cleanup, shutdown during
setup and two independent fake hosts. Assert no worker→main→join cycle.

Run root checks/build/rustdoc from part 01, release compile and fake integration
suite, deterministic schemas/bindings, and existing noTunes/proxyAudio/startup
regressions. Optional real shortcut/layout/Caps/process-lifecycle manual gates
require each-run approval and verified restoration/unregister; never run them by
default or use real configured application commands as fixtures. Record manual
coverage gaps honestly. Materialize feature behavior only after acceptance;
update SDK/config/overview/testing and additive index/roadmap for shipped facts.
