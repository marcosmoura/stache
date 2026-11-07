---
branch: feat/menu-anywhere
---

# MenuAnywhere rewrite

Read when planning the MenuAnywhere module. This is a draft, not shipped behavior or permission to implement.

## Goal

Rebuild MenuAnywhere as `stache-menu-anywhere`. A configured mouse button plus an exact set of modifiers shows the frontmost application's menu bar at the click location, rebuilt from Accessibility. The click is consumed by the shared SDK input-tap service, with no private tap. The rewrite keeps the legacy configuration and trigger semantics and fixes the documented legacy defects. Get approval before implementing each part.

## Non-goals

- No private APIs, no menu caching, search, favourites or history.
- No Settings UI, window, webview, CLI command or socket endpoint.
- No private event tap and no MenuAnywhere-owned input framework.
- No fix for the separate Cmd-Q/logout coordinated-teardown gap.
- No change to proxyAudio, noTunes, Keybindings or Hold-to-Quit behavior.

## Decisions

1. Crate `src/crates/modules/menu-anywhere`, package `stache-menu-anywhere`, catalog ID and file `menuAnywhere` / `menuAnywhere.jsonc`. Keys, defaults and enum spellings stay as they are: `enabled` (`false`), `modifiers` (`["control","command"]`; `control|option|command|shift`), `mouseButton` (`rightClick|middleClick`, default `rightClick`). Modifier matching stays exact; empty modifiers remain valid and capture bare clicks, with an explicit caution. Migration reuses the shipped catalog-driven typed migration.
2. Input arrives only through the shared input-tap service planned in the [Keybindings spec](../2026-11-03-keybindings/00-overview.md) (`feat/keybindings`, Parts 02/05). The host provides it through `ModuleContext`. It takes a declarative filter and a pure decision callback, and has one owner tap in `stache-macos`. Priority is Hold-to-Quit > MenuAnywhere > Keybindings. The service reports overlaps and re-enables a disabled tap on its own. The callback decides suppress or pass from precomputed config and enqueues one request. Part 03 is **blocked** until the service, finite subscription gates and generic user-prompt-only metadata are on `main`. MenuAnywhere inherits them: permission checks only poll; only an explicit tray request may prompt. Current main's automatic-prompt policy is not assumed changed. No duplicate policy hook or tap owner.
3. Legacy bugs are fixed and config semantics kept. The popup is placed on the screen that contains the click. AX elements are retained for the life of the menu. A trigger while a menu is being built or shown is swallowed and dropped (`Coalesced`). Missing Accessibility or input service is reported truthfully.
4. New `stache-macos` adapters (Part 02):
   - a dedicated-owner-thread AX menu-bar reader that returns a `Send` pure-data snapshot under a bounded budget and explicit timeout on each element;
   - a main-thread `NSMenu` presenter carrying only opaque tokens; retained AX elements stay in the AX owner's per-presentation map;
   - a minimal additive exact retained-instance activation operation in the existing noTunes `applications` adapter, reusing its identity/permit/cancellation/acknowledgement machinery and Hold-to-Quit Part 03's frontmost extension when landed, then invocation that sends `AXPress` on its AX owner. Hold-to-Quit does not supply activation. No competing application map or blanket unsafe `Send`/`Sync`.
5. Tests use fakes only. Native checks are manual and need approval for each run.
6. A matching click's presentation is independent of config. The filter revision that matched decides the swallow. Stop, disable and pause invalidate any queued or open presentation. A reconfigure that stays enabled leaves an in-flight presentation alone (Part 03).

## Integration order

Roadmap order: noTunes → Keep Awake → Wallpapers → Keybindings → Hold-to-Quit → MenuAnywhere → Bar → Tiling. Current `main` `ad34b46` includes noTunes `ccae030`: its runtime, catalog migration (`ModuleConfig::migrate_legacy_section`, `ConfigCatalog::rewrite_legacy`) and `applications` identity/issuance adapter ship. This worktree's `e0b4648` checkout predates them; source evidence below was checked with `git show main:…`. Do not implement against the obsolete checkout.

