---
branch: feat/keep-awake
---

# Keep Awake rewrite

Read when planning the independent Keep Awake module. This is a draft, not shipped behavior or permission to implement.

## Goal

Move legacy power prevention out of Bar into `stache-keep-awake`, with owned lifecycle resources, lock-aware release and truthful read-only status. Obtain approval before implementing each part.

## Non-goals

- No Bar, window, Settings screen, coffee-cup toggle, independent desired-awake flag, modes, durations or new CLI.
- No Wallpapers implementation, noTunes adapters, module-specific mutating endpoint or generic service framework.
- No TCC permission, automatic screen locking/waking, private API replacement or unrelated SDK/shutdown redesign.
- No changes to proxyAudio policy, permissions or native behavior.

## Decisions

- Crate: `src/crates/modules/keep-awake`; package: `stache-keep-awake`; module/catalog ID: `keepAwake`; file: `keepAwake.jsonc`.
- Authoritative config has only `enabled`, default `false`. Migration emits disabled defaults even when legacy `bar.enabled` is true; it never infers consent from Bar.
- Preserve `.display(true).idle(true).sleep(true)` prevention. One owned lease may contain three IOPM assertion IDs, not one ID or a promise that macOS cannot sleep.
- Existing configuration-backed tray Enable/Disable and session Pause/Resume are the only controls. Enabled and unpaused means request awake while the session is known unlocked; lock releases, unlock restores. Resume probes again before acquisition.
- Keep Awake is independent of Bar and of noTunes. A future Bar may consume status, never own the assertion.
- Read-only `keepAwake.status` distinguishes confirmed `inactive`, in-flight unadopted `acquiring`, complete adopted `active` prevention and in-progress or unresolved cleanup (`releasePending`), lock knowledge, lifecycle, enabled and paused state. Publish acquiring before native creation/candidate validation and releasePending before native release/rollback; neither transition alone is a release failure. The app owns the envelope/handler and overlays held file diagnostics; the module owns only policy, runtime DTOs and a snapshot reader. Export Rust-derived DTOs without adding a UI surface.
- Preserve SDK lifecycle, malformed-file diagnostics, last-good reload behavior and generic Retry. No Keep Awake switch belongs in the SDK.
- Native APIs and ownership must pass part 01's feasibility gate before runtime work. All project Cocoa/FFI belongs in `stache-macos`; module code forbids unsafe.
- Notifications are refresh hints, supplemented by an owned, cancellation-wakeable two-second reconciliation timer whenever active. Unknown lock state suppresses assertion acquisition.
- Preserve the approved legacy compatibility mapping: a valid session dictionary with no lock key means unlocked; explicit CFBoolean true/false means locked/unlocked. A null/unreadable dictionary or present non-boolean key means unknown and fails closed. Absence is not query failure; this best-effort mapping is not a public lock guarantee or a claim that there is no safety gap.
- Explicit release and partial rollback are fallible. Retain ownership of unresolved IDs, report `assertionReleaseFailed` only on actual cleanup failure and block fresh acquisition until cleanup resolves. Best-effort Drop is not proof of release; process exit is the ultimate OS boundary, not successful application cleanup.

## Ordering and isolation

The agreed [roadmap](../../roadmap.md) order is noTunes → Keep Awake → Wallpapers → Keybindings/Caps/section → Hold-to-Quit → MenuAnywhere → Bar → Tiling (last). These are independent tool specs/branches; that sequence creates no dependency on unimplemented noTunes adapters. This spec covers Keep Awake only; sibling drafts are also available on `main`.

The first implemented/merged noTunes part 01 defines generic catalog migration/default emission. Before implementing each Keep Awake part, rebase on current main and reuse merged helpers; do not create competing migration/factory frameworks or depend on unmerged module crates. Keep Awake is likely the first consumer of small generic factory context for published registry reads, held store diagnostics and main-owner access; define it once, only as needed. Wallpapers may later reuse/extend it for accepted-config watching or host cancellation when justified. Catalog, bindings and index additions must be additive, with generic regression tests preserving existing consumers. Update the [roadmap](../../roadmap.md) when later branches merge.

## Parts

| Part                       | Summary                                                                   | Status |
| -------------------------- | ------------------------------------------------------------------------- | ------ |
| [01](01-config-native.md)  | Typed config/catalog/migration and safe native adapters; feasibility gate | draft  |
| [02](02-runtime-status.md) | Owned lock-aware runtime, production factory and typed read-only status   | draft  |

Each part must independently build, test and be committable. Part 01 registers configuration, not a production runtime; part 02 depends only on part 01 and the shipped foundation. Only the orchestrator changes status, after approval and verification. Unfinished parts block merge.

## Source evidence and compatibility limits

