---
status: done
---

# 01 — Configuration and native boundary

Read when implementing the independently testable prerequisite. **DONE.**
Configuration/catalog support and the native boundary shipped here; the blocker
runtime followed in [Part 02](02-runtime-integration.md), which also replaced the
preset enum described below.

## Scope

Create `src/crates/modules/notunes` (`stache-notunes`) with authoritative Rust
configuration, but no automatic blocker runtime/factory registration. Add a narrow,
safe, injectable application-observation/query/termination/opening boundary in
`src/crates/macos`; export it through `src/crates/macos/src/lib.rs`.

## Requirements

> Superseded in Part 02: the preset enum, its default, the table below and the
> preset clause of `N01-CFG` record the shape Part 01 shipped and its tests
> covered; they are history, not current behavior. The shipped configuration now
> takes an optional `targetApp` holding an absolute `.app` path or a bundle
> identifier, with no preset list, and Part 02's criteria are authoritative; see
> [Part 02](02-runtime-integration.md).

- Define `NoTunesConfig` / `TargetMusicApp`, `ModuleConfig`, serde camelCase,
  schemars and Specta derives. ID/file: `notunes` / `notunes.jsonc`.
  Defaults: `enabled: false`, `targetApp: "spotify"`; accepted enum strings exactly
  `tidal`, `spotify`, `feishin`, `none`. Unknown keys retain SDK warnings;
  invalid enum values reject rather than silently selecting a preset.
- Preset mappings are authoritative module data:

  | Value   | Path                        | Bundle ID             |
  | ------- | --------------------------- | --------------------- |
  | tidal   | `/Applications/TIDAL.app`   | `com.tidal.desktop`   |
  | spotify | `/Applications/Spotify.app` | `com.spotify.client`  |
  | feishin | `/Applications/Feishin.app` | `org.jeffvli.feishin` |
  | none    | No replacement              | No replacement        |

- Wire workspace/app dependencies, `app/src/catalog.rs::catalog` and
  `app/src/bindings.rs::builder`; generate schema/template/bindings from Rust,
  never hand-maintained mirrors. Catalog-only support must not report an active
  module, create a webview, install observation or start players.
- In `cli/src/migrate.rs`, replace noTunes' unported verbatim-copy path with
  catalog-driven typed validation; generalize the registered-module path rather
  than add tool-specific SDK behavior. Emit its default template when absent.
  Preserve source precedence/immutability, preflight collisions, `--force`, path
  identity protections, reporting, and every unrelated command/output contract.

### Enforceable native ownership

- `Module` is `Send` and lifecycle runs off-main. Provide a safe module-facing
  handle with main-thread-owned native state; observer guards/retained Cocoa
  objects never cross threads if `!Send`. Existing `activation.rs::ActivationObserver`
  is a `!Send` ownership reference, not a guard to put in the module.
- Install/remove observation and appropriate AppKit operations on main through
  the existing dispatcher. Off-main lifecycle waits for explicit installation/
  cleanup acknowledgement with a cancellation-aware, bounded failure path for
  dispatch rejection, unavailable run loop or late/missing acknowledgement; neither
  start nor stop may hang indefinitely. Document and test the bound against SDK/host
  behavior rather than inventing an arbitrary timeout. The main thread must never
  wait on a dependent worker. Account for partial install and stop/install races;
  late main-thread actions after failed/cancelled install check generation first
  and roll back exactly that generation or no-op, never resurrecting observation.
- Failed/cancelled installation and stop immediately invalidate the generation,
  rendering its callbacks and queued actions logically inert, even if installation
  completed but its acknowledgement was lost. Successful cleanup acknowledgement
  means actual observer removal and callback/action drain or invalidation. If removal
  fails or times out on a stalled main queue, return a truthful error; an inert native
  guard may remain safely owner-thread-held for eventual removal. Never free it
  off-main, wait indefinitely or hide cleanup failure. Late removal owns only its old
  registration and cannot remove a successor's observer. Cleanup/drop is owned and
  idempotent after partial failure.
- A stop invalidates generation before queued work can act.
  The native side-effect commit point is the active-generation/current-accepted-config
  check immediately before issuing termination/opening. Queued work can be cancelled;
  requests already accepted by macOS may complete after reconfigure/stop and cannot
  be cancelled or undone by terminating the replacement app. Stale completion may
  log its result, but cannot queue follow-on work, erase current pending-open
  bookkeeping or mutate successor state.
- Expose safe application-instance identity (or retained main-owned target with
  revalidation), not bare PID/name termination. PID reuse must never kill an
  unrelated process. Query/terminate/open results distinguish failure from request
  acceptance; force-termination request acceptance is not confirmed process exit.
