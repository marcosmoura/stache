---
status: done
---

# 02 — Runtime and host integration

Read when implementing noTunes after [Part 01](01-config-native.md). **DONE.**

## Scope

Implement/export the noTunes `Module` in `src/crates/modules/notunes` over Part
01's injected safe native boundary. Add an explicit factory in
`src/crates/app/src/host.rs`, using `register_module` and
`ModuleFactories::production`; keep all blocker policy out of the app.

## Requirements

- Descriptor ID `notunes`; no declared permissions, required services,
  executables or window surfaces. Use SDK lifecycle/owned resources, not a second
  registry or module-specific SDK/config/IPC special case. Factory contributes no
  dedicated command or socket handler; existing CLI/IPC contracts remain intact.
- `configure` stages accepted config. `start` subscribes on the workspace's
  notification center before taking the running-app snapshot, then sweeps exact
  `com.apple.Music` / `com.apple.iTunes` instances with force-termination attempts
  **without opening a replacement**. Perform this on every actual start, including
  enabling/resuming/retrying, not just host bootstrap.
- Return successful start/`Running` only after installation acknowledgement and
  initial sweep processing. Install failure returns an actionable start error;
  immediately invalidate its callbacks and queued native actions logically, even
  when installation completed but its acknowledgement was lost. Successful cleanup
  acknowledgement requires actual observer removal and drain/invalidation. Failed
  or timed-out cleanup returns a truthful error; an inert guard may remain safely
  owner-thread-held for eventual removal, never freed off-main or awaited indefinitely.
  Registry Retry must remain possible without resurrecting old observation.
  Individual termination/query/open failures warn without tearing
  down healthy observation; never report a failed native operation as successful.
- A matching launch notification attempts force-termination of that instance,
  then replacement according to the current accepted config. Ignore missing/
  malformed application data and all nonmatching IDs, including case variants.
  Subscribe/sweep overlap and repeated notifications must not duplicate
  replacement opening for the same application instance; sweep-only processing
  never opens one. Deduplication state must be runtime-owned and bounded/retired,
  not permanent PID history; no launch gap between sweep and observation.
- Cocoa callbacks only enqueue/coalesce instance/refresh notifications and return,
  with panics contained; no inline query, termination, opening or blocking wait.
  Native actions run through the owned generation-scoped main-thread queue and
  recheck current accepted config at the commit point. Keep will-launch instance
  identity, subscribe-before-sweep ordering and current-target behavior unchanged.
- The replacement target is user data, never a preset. `targetApp` is an optional
  string holding either an absolute `.app` path or a bundle identifier; its
  absence means block only. No replacement path or bundle identifier is hardcoded
  in the policy, defaults, template or schema; the only preset knowledge is the
  legacy translation table in `NoTunesConfig::migrate_legacy_section`, and the
  legacy `spotify`/`tidal`/`feishin`/`none` spellings are rejected by the module
  and translated only by `config migrate`.
  A path target derives its bundle identifier natively for the already-running
  check; a bundle-identifier target resolves its path natively to open it. Do not
  relaunch an already-running replacement; an unresolvable target (missing app,
  unknown identifier, or a path that is not an application bundle) warns while
  blocking remains `Running`, not `Unavailable`. Surface query/open errors
  accurately; track pending opens so repeated callbacks cannot race duplicate
  requests. Do not discover arbitrary players or preserve the legacy cached-Tidal
  fallback.
- `reconfigure` updates the accepted target snapshot live, transactionally, without
  resubscribing or sweeping again; a rejected candidate retains old config,
  revision and runtime. Subsequent callback decisions use the new target, including
  an absent one; queued work rechecks active generation and current accepted config at
  Part 01's commit point, immediately before issuing termination/opening, and must
  not resurrect an obsolete target after acceptance. A request already accepted by
  macOS may complete afterward; do not claim cancellation or terminate a replacement
  to undo it. Stale completion may log, but cannot queue follow-on work, erase
  current pending-open bookkeeping or mutate a successor generation.
  Unchanged config is a no-op; paused reload stages the next-start snapshot only.
- `stop` invalidates the runtime generation and acknowledges observation removal/
  drain, with Part 01's cancellation-aware bounded acknowledgement failure path.
  Successful pause, disable and registry shutdown leave no live observer or queued
  termination/open side effect; already-accepted native requests may still complete
  in isolation as above. Old callbacks cannot affect successor observation, and
  failed/cancelled install actions cannot resurrect it. Late removal removes only
  its old registration, never a successor's observer. Cleanup errors/timeouts are
  surfaced in status/logs, not represented as successful pause/disable/shutdown.
  Stop is safe after partial start and repeated cleanup.
- Existing generic Enable, Pause/Resume, Retry, host status and config diagnostics
  expose the module. Permission grants/refresh are no-ops for noTunes, with no
  prompt. No new webview, Settings UI, idle polling or feature-specific timer.
  Do not expand scope to fix every Cmd-Q/logout termination route.

## Acceptance criteria and tests

IDs below are owned only by this part. Native actions use fakes by default.

- [x] N02-POLICY: Both exact blocked IDs, empty/missing/malformed and unrelated IDs
      (including case variants and every other running player); startup/enable/resume
      sweep terminates existing instances without replacement. Notification tests
      cover a path target, a bundle-identifier target, an absent target, already
      running, unresolvable target, query failure, terminate refusal and open
      failure. Ordering and truthful warning logs are asserted.
