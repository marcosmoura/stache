---
branch: feat/notunes
---

# noTunes rewrite

Read when implementing the first [roadmap](../../roadmap.md) tool. **SHIPPED.**
Part 01 configuration/native groundwork and Part 02 runtime integration are done.
Destructive native verification requires separate approval for each run.
Base: `main` at `e0b4648`.

## Goal

Port noTunes into an owned, live-configurable module using the shipped foundation.
Preserve best-effort Apple Music/iTunes blocking, including the explicitly approved
force-termination of already-running instances on enable/start/resume.

## Non-goals

No Settings UI, widgets, dedicated CLI/IPC commands, player discovery beyond the
user-chosen target, arbitrary app blocking, replacement presets, polling or new
idle timer. No generic input framework,
legacy singleton scaffolding, or repair of the separate Cmd-Q/logout teardown gap.
Foundation specs and archived docs remain unchanged.

## Decisions

- `stache-notunes` lives in `src/crates/modules/notunes`, forbids unsafe code and
  depends only on workspace `stache-sdk`/`stache-macos`, plus needed third-party crates.
- Policy stays in the module; all Cocoa/native work stays in `stache-macos`.
  App registration is thin composition, with no noTunes-specific SDK registry/config/IPC match.
- Observe `NSWorkspaceWillLaunchApplicationNotification` on the workspace's
  notification center, subscribing before the initial running-app sweep.
  Sweep terminates without replacement; launch notifications attempt termination
  then optional replacement. Only exact `com.apple.Music`/`com.apple.iTunes` match.
- This is interception after launch, not an OS veto: apps can appear briefly;
  manual launches cannot be distinguished from media-key launches.
- No Accessibility/TCC permission or required replacement-player prerequisite.
  Missing optional players warn; observer installation failure is a failed start
  with rollback and existing Retry, not `Unavailable`.
- Preserve SDK transactional reload/last-good semantics. Accepted target changes
  apply live without restart or duplicate observation; no `OnceLock`/Tidal fallback.
- Use existing tray Enable, Pause/Resume, Retry and generic host status/diagnostics.

## Parts

| Part                            | Summary                                                                         | Status |
| ------------------------------- | ------------------------------------------------------------------------------- | ------ |
| [01](01-config-native.md)       | Authoritative config/catalog and safe native boundary; no blocker registration. | done   |
| [02](02-runtime-integration.md) | Lifecycle/policy, production factory, integration and opt-in checks.            | done   |

Part 02 depends on 01; each must independently build, test and commit cleanly.

## Future integration and rebase

Before implementing later parts, rebase on latest `main` and reuse generic plumbing
already landed by earlier tools; do not depend on unmerged branches. noTunes Part 01,
as the first landing, defines catalog-driven typed migration/default emission.
Keep Awake and Wallpapers reuse that path rather than build parallel frameworks or
compile-time dependencies between module crates. Keep catalog/schema/bindings/index
changes additive and preserve other modules and their tests.

The first consumer needing generic factory context (Keep Awake in the roadmap)
defines only the registry/store/main-owner plumbing it needs, once. Wallpapers later
reuses or extends it as needed for accepted-config watching and host-lifetime
cancellation. This is future integration guidance, not a new API requirement for
completed Part 01 or an expansion of current implementation scope.

## Affected docs

Part 01 updates [configuration](../../architecture/config.md) for shipped catalog
support only. Part 02 replaces the [planned feature stub](../../features/notunes.md)
with shipped behavior and updates [testing](../../guides/testing.md),
[module guidance](../../guides/new-module.md) and index references as needed.
Keep future feature claims in this spec until their owning part ships.
