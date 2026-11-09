# Keep Awake

Read when changing the Keep Awake module, its lock-aware power policy or its
read-only status.

Keep Awake is the shipped module in `src/crates/modules/keep-awake`: it prevents
display, idle and system sleep while the session is known unlocked and the
module is enabled and unpaused. The production factory in
`src/crates/app/src/host.rs` only composes it into the desktop host; every
policy decision lives in the module and every native call goes through the
injectable adapters in `stache_macos::keep_awake`.

## Configuration

`keepAwake.toml` is the authoritative document (`KeepAwakeConfig`):

| Key       | Values           | Default |
| --------- | ---------------- | ------- |
| `enabled` | `true` / `false` | `false` |

There is no second setting: no duration, mode, schedule, independent
desired-awake flag or separate toggle. A missing file and `{}` both mean
disabled. A malformed, empty or mistyped file is invalid with a diagnostic and
is never silently defaulted; unknown keys stay non-fatal warnings, so a typo
never enables anything.

`config init`, `config path`, `schema keepAwake` and the schema/bindings
generators discover the entry through the shared catalog. `config migrate`
always emits the commented disabled template, whether the legacy `bar` section
was enabled, disabled or absent; legacy has no Keep Awake setting, so migration
never infers consent from Bar.

## Controls and lifecycle

The generic configuration-backed tray **Enable/Disable** and session
**Pause/Resume** are the only controls. There is no dedicated window, Settings
screen, CLI command, TCC permission or webview. Keep Awake is independent of
Bar; a future Bar may consume its status but never owns the assertion.
Construction and `configure` acquire nothing; registration starts no assertion,
observer, worker or timer.

`start` installs the lock observation (or records polling fallback), probes the
current lock state before any acquisition, then establishes one generation, one
SDK-owned worker and one owned reconciliation deadline. A known-locked startup
succeeds as `Running` with no assertion. Enabled, unpaused and known-unlocked
starts the normal path: request one all-three assertion lease, publish
`acquiring` before native creation, validate the candidate against the current
lock and generation, then publish `active` only after the complete lease is
adopted.

A screen lock releases the lease; an unlock restores it. Resume re-probes and
reinstalls observation before acquisition and never trusts stale unlocked
knowledge.
Disable, pause and stop cancel the generation, wake and join the worker,
unregister observation and release the lease. Stage-1 host teardown invalidation
is wired to the same generation before the initial reconciliation: it cancels only
the generation's module-owned native-operation token, marks the generation dead
and wakes the worker without probing, creating, releasing, removing or logging
anything. An in-flight native creation
therefore stops before the next assertion ID and a candidate is never adopted,
even while `stop` is still waiting behind a registry lifecycle lock. The SDK
lifecycle token is observed only and remains registry-owned. Repeated
starts, unchanged configuration, equivalent reload bytes and the watcher echo
after a tray edit create no duplicate lease, subscription, worker or timer.

## Assertion ownership

One module instance owns one `AssertionOwner`. One active lease contains up to
three IOPM assertion IDs for display, idle and system sleep; `active` means that
complete lease was adopted for the current generation. Pending or unresolved
IDs keep their original owner identity, block fresh acquisition and cannot be
released by a successor's cleanup.

Explicit release and partial rollback are fallible. A best-effort `Drop` is not
proof of release, and process exit is the ultimate OS boundary, not evidence
that application cleanup succeeded. The desktop host instead runs one bounded
coordinated teardown: ordered stages with a five-second total budget, a 250 ms
escalation for a later stop request and deadline-bounded waits per owner. Its
first stage is logical invalidation, not cleanup, and Keep Awake's listener is
wired to it as described above. Invalidation releases nothing.

