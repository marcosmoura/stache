---
branch: feat/tiling-animation
---

# Tiling animation pipeline

Read when planning or implementing the faster opt-in Tiling animation path. **DRAFT — NOT SHIPPED.** Every part needs separate approval, and every native gate needs its own per-run approval under [validation](../2026-11-03-tiling/validation.md#per-run-native-approval). This document authorizes no native run.

## Goal

Make opt-in Tiling animations smooth on real applications without Screen Recording, snapshot proxies or SIP changes, by rebuilding the write path the Rift way: one AX thread per application, latest-frame-wins mailboxes, no per-frame readback, display-paced position-only frames, a scoped `AXEnhancedUserInterface` suppression lease and optional SkyLight update batching. An application that still cannot keep up moves instantly, with a status diagnostic, instead of stepping visibly.

## Problem (current code)

Cited from `main` after `fix/tiling-fixes` merged as `ba51f20` (re-checked 2026-11-10 at `3195740`). Paths are under `src/crates/`.

- One animation thread (`stache-tiling-animation`, `modules/tiling/src/animation/worker.rs:382`) steps every window serially in `Animator::tick` (`worker.rs:198–231`); each `AnimationSink::write` (`animation/control.rs:46–55`) is a synchronous `FrameControl::execute` → `WindowBackend::act` (`actor/frames.rs:167–212`) that hops to the **single** `stache-tiling-ax-owner` thread (`backend.rs:31`, `macos/src/windows/adapter.rs:197–199`) and waits for its reply. A slow application delays every other window's frames.
- On the owner, every action first re-lists `AXWindows` to revalidate membership (`validate`, `macos/src/windows/core.rs:1040–1093`), then writes position **and** size (`macos/src/ax/window.rs:424–443`) and reads the frame back (`act`, `core.rs:1259–1264`) before replying. That is at least five AX round trips per window per frame, and the size write makes the application relayout and repaint on every frame.
- Applications with `AXEnhancedUserInterface` on animate each write themselves and lag about 250 ms; nothing in the tree reads or clears it.
- Pacing is a `Condvar` timeout at the clamped display refresh (`modules/tiling/src/events/geometry.rs:24–35`), not a display clock, so frames drift against vsync.

Workspace switches already never animate (`state/frames.rs:619–622`, the `revealed` set); this spec keeps that.

## Non-goals

- No GPU proxy, window snapshot, Screen Recording permission or SIP-requiring API (yabai's approach is rejected).
- No change to `animations` configuration: `enabled` (default `false`), `duration` and `easing` keep their schema, defaults and semantics (`modules/tiling/src/config/layout.rs:73–90`). No new configuration key.
- No animation of workspace switches, reveals, hide/unhide or focus.
- No change to layouts, CLI or socket contracts beyond additive status diagnostics.
- No legacy build, automation or behavior port; `_legacy/` is read-only reference.

## Decisions

Approved by the user on 2026-11-09.

1. Build the Rift-style AX pipeline: per-application AX threads, latest-frame-wins per-window mailboxes, per-application frame batches, no per-frame readback, display-link pacing, position-only interactive frames and a batch-scoped Enhanced UI suppression lease. No Screen Recording, snapshot proxy or SIP-requiring API.
2. Minimal private SkyLight APIs are allowed behind compile gates and approved native checks, with a public fallback, under the same policy as `_AXUIElementGetWindow` (`macos/src/ax/window_id.rs:1–19`: runtime `dlsym`, explicit capability, nothing else disabled when absent).
3. When the smooth path is unavailable, or an application cannot keep up, its windows are placed instantly (one final write) and status shows a diagnostic. Never janky stepping.
4. Workspace switches never animate (already on `main` since `ba51f20`; keep).
5. Animations stay opt-in (`animations.enabled`, default off); duration and easing configuration are unchanged.
6. The [Tiling decisions](../2026-11-03-tiling/00-overview.md#decisions) 5–9 apply unchanged to every part: exact-token, generation, accepted-revision and plan-sequence authority checked at the native send; logical invalidation stays atomic; Applied/Partial/Uncertain outcomes stay truthful; stop seals, drains issued frames **before** owned restore, and quarantines what it cannot clean up. Owner confinement stays: no AX on main, AX objects stay `!Send` on the thread that created them (`macos/src/ax/mod.rs:10–15`), hops are bounded, no thread is detached, no lock is held across a native call, every queue is bounded and A–B–A histories never mark a successor applied.

## Resolved questions (approved 2026-11-09)

- **OQ-1 (part 02): membership lease — approved.** An animation validates `AXWindows` membership at its first and final write; intermediate writes rely on the retained element, destroy-notification invalidation and the issuance gate (seal, generation, exact token on every write). A write failing with an invalid element ends that animation.
- **OQ-2 (part 04): VoiceOver — no lease.** While VoiceOver is running, `AXEnhancedUserInterface` is never touched; applications that are then too slow use instant placement with a diagnostic.
- **OQ-3 (part 04): hard-kill residue — documented limit.** No journal; a crash or SIGKILL during a lease leaves the attribute off for that application until it relaunches. Documented in the feature doc.
- **OQ-4 (part 06): pass threshold — all four.** Every listed application (Ghostty, Zed, Safari or Aside, Spotify) must meet the smooth criterion in the approved native run; a fallback for any of them fails the run. The per-application instant fallback still ships for other applications.
- **OQ-5: Reduce Motion — no change.** `animations.enabled` alone decides; the system Reduce Motion setting does not override it.

## Sources

Research by the orchestrator, 2026-11-09; not re-verified while writing this spec.

- No public macOS API animates another application's windows; AX writes are the only public path.
- yabai (`github.com/koekeishiya/yabai`) animates GPU proxies through private SkyLight, needing Screen Recording and partially disabled SIP. Rejected.
- AeroSpace (`github.com/nikitabobko/AeroSpace`) omits animations.
- Rift (`github.com/acsandmann/rift`, `src/actor/reactor/animation.rs`, `src/actor/app.rs`) animates without SIP: per-display CVDisplayLink pacing, one AX writer thread per application, latest-frame-wins per-window mailbox (slow applications skip frames instead of queueing), per-application frame batches, position-only interactive frames unless the size changes or the frame is final, no per-frame readback, a batch-scoped `AXEnhancedUserInterface` lease (off before the batch's first write, restored after the application's last write) and optional legacy-style SkyLight `SLSDisableUpdate`/`SLSReenableUpdate` batching.
- Momentum PR #9: a resize measured 257 ms with Enhanced UI on and 20 ms with it off.
- mimi PR #421 and #419, Segue PR #19: the same Enhanced UI off/write/restore pattern.
- KiwiDesk PR #2424: every write path needs the off/on wrapper, including instant placement and the Reduce Motion path.
- Chromium issue 40865608: toggling Enhanced UI itself costs time in Chromium-based applications.
- Rectangle #912: some applications (for example Grammarly) turn Enhanced UI back on.
- Legacy reference: `_legacy/app/native/src/modules/tiling/ffi/skylight.rs` (`UpdateGuard` over `SLSDisableUpdate`/`SLSReenableUpdate`), inventoried in the [Tiling inventory](../2026-11-03-tiling/inventory.md).

## Parts

| Part                             | Summary                                                                        | Status |
| -------------------------------- | ------------------------------------------------------------------------------ | ------ |
| [01](01-application-shards.md)   | Per-application AX owner shards behind the existing adapter                    | draft  |
| [02](02-async-frame-pipeline.md) | Asynchronous latest-frame mailboxes, per-app batches, no per-frame readback    | draft  |
| [03](03-display-pacing-size.md)  | Display-link pacing, position-only frames and the size envelope                | draft  |
| [04](04-enhanced-ui-lease.md)    | `AXEnhancedUserInterface` suppression lease on every frame write path          | draft  |
| [05](05-skylight-batching.md)    | Optional, gated SkyLight update batching with public fallback                  | draft  |
| [06](06-measurement-fallback.md) | Metrics, approved native measurement, instant fallback policy and lasting docs | draft  |

## Sequencing

Starts after `fix/tiling-fixes` merged into `main` (`ba51f20`); the cited lines were re-checked against `main` on 2026-11-10. Order: 01 → 02 → 03 → 04 → 05 → 06. Part 04 depends only on 01–02 and may run before 03 if approved; part 05 is optional and may be skipped (its fallback is the shipped behavior). Part 06 runs last because its fallback policy consumes the metrics and leases of the earlier parts. Each part compiles, tests and commits alone and leaves animations working (or explicitly instant). The [merge gate](../../workflow.md#merge-gate) blocks merging while any part is not `done`; a skipped part 05 needs an explicit user decision recorded in this overview before merge.

Acceptance IDs `TA-NN-MM` have exactly one owning part.

## Affected docs

Each part updates only its shipped slice: [features/tiling.md](../../features/tiling.md#animation) (Animation and native adapter notes), [guides/performance.md](../../guides/performance.md) (threads, pacing, measurement), [architecture/overview.md](../../architecture/overview.md) (owner map) and [guides/testing.md](../../guides/testing.md) (fakes and native gates). Until then, the feature doc carries one "planned" line pointing here.
