# noTunes spec review clarifications

Read when reconciling the consolidated reviews with ongoing implementation.

Both the first and Opus reviews examined immutable `6716f27`, not the current
implementation baseline. Part 01 and Part 02 are done. Their status, checked Part
01 criteria and original completion evidence are preserved. These edits clarify
the spec; they neither fix runtime code nor claim new implementation verification
beyond the Part 02 completion evidence.

| ID  | Review source           | Decision                                                                                                                                                                                                                                                                                                                                                                                                                           |
| --- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| C02 | First review major 1    | Distinguish immediate logical invalidation from successful acknowledged native removal/drain. A lost install acknowledgement or stalled removal returns bounded truthful errors, retaining an inert owner-thread guard for eventual old-registration-only cleanup. Successful pause/disable/shutdown remain strict. Already-accepted OS requests may finish without follow-on work or mutation of current pending/successor state. |
| C08 | Opus cross-tool major 4 | Future parts rebase and reuse catalog-driven migration/default emission and only-needed shared factory context. Later tools extend landed plumbing additively, without parallel frameworks, cross-module compile dependencies or retroactive Part 01 API requirements.                                                                                                                                                             |
| C18 | Opus minor 9            | Cocoa callbacks only enqueue/coalesce instance/refresh notifications and return with panic containment. Main-thread native actions use the owned generation-scoped path and current accepted-config commit check.                                                                                                                                                                                                                  |
| C19 | Opus minor 10           | Roadmap acknowledges separate Keep Awake/Wallpapers draft branches without claiming shipped features or linking absent local directories; approved order and the separate Cmd-Q/logout follow-up remain unchanged.                                                                                                                                                                                                                 |

Part 02's N02-RACES/N02-LIFECYCLE acceptance owns the pending runtime proof:
completed install with missing acknowledgement, stalled removal, successor
generation, then old-main-queue drainage with safe isolated cleanup and truthful
status/log errors. It must recheck the completed adapter; the historical Part 01
cleanup-retry evidence is not evidence that this new matrix ran. Runtime proof
remains pending acceptance in the other implementing session.

The consolidated 20-row report is maintained separately at
`feat/wallpapers:docs/specs/2026-11-03-wallpapers/review-findings.md`;
this local record covers only C02, C08, C18 and C19.
