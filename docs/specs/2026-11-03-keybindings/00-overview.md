---
branch: feat/keybindings
---

# Keybindings rewrite

Read when planning Keybindings, Caps Lock and section-key shortcuts. **Not shipped.**
All APIs introduced here are planned; obtain approval before implementing each part.

## Goal

Move legacy shortcuts into an independently owned, reloadable `stache-keybindings`
module, with a shared host input boundary and shared command runner. Preserve
shortcut/command compatibility without copying legacy global ownership hazards.

## Non-goals

No Settings/UI, new CLI commands, Hold-to-Quit, MenuAnywhere, Bar, Tiling or
wallpaper implementation. No shell interpreter, input injection, process-name
killing, arbitrary application termination, competing configuration/factory
framework, or repair of the host Cmd-Q/logout coordinated-teardown gap.

## Baseline and ordering

This worktree starts at `e0b4648`, but planning reads **main `ad34b46`**, including
shipped noTunes (`ccae030`). Before each implementation part, rebase onto current
main and source-check the facilities that have landed since this draft.

- Shipped: `ModuleConfig::migrate_legacy_section`,
  `ConfigCatalog::rewrite_legacy`, generic registered-module migration/defaults
  in `cli/src/migrate.rs`, noTunes/application control, and production factories
  in `app/src/host.rs`. Reuse these; no replacement migration framework.
- Existing `ModuleContext` supplies main dispatch, cancellation and workers;
  it does **not** yet supply shared input. Current `ModuleFactory::register`
  receives registry/store, not a generic factory context. Reuse a context if one
  lands first; otherwise add only the small context/services this feature needs.
- [Keep Awake](../2026-11-03-keep-awake/00-overview.md) and [Wallpapers](../2026-11-03-wallpapers/00-overview.md) are drafts on `main`, with implementation on `feat/keep-awake` and `feat/wallpapers`. They are
  precedents, not code dependencies. Roadmap: noTunes → Keep Awake → Wallpapers
  → Keybindings → Hold-to-Quit → MenuAnywhere → Bar → Tiling. Update a roadmap
  if present after rebase, without overwriting other module/index additions.

## Decisions

- Package `stache-keybindings`, path `src/crates/modules/keybindings`, ID
  `keybindings`, file `keybindings.jsonc`. Its only workspace-crate dependencies
  are `stache-sdk` and `stache-macos`; external shared dependencies use workspace
  declarations. Module/SDK forbid unsafe and contain no Tauri dependency.
- Authoritative `KeybindingsConfig` has `enabled: false` and `bindings: {}`.
  Values are `string | string[]`. Migration wraps the legacy map; nonempty means
  enabled, empty means disabled. This product choice is settled.
- Preserve ordinary grammar and aliases, Caps parsing first, then `§`, then
  ordinary parsing; raw keys sort lexically and the last normalized duplicate
  wins with a diagnostic. Invalid shortcut entries are skipped with diagnostics,
  not whole-file rejection. Type/parse errors reject the file unchanged.
- Blank strings/array entries are trimmed/filtered; an empty effective sequence
  captures the successfully registered shortcut without spawning a process.
- Ordinary/`§` use a safe main-owned Carbon adapter. Only Caps needs the shared
  HID tap now. No blanket Accessibility prerequisite for ordinary shortcuts.
- One host-provided SDK input service reaches modules through `ModuleContext`;
  `stache-macos` owns one input thread/run loop, at most one **suppressing**
  head-insert HID tap active only while suppressing/cleanup subscriptions exist,
  and at most one **listen-only** observation tap active only while passive
  observation subscribers exist. Passive consumers can never intercept, suppress
  or change priority; no module owns a private tap.
- Generic priority values order Hold-to-Quit > MenuAnywhere > Keybindings;
  SDK knows no module IDs. First suppression wins within shared HID routing.
  Carbon reservations are centrally tracked and part 03 owns a generic
  yielding-reservation primitive: a higher-priority HID claimant (Hold-to-Quit
  for physical Command-Q) yields an overlapping Carbon reservation so one press
  cannot run both actions, and the reservation is restored on claimant
  release/disable/pause/stop/reload. [Hold-to-Quit part 04](../2026-11-03-hold-to-quit/04-runtime.md) only consumes that
  primitive. Whether a suppressing HID tap already prevents Carbon dispatch is
  unproven until part 03's measured manual gate records it; the primitive stays
  regardless.
  Pure bounded decision callbacks see immutable events/snapshots and may update
  preallocated per-press state or enqueue only; never query native state, execute
  commands, wait, take blocking locks or allocate without a bound inline.
- Caps remaps globally across keyboards, Caps `0x7_0000_0039` → F18
  `0x7_0000_006D`. Preserve unrelated mappings; refuse **any foreign Caps
  mapping**, even identical F18. Use a durable Next-identity write-ahead journal
  and identity-wide lease, never the legacy state file. Restore on pause,
  disable, stop and removal of all Caps bindings; uncertain restoration is not
  successful restoration. A real F18 is indistinguishable while active.
- Commands spawn directly, inherit environment/cwd/stdio, run sequentially per
  accepted sequence and abort on first failure, with no command-duration timeout.
  Shared extraction preserves startup errors/cancellation/direct-child reaping.
  The SDK runner also gains a second, captured mode (caller deadline, separate
  bounded stdout/stderr, cancellation) for hidutil, the media adapter and other
  bounded helpers; the inherited-stdio legacy mode stays unchanged.
  Bare `stache` resolves the running Next executable, never legacy/PATH's stache,
  with part 01's internal CLI discriminator for standalone and packaged children.
