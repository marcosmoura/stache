# Runtime rewrite roadmap

Read when choosing the next module to specify. **Mostly planned work; shipped
modules are named as shipped.**

The foundation parts 01–06 are shipped; their specs remain historical records.
Each tool below gets its own future spec/branch and approval before implementation.
[noTunes](specs/2026-11-03-notunes/00-overview.md) is shipped: Part 01
configuration/native groundwork and Part 02 runtime integration are done.
Keep Awake is the next rewrite target and has a draft spec on a separate branch
(`feat/keep-awake` at `34b77d7`); Wallpapers has one at `70a2447`. Those drafts are
not shipped main features or local spec directories here.

| Order | Tool                                  | Sequencing rationale                                                                   |
| ----- | ------------------------------------- | -------------------------------------------------------------------------------------- |
| 1     | noTunes                               | First approved rewrite target; narrow application-observation boundary.                |
| 2     | Keep Awake                            | Own configuration and lifecycle, independent of Bar; Bar later consumes state.         |
| 3     | Wallpapers                            | Independent native tool; can also proceed independently of noTunes.                    |
| 4     | Keybindings / Caps Lock / section-key | Establish concrete input needs and safe adapter reuse.                                 |
| 5     | Hold-to-Quit                          | Reuse input adapters where justified by actual requirements.                           |
| 6     | MenuAnywhere                          | Follow input/native-menu work, without a speculative generic SDK.                      |
| 7     | Bar                                   | Status/UI can ship before tiling; never owns the native Keep Awake service.            |
| 8     | Tiling                                | Last; owns tiling-specific workspace display and Bar-reserved screen-area integration. |

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

## Separate foundation follow-up

The known Cmd-Q/logout coordinated-teardown gap needs its own scoped follow-up.
noTunes must honor the existing registry stop/shutdown contract; its spec does not
claim to repair every host termination path.
