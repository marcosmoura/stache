# Consolidated spec review findings

Read when auditing remediation of both planning reviews. This is **specification
remediation**, not implementation acceptance or a claim that planned APIs ship.
All four Wallpapers parts remain draft; their existing WP criteria keep ownership.

## Reviewed snapshots and verdict reconciliation

- Review 1 (R1), supplied by the parent: no blockers; Keep Awake two majors,
  noTunes one major, Wallpapers one minor with PASS. Its findings remain required
  even where the second reviewer did not repeat them.
- Second Opus review (O), supplied by the parent: Keep Awake one blocker;
  Wallpapers three runtime/IPC majors plus the cross-tool integration major;
  noTunes PASS with minors. PASS is not evidence that R1's major was resolved.
- Both reviewers read the same exact spec snapshots: noTunes `6716f27`,
  Keep Awake `34b77d7` and Wallpapers `70a2447` (based on main `e0b4648`).
  The supplied review reports explicitly identify these snapshots;
  the reviews are session reports, not Git commits. `bc3e8a1` is the later noTunes
  Part 01 implementation baseline, not a reviewed spec snapshot. At remediation,
  noTunes Part 01 is done and Part 02 in-progress; all Keep Awake and Wallpapers
  parts remain draft. Parent retains the reports; this writer did not inspect
  sibling worktrees.

The table consolidates 20 unique corrections: one blocker, seven majors and
twelve minors. Overlapping observations are one row, not duplicate acceptance
criteria. WP = Wallpapers; KA/NT = sibling fixer ownership. “Specified” means
only this branch's docs changed; “delegated” is unverified here.

## Corrections and ownership

| ID  | Severity | Source           | Owner / status                              | Required correction / spec location                                                                                                                                                                                                                                         |
| --- | -------- | ---------------- | ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C01 | blocker  | O B1             | KA / delegated                              | Restore legacy lock-key compatibility: present dictionary + absent key means unlocked; null/nonboolean means unknown. Best-effort undocumented probe, not a public guarantee.                                                                                               |
| C02 | major    | R1 M1            | NT / delegated                              | Immediate logical invalidation; native observer removal requires acknowledgment or truthful timeout, retaining inert owner-thread-safe resources until eventual cleanup. Preserve sibling Part 01 done / Part 02 in-progress, not reset by this audit.                      |
| C03 | major    | R1 M2            | KA / delegated                              | App overlays held ConfigStore parse/type/I/O/deletion diagnostics on read-only ModuleStatusView projection without disk access; test repair.                                                                                                                                |
| C04 | major    | R1 M3            | KA / delegated                              | Observable fallible assertion release, retained failed IDs and truthful releasePending/errors, including partial-create rollback; keepawake 0.6 Drop alone ignores release results.                                                                                         |
| C05 | major    | O M1             | WP Part 03 / specified                      | Enqueue initial selection first, return Running before cold processing/native effects; log initial errors without asynchronous Failed state. Source validation can fail start; interval zero has no retry loop. Barrier-test lifecycle responsiveness/cancellation.         |
| C06 | major    | O M2             | WP Parts 01, 03 / specified                 | Same-cell typed accepted watch/read handle, publication-before-notification, race-free bounded wait/cancellation. Match typed equality + actual published revision; prove current reconfigure-to-RAII-publication invariant, no generic commit callback.                    |
| C07 | major    | O M3             | WP Part 03 / specified                      | Inert factory registration; lazy bounded work independent of rotation WorkerGroup. Generic begin-shutdown wakes all host waits before IPC joins; registry reverse-stop precedes factory finish/drain/join/drop. Cover disabled jobs, watch/timer races and partial startup. |
| C08 | major    | O M4 cross       | WP overview / specified; KA, NT / delegated | noTunes lands catalog migration/templates first; first context consumer introduces one generic context. Later parts rebase on current main and reuse landed facilities, add catalog/bindings/index surfaces without overwriting predecessors.                               |
| C09 | minor    | R1 minor 4       | WP Parts 02–04 / specified                  | Module-owned typed operation outcome in Response::Ok data, not extended wire errors/JSON strings. Handled ≠ successful; preserve applied/failed/not-attempted/unknown IDs through real router/client serde; only full known success exits 0.                                |
| C10 | minor    | O minor 1        | WP Part 01 / specified                      | Include conditional generic factory-context touch location app/src/host.rs; reuse landed context.                                                                                                                                                                           |
| C11 | minor    | O minor 2        | WP Part 02 / specified                      | Five-second cancellable exclusive-clear lease wait, local not global deadline; busy failure has exact stderr/exit 1 and no mutation; fake-time tests, unchanged successful clear text.                                                                                      |
| C12 | minor    | O minor 3        | WP Part 02 / specified                      | Never unlink coordination lock; private/no-follow identity validation and path/inode replacement detection before critical work. Fail closed/reacquire safely; disclose OS/temp-cleaner/uncooperating deletion limitations.                                                 |
| C13 | minor    | O minor 4        | WP Part 04 / specified                      | Valid-empty generate-all prints exactly `No wallpapers to generate.` + newline, exit 0, zero setters; structural/directory errors still fail.                                                                                                                               |
| C14 | minor    | O minor 5        | WP overview, Part 04 / specified            | Name intentional corrections: reachable valid-empty list (`No wallpapers found.`, exit 0) and both/neither validation before manager/source setup. Retain legacy argument text/exit 1 and grammar exit 2.                                                                   |
| C15 | minor    | O minor 6        | WP Part 04 / specified                      | Freeze actionable Wallpaper error adapter bodies/exit 1, deterministic quoted paths/IDs. Distinguish proven pre-native expiry from uncertain transport/issued effects; partial/unknown identity reporting and no automatic retry.                                           |
| C16 | minor    | O minor 7        | WP Parts 01, 04 / specified                 | Config path/list strings trim before tilde expansion; explicit CLI filesystem arguments preserve legitimate spaces. Test both and invocation-CWD versus config-directory bases.                                                                                             |
| C17 | minor    | O minor 8        | WP Part 04, feature / specified             | Document starting-host/no-socket race: absence-before-send local set allowed; later initial job may override. No extra startup protocol or second attempt after uncertainty.                                                                                                |
| C18 | minor    | O minor 9        | NT / delegated                              | Cocoa callbacks only enqueue/coalesce instance-refresh signals with panic containment; no inline query/terminate/open/wait; queued authorized main actions recheck commit validity.                                                                                         |
| C19 | minor    | O minor 10 cross | NT / delegated; WP overview / specified     | Roadmap must honestly acknowledge other separate-branch drafts, not shipped modules or broken local links. Later merge updates stale roadmap/additive index when present.                                                                                                   |
| C20 | minor    | O minor 11       | KA / delegated                              | App owns top-level KeepAwakeStatus envelope/handler with ModuleStatusView; module owns power/runtime DTOs without app dependency.                                                                                                                                           |

