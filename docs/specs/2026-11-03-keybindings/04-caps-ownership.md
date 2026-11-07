---
status: done
---

# 04 — Caps state, durable journal and remap ownership

Read when implementing the Caps adapter and recovery protocol. Depends on parts
01–03 and their ownership compile gates; no production remap occurs in this part.

## Scope and files

Add `modules/keybindings/src/caps/{mod.rs,parser.rs,state.rs}` and fake tests.
Add safe `macos/src/caps_remap.rs` (or focused submodules), fake hidutil/filesystem/
lease/process boundaries and exports. Extend identity path support only as needed;
durable state must not live in clearable cache, ephemeral runtime or user-selected
config directories. No legacy file/path reuse.

## Requirements

Preserve legacy Caps parser key/alias table, case behavior and only
`CapsLock+<single key>` grammar. Preserve physical chord state: bare Caps tap does
nothing; F18 surrogate down/up suppressed while active, capitalization/LED never
toggle; one dispatch per discrete bound chord-key press, suppress autorepeat and
matched key-up even after Caps release, duplicate Caps down does not rearm,
unbound keys pass. Releasing/repressing a chord under held Caps may dispatch again.
Use preallocated pure state plus part 02's physical-press ledger, not legacy
mutexes/command cloning in the tap. Real F18 is swallowed too: no reliable device
distinction is promised by a global HID remap.

Subscribe and obtain confirmed active tap acknowledgement **before any remap**.
Missing permission, cancelled readiness, timeout or disabled tap cannot mutate
Caps. Explicit trust checks never prompt. Tap loss cancels dispatch and starts
bounded remap cleanup off-tap; no re-enable resumes an armed old chord. A cleanup
lease may remain after module dispatch is invalidated, but is not an active
Keybindings subscription.

Reserve a private durable Next identity directory, planned
`~/Library/Application Support/com.marcosmoura.stache.next/keybindings/`, injected
in tests, containing a versioned journal and persistent coordination lock. One
identity-wide lease serializes this application's writers across processes and
different config directories. Never unlink the lock; use no-follow/private owned
regular files, validate identity/inode after locking and before mutation, and
fail closed on replacement/symlink/corrupt state. Lease wait is **5 s**, with
cancellation checks at most **10 ms** apart. This is not
a compare-and-swap against external hidutil users; disclose external races.

Under the lease: read/strictly parse the complete current UserKeyMapping; refuse
unknown/truncated/malformed output, duplicate Caps sources and any unowned Caps
mapping, even Caps→F18. Preserve unrelated mappings exactly in apply/restore
plans; never replace the whole array from a stale startup snapshot. A matching
journal proves only this application's intended ownership, not that an external
writer could not have independently written the same mapping.

Persist an atomic durable **write-ahead journal before mutation**: identity,
operation/version, intended Caps transition, prior relevant state, and pending
phase sufficient to recover an interrupted apply/restore. Sync file and directory
as needed; failure means no mutation. Set via `/usr/bin/hidutil`, then reread and
verify before marking applied. Journal phase updates cannot erase recovery on
failure. Restore journals intent before write, verifies absence/restoration before
retiring ownership; never delete journal before confirmed restoration. Recovery
reconciles actual state idempotently under the same lease before a fresh apply,
including crash after write but before acknowledgement/phase update. Recover own
crash-left mapping even with module disabled/no Caps bindings; never adopt legacy
state or a foreign mapping as ours. Corrupt/uncertain ownership blocks acquisition.

That recovery needs a trigger independent of module activation: the host runs a
keybindings-owned bounded `recover_stale_caps_remap` step once per desktop start,
composed like the production factory (the app may depend on the module crate; the
SDK gets no module-ID special case). It runs before/independent of module start,
tray state and config validity, acts only when Next's own journal exists, restores
exactly the mapping that journal records, never touches foreign or legacy
mappings, and reports failure without blocking startup or entering a retry loop.
Recovery requires a relaunch after a crash; until it runs, Caps may still send F18.
Files: the module entry point plus app startup/finish wiring and its tests.

External changes are preserved: reread immediately before restore, remove only
the demonstrably owned Caps entry and preserve latest unrelated mappings. If
Caps no longer matches the owned transition, do not overwrite the foreign value;
record an explicit ownership-lost/recovery outcome rather than "restored". Exact
atomicity with uncooperating writers is impossible; verify/reconcile and disclose
that limitation. Durable ambiguous state remains available for operator/retry
recovery; do not clear it as a convenience.

