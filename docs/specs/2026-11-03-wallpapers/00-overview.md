---
branch: feat/wallpapers
---

# Wallpapers

Read when planning or implementing Wallpapers. Everything here is planned, not shipped.

## Goal

Port wallpaper sources, image processing, rotation and the existing CLI into
Stache Next, with native per-display application and safe lifecycle ownership.
Base: `main` at `e0b4648`. Implementation requires approval before each part.

## Non-goals

No Settings UI, widgets, windows, commands beyond the legacy wallpaper tree,
selection modes or scheduling features beyond the existing rotation interval
(an owned wakeable timer is required). No no-repeat selection, private APIs or
AppleScript fallback. No dependency on unimplemented
noTunes or Keep Awake, and no legacy runtime/global-manager import. Other tools
get separate branches/specs; tiling comes last in the overall planning order.

## Decisions

- The new `stache-wallpapers` crate lives at `src/crates/modules/wallpapers`.
  Its only workspace dependencies are SDK and macOS; third-party dependencies
  must serve actual needs. Module code forbids unsafe; native calls and geometry
  belong behind an injected safe adapter in `stache-macos`.
- Explicit `list`, `generate-all` and `set` work with `enabled: false`. They
  never enable the module, start rotation or launch a desktop host.
- `set` uses a reachable host only for the same canonical config directory,
  enforced again by the server. A different directory is an actionable refusal.
  Local fallback is allowed only for host absence established before sending
  any mutation; uncertain sends/responses never trigger a second application.
- Host requests use a typed accepted-config/revision watch/read handle, not current disk
  or a module-local cache. Part 01 adds this narrow generic SDK capability:
  today's `ConfigStore::initial` rereads disk, and paused/disabled reloads do not
  call `Module::configure`. The same published cell supplies coherent reads and
  bounded revision-change notification. Standalone commands use current disk instead.
- Reconfigure preflight precedes SDK accepted-state publication. Keep candidates
  staged until their matching accepted snapshot is published; never combine an
  old accepted config with a new pool, cursor or schedule. Part 03 proves this
  publication barrier and a finite cold-request processing/response budget before
  Part 04 adopts host routing; five-second transport defaults are not assumed
  sufficient for multi-display resize and Best PNG encoding.
- Config paths are relative to the directory containing `wallpapers.jsonc`;
  explicit CLI file paths are relative to invocation CWD and sent absolute.
  Migration warns when historical CWD-dependent meaning cannot be inferred.
- A real start (including enable/resume) validates sources, establishes owned
  coordination and enqueues initial selection as the first asynchronous job,
  returning Running without waiting for decode/encode/native effects. Running
  does not guarantee an image was applied. Initial application failure logs a
  typed error and keeps Running; interval zero has one initial job and no timer
  or automatic retry loop. Accepted source/list/effects reloads leave the visible image
  alone until the next scheduled or explicit change. Paused reloads stage.
  Stop, disable and pause leave the current desktop image without restoration.
- One factory-owned host-lifetime coordinator serializes timer and explicit
  application independently of rotation's runtime WorkerGroup. Generic factory
  begin-shutdown wakes all host waits before IPC connection joins; registry
  reverse-stop joins rotation before factory finish/drain/join and service drop.
  Factory/native owners survive Module.stop; disabled registration is inert,
  with processing workers/capacity lazy on request or active rotation. Explicit success
  resets the next interval only when rotation is running; disabled/paused set
  leaves lifecycle unchanged. Selection state is coherent per native display.
- Prepare every target output before the first native set. Native partial
  failure reports actual applied targets, not success or fictitious rollback.
  Cancellation cannot undo an OS request already issued; stale completions may
  not alter successor state or issue further requests.
- Replace the legacy all-screen setter with the public per-display native API:
  no Automation prompt or required Accessibility permission. Space coverage is
  an empirical verification item, never an unproved “every Space” promise.
- Shared cache operations coordinate across host, CLI and `cache clear` using
  SDK/AppPaths leases outside the deletable cache root. No process-global
  `OnceLock`, speculative eviction knobs or new persistent-storage scheme.
- Retain CLI flags, text and exit contracts, including exit zero for completed
  `generate-all` batches with per-image failures. Approved corrections are
  disabled/offline use, actual explicit file paths, host serialization/timer
  reset, per-display sizing/state and terminal-only ANSI. Also intentional:
  valid-empty list output is reachable, and both/neither argument checks precede
  manager/source setup (their legacy text/exit remains unchanged).

## Integration order

Roadmap order is noTunes, Keep Awake, then Wallpapers. noTunes first lands
catalog-driven typed migration/default templates; the first factory-context
consumer (Keep Awake in that order) introduces one narrow generic context.
Before implementing each later part, rebase onto current main and reuse those
landed facilities, extending only for the justified accepted-state watch and
host cancellation here. Do not build parallel frameworks/tool dependencies or
depend on unmerged sibling code. Catalog, bindings and index rows are additive
and must preserve earlier surfaces. Whichever merge finds the roadmap stale
updates it when present; this branch does not claim sibling drafts are shipped.

The [consolidated review audit](review-findings.md) records both reviews and
spec corrections, not implementation acceptance.

## Parts

Each part builds, tests, documents and commits independently after its
dependencies; none is a separate tests/docs-only tail. All remain draft.

| Part                                | Summary                                                            | Status |
| ----------------------------------- | ------------------------------------------------------------------ | ------ |
| [01](01-config-sources.md)          | Config/catalog/migration, source pool and accepted watch           | draft  |
| [02](02-native-processing-cache.md) | Native compile proof, processing and coordinated cache             | draft  |
| [03](03-runtime.md)                 | Lifecycle, apply coordinator, production factory and typed set IPC | draft  |
| [04](04-cli-integration.md)         | Existing CLI contribution, safe routing and integration contracts  | draft  |

Acceptance ownership: Part 01 owns WP01-01–WP01-05; Part 02 owns WP02-01–WP02-05;
Part 03 owns WP03-01–WP03-05; Part 04 owns WP04-01–WP04-06. Each ID labels one
checkbox in its owning part, not duplicate criteria in this overview.

## Evidence and checkpoints

Authoritative legacy behavior is in
`_legacy/app/native/src/modules/wallpaper/{manager,processing,macos}.rs`,
`cli/commands/{wallpaper,types}.rs` and `config/types/wallpaper.rs` beneath the
same native source root. The archived feature page contains outdated thread,
cache and all-screen claims; leave it unchanged. Capture output fixtures from
these sources, never by running legacy wallpaper actions.

Installed `objc2-app-kit 0.3.2` declares `NSScreen` main-thread-only, with
`screens(MainThreadMarker)`; `NSWorkspace`'s desktop setter is an unsafe binding
whose options dictionary needs a safety proof. Part 02 must compile-prove its
wrapper, stable display identity, feature gates and standalone dispatch before
building on them. This document does not invent a shipped adapter API.

## Affected docs and validation

The [planned feature reference](../../features/wallpapers.md) and
[index](../../index.md) link this proposal. Each implementation part materializes
only its shipped behavior in the feature reference and task-relevant SDK/config/
IPC/new-module docs; do not rewrite archive or foundation specs.

Each part runs its crate nextest suites plus `bun run lint`,
`bun run format:check`, `bun run test`, `bun run check:generated` and Rustdoc
with warnings denied. Regenerate schemas/bindings through the shared Rust
generators when their inputs change. Final integration also runs
`bun run tauri:build`. All automatic tests use fakes and disposable paths;
they never change a real desktop. Real desktop checks require explicit approval
for **each run** and report untested displays/Spaces as gaps.