Before each part, rebase onto current `main`. [Keep Awake](../2026-11-03-keep-awake/00-overview.md) (`feat/keep-awake`) and [Wallpapers](../2026-11-03-wallpapers/00-overview.md) (`feat/wallpapers`) are drafts on `main`, not shipped dependencies. [Keybindings](../2026-11-03-keybindings/00-overview.md) and [Hold-to-Quit](../2026-11-03-hold-to-quit/00-overview.md) also have drafts on `main`; implementation remains on `feat/keybindings` and `feat/hold-to-quit`. Reuse any landed generic factory context and shared input service. Part 02 depends on [Hold-to-Quit Part 03](../2026-11-03-hold-to-quit/03-applications-frontmost.md) for frontmost observation/identity; that part adds graceful termination, not activation. MenuAnywhere Part 02 owns exact-instance activation in the existing `applications` adapter. Add catalog, bindings, index and roadmap entries additively; preserve proxyAudio, noTunes and later consumers. If a required capability is still absent, block the affected part rather than fork ownership or assume draft APIs compile.

## Parts

| Part                         | Summary                                                                  | Status |
| ---------------------------- | ------------------------------------------------------------------------ | ------ |
| [01](01-config-migration.md) | Typed config, catalog, schema, bindings and typed migration              | done   |
| [02](02-native-menu.md)      | AX snapshot reader, NSMenu presenter, exact-instance invocation; gate    | done   |
| [03](03-runtime.md)          | Module runtime on the shared input service, factory, status and teardown | done   |

Part 02 depends on Part 01 only for crate placement and the landed Hold-to-Quit Part 03 frontmost extension; Part 02 itself supplies and tests exact-instance activation. Part 03 depends on 01, 02 and the landed input service with its prompt-policy/subscription gates. Each part builds, tests and commits on its own. Unfinished parts block merge.

## Source evidence

Legacy paths are relative to `_legacy/`.

- Feature doc `docs/features/menu-anywhere.md:28-34` (data flow), `:42` (Accessibility cached in a `OnceLock`), `:48-52` (exact match, no `init` guard, pause flips tap). Module paths below are under `app/native/src/`.
- Config `app/native/src/config/types/menu_anywhere.rs:8-31` (enums), `:37-61` (defaults, no validation), `:76-87` (mask). Sample `docs/samples/config.jsonc:146-160`.
- `modules/menu_anywhere/mod.rs:35-59`: `init` has no re-entry guard and sets `IS_RUNNING` even when the spawned tap fails. `:64-83`: status mapping.
- `modules/menu_anywhere/event_monitor.rs`:
  - `:88-123`: HID head tap on a private run-loop thread; a null tap returns silently.
  - `:160`: disabled-by-timeout and disabled-by-user-input events are passed through and never re-enabled.
  - `:169`: exact `0x1E0000` equality. `:177`: swallow.
  - `:235-236`, `:278-292`: Y is flipped against `NSScreen.mainScreen` (the key-window screen) with a 1080 fallback.
  - `:80-83`, `:184-186`: blanket `unsafe impl Send/Sync`.
  - `:98`: the mask `1 << otherMouseDown` matches any non-left/right button.
- `modules/menu_anywhere/menu_builder.rs`:
  - `:215-264`: PID-based frontmost read on every trigger, no messaging timeout or budget.
  - `:283`: root Apple menu skipped. `:310`: empty title or `AXSeparator` becomes a separator. `:329`: enabled defaults to true. `:333-342`: ✓ → on, • → mixed. `:345-354`: lowercased key equivalent. `:356-367`: first `AXMenu` child becomes a submenu. `:370-373`: action only on enabled leaves. `:375-378`: first root item bold. `:279`: `setAutoenablesItems: NO`.
  - `:199`: an unbalanced `CFRetain` (leaked per trigger). `:478-487`: non-owning `NSValue valueWithPointer:`, so element lifetime depends on that leak.
  - `:400-402`: AX modifier bit 3 (`kAXMenuItemModifierNoCommand` in `HIServices AXAttributeConstants.h:995-999`) is mapped to Command.
  - `:529-536`, `:549-567`: selecting a popup item activates whatever app is frontmost at selection time and ignores the `AXPress` result, rather than retaining the menu-read target.