## Key decisions and remaining verification

The parent reports direct read-only `CGSessionCopyCurrentDictionary` queries
observing a valid dictionary with the lock key absent and an earlier CFBoolean
true. These are not controlled manual UI lock/unlock verification; no system
power operations were performed. C01 restores approved legacy behavior; this
writer did not run those probes. Sibling corrections are delegated, not inspected or certified
from their live worktrees. No sibling code is an implementation dependency here.

Explicit commands remain usable disabled/paused. Only established pre-send host
absence permits standalone set; the server checks config identity. Public native
per-display APIs, no private/System Events fallback, config-relative sources,
literal invocation-CWD CLI files, no image restoration, legacy flags/text/exits
and bounded atomic cache coordination remain approved decisions.

Documentation checks do not prove native ownership, performance, Space coverage,
power assertion behavior, watch/factory API compilation or shutdown feasibility.
Those remain implementation-part gates with fake tests and real processing
measurements; all real native application/restoration requires each-run approval.
This remediation runs no build or desktop tests and changes no implementation,
generated files, part status, git index or history. Parent independently validates
the combined branches before any implementation approval/acceptance claim.

Local documentation checks: scoped formatter check, tracked and new-file
whitespace checks, local file/anchor links, four draft statuses, 21 unique WP
criteria in their original owning parts, and exactly one audit row per C01–C20.
All pass; none substitutes for future implementation validation.

## Orchestrator verification

The orchestrator independently inspected the corrections for C01–C20 across the
three scoped specs. All findings are addressed as specification requirements and
acceptance tests, not verified runtime behavior. The table's delegated labels
record fixer ownership; they do not indicate missing spec corrections.

Keep Awake's corrections are committed as `2ddd0e3`. At this verification point,
noTunes' docs-only clarifications remain in its active worktree for the implementing
session to include; its code and index were left untouched. Part 01 remains done
and Part 02 in progress. That session must recheck the clarified cleanup contract
before accepting runtime work. All Keep Awake and Wallpapers parts remain draft.
