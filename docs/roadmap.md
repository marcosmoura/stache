# Runtime rewrite roadmap

Read when choosing the next module to specify. **Mostly planned work; shipped
modules are named as shipped.**

The foundation parts 01–06 are shipped; their specs remain historical records.
Each tool below has its own spec/branch and needs approval before implementation.
[noTunes](specs/2026-11-03-notunes/00-overview.md) is shipped: Part 01
configuration/native groundwork and Part 02 runtime integration are done.

## Planned rewrites

These specs are drafts on `main`, not shipped features. Implementation remains
one authorized part at a time on each workstream's own branch. Keep Awake is
the next rewrite target; the approved order is below, with Tiling last and blocked
until the smaller rewrites land and their shared foundations are exercised together.

| Order | Tool                                                                                                      | Sequencing rationale                                                                   |
| ----- | --------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| 1     | noTunes                                                                                                   | First approved rewrite target; narrow application-observation boundary.                |
| 2     | [Keep Awake](specs/2026-11-03-keep-awake/00-overview.md) (`feat/keep-awake`)                              | Own configuration and lifecycle, independent of Bar; Bar later consumes state.         |
| 3     | [Wallpapers](specs/2026-11-03-wallpapers/00-overview.md) (`feat/wallpapers`)                              | Independent native tool; can also proceed independently of noTunes.                    |
| 4     | [Keybindings / Caps Lock / section-key](specs/2026-11-03-keybindings/00-overview.md) (`feat/keybindings`) | Establish concrete input needs and safe adapter reuse.                                 |
| 5     | [Hold-to-Quit](specs/2026-11-03-hold-to-quit/00-overview.md) (`feat/hold-to-quit`)                        | Reuse input adapters where justified by actual requirements.                           |
| 6     | [MenuAnywhere](specs/2026-11-03-menu-anywhere/00-overview.md) (`feat/menu-anywhere`)                      | Follow input/native-menu work, without a speculative generic SDK.                      |
| 7     | [Bar](specs/2026-11-03-bar/00-overview.md) (`feat/bar`)                                                   | Status/UI can ship before tiling; never owns the native Keep Awake service.            |
| 8     | [Tiling](specs/2026-11-03-tiling/00-overview.md) (`feat/tiling`)                                          | Last; owns tiling-specific workspace display and Bar-reserved screen-area integration. |

noTunes first, independent Keep Awake and tiling last are explicit decisions.
The middle order is a dependency/risk recommendation, not a hard technical chain.
Share safe `stache-macos` adapters where justified; do not require a universal
event-tap framework before a real consumer. Keep Bar/tiling integration in the
final tiling spec so neither feature acquires a circular prerequisite.
Later merges must rebase on latest `main`, preserve additive catalog/schema/bindings/
index routes and other-module tests, and reuse earlier generic facilities rather
than introduce parallel plumbing or dependencies on unmerged module branches.

Preserve existing CLI commands/output contracts. No Settings UI is planned here;
widgets are permanently removed, not deferred. No legacy runtime is ported implicitly.

## Tray: no Reload item

The shipped tray ([feature](features/tray.md)) deliberately does not port the legacy
release-only **Reload Stache** item. Configuration now reloads per module through the
watcher and `stache reload` exists on the CLI, so restarting the whole application for a
configuration change is no longer part of the model. Enable, Pause/Resume, Retry, the
permission request, the test window and Quit cover the shipped actions, and the menu is
rebuilt from a status snapshot on the main thread rather than mutated in place.

The user asked for a release-only Reload item to be preserved anyway. It is a planned
follow-up, not shipped behavior: it uses the coordinated restart path from the
[teardown spec](specs/2026-11-03-teardown/00-overview.md), must prove the bundled `open -n`
relaunch, must stay
release-only and main-thread-bound, and must not reintroduce whole-app restart as the
config-change path.

## Coordinated teardown limitations

Every AppKit-initiated terminate path now converges on one bounded coordinator
([spec](specs/2026-11-03-teardown/00-overview.md)). Real Cmd-Q is verified. Logout, restart and
shutdown use the same route but have not been run for real; the owner accepted that as a
documented limitation. Verifying them needs a disposable login session or a spare machine.

## Release and packaging

Planned, not shipped. The [release spec](specs/2026-11-03-release/00-overview.md) covers one
version, DMG and zip artifacts, Developer ID signing and notarization, the staged switch to
`Stache` / `com.marcosmoura.stache`, install and legacy-removal rules, and release verification.
It adds no runtime feature and is independent of the tool order above. Its branch cannot merge
until signing credentials exist and the legacy app is removed and approved for removal.
