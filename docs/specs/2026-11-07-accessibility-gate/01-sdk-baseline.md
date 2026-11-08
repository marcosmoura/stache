---
status: done
---

# 01 — SDK baseline permission requirement

## Scope

`src/crates/sdk/src/registry.rs` (builder, `permission_requirements`, slot requirement storage), `src/crates/sdk/src/permissions.rs` docs, registry tests and [sdk.md](../../architecture/sdk.md). No app or module change; the host does not use it until part 02.

## Requirements

1. `Registry` gains a builder method, for example `Registry::with_baseline_requirements(Vec<PermissionRequirement>)`, callable only before the first `register`. Default: empty, so every existing registry behaves exactly as today.
2. The effective requirements of a slot are its module's `required_permissions(accepted config)` merged with the baseline, recomputed wherever they are recomputed today (registration, `register_rejected`, committed reload). Merge rule per permission is the existing one: user-only wins over automatic. A module with no accepted configuration keeps today's `Stopped` without a permission check.
3. `permission_requirements()` returns the baseline **unconditionally**, merged with the requirements of enabled, non-paused modules. A baseline permission is therefore needed even when every module is disabled or paused, or none is registered.
4. Start, reload and revocation use the effective set through the existing paths: `start_slot`'s `missing_permission` check runs before `configure`/`start`, so no module code runs while a baseline permission is missing; a candidate reload while the baseline is missing is accepted and waits; `on_permission_change(Denied | Unknown)` stops every running enabled, unpaused module in reverse start order and marks it `WaitingForPermission`; `Granted` starts waiting modules in dependency order.
5. Disabled and paused modules keep `Disabled`/`Paused`; the baseline never overrides them, as today.
6. The function stays pure and cheap; the baseline is immutable after construction. No platform call, no cache of a grant.

## Acceptance criteria

- [ ] AG-01-01 An empty baseline leaves every existing registry test green without edits.
- [ ] AG-01-02 With baseline `[user_only(Accessibility)]` and a fake provider answering `Denied`, `start_all` leaves every enabled module `WaitingForPermission(accessibility)`, and no module's `configure` or `start` is called, including a module that declares no permissions.
- [ ] AG-01-03 `permission_requirements()` contains the baseline with all modules disabled, all paused, and with zero modules.
- [ ] AG-01-04 `on_permission_change(Accessibility, Granted)` starts all waiting modules in dependency order; a module whose start fails is `Failed` and does not block the others.
- [ ] AG-01-05 After a grant, `on_permission_change(Accessibility, Denied)` stops every running module in reverse start order and each becomes `WaitingForPermission`; disabled and paused modules are untouched; providers' dependents follow the existing reconciliation.
- [ ] AG-01-06 A module that declares `shared_input(Accessibility)` and a baseline `user_only(Accessibility)` merge to one user-only requirement; an `automatic` module requirement merged with a user-only baseline is user-only.
- [ ] AG-01-07 A reload while the baseline is missing commits the revision and keeps `WaitingForPermission`; a later grant starts with the newest accepted configuration.
- [ ] AG-01-08 Calling the builder after registration is a typed error or compile-time impossible (implementer's choice), never a silent no-op.

## Tests

Registry unit tests in `registry.rs` with the existing fake modules and a scripted fake `PermissionProvider` (status sequence, request counter asserting zero requests). Cover the dependency-ordered start and reverse stop with recorded call logs.

## Notes

The sdk.md state table gains rows for "Start, baseline permission absent" and "Baseline permission revoked"; the prompt-policy section describes the baseline and keeps `shared_input` for future non-baseline permissions.
