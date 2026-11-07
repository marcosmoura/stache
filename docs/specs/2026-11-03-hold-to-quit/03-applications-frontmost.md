---
status: done
---

# 03 — Applications adapter: frontmost and graceful terminate

Read when extending the shipped applications adapter, after explicit approval. Baseline: `main` `ad34b46`, including noTunes `ccae030`; these extensions do not ship yet.

## Scope

Additive changes to `src/crates/macos/src/applications.rs`, `applications/{testing,tests}.rs`, exports and only required binding features. Rebase the old worktree first. `ApplicationObservation::terminate` currently means **force** termination; preserve its signature/behavior and noTunes callers/tests. Add a separately named graceful operation, not a silent mode change or competing application-control adapter. All retained instances stay in the existing main-owner maps.

## Requirements

1. Add workspace activation observation and `frontmost()` on the same exact-instance machinery without changing will-launch observation. Pure `FrontmostSnapshot` data includes opaque `ApplicationId`, optional bundle/name, exact current-process flag, owner generation and monotonically changing activation epoch/confirmation state. Identity remains retained `NSRunningApplication` equality, never PID/bundle/name. Compare self against `NSRunningApplication.currentApplication`, not a shared executable name.
2. At activation ingestion invalidate the published frontmost confirmation/epoch immediately through nonblocking plain state before deferred refresh. Native callbacks contain panics and only intern minimal native identity, invalidate and enqueue/coalesce refresh; no policy, query/wait or terminate inline. Deferred main refresh queries the actual current frontmost and rechecks owner generation/epoch before publishing. A→B→A invalidates an A press even if final identity equals A; dropped/failed delivery becomes unconfirmed, never an indefinitely trusted old identity. Install observation before initial query and epoch-check its result to avoid overwriting newer activation.
3. Provide a prepublished atomic/small nonblocking read snapshot for the shared input path. Main publishes identity, Finder/self classification and confirmation coherently. No callback calls `frontmost`, `bundle_identifier`, `is_current`, `accepted` or any other blocking/native query. Unknown metadata needed for an exemption, missing app or invalidated generation is unconfirmed. Part 04 adds config/HUD readiness to its own coherent decision snapshot.
4. Before showing a required HUD and again before graceful native issuance, main queries and compares the current frontmost with the exact captured instance; it must still run and be neither Finder nor self. A changed/missing/unconfirmed result cancels; never retarget to the app now frontmost. Refresh notification epochs cannot be substituted for this native preflight. Publication cannot atomically track an OS focus switch preceding notification ingestion: a down can be suppressed in that unobservable interval, but revalidation prevents showing/quitting a now-exempt or different app. Document that limitation; never knowingly suppress using a stale cache or promise it is eliminated by polling.
5. Add a separate graceful request using `terminate` only. Reuse `IssuancePermit::issue` at the actual send with accepted handle/generation/revision and request acknowledgement deadline. Add narrow typed per-press authorization for immediate cancellation, matching HUD-show success, press identity and accepted publication; revalidate it at the issuance boundary, not only when enqueueing. Main exact-identity/frontmost preflight occurs immediately before that boundary. No arbitrary module callback under the permit, no work/dispatch/wait except the immediate send. Input release/cancel and issuance have an explicit one-shot commit ordering: cancellation committed first prevents send; a send already committed cannot be undone. Part 04 supplies/invalidate tokens; part 03 owns native enforcement/fake barriers.
6. Keep outcomes truthful: `Requested` means native request accepted, not exited; `Refused` means a confirmed false return, not a generic failure/no-side-effect claim. `NotRunning`/`Stale` prove no send for that operation. Dispatch rejection/pre-send abandoned deadline can prove no send; timeout after issuance is uncertain, never relabeled `Refused`. Do not automatically retry or force. Existing terminate has no late-result callback; expose a typed tagged diagnostic completion only if needed, otherwise log uncertainty without inventing one. A late result never authorizes successor actions.
7. Add best-effort `display_for(id)`/display hint from public APIs, on main after exact-identity/current-frontmost validation. Compile-gate `CGWindowListCopyWindowInfo` ownership, owner-PID/layer/bounds extraction and `NSScreenNumber` mapping. Select front-to-back on-screen layer-0 window geometry and screen with largest intersection; normalize coordinate spaces in pure tested code. PID is only a nondestructive placement hint. Do not read titles, promise unavailable AX geometry or prompt for Screen Recording. Missing/filtered geometry is explicit `None` plus fallback source diagnostics, not a guessed app display. If public geometry is unavailable, retain part 02's fallback and report it.
8. New observer/query/cleanup operations follow shipped budgets: 2 s per-hop acknowledgement, acquisition observes cancellation within 10 ms subject to scheduling, immediate logical invalidation before native cleanup, retained inert owned resources on error/timeout. No waits for worker on main. Compile-prove notifications/removal, current-app comparison, graceful send and public display APIs before runtime wiring; compile-only tests have no native side effects.

## Acceptance criteria

- [x] HQ-03-01: Exact frontmost/self identity, epoch invalidation and nonblocking pure snapshots survive install/query overlap, deferred delivery, drops and A→B→A; will-launch/noTunes behavior remains compatible.
- [x] HQ-03-02: Separate graceful issuance calls only `terminate`, under existing permit plus current per-press authorization, with truthful confirmed/uncertain results. Force API and noTunes regressions pass unchanged.
- [x] HQ-03-03: Main preflights before authorized show and actual send reject mismatched, exempt, missing or dead instances, including reused PID and same-bundle replacement; notification-race limits are documented.
- [x] HQ-03-04: Public display selection, coordinate conversion, nil/error fallback and owner-thread cleanup are compile-/fake-proven without permissions or assumed AX data; limitations are recorded.

## Tests

- Extend `FakeApplicationBackend` with exact frontmost/current-process instances, activation epochs, unknown metadata, delayed/dropped refresh and same-PID/different-instance fixtures.
- Query versus install activation; deferred stale activation after stop; A→B→A; main preflight sees Finder/self/B before notification refresh; cannot overwrite a successor's snapshot.
- Distinct graceful/force recording, all `TerminateOutcome`s, dispatch rejection and request abandonment. Barriers before preflight, before permit/press commit, and after committed send cover release/cancel/reconfigure/stop. Timeout then late native return remains uncertain; no second send or force fallback.
- Display mapping fixtures cover coordinate origins/scales, multiple windows, detached display, no geometry and permission-filtered results. Observer removal blocked on main times out with inert retained ownership, then cleans up exactly once.
- Manual, separately approved each run: ignored read-only harness prints frontmost/display hints while switching two apps/displays. No app termination or permission prompt. Record actual fallback/Space limitations and unexecuted cases.

## Notes

After implementation: `cargo nextest run -p stache-macos -p stache-notunes`, `bun run lint`, `bun run format:check`, `bun run test`, and `RUSTDOCFLAGS='-D warnings' cargo doc -p stache-macos --no-deps`. Update [architecture](../../architecture/overview.md) for additive adapter capabilities only; do not claim a Hold-to-Quit runtime.