- `app/native/src/platform/accessibility.rs:35,49-57`: trust check and prompt.
- Next: `src/crates/macos/src/accessibility.rs:28-56` (trust and permission provider only, no `AXUIElement`), `src/crates/cli/src/migrate.rs:21-29` (`menuAnywhere` is copied as unported), `src/crates/app/src/tray.rs:146-156` (Request Accessibility row).
- Current `main` `src/crates/macos/src/applications.rs`: `ApplicationId`, `ApplicationObservation`, `IssuancePermit`, `InstanceStore`; retained-instance identity, 2 s per-hop `ACKNOWLEDGEMENT_BUDGET`, 10 ms `CANCELLATION_QUANTUM`, immediate logical invalidation and acknowledged removal. Frontmost/activation methods are not shipped at this baseline.
- Current `main` `src/crates/sdk/src/config/catalog.rs`, `src/crates/sdk/src/module.rs`, `src/crates/cli/src/migrate.rs`: generic registered-section rewrite/validation/template migration already ships. `menuAnywhere` is still unported until Part 01.
- Current `main` `src/crates/app/src/host.rs`: `ModuleFactories::production(main_thread)` adds noTunes alongside proxyAudio; do not replace with the old zero-argument production factory.
- `AXUIElement.h:315-320` (an `AXPress` that returns `CannotComplete` may still have run) and `:387-396` (the per-element timeout does not apply to other objects).

## Intentional compatibility changes

1. The popup is positioned in AppKit global coordinates derived from the primary display and validated against the screen that contains the click, instead of the key-window screen height.
2. A tap disabled by timeout or user input is re-enabled by the shared service. Pause, disable and stop release the subscription instead of toggling a private tap.
3. Accessibility is observed live through the permission policy (`WaitingForPermission` plus the tray request row), not cached at startup.
4. Invocation targets the exact app instance whose menu was read. If that instance terminated, the press is dropped as stale instead of pressing on whatever app is now frontmost.
5. AX elements are retained by the AX owner and released on exact-presentation cleanup. A selected action's elements survive menu closure until its in-flight press resolves. Stop invalidates immediately; native cleanup may remain pending and must be reported. No leak-dependent raw pointers.
6. A matching trigger while busy is swallowed and dropped. Legacy queued another main-thread build.
7. An unresponsive app ends in a typed timeout with no menu. Legacy had no budget.
8. When the frontmost app is Stache itself, the result is a typed `SelfTarget` and no menu. Reading its own AX tree from the main thread would deadlock.
9. Statuses are the SDK lifecycle statuses, not legacy `ConfiguredOff` or `Paused` from the tap state.
10. `middleClick` means `otherMouseDown` **and button number 2**, not back/forward buttons. Right click also validates its event type and button number 1; left click never matches.
11. Shortcut display follows the installed header: Shift/Option/Control are bits 0/1/2; Command is present unless bit 3 (`NoCommand`) is set. Missing modifiers still default to Command.
12. Engineering budgets are 0.5 s per-element messaging timeout, 1.5 s cumulative read deadline, 4096 nodes, depth 16 and 1 s `AXPress`. No partial menu. An in-flight AX call cannot be interrupted: read completion can take the deadline plus its last in-flight timeout and scheduling overhead, not an instantaneous 1.5 s hard bound.

## Open questions and gates

No unresolved product choices. Dedicated AX-owner feasibility, modal menu cancellation and measured bounds are mandatory Part 02 gates, not assertions of verified native behavior. Part 03 is blocked until the shared input service lands. Apply the cancellation, acknowledged-cleanup and uncertain-outcome lessons recorded in the [Wallpapers audit](../2026-11-03-wallpapers/review-findings.md), source `f13cd3a`; this is historical source text, not a shipped dependency.

## Validation and safety

Panic containment prevents unwinding across FFI in unwind-enabled builds; the retained release `panic = "abort"` profile cannot recover arbitrary Rust panics. Production callbacks must be bounded and nonpanicking. Fake unwind tests do not prove release recovery.

Default tests never install a tap, read a real app's menu, show a real menu, press an AX item, activate an app or prompt for a permission. Each part runs its listed crate tests, full root checks, builds and `RUSTDOCFLAGS='-D warnings'` docs. Manual native gates (multi-display placement, unresponsive-app timeout, terminated-app staleness) require explicit approval **for each run**, follow the exact steps in Parts 02 and 03, and signal only a PID the tester launched. Never kill by executable name. Never run the legacy MenuAnywhere at the same time.

## Affected docs

- [Planned feature notice](../../features/menu-anywhere.md), clearly marked NOT SHIPPED, and an additive [index](../../index.md) route.
- Part 01: [config](../../architecture/config.md). Part 02: [overview](../../architecture/overview.md) adapter list. Part 03: feature behavior, plus narrow [new-module](../../guides/new-module.md) and [testing](../../guides/testing.md) references. Legacy docs stay archived and untouched.
