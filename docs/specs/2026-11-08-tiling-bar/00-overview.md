---
branch: feat/tiling-bar
---

# Tiling–Bar integration

Read when planning the final slice that joins Tiling and Bar. **DRAFT — NOT SHIPPED.** The part needs separate approval.

## Goal

Show Tiling's workspace/app strip, visually identical to legacy, in Bar's reserved empty slot through optional typed published state, and prove the Bar reservation lifecycle end to end against Tiling's landed normalization.

## Non-goals

- No change to Tiling 01–09 or Bar 03–08 behavior beyond the strip slot and the bridge.
- Everything listed as a non-goal in the [Tiling overview](../2026-11-03-tiling/00-overview.md#non-goals) and the [Bar overview](../2026-11-03-bar/00-overview.md#non-goals).

## Prerequisites

All merged into `main` before part 10 starts:

- [Bar foundation](../2026-11-08-bar-foundation/00-overview.md) (published-state channel, `observes`, host reservations, display ID).
- [Tiling](../2026-11-03-tiling/00-overview.md) parts 01–09, including part 05's reservation normalization.
- [Bar](../2026-11-03-bar/00-overview.md) parts 03–08, including the Bar shell's empty slot and the independent visual-parity reference from part 04.

## Decisions

The [Tiling decisions](../2026-11-03-tiling/00-overview.md#decisions) and its compatibility corrections, [inventory](../2026-11-03-tiling/inventory.md), [CLI contract](../2026-11-03-tiling/cli-contract.md), [review lessons](../2026-11-03-tiling/review-lessons.md) and [validation](../2026-11-03-tiling/validation.md) apply unchanged. Bar's visual-identity decision applies to the strip. Neither module is the other's lifecycle prerequisite.

The part keeps its Tiling number and acceptance IDs (`TI-10-*`) so references in the Tiling appendices stay valid. Approved order (2026-11-08): Bar foundation → Tiling 01–09 → Bar 03–08 → **Tiling–Bar integration**.

## Parts

| Part                        | Summary                                                  | Status |
| --------------------------- | -------------------------------------------------------- | ------ |
| [10](10-bar-integration.md) | Bar strip/final reservation integration and safety gates | draft  |

The part builds, tests and commits alone; an unfinished part blocks merge.

## Affected docs

The [Tiling](../../features/tiling.md) and [Bar](../../features/bar.md) feature docs gain the strip and integration facts only when the part lands, together with `architecture/{sdk,ui,overview,config,ipc}.md`, testing, performance, index and roadmap.