- [Legacy feature](../../../_legacy/docs/features/keep-awake.md) and `keepawake.rs` under `_legacy/app/native/src/modules/bar/components/` describe the old Bar-bound controller. Its global watcher, detached ownership and automatic activation are not the target architecture.
- Legacy `KeepAwake.state.ts` caches an object event as a boolean; do not port the hook or payload contract.
- Installed `keepawake 0.6.0`, `src/sys/macos.rs`, creates up to three IDs through `IOPMAssertionCreateWithName`; its `Drop` attempts release, including partial-creation rollback, but ignores every `IOPMAssertionRelease` return code (lines 109–121). The unmodified wrapper cannot be the sole proof of cleanup. A minimal suitable IOPM binding wrapper in `stache-macos` is allowed to expose fallible release and retained pending ownership. `reason` applies on macOS; `app_name` and reverse domain are Linux-only metadata.
- Installed `objc2-foundation 0.3.2`, generated `NSDistributedNotificationCenter.rs`, exposes selector-based registration/removal and inherits `NSNotificationCenter`. Do not assume an inherited block API has suitable distributed semantics.
- Installed macOS `Foundation.framework/Headers/NSDistributedNotificationCenter.h` says delivery uses the main run loop on modern macOS, can be delayed/dropped, and does not retain selector observers. Retain the observer until removal; callbacks only signal refresh and contain panics.
- Installed `CoreGraphics.framework/Headers/CGSession.h` documents a nullable, caller-owned session dictionary. Installed `objc2-core-graphics 0.3.2`, generated `CGSession.rs`, wraps it as `Option<CFRetained<CFDictionary>>`.
- `CGSSessionScreenIsLocked` is **not** a documented key in that header. Treat this legacy compatibility indication and the two `com.apple.screenIsLocked/Unlocked` names as best-effort, not a public lock-state guarantee. Apply the compatibility mapping above, validating CF types and Copy ownership. The parent reported two read-only `CGSessionCopyCurrentDictionary` observations: first the key was present as CFBoolean true; later it was absent in a valid dictionary. The parent did not independently establish controlled UI-locked/unlocked baselines or perform manual lock/unlock, so physical lock state was not verified. The Opus review's earlier observation used `ioreg` only. Absence → unlocked is selected from approved legacy semantics, not these observations or a public API guarantee. Session-active/on-console/login-done is not an equivalent lock test.
- [`activation.rs`](../../../src/crates/macos/src/activation.rs) demonstrates a main-owned `!Send` observer, not a guard that can be placed into a `Module: Send`. Part 01 must prove the actual distributed observer ownership and dispatch path.

## Validation and safety

Default tests use fakes: no real assertion, screen lock, wake, app launch or competing legacy Bar. Each implementation part runs its listed native tests, full root checks/builds and warnings-denied rustdoc. Generated output must be deterministic.

Real-device power verification is optional and requires explicit approval **for each run**. If approved later, capture `pmset -g assertions` before/after, identify only the owned Next process and exclude competing legacy assertions. Screen-lock interaction is manual, separately authorized; never automate lock/wake. Record unavailable/manual cases honestly rather than claiming execution.

Process exit releases OS assertions, but is not proof of successful application cleanup. The branch adopts the [coordinated teardown contract merged into `main`](../2026-11-03-teardown/00-overview.md): SDK invalidation makes the generation inert and cancels/wakes native acquisition; `Module::stop`/rollback owns physical cleanup. `NativeSlot::PowerAssertion` (stable name `power-assertion`) acknowledges coherent native-ledger state from the same `AssertionOwner`, not a second cleanup path. Never acknowledge active, acquiring, cleaning or unresolved ownership as clean. Branch integration is verified with fakes and builds; merge approval is pending. Real power/lock behavior remains **unverified**, and each native run requires separate approval. No real power operation is permitted during spec writing.

## Affected docs

- [Planned feature notice](../../features/keep-awake.md) remains clearly not shipped until implementation.
- [Index](../../index.md) routes this planned spec alongside the other workstreams.
- Part 01 updates [config](../../architecture/config.md) and the feature notice with shipped configuration/native limitations, without claiming a runtime.
- Part 02 materializes feature behavior, [IPC](../../architecture/ipc.md), and narrowly updates [new-module](../../guides/new-module.md)/[testing](../../guides/testing.md) references where required. Do not rewrite generic SDK docs or historical legacy docs.

## Gate/open evidence

No new product choice is assumed. Before part 02, report the compile-proven native ownership, cancellation deadlines and documented-vs-best-effort lock evidence. If permitted public APIs cannot support the approved behavior safely, stop for a decision; do not substitute session activation or start a runtime with guessed unlocked state.