- Pause/stop/accepted reload cancel queued sequences and terminate/reap only
  directly spawned children. OS effects already issued cannot be undone. Part 05's
  lazy bounded task pool preserves concurrent independent presses and sequential
  steps within each sequence; saturation is an intentional diagnosed safety limit
  replacing legacy unbounded per-invocation threads.
- A crash-left Next Caps remap is recovered by a host-owned startup step that runs
  once per desktop launch regardless of module enabled state or config validity,
  calling a keybindings-owned bounded entry point. It acts only on Next's own
  journal and never touches foreign/legacy mappings. Recovery requires a relaunch
  after a crash.
- Config/revision publication follows one canonical contract shared by every
  rewrite: the registry publishes accepted typed config plus the actual revision
  coherently before notifying, a rejected candidate publishes nothing, and a
  module flips staged resources only after observing that same revision through a
  bounded, cancellation-wakeable watch. No callback into module code runs under
  the publication lock, and no module-local revision guessing is allowed.
  Immediate dispatch invalidation is distinct from finite, acknowledged cleanup.

## Parts

| Part                              | Summary                                                               | Status |
| --------------------------------- | --------------------------------------------------------------------- | ------ |
| [01](01-config-command-runner.md) | Config/catalog/migration and shared SDK runner; preserve startup      | done   |
| [02](02-shared-input.md)          | Shared input contract, native owner, fakes and ownership gate         | done   |
| [03](03-shortcut-adapter.md)      | Ordinary/section parsing, Carbon registration and layout observer     | done   |
| [04](04-caps-ownership.md)        | Caps state machine, journal, identity lease and safe remap recovery   | done   |
| [05](05-runtime-integration.md)   | Runtime, bounded dispatch, accepted reload, factories and diagnostics | done   |

Parts are independently testable/committable in order. Parts 01–04 install no
production Keybindings runtime. Criteria `KB-NN-MM` belong only to their owning
part. Only the orchestrator updates status; unfinished parts block merge.

## Source evidence and intentional corrections

Paths below refer to repository sources; current-main-only sources must be read
with `git show main:<path>` until this older worktree is rebased.

- [Legacy feature](../../../_legacy/docs/features/keybindings.md),
  [hotkey planner/runner](../../../_legacy/app/native/src/modules/hotkey/mod.rs)
  (`collect_planned_shortcuts`, `normalize_shortcut`, `split_command`) establish
  routing, grammar, last-wins normalization and direct sequential execution.
- [Command resolver](../../../_legacy/app/native/src/platform/command.rs),
  [root types](../../../_legacy/app/native/src/config/types/root.rs)
  (`ShortcutCommands::get_commands`) and
  [sample](../../../_legacy/docs/samples/config.jsonc) establish lookup order,
  trimmed empty capture and string/array values. Sample wallpaper commands do
  not imply those commands ship in Next.
- [Section](../../../_legacy/app/native/src/modules/hotkey/section.rs) establishes
  `§`-last grammar, layout-required modifiers, deterministic candidate choice
  and foreign Carbon event forwarding. Next adds owned removal/layout refresh.
- [Caps engine](../../../_legacy/app/native/src/modules/hotkey/caps_lock/mod.rs),
  [parser](../../../_legacy/app/native/src/modules/hotkey/caps_lock/parser.rs),
  [state](../../../_legacy/app/native/src/modules/hotkey/caps_lock/state.rs) and
  [remap](../../../_legacy/app/native/src/modules/hotkey/caps_lock/remap.rs)
  establish physical-key behavior and tap-before-remap. Legacy permits repeated
  discrete chord presses while Caps remains held: preserve one dispatch per
  physical **chord-key** press, not one dispatch per Caps hold. Do not copy its
  post-mutation journal write, early journal deletion, blocking callback locks,
  detached threads or overwrite of foreign Caps mappings.
- Current `app/src/startup.rs`: `CommandRunner`, `ProcessCommands`, `StartupJob`
  and parser/resolver are the extraction source, not an archived implementation.
  Current lookup lacks the required running-Next `stache` override: add it once
  in the shared runner and document this intentional identity correction.
  Current main `app/src/lib.rs::dispatch` and `cli/src/mode.rs` give bundled
  invocations desktop precedence after validation. Part 01 must prove its narrow
  internal CLI path bypasses that precedence without changing ordinary direct/
  Finder launches; unresolved feasibility blocks runtime, not an accepted no-op.
- Safety precedents read from [Keep Awake](../2026-11-03-keep-awake/00-overview.md), source `2ddd0e3`,
  and the [Wallpapers review](../2026-11-03-wallpapers/review-findings.md), source `f13cd3a` (C01–C20).
  Apply relevant lessons: callback-only signaling, truthful uncertain cleanup,
  published diagnostics, exact accepted revisions, inert factories, additive
  integration, retained ownership and bounded identity coordination. Their
  feature-specific lock/power/wallpaper criteria remain outside this feature.

## Validation and affected docs

Default tests use fakes/temp roots only: never real taps/hotkeys, hidutil,
permissions, input injection or unrelated applications. Native compile gates are
not live native checks. Every real shortcut/remap/permission/manual recovery run
requires separate explicit user approval, preflight and owned-resource cleanup.
No such checks run during spec writing. Release `panic = "abort"` cannot recover
arbitrary Rust panics; containment claims must respect that build policy.

Each part updates [SDK](../../architecture/sdk.md),
[config](../../architecture/config.md), [overview](../../architecture/overview.md),
[testing](../../guides/testing.md) or [IPC](../../architecture/ipc.md) only for
behavior that part actually ships. The [feature notice](../../features/keybindings.md)
stays not shipped until runtime acceptance; the [index](../../index.md) addition
is additive. No legacy docs are rewritten. No unresolved product question;
compile/transaction/ownership feasibility must be proven before runtime approval,
not silently replaced by a different architecture.
