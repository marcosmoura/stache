# Tiling (planned)

Read when finding the Tiling rewrite plan. **NOT SHIPPED:** `feat/tiling` contains draft specifications only, not an active runtime, CLI, workspace strip or safe native window-control guarantee.

The [draft overview](../specs/2026-11-03-tiling/00-overview.md) divides implementation into nine separately approved parts that start after [Bar foundation](../specs/2026-11-08-bar-foundation/00-overview.md) merges. The workspace strip is part 10 in the [Tiling–Bar integration](../specs/2026-11-08-tiling-bar/00-overview.md) spec, which runs last, after Bar 03–08. Main's noTunes applications identity and catalog migration hook ship; sibling accepted-watch/input/published-state/reservation proposals are not shipped by this notice.

Planned behavior preserves virtual workspaces (not macOS Spaces), layouts, rules, commands and app-level hide/unhide. The latter cannot isolate windows of one application across workspaces: a visible workspace needing the app wins; other windows may remain visible. Only exact runtime-owned hides are restored. Grid alone will tile at most ten eligible windows and leave tracked extras floating without automatic centering; new ordinary floating windows will honor advertised defaultPosition. Refusing/unresizable applications can produce partial/unapplied outcomes, not guaranteed geometry.

The final workspace/app strip will be visually identical to legacy in the approved Bar slot. Data uses optional typed published state; reserved screen area comes from the host registry, not Bar configuration or circular module dependencies. JankyBorders remains optional external integration with explicit settings authority and non-restorability limits. All native effects/tests need per-run approval; none ran for this plan.

Lasting shipped behavior replaces only completed slices of this notice during implementation. Historical evidence remains in [legacy tiling docs](../../_legacy/docs/features/tiling.md); they do not describe current Next capabilities.