- [x] N02-RACES: Notification during subscribe/sweep, duplicate same-instance
      notification, simultaneous pending replacement, and process identity reuse
      cannot duplicate opens or terminate an unrelated instance. Stop during install,
      callback and queued action leaves inert work; a successor generation is unaffected.
      Contrast cancelled queued requests with already-issued native requests completing
      after stop; late completion cannot trigger follow-on work, erase current
      pending-open bookkeeping or mutate the successor, and never force-quits a
      replacement to undo an open. Install/remove acknowledgement failures are bounded.
      Include completed installation with missing acknowledgement, stalled removal,
      a subsequent generation, then drainage of the old main queue: old callbacks/
      actions stay inert and eventual cleanup removes only the old registration.
      Assert callback enqueue-only behavior and panic containment.
- [x] N02-CONFIG: Target changes take effect live without observer duplication;
      unchanged config stays no-op; a rejected target/reload retains last-good
      revision/config/runtime. Paused reload changes no resources and resume uses it.
      Queued actions use current config at the commit point, including an absent
      target;
      already-accepted requests may complete after reload without follow-on work or
      changes to current pending-open bookkeeping.
- [x] N02-LIFECYCLE: Disabled startup holds nothing; enable/pause/resume/disable,
      partial-install failure/rollback, Retry, repeated stop and registry shutdown
      follow SDK states. Permission grant/refresh is a no-op and requests no permission.
      For completed install with lost acknowledgement and stalled cleanup, assert
      bounded truthful errors/status/logs, owner-thread retention of the inert guard,
      successful subsequent-generation isolation and eventual old-only cleanup.
      Successful pause/disable/shutdown still require actual removal/drain; errors
      must never pretend success. This rechecks Part 01's clarified adapter contract,
      not an expansion of its historical completion evidence.
- [x] N02-HOST: Production factory/store binding, invalid initial file and later
      recovery, generic tray/status/config diagnostics, and zero automatic webviews
      are covered with injected services. No new CLI/IPC surface; existing command
      output/exit contracts and baseline proxyAudio tests remain unchanged.
- [x] N02-REAL: With explicit approval for **each run**, isolate temporary config/
      identity and verify real observer installation/removal, already-running sweep,
      later launch blocking, a configured replacement or none, and live target
      changes. Pause/resume is verified at the native stop/start path through
      enable/disable; the tray-driven click is recorded as a gap, not a pass. Warn
      that this force-quits Music/iTunes and can disrupt playback. Never run
      simultaneous legacy noTunes. Record macOS/hardware/player availability and
      skipped iTunes or unavailable-player cases as gaps, not passes. This is opt-in
      and never a default test job.

## Validation and docs

Run `cargo nextest run -p stache-notunes -p stache-macos`, affected app integration
tests, `bun run check:generated`, `bun run lint`, `bun run format:check`,
`bun run test`, `bun run build`, `bun run tauri:build`, and public Rust docs with
`RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --features stache-app/dev-tools`.
Automated tests must never force-terminate user apps, open players or prompt;
bundle success is not proof of N02-REAL. Materialize shipped behavior in
`docs/features/notunes.md`, update testing/config/index references, and keep
unimplemented roadmap modules explicitly planned. Preserve historical specs/archive.

## Completion evidence

Implemented and reviewed on 2026-11-03 on `feat/notunes`.

Automated (no real application touched):

- `cargo nextest run -p stache-notunes -p stache-macos` → 143 passed.
- `cargo nextest run -p stache-app -p stache-notunes -p stache-macos` → 249 passed.
- `bun run test` → UI browser tests plus 622 workspace tests.
- `bun run lint`, `bun run format:check`, `bun run check:generated`, `bun run build`,
  `bun run tauri:build` (6.03 MiB bundle) and `RUSTDOCFLAGS='-D warnings' cargo doc`
  all clean.
- The race regressions named in N02-RACES/N02-LIFECYCLE run against
  `FakeApplicationBackend` and `ManualExecutor`, including the completed-install
  with lost acknowledgement, stalled removal, successor generation and delayed
  old-queue drainage matrix.

Opt-in real check (separately approved per run; no simultaneous legacy noTunes):

- Environment: macOS 27.0 (26A428), MacBookPro18,3. Present: Apple Music and
  Spotify. Absent: iTunes, TIDAL, Feishin.
- Observer installation and removal, a startup sweep that terminated an
  already-running Apple Music without opening a replacement, and later launch
  blocking, all for the shipped `targetApp` forms in one run:
  - `/Applications/Spotify.app` (path) resolved to `com.spotify.client` and opened
    that path once, with the open request reported successful;
  - a live switch to `com.spotify.client` (bundle identifier) was accepted without
    reinstalling observation, blocked the next Music launch and skipped the
    already-running Spotify with an unchanged PID;
  - omitting `targetApp` blocked Music with Spotify closed and nothing launched;
  - `com.example.absent` blocked Music, logged `com.example.absent is not
    installed; not launching a replacement`, and the module stayed running.
- An earlier run, before the configurable-target change, also covered two
  generations, `enabled: false` leaving a launched Apple Music running and
  `enabled: true` starting a new generation whose sweep terminated the player.
  It used the Part 01 preset spellings, so only its lifecycle observations carry
  over.

Gaps recorded as gaps:

- Pause/resume was exercised through the injected host harness, not a real tray
  click. The native stop/start path it uses was verified for real through
  enable/disable; the tray-driven run is an automation limitation, not a pass.
- iTunes, TIDAL and Feishin are not installed here, so a path target pointing at an
  absent bundle is covered by tests only; the real path target was Spotify.
- The legacy `/Applications/Stache.app` was running during every run. Its on-disk
  config still says `notunes.enabled: true`, but Music survived a plain
  `open -a Music` with only that app running, so its blocker was inert and did not
  confound the evidence.
- The first two exploratory runs were confounded by a legacy Stache noTunes
  instance; only the later runs with that blocker disabled count as evidence.
