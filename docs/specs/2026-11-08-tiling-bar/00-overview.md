---
branch: feat/tiling-bar
---

# Tiling–Bar integration

Read when planning the final slice that joins Tiling and Bar. Approved 2026-11-10; part 10 is done (2026-11-11). The user accepted the `measure:bar` evidence as is; the native live strip check was not run and stays unverified.

## Goal

Show Tiling's workspace/app strip, visually identical to legacy, in Bar's reserved empty slot through optional typed published state, and prove the Bar reservation lifecycle end to end against Tiling's landed normalization.

## Non-goals

- No change to Tiling 01–09 or Bar 03–08 behavior beyond the strip slot and the bridge.
- Everything listed as a non-goal in the [Tiling overview](../2026-11-03-tiling/00-overview.md#non-goals) and the [Bar overview](../2026-11-03-bar/00-overview.md#non-goals).

## Prerequisites

All merged into `main` before part 10 starts (all met as of 2026-11-10):

- [Bar foundation](../2026-11-08-bar-foundation/00-overview.md) (published-state channel, `observes`, host reservations, display ID).
- [Tiling](../2026-11-03-tiling/00-overview.md) parts 01–09, including part 05's reservation normalization, plus later fixes and the tiling-animation pipeline.
- [Bar](../2026-11-03-bar/00-overview.md) parts 03–08, including the Bar shell's empty slot and the independent visual-parity reference from part 04; squash-merged as `98b0c23` on 2026-11-10.

## Decisions

The [Tiling decisions](../2026-11-03-tiling/00-overview.md#decisions) and its compatibility corrections, [inventory](../2026-11-03-tiling/inventory.md), [CLI contract](../2026-11-03-tiling/cli-contract.md), [review lessons](../2026-11-03-tiling/review-lessons.md) and [validation](../2026-11-03-tiling/validation.md) apply unchanged. Bar's visual-identity decision applies to the strip. Neither module is the other's lifecycle prerequisite.

The part keeps its Tiling number and acceptance IDs (`TI-10-*`) so references in the Tiling appendices stay valid. Approved order (2026-11-08): Bar foundation → Tiling 01–09 → Bar 03–08 → **Tiling–Bar integration**.

### Decisions (2026-11-10, user-approved)

These replace any conflicting wording in part 10.

- **Clickable strip.** As in legacy, clicking a workspace focuses it and clicking an app focuses that window. One new Bar-only generated command (working name `bar_tiling_action`) takes a closed enum `focusWorkspace{name}` | `focusWindow{window}` plus the rendered Tiling generation/sequence. It is ACL-gated in `src/crates/app/build.rs`, granted only in `capabilities/bar.json`, and checks the live Bar label. The host resolves the opaque window key to the exact current token; Tiling revalidates at issuance through its existing `Intent::FocusWorkspace` / `Intent::FocusWindow(FocusTarget::Window)`. Outcomes are typed (applied/stale/unavailable/busy/unauthorized/…, enumerated concretely during implementation). An outcome produces no UI (`console.debug` only); the strip changes only through the next `BarChanged`. No generic dispatcher.
- **State shape.** Tiling publishes a minimal Tiling-owned DTO, schema `tilingState` v1 (provider = Tiling module ID), through the SDK published-state channel from new `src/crates/modules/tiling/src/published.rs`: workspaces (name, screen, visible, focused, order), windows (opaque per-generation key as decimal string, app name, title, workspace, minimized/hidden) and focus (workspace, window). Published with the accepted config revision; stop tombstones it. No native token or pointer crosses to JS. The app projects it into typed `BarView.tiling` (`null` until resolved) through a no-timeout watcher, like Keep Awake. The projection owns a never-resetting change counter, because the SDK sequence restarts per generation (`bar_bridge` sums sequences). Bar declares `observes` Tiling, never `requires`.
- **Strip data (legacy semantics).** All workspaces, sorted in the UI by legacy order then unknown alphabetical; the windows of the globally focused workspace, excluding minimized and hidden ones (legacy `!is_minimized && !is_hidden`); a native tab group shows only its active tab; the focused window.
- **Empty/loading mapping (legacy).** `SpacesFallback` "Loading..." shows only while Tiling is Running and its first state is unpublished. The empty `<div />` shows for absent/disabled/paused/stopping/failed/tombstoned/schema mismatch/zero workspaces. Reconciling/incomplete phases render whatever is published. A snapshot error stays an explicit query state internally but renders `<div />` (no new visual).
- **Icons.** `@hugeicons/core-free-icons` stays at 4.4.0 and `@hugeicons/react` at 1.1.10; no downgrade. The strip's parity reference takes icon SVGs from the 4.4.0 package (still independent of the new components); all other markup/CSS/layout comes from the pinned legacy reference. Implementation lists every strip icon whose 4.2.3 and 4.4.0 glyphs differ. Replaces "no dependency-induced icon changes" for icons.
- **App icon map extended beyond legacy** (user request). Keep every legacy entry (including U+200E hidden-mark WhatsApp, Microsoft Edge Dev and the `DashboardCircleIcon` fallback) and add, keyed by the exact `CFBundleName`: `Zed` → `CodeSquareIcon`, `Aside` → `AiBrowserIcon`, `Herdr` → `CommandLineIcon`, `DeviceHub` → `SmartPhone01Icon`, `AIOStreams` → `PlayListIcon`, `Nuvio` → `Tv01Icon`, `FigCraft` → `PaintBoardIcon`. Approved deviations from legacy; their reference entries record this decision as provenance instead of a legacy source. Parity scenarios render every workspace icon, every app-map icon (legacy and new), the fallback and the unknown-workspace case.
- **Unknown workspace icon.** No fallback icon. Legacy passed an undefined icon, which WebKit verified throws and replaces the whole Bar with its error fallback; an empty `tiling.toml` names workspaces `workspace-N`, so the crash is not kept: the button renders without an icon (orchestrator decision during 10c; user-confirmed 2026-11-11).
- **Reservation while loading.** Keep Bar's shipped behavior: the constant top inset is staged at panel open and held while the page loads; a failed panel clears it.
- **zustand** only if a slice needs UI-local state; nothing requires a store.
- **End-to-end tests** use the real Tiling runtime with a fake backend, not the `Rig::tiling()` mirror in `src/crates/app/src/bar_tests.rs`. They model the user's setup (`bar.toml` height 28 / padding 14, Tiling gaps 14/10, two displays, menu-bar auto-hide on) and assert what Tiling plans and what the UI receives, including no config reads for the reservation.
- **Performance.** No idle timers or polling. After the part, run `bun run measure:bar` on a release `.app` and compare with the [2026-11-10 baseline](../2026-11-03-bar/measurements/2026-11-10T19-33-59-056Z/report.md) (+0.59 % of one core, +0.39 wakeups/s).

## Parts

| Part                        | Summary                                                  | Status |
| --------------------------- | -------------------------------------------------------- | ------ |
| [10](10-bar-integration.md) | Bar strip/final reservation integration and safety gates | done   |

The part is implemented in sequential slices 10a–10d (see the part file), each independently testable. The part builds, tests and commits alone; an unfinished part blocks merge.

## Affected docs

The [Tiling](../../features/tiling.md) and [Bar](../../features/bar.md) feature docs gain the strip and integration facts only when the part lands, together with `architecture/{sdk,ui,overview,config,ipc}.md`, testing, performance, index and roadmap.
