# Keep Awake (planned)

Read when locating the Keep Awake rewrite plan. **The independent module and status endpoint are not shipped.**

The [draft spec](../specs/2026-11-03-keep-awake/00-overview.md) proposes a default-disabled `keepAwake` module, independent of Bar, with display/idle/system sleep prevention while unlocked. Existing generic tray Enable/Disable and session Pause/Resume would be its controls; no separate toggle, window, duration, mode or CLI is planned.

The planned legacy-compatible lock probe treats an absent lock key in a valid session dictionary as unlocked, explicit CFBoolean true/false as locked/unlocked, and null/unreadable dictionaries or present non-boolean values as unknown (suppressing acquisition). This best-effort mapping and notification hints are not public lock-state guarantees. Planned read-only status distinguishes confirmed `inactive`, in-flight unadopted `acquiring`, complete adopted `active` prevention and in-progress or unresolved release/rollback (`releasePending`). Publish acquiring before native creation/candidate validation and releasePending before cleanup; only actual cleanup failure sets assertionReleaseFailed, and inactive requires confirmed cleanup with no outstanding work/candidates/IDs. Status does not promise immunity to macOS sleep policy.

Implementation requires separate approval for each draft part and a native feasibility gate before runtime work. Planned APIs are not callable shipped APIs. This notice will be replaced with verified behavior as the parts ship; detailed planned reference material stays in the spec.

For the old Bar-bound behavior, read the [archived legacy feature](../../_legacy/docs/features/keep-awake.md). Its automatic activation and UI payload mismatch are not the new contract.