- Supply deterministic fakes. Objective-C callbacks only enqueue/coalesce owned
  instance/refresh notifications and return, with panics contained. Never query,
  terminate, open or wait for blocking work inline in a Cocoa callback. Authorized
  main-thread native actions use the owned generation-scoped queue and the current
  accepted-config commit check above. Preserve will-launch instance identity and
  subscribe-before-sweep ordering. Do not use a lossy generic EventBus as the sole
  enforcement path. No blanket raw-pointer
  unsafe `Send`/`Sync`, detached workers or global noTunes singleton.

### Binding evidence and implementation gate

Installed `objc2-app-kit 0.3.2` generated `NSWorkspace.rs` contains
`sharedWorkspace`, `notificationCenter`, the will-launch notification/application
key, and `openApplicationAtURL_configuration_completionHandler` (gated by
`NSRunningApplication`/`block2`). `NSRunningApplication.rs` contains workspace
`runningApplications`, instance `bundleIdentifier`, `isTerminated`, and
`forceTerminate -> bool`; opening uses completion reporting, not an invented bool.
Current macOS manifest enables only `NSApplication` in AppKit: add only required
binding features. Part 01 must compile-prove the chosen dispatcher/handle ownership
and cleanup design against installed bindings before Part 02; these are constraints,
not a promised broad SDK service or invented Rust API signatures.

## Acceptance criteria and tests

IDs below are owned only by this part; use temporary roots and injected backends.

- [x] N01-CFG: Defaults, empty-object defaults, all preset mappings, serde roundtrip,
      invalid enums and unknown-key warnings match requirements; schema/template/Specta
      output is deterministic and generated checks pass. The preset half of this
      criterion is historical — see the superseded note above.
- [x] N01-CATALOG: Init/path/schema/migrate cover typed valid/invalid/absent noTunes,
      source identity, force/collision atomic preflight and existing CLI output/exit
      invariants. Catalog-only state stays unbound/inactive; no native side effects.
- [x] N01-OWN: Fake dispatcher proves main ownership, acknowledged install/remove,
      cancellation-aware bounded failure for rejected dispatch/unavailable run loop/
      late or missing acknowledgements, partial-install rollback, and stop before
      install acknowledgement. Late install actions cannot resurrect observation;
      no main-thread wait depends on a worker. Prove queued callback/open/terminate
      cancellation, successor isolation and idempotent cleanup. Compile-check the
      module-facing handle's required `Send` bounds without moving the native
      observer; prove stale identity/PID reuse is refused.
- [x] N01-RESULT: Native fake errors and termination refusal/open completion failure
      are surfaced accurately, never claimed as success; callback panic is contained.
      Distinguish force-termination request acceptance from confirmed exit and
      cancelable queued work from irreversible already-issued requests; late
      completion can log but cannot trigger follow-on work or alter current
      pending-open bookkeeping/successor state.

## Validation and docs

Run `cargo nextest run -p stache-notunes -p stache-macos`, affected app/CLI catalog
tests, `bun run check:generated`, `bun run lint`, `bun run format:check`,
`bun run test`, `bun run build`, `bun run tauri:build`; build public Rust docs with
`RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --features stache-app/dev-tools`.
Update configuration docs for catalog support, explicitly leaving runtime planned.
Automated tests never terminate user apps, open players or prompt. Real native
behavior is deferred to Part 02's explicitly approved opt-in checks.

## Completion evidence

The evidence below and checked criteria above record the original Part 01 baseline.
The cleanup/callback clarification from spec review does not claim new tests ran
or code was fixed. Part 02 acceptance must recheck the completed adapter against
this clarified contract, including completed install with missing acknowledgement
and stalled removal; that runtime proof remains pending in the implementing session.

Independently reviewed and validated on 2026-11-03:

- Native/config tests: 74 passed; affected app/CLI tests: 165 passed.
- `bun run test`: 547 Rust tests and 24 browser tests passed.
- Generated checks, lint, formatting, frontend build, Tauri bundle build and
  public Rust docs with warnings denied passed. The bundle is ad-hoc signed;
  notarization was skipped because credentials were unavailable.
- Fake regressions cover cancellation, late acknowledgement, cleanup retry,
  instance/handle isolation, pre-issuance stop/reconfigure, already-issued work,
  stale completions, query failures and synchronous callback reentrancy.
- The native acknowledgement budget is two seconds per hop, below the host
  smoke harness's five-second main-hop check; install cancellation is checked
  every ten milliseconds. This is not a global host shutdown deadline.
- AppKit ownership and bindings are compile-verified. No real observation,
  application termination or player launch was exercised; those remain Part 02
  opt-in verification, requiring separate approval for each run.
