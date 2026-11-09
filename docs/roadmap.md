# Runtime rewrite roadmap

Read when checking what is finished, awaiting integration, specified or still planned.
Snapshot: **2026-11-08**. Here, **finished/shipped means merged into `main`**, not a
packaged public release. Only an ad-hoc development bundle exists; no release artifact
has shipped.

## Status overview

| Status                     | Workstream                                                                           | Progress                            | Next step / notes                                                                                                                                                                       |
| -------------------------- | ------------------------------------------------------------------------------------ | ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Finished                   | [Foundation](specs/2026-11-02-foundation/00-overview.md)                             | 6/6 done on `main`                  | Includes native tray and proxyAudio (Part 06).                                                                                                                                          |
| Finished                   | [noTunes](specs/2026-11-03-notunes/00-overview.md)                                   | 2/2 done on `main`                  | Configuration/native groundwork and [owned runtime](features/notunes.md) shipped.                                                                                                       |
| Finished                   | [Coordinated teardown](specs/2026-11-03-teardown/00-overview.md)                     | 4/4 done on `main`                  | Bounded teardown, native-owner acknowledgements and coordinated restart shipped; verification limits below.                                                                             |
| Finished                   | [Keep Awake](specs/2026-11-03-keep-awake/00-overview.md)                             | 2/2 done on `main`                  | Integration verified with fakes and builds; native power/lock gate remains unverified.                                                                                                  |
| Finished                   | [Wallpapers](specs/2026-11-03-wallpapers/00-overview.md)                             | 4/4 done on `main`                  | Rotation, processing cache and CLI shipped; see [feature](features/wallpapers.md).                                                                                                      |
| Finished                   | [Keybindings / Caps Lock / section-key](specs/2026-11-03-keybindings/00-overview.md) | 5/5 done on `main`                  | Runtime and shared input service shipped; real hotkeys, Caps remap and taps unverified.                                                                                                 |
| Finished                   | [Hold-to-Quit](specs/2026-11-03-hold-to-quit/00-overview.md)                         | 6/6 done on `main`                  | Runtime, HUD, `dismiss_delay` linger and position/animation shipped; see [feature](features/hold-to-quit.md). Slide, other positions, Spaces and multiple displays unverified natively. |
| Finished                   | [MenuAnywhere](specs/2026-11-03-menu-anywhere/00-overview.md)                        | 3/3 done on `main`                  | Native menu, exact activation and runtime shipped; manual gates passed 2026-11-07/08; see [feature](features/menu-anywhere.md).                                                         |
| Finished                   | [Accessibility launch gate](specs/2026-11-07-accessibility-gate/00-overview.md)      | 5/5 done on `main`                  | App-wide Accessibility requirement, gate status, socket refusal and the frameless guidance window shipped; manual gate checks (AG-02..05-M*) not run.                                   |
| Finished                   | [Bar foundation](specs/2026-11-08-bar-foundation/00-overview.md)                     | 2/2 done on `main`                  | SDK published state with `observes`, host screen reservations and SDK display ID, plus inert `bar.toml`; no Bar runtime. Display topology is supplied by Bar 03.                        |
| Specified, not started     | [Tiling](specs/2026-11-03-tiling/00-overview.md)                                     | 9 draft parts                       | `feat/tiling`; 01–04 parallel with Bar foundation, 05–09 after it merges.                                                                                                               |
| Specified, not started     | [Bar](specs/2026-11-03-bar/00-overview.md)                                           | 6 draft parts (03–08)               | `feat/bar`; after Tiling; preserve legacy visual parity.                                                                                                                                |
| Specified, not started     | [Tiling–Bar integration](specs/2026-11-08-tiling-bar/00-overview.md)                 | 1 draft part                        | `feat/tiling-bar`; last runtime rewrite; workspace strip and end-to-end reservation gates.                                                                                              |
| Specified, not started     | [Release and packaging](specs/2026-11-03-release/00-overview.md)                     | 6 draft parts                       | `feat/release`; independent of runtime rewrite order.                                                                                                                                   |
| Planned, no dedicated spec | Tray “Reload Stache”                                                                 | Not shipped or separately specified | Release-only follow-up; coordinated restart prerequisite is shipped.                                                                                                                    |

Foundation also includes the owned [module SDK](architecture/sdk.md),
[typed configuration and reload](architecture/config.md), [socket IPC and standalone CLI](architecture/ipc.md),
and [typed UI SDK and test window](architecture/ui.md). The [tray](features/tray.md)
belongs to foundation, not a separate spec; [proxyAudio](features/proxy-audio.md) switches audio devices.

Keep Awake and Wallpapers are recorded on `main` after their approved squash merges.
Keybindings is recorded on `main` after its approved squash merge. It ships the runtime,
the bounded dispatcher and the shared input service that Hold-to-Quit and MenuAnywhere
need; the host restores a stale Next-owned Caps remap at startup. Real hotkeys, Caps remap,
taps and the measured HID-tap-versus-Carbon gate are unverified. Hold-to-Quit is recorded on
`main` after its approved squash merge, including the `dismiss_delay` linger and the
configurable HUD position and fade/slide animation. User-reported manual runs on 2026-11-07
held ⌘Q and quit the app after granting Accessibility, and confirmed the centered fade, the
release linger and the fade-out; the other native cases (slide, other positions, Spaces,
multiple displays) are unverified. The Accessibility launch gate is recorded on `main` after its approved squash merge (2026-11-08):
every module waits for Accessibility, and the guidance window explains how to grant it; its manual
native checks were not run. Bar foundation is recorded on `main` after its approved squash
merge (2026-11-08): the generic published-state and screen-reservation contracts Tiling 05 and Bar 03–08
build on, with no Bar runtime. Specified work has only documentation on its branches: a draft is not
approval. Implement one authorized part at a time.

## Runtime sequence and hard gates

noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → Bar foundation
→ Tiling → Bar → **Tiling–Bar integration LAST**.

Reordered 2026-11-08: Bar and Tiling interleave because each needs only a slice of the
other, and the merge gate plus the no-unmerged-dependency rule give each slice its own spec and branch.

The middle order is a dependency/risk recommendation, not a universal technical chain.

| Gate            | Required before proceeding                                                                                                                                                                                 |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shared input    | Keybindings lands the service needed by Hold-to-Quit and MenuAnywhere runtimes.                                                                                                                            |
| Bar indicator   | Keep Awake owns independent, lock-aware power prevention, lifecycle resources and read-only status. Bar only consumes state, never owns the native power service.                                          |
| Tiling–Bar last | Bar foundation, Tiling 01–09 and Bar 03–08 land and shared contracts are exercised. Tiling–Bar integration owns the workspace strip and end-to-end Bar reservation gates, avoiding circular prerequisites. |

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