The module's `stop` and partial rollback remain the only physical release path.
The host's power-assertion slot is an acknowledgement only: it reads the same
`AssertionOwner` ledger, counts `active`, `acquiring` or in-progress cleanup as
not released, and never releases IDs itself, so it adds no second cleanup path.
Real-device assertion, screen-lock and teardown behavior remains unverified by
the fake-backed suite ([Native boundary and limits](#native-boundary-and-limits)).

## Assertion states

| State            | Meaning                                                                                                             |
| ---------------- | ------------------------------------------------------------------------------------------------------------------- |
| `inactive`       | Release is confirmed: no owned IDs, in-flight candidate or acquisition remains.                                     |
| `acquiring`      | Published before the first native creation and through candidate validation; work is in flight but not adopted.     |
| `active`         | The current generation owns a complete, adopted all-three lease.                                                    |
| `releasePending` | Published before native release or rollback; cleanup is in progress or unresolved, so sleep may still be prevented. |

Invalidated, cancelled, locked, unknown or superseded refresh candidates are
rolled back. Normal release becomes `inactive` only after confirmed cleanup with
no outstanding work, candidate or unresolved ID. Stop, pause and disable never
reset assertion state to `inactive` merely because they ran. An explicit release
failure keeps `releasePending` and `assertionReleaseFailed` across paused,
disabled and failed states until cleanup is confirmed; normal transition
timing alone never sets that condition.

## Lock knowledge and observation

The probe reads the public `CGSessionCopyCurrentDictionary`. The
`CGSSessionScreenIsLocked` dictionary key is undocumented legacy compatibility,
not a public lock-state guarantee:

- a valid dictionary with no key means unlocked;
- a present `CFBoolean` true/false means locked/unlocked;
- a null or unreadable dictionary, or a present non-boolean value, means
  unknown.

Unknown suppresses acquisition and attempts release of any held lease; absence
of the key is an answer, not a query failure. The two
`com.apple.screenIsLocked`/`Unlocked` notifications are coalesced, droppable
refresh hints; a callback never supplies the lock state. While active, one
owned cancellation-wakeable reconciliation deadline probes at most two seconds
apart, so dropped hints still converge. Notification registration success does
not prove delivery.

`observation` reports `inactive`, `notificationsAndPolling` or
`pollingFallback`. An installation failure succeeds degraded when probing works,
publishes `observerUnavailable` and retries on the next lifecycle start or
resume; the warning clears only after a successful install. No recurring
reinstaller exists.

## Status contract

`keepAwake.status` is the only new socket method; it uses the existing router
and server. The app owns the envelope and handler:

```json
{ "module": { "...": "ModuleStatusView" }, "runtime": { "...": "KeepAwakeRuntimeStatus" } }
```

`module` projects the published SDK snapshot: identifier, lifecycle `state`,
`enabled`, `paused`, the last accepted revision as an exact decimal string,
failure detail and diagnostic. The held `ConfigStore` file diagnostic takes
precedence over the registry diagnostic. A rejected parse, type, I/O or deleted
file keeps the accepted configuration, revision and live lease; saving identical
last-good bytes clears both diagnostics without lease churn.

`runtime` is one coherent published cell:

| Field            | Values                                                          |
| ---------------- | --------------------------------------------------------------- |
| `assertionState` | `inactive`, `acquiring`, `active`, `releasePending`             |
| `lockState`      | `unknown`, `locked`, `unlocked`                                 |
| `observation`    | `inactive`, `notificationsAndPolling`, `pollingFallback`        |
| `conditions`     | typed `kind` and actionable `message`, deduplicated and ordered |

Condition kinds keep the stable order `observerUnavailable`, `lockProbeFailed`,
`assertionAcquireFailed`, `assertionReleaseFailed`, `observationDispatchFailed`.
Conditions describe current failures; an unchanged condition is logged once and
recovery is published when it clears. `assertionReleaseFailed` appears only
after actual release or rollback failure. An inactive runtime reports
`lockState: unknown` rather than a stale observation; lock knowledge may reset
independently of pending cleanup.

The method answers in every lifecycle, including disabled, paused, failed,
locked, acquiring and `releasePending`. It copies only published and held data:
no native probe or assertion, worker start, observer install, filesystem read or
write, configuration apply or window effect. Lifecycle, diagnostic and runtime
values are sampled separately, so a response is an observation, not one globally
atomic snapshot. `active` proves owned prevention, not immunity to lid close,
forced sleep or low-battery policy.

## Failures

An observation failure may degrade to probe-only polling while the module stays
`Running`. A probe that cannot produce an answer suppresses acquisition, attempts
release and reports `lockProbeFailed`. An acquisition failure ends in `inactive`
only when rollback confirms every owned ID released; while rollback is in
progress or unresolved the runtime stays `releasePending`, adding
`assertionReleaseFailed` only on actual rollback failure. Retry happens on
scheduled reconciliation, resume or the next lifecycle cleanup, never a tight
loop.

Runtime degradation does not require an SDK lifecycle change: `Running`
describes resource ownership, not successful prevention. An initial required
probe or acquisition failure fails `start` with a typed `Operation::Start`
platform error and attempts full rollback. A failure during `Stop` cleanup
returns the existing SDK lifecycle cleanup error and retains pending ownership.
Generic **Retry** resolves pending cleanup before any fresh acquisition.

Regular lifecycle `Stop` waits for in-flight native work before joining and
releasing. An in-flight candidate's rollback failure therefore fails that stop
instead of appearing only in status, and slow work that ends cleanly is a clean
stop. A native call that never returns can block regular `Stop`. During host
coordinated teardown, stage-1 invalidation makes the generation inert and cancels
its module-owned native-operation token; the host caps its module-stop wait at
the lesser of two seconds and the remaining teardown budget. On timeout it
abandons the inert owner and names it as unacknowledged. This does not interrupt
an internal native call or prove cleanup completed: unresolved IDs stay owned
instead of being claimed released or `inactive`. A failed removal of lock
observation fails the stop and reports `observationDispatchFailed`; the callback is inert and removal
retries on the next lifecycle cleanup.

After a failed disable or pause no worker or timer runs, so nothing retries in
the background. The module shows `failed` with **Retry**, and status keeps
reporting the pending IDs. Retry repeats only the cleanup: the module stays
disabled or paused, no configuration is written and no lease is created. Enable,
Resume and shutdown also retry it.

## Native boundary and limits

All project Cocoa/FFI lives in `stache-macos`; the module crate forbids unsafe
code. The assertion backend, lock probe, observation installer, owner and lease
are injectable, and constructing them acquires nothing.

The default suite is fake-backed: no real power assertion, screen lock, wake,
application launch or distributed-observer installation. Real three-assertion
behavior, notification delivery, manual screen lock/unlock and resume or
forced-sleep checks are **unverified**. A real-device power check requires
explicit approval for each run and the `pmset -g assertions` protocol in the
[spec](../specs/2026-11-03-keep-awake/00-overview.md#validation-and-safety).
Bundle signing and notarization remain outside this scope.

For the old Bar-bound behavior, read the
[archived legacy feature](../../_legacy/docs/features/keep-awake.md). Its
automatic activation and UI payload mismatch are not the new contract.