Use engineering constants, not user config: hidutil child lifetime **5 s**,
stdout **1 MiB**, stderr **64 KiB**, and part 02's tap/native **2 s**
acknowledgement / **10 ms** cancellable wait quantum. Output caps count raw bytes;
overflow fails the operation, never parses a truncated success. These administrative
deadlines do not add a user-command timeout. Own exact child
handles, drain bounded output concurrently, terminate/reap only that child on
overflow/cancellation/timeout. A post-spawn timeout means mutation **uncertain**
until reread verifies; retain journal, child/reap ownership and diagnostics.
Allow part 01's **2 s** child cleanup then retain the exact handle in its bounded
reaper; reserve capacity before spawning hidutil too. OS scheduling and
uninterruptible calls can exceed elapsed targets; a deadline cannot undo mutation.
No kill-by-name, detached unbounded workers or infinite stop wait.

Pause/disable/stop/removal of Caps bindings first invalidate command dispatch,
cancel queued sequences and clear armed state, then restore under lease. Cleanup
may retain an inert safety-only F18/press-ledger subscription while the known own
mapping remains pending: it suppresses surrogate/matched ups but cannot enqueue
commands. Native stop acknowledgement uses part 02's **2 s** bound; a longer
restore remains pending off-main, not a reason to extend that wait. Failed restore
stays `cleanupPending` or `uncertain`, not Paused-and-restored. If the tap is unavailable, disclose that
physical Caps may emit F18 until recovery; never revive module dispatch to hide
that degradation. Bound retained cleanup ownership and block successor remap
acquisition until resolved, without blocking unrelated module teardown.

## Acceptance criteria

- [x] **KB-04-01** — Parser/state preserves legacy discrete-press, bare-tap,
      repeat/unbound/key-up behavior and documents real F18/global-keyboard limits.
- [x] **KB-04-02** — Tap-before-remap, foreign-map refusal and strict bounded
      hidutil parsing prevent unacknowledged or unowned remap acquisition.
- [x] **KB-04-03** — Durable journal-before-write and verify-before-retire survive
      every apply/restore crash boundary and recover only Next-owned state.
- [x] **KB-04-04** — Identity-wide coordination preserves unrelated/external
      mappings, fails safely on path replacement and states non-CAS limitations.
- [x] **KB-04-05** — All administrative waits/output are bounded; uncertain
      children/mutations/journals remain owned with no false cleanup success.
- [x] **KB-04-06** — Stop immediately invalidates dispatch; retained safety-only
      cleanup or unavailable-tap F18 exposure is truthful and cannot run commands.
- [x] **KB-04-07** — Host startup recovery restores a stale Next-owned journal
      mapping exactly once regardless of module enabled state/config validity,
      refuses foreign mappings, is a no-op without a journal, and reports failure
      without blocking startup or retrying in a loop.

## Tests and gates

Fake all native/process/storage effects. Table-test every parser key/alias and
state trace, including repeated discrete chords, up after Caps release, overflow,
tap loss, real F18 and reload mid-press. Fault-inject journal creation/fsync/rename,
apply/restore/read/verify failure and a crash before/after **each** durable/native
transition. Cover own stale journal while disabled, corrupt/foreign/identical F18,
external unrelated edits, concurrent identities/config roots, duplicate source,
malformed/null/quoted output, symlinks/inode replacement, lock timeout, child
output overflow, kill/reap failure, stop/late ack/successor races. Assert no
mutation on 5 s lease expiry; fake-time tests check 5 s child lifetime, exact
1 MiB/64 KiB caps, 2 s acknowledgement/cleanup, 10 ms cancellation quantum and
retained reaper saturation without parsing partial output as success. Assert no
mutation when readiness/journal/lease fails, and no deletion on unverified restore.
Host-recovery tests: stale journal with the module disabled and with invalid
config → restored exactly once; no journal → no-op; foreign/legacy or ambiguous
mapping → refused with a diagnostic; recovery failure → reported, startup still
proceeds, no retry loop.

Compile real safe macOS adapter/lease/process ownership, crate tests and all root
checks from part 01. Never invoke real hidutil or remap/input APIs in default
tests. Manual global-remap/restart/crash-recovery verification requires approval
for **each run**, exact Next process/journal identification, saved before/after
mapping evidence, no competing legacy/foreign remap, and restoration verification.
If unavailable, record unverified, not passed. Update testing/architecture docs
and notice for recovery/adapter facts only; runtime still not shipped.

## Gate evidence

- **Automated:** all KB-04 criteria are covered by fake-backed tests named
  `kb_04_NN_*` in the keybindings, macOS, SDK and app crates, including the six
  `caps_recovery` host integration tests. Full native suite (1618 tests), UI (24)
  and script (6) tests, lint and format checks passed on 2026-11-07.
- **Real `hidutil` remap, restart and crash recovery: unverified, not run.** Each
  manual run needs its own approval; no before/after mapping evidence is recorded.
- **Deviation:** the strict `UserKeyMapping` parser refuses quoted numeric values,
  which the legacy parser accepted, and fails closed instead.
- **Handoff to Part 05:** tap-loss cleanup is caller-driven. The runtime must poll
  `CapsOwner::check_tap` from its bounded watch, because the input service only
  cancels presses that were held when the tap was lost.
