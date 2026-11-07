# Runtime rewrite roadmap

Read when checking what is finished, awaiting integration, specified or still planned.
Snapshot: **2026-11-04**. Here, **finished/shipped means merged into `main`**, not a
packaged public release. Only an ad-hoc development bundle exists; no release artifact
has shipped.

## Status overview

| Status                     | Workstream                                                                           | Progress                            | Next step / notes                                                                                           |
| -------------------------- | ------------------------------------------------------------------------------------ | ----------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| Finished                   | [Foundation](specs/2026-11-02-foundation/00-overview.md)                             | 6/6 done on `main`                  | Includes native tray and proxyAudio (Part 06).                                                              |
| Finished                   | [noTunes](specs/2026-11-03-notunes/00-overview.md)                                   | 2/2 done on `main`                  | Configuration/native groundwork and [owned runtime](features/notunes.md) shipped.                           |
| Finished                   | [Coordinated teardown](specs/2026-11-03-teardown/00-overview.md)                     | 4/4 done on `main`                  | Bounded teardown, native-owner acknowledgements and coordinated restart shipped; verification limits below. |
| Finished                   | [Keep Awake](specs/2026-11-03-keep-awake/00-overview.md)                             | 2/2 done on `main`                  | Integration verified with fakes and builds; native power/lock gate remains unverified.                      |
| Finished                   | [Wallpapers](specs/2026-11-03-wallpapers/00-overview.md)                             | 4/4 done on `main`                  | Rotation, processing cache and CLI shipped; see [feature](features/wallpapers.md).                          |
| Finished                   | [Keybindings / Caps Lock / section-key](specs/2026-11-03-keybindings/00-overview.md) | 5/5 done on `main`                  | Runtime and shared input service shipped; real hotkeys, Caps remap and taps unverified.                     |
| Specified, not started     | [Hold-to-Quit](specs/2026-11-03-hold-to-quit/00-overview.md)                         | 4 draft parts                       | `feat/hold-to-quit`; depends on shared input service.                                                       |
| Specified, not started     | [MenuAnywhere](specs/2026-11-03-menu-anywhere/00-overview.md)                        | 3 draft parts                       | `feat/menu-anywhere`; depends on shared input service.                                                      |
| Specified, not started     | [Bar](specs/2026-11-03-bar/00-overview.md)                                           | 8 draft parts                       | `feat/bar`; preserve legacy visual parity.                                                                  |
| Specified, not started     | [Tiling](specs/2026-11-03-tiling/00-overview.md)                                     | 10 draft parts                      | `feat/tiling`; **last runtime rewrite**.                                                                    |
| Specified, not started     | [Release and packaging](specs/2026-11-03-release/00-overview.md)                     | 6 draft parts                       | `feat/release`; independent of runtime rewrite order.                                                       |
| Planned, no dedicated spec | Tray “Reload Stache”                                                                 | Not shipped or separately specified | Release-only follow-up; coordinated restart prerequisite is shipped.                                        |

Foundation also includes the owned [module SDK](architecture/sdk.md),
[typed configuration and reload](architecture/config.md), [socket IPC and standalone CLI](architecture/ipc.md),
and [typed UI SDK and test window](architecture/ui.md). The [tray](features/tray.md)
belongs to foundation, not a separate spec; [proxyAudio](features/proxy-audio.md) switches audio devices.

Keep Awake and Wallpapers are recorded on `main` after their approved squash merges.
Keybindings is recorded on `main` after its approved squash merge. It ships the runtime,
the bounded dispatcher and the shared input service that Hold-to-Quit and MenuAnywhere
need; the host restores a stale Next-owned Caps remap at startup. Real hotkeys, Caps remap,
taps and the measured HID-tap-versus-Carbon gate are unverified. Hold-to-Quit is next and
needs its own approval. Specified work has only documentation on its branches: a draft is not
approval. Implement one authorized part at a time.

## Runtime sequence and hard gates

noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → Bar
→ **Tiling LAST**.

The middle order is a dependency/risk recommendation, not a universal technical chain.

| Gate          | Required before proceeding                                                                                                                                             |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shared input  | Keybindings lands the service needed by Hold-to-Quit and MenuAnywhere runtimes.                                                                                        |
| Bar indicator | Keep Awake owns independent, lock-aware power prevention, lifecycle resources and read-only status. Bar only consumes state, never owns the native power service.      |
| Tiling last   | All smaller rewrites land and shared contracts are exercised. Tiling owns workspace display and Bar-reserved screen-area integration, avoiding circular prerequisites. |

Later branches must integrate latest `main`, preserve additive catalog/schema/bindings/
index routes and other-module tests, and reuse merged generic facilities. Share safe
`stache-macos` adapters for concrete consumers; do not introduce parallel plumbing,
dependencies on unmerged module branches or a speculative universal event-tap framework.

## Verification and release gates

| Area                   | Evidence / remaining gate                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Teardown               | Real Cmd-Q, SIGTERM and harness direct-spawn relaunch verified. Real logout, system restart and shutdown **not verified**; owner accepted this limitation. See [gate evidence](specs/2026-11-03-teardown/04-verification-gates.md#gate-evidence).                                                                                                                                                                                                                                                        |
| Keep Awake             | Integration reviews and gates passed: lint, formatting, 922 native tests, 24 browser tests, 6 script tests, UI coverage, deterministic generated checks, builds, warnings-denied rustdoc and workspace doctests. Squash merge into `main` approved. The ad-hoc signed app was built, not launched or notarized. Native power/lock checks remain **unverified**; fakes/builds do not prove them. Each native run requires explicit approval; screen-lock interaction is manual and separately authorized. |
| Signing / notarization | [Release Part 03](specs/2026-11-03-release/03-signing-notarization.md) is blocked on signing/notary credentials.                                                                                                                                                                                                                                                                                                                                                                                         |
| Release identity       | [Release Part 04](specs/2026-11-03-release/04-identity-switch.md) requires legacy removal and recorded approval.                                                                                                                                                                                                                                                                                                                                                                                         |
| Release merge          | All six release parts must be done. Ad-hoc builds do not satisfy signing or publication gates.                                                                                                                                                                                                                                                                                                                                                                                                           |
| Tray “Reload Stache”   | Prove bundled `open -n` relaunch; keep release-only, main-thread-bound and surface restart failure. A non-`Clean` teardown does not relaunch and leaves no app running.                                                                                                                                                                                                                                                                                                                                  |

The release draft plans arm64/macOS 26+ DMG and zip artifacts, Developer ID signing,
notarization, the staged release identity `Stache` / `com.marcosmoura.stache` with isolated
development identity, CLI access, install/migration/removal rules, a Homebrew cask and
release verification. It adds no runtime feature.

Tray Reload must never make whole-app restart the configuration-change path. The per-module
configuration watcher and `stache reload` remain the normal reload model.

## Standing boundaries

Preserve existing CLI commands and output contracts. No Settings UI is planned;
widgets are permanently removed, not deferred. No legacy runtime is ported implicitly.
Keep legacy/new identities isolated and never terminate by shared executable name.
Follow the [approval and merge workflow](workflow.md): unfinished parts block merging.
