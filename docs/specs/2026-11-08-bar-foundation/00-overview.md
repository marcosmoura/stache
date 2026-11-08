---
branch: feat/bar-foundation
---

# Bar foundation

Read when checking the first slice of the Bar rewrite: the inert `bar` configuration and the SDK published-state and screen-reservation contracts. Both parts are done on `feat/bar-foundation`; this record does not authorize further implementation.

## Goal

Land the inert `bar` configuration and the generic SDK contracts that Tiling and the later Bar parts consume: the typed published-state channel with optional `observes`, the host-owned screen reservation registry and the SDK-side typed display ID. No Bar window, provider or UI ships here.

## Non-goals

- No production Bar factory, panel window, provider, generated UI command or UI route. `BarConfig` is registered in the catalog as inert configuration only.
- No Keep Awake provider (Bar part 08) and no Tiling code.
- Everything listed as a non-goal in the [Bar overview](../2026-11-03-bar/00-overview.md#non-goals).

## Decisions

This spec is the first slice of the Bar rewrite, split out on 2026-11-08 so it can merge before Tiling (see [Sequencing](#sequencing)). The following apply unchanged and are not duplicated here:

- [Decisions](../2026-11-03-bar/00-overview.md#decisions), [intentional compatibility changes](../2026-11-03-bar/00-overview.md#intentional-compatibility-changes), [integration order and reuse rules](../2026-11-03-bar/00-overview.md#integration-order) and [validation and safety](../2026-11-03-bar/00-overview.md#validation-and-safety) in the Bar overview.
- The C01–C20 audit in the Bar [review lessons](../2026-11-03-bar/review-lessons.md).

Part numbers 03–08 and "overview Decision N" inside these parts refer to the [Bar spec](../2026-11-03-bar/00-overview.md). Acceptance IDs keep their `BAR-01-*` and `BAR-02-*` names.

## Sequencing

Approved order (2026-11-08): **Bar foundation** → [Tiling](../2026-11-03-tiling/00-overview.md) 01–09 → [Bar](../2026-11-03-bar/00-overview.md) 03–08 → [Tiling–Bar integration](../2026-11-08-tiling-bar/00-overview.md). Each slice has its own spec directory and branch because a branch merges only when every part in its spec is done, and no branch may depend on an unmerged module branch.

Downstream consumers after this spec merges:

- Tiling 05 consumes the host screen reservations and display ID from part 02.
- Bar 03–08 build on both parts; Bar 08 publishes the first real provider through the part 02 channel.

## Parts

| Part                               | Summary                                                                       | Status |
| ---------------------------------- | ----------------------------------------------------------------------------- | ------ |
| [01](01-config-catalog.md)         | `BarConfig`, catalog, schema/bindings, typed migration (inert config only)    | done   |
| [02](02-sdk-state-reservations.md) | Published-state channel, change notifier, screen reservation registry (fakes) | done   |

Dependency chain: 01 → 02. Every part builds, tests and commits alone; unfinished parts block merge.

## Affected docs

Parts update [config](../../architecture/config.md), [sdk](../../architecture/sdk.md), [new-module](../../guides/new-module.md), [testing](../../guides/testing.md) and the index only for the slice that ships. The [planned Bar notice](../../features/bar.md) stays NOT SHIPPED.
