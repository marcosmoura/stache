---
branch: feat/tiling
---

# Tiling rewrite

Read when planning the Tiling runtime rewrite. **DRAFT — NOT SHIPPED.** Each part needs separate approval; this document authorizes no native work.

## Goal

Rebuild legacy tiling as one `stache-tiling` module at `src/crates/modules/tiling`, with pure actor-owned state and safe native adapters. The identical legacy workspace/app strip in Bar's reserved UI slot is part 10, which lives in the [Tiling–Bar integration](../2026-11-08-tiling-bar/00-overview.md) spec.

## Non-goals

No macOS Spaces management, per-window hiding engine, focus-follows-mouse, persistence, Settings UI, new settings, unlimited grid redesign, competitor implementation copying or unrelated host Cmd-Q/logout repair. No legacy changes/build/automation.

## Sequencing and shipped baseline

Approved order (2026-11-08): Bar foundation → **Tiling 01–09** → Bar 03–08 → Tiling–Bar integration. Parts 01–04 may run in parallel with [Bar foundation](../2026-11-08-bar-foundation/00-overview.md) (approved 2026-11-08); parts 05–09 start only after Bar foundation merges and `feat/tiling` rebases onto it, because it supplies the published-state channel and the host screen reservations that part 05 consumes. No part waits for Bar 03–08. Both branches touch the same shared files (workspace/app manifests, `catalog.rs`, `bindings.rs`, `UNPORTED` in `cli/src/migrate.rs`, generated schema/bindings, config docs); edits stay additive and the branch that rebases second regenerates generated files instead of hand-merging them. Part 10 lives in the [Tiling–Bar integration](../2026-11-08-tiling-bar/00-overview.md) spec and runs last. Rebase each part on current main. Main `f0fc70a` also ships the Accessibility launch gate; main `f260c6f` ships noTunes, Keep Awake, Wallpapers, Keybindings, Hold-to-Quit and MenuAnywhere, including exact opaque applications identity/issuance permit/cancel/ack, the catalog migration hook, shared input and the accepted-config watch; it does **not** ship Bar's published-state or reservation proposals.

Read-only draft dependency records on `main` (branch names identify future implementation):

| Branch                | Draft contract path                                                               | Reuse after landing                                                                        |
| --------------------- | --------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `feat/wallpapers`     | [Wallpapers](../2026-11-03-wallpapers/00-overview.md), source `f13cd3a`           | Same-cell typed accepted config/revision watch; generic factory context and begin-shutdown |
| `feat/keybindings`    | [Shared input](../2026-11-03-keybindings/02-shared-input.md)                      | One lazy host input service, owner-thread tap and press ledger                             |
| `feat/hold-to-quit`   | [Applications/frontmost](../2026-11-03-hold-to-quit/03-applications-frontmost.md) | Additive exact applications/frontmost adapter, 2 s hops                                    |
| `feat/menu-anywhere`  | [Runtime](../2026-11-03-menu-anywhere/03-runtime.md)                              | Shared input priorities and actual-send authority/cleanup integration                      |
| `feat/bar-foundation` | [State/reservations](../2026-11-08-bar-foundation/02-sdk-state-reservations.md)   | Versioned optional observation and host reservations (parts 01–09)                         |
| `feat/bar`            | [UI shell parity](../2026-11-03-bar/04-ui-shell-parity.md)                        | Approved empty slot and independent parity reference (tiling-bar part 10 only)             |

No dependencies on unmerged module crates. Tiling depends only on SDK + macOS (ordinary serialization/geometry dependencies are not sibling services). Bar cannot import Tiling crate/config; Tiling cannot import Bar crate/config. App composition registers original DTO codecs and concrete generated commands/events.

## Decisions

1. Virtual workspaces retain **application-level** hide/unhide. Same-app windows cannot be isolated across workspaces/displays: a visible workspace needing that app wins; leave its other windows visible, report the conflict, never oscillate hide/unhide or claim isolation. Restore only exact instances this runtime hid; prehidden apps are not owned.
2. Preserve eight names: `dwindle` (one authoritative default), `split`, `split-vertical`, `split-horizontal`, `monocle`, `master`, `grid`, `floating`; rules are AND, appId case-insensitive exact and appName/title case-insensitive substring. Preserve system/PiP exclusions, gaps, presets, master controls, native tabs, fullscreen/minimized/hidden behavior and commands.
3. Grid alone caps at **10 tiles**. First ten eligible windows in stable layout order tile; tracked extras float without automatic preset/centering or focus displacement. Overflow is ephemeral, not the explicit user-floating flag; removal promotes the earliest eligible extra. Other layouts have no ten-window cap. See [02](02-pure-layouts.md).
4. `floating.defaultPosition=center` now centers a newly floating window on its own display once; `default` retains the application frame. An eligible explicit preset in a Floating workspace wins. `--preset` remains a silent admitted no-op outside Floating workspaces, even for an individually floating window. Legacy `preset-on-open` is a config-only field with no runtime reads; retain it inert, not a newly invented automatic policy. Grid overflow is exempt. Reload/refresh/focus never repeatedly center. Default positioning is an intentional correction, not legacy parity.
5. Applications use landed retained `NSRunningApplication` equality. Windows require owner-retained AX-instance tokens scoped by exact app + runtime epoch with revalidation; PID/CGWindowID alone, even combined, cannot authorize an action. All unsafe/platform FFI lives in `stache-macos`; no blanket unsafe Send/Sync. Public equivalent first, minimal private AX/SkyLight only behind compile/manual gates. Focus/raise and unhide-then-focus consume MenuAnywhere part 02's exact-instance activation; Tiling owns hide/unhide and adds no second activation path.
6. Actor is sole writer; pure transitions produce typed effects. Bounded semantic queues preserve focus/create/destroy ordering and reconcile loss; geometry coalesces per display at 30–360 Hz, 60 fallback. No callbacks walking AX inline; callbacks enqueue pure IDs/refresh hints only. No global runtime statics or 2,300-line init port.
7. Native effects recheck generation, accepted revision, exact window identity, plan sequence and deadline at issuance. Logical invalidation is atomic independent of a stalled send/commit gate. Native writes are not transactionally reversible: report actual Applied/Partial/Uncertain; acceptance is only Enqueued. Never auto-retry uncertain writes or fight an unresizable app.
8. Prepare fallible resources before acceptance; consume the landed same-cell accepted watch, never guessed revision + 1. Rejection preserves old runtime/publication; accepted physical failure publishes truthful actual outcome. Startup enumeration is asynchronous and reconciled; Running means owned healthy infrastructure, not every window moved.
9. Stop seals new issuance, cancels/wakes work, drains issued animations/frame writes **before** owned restore/unhide, removes observers with acknowledgements and joins off-main. Retain failed cleanup/restore ownership inertly in quarantine; retry exact resources, no restart while quarantined. Main never waits on a worker awaiting main; host cancels waits before IPC joins.
10. Optional JankyBorders Mach IPC + CLI fallback remains, styles/hidpi/state/layout overrides/gradients/glow/blacklist/8 fps unchanged. Connection ownership is not service-process/settings ownership. Never silently launch/kill/reconfigure a foreign service or send blanket width=0; unsafe shared ownership blocks part 09 for a decision, not silent integration removal.
11. Shared input remains HQ > MA > KB. Tiling passively observes drag through the service's **listen-only** observation tap (approved exception: at most one suppressing tap plus one listen-only tap that only exists while passive subscribers exist); it claims no key/click interception, injects nothing and adds no private suppressing tap. AX observation/queries/actions run on a dedicated AX owner thread per consumer, never on main and never through one serialized thread shared with MenuAnywhere. Failures degrade optional drag/borders/tabs honestly, not other modules.
12. Bar optionally observes atomic Tiling workspace/window/focus state through SDK. From part 05's first frame writes, Tiling consumes landed host per-display reservations, never `bar.height`/config. Edges are inward from full logical display bounds: per-edge exclusion is max(native visible-frame inset, host aggregated reservation), then own outer gap once. This explicitly corrects legacy double-counting. Part 10 ([Tiling–Bar integration](../2026-11-08-tiling-bar/00-overview.md)) owns the strip/final integration, not first safe reservation consumption. Bar reservation stays constant while active but slid hidden, tombstones on stop/pause/disable/failure. Neither module is the other's lifecycle prerequisite.
13. Numeric CLI focus is an approved bug fix: legacy String help advertised IDs but IPC parsed directions only. Resolve the numeric selector once to the current exact window token, then preserve every stale identity/revision/issuance gate. Keep case-insensitive focus directions including `prev`, but case-sensitive six-value swap grammar with no `prev` alias.
14. Non-Grid new windows insert after the focused window in their assigned workspace, or append without such focus, matching legacy. Grid alone appends new eligible windows to protect the existing ten tiles; this stable-cap exception is intentional. Promotions/swaps follow part 02, never focus replacement.

## Parts

| Part                            | Summary                                                    | Status |
| ------------------------------- | ---------------------------------------------------------- | ------ |
| [01](01-config-catalog.md)      | Inert typed config/catalog/migration                       | draft  |
| [02](02-pure-layouts.md)        | Pure layouts/rules/presets/min sizing and grid correction  | draft  |
| [03](03-native-adapters.md)     | Safe AX/window/display adapters; tab/private compile gates | draft  |
| [04](04-event-tracking.md)      | Bounded event pipeline/read-only tracking/fake actor       | draft  |
| [05](05-runtime-effects.md)     | Frame effects/reservation normalization/core factory       | draft  |
| [06](06-virtual-workspaces.md)  | Virtual workspaces/owned hide-unhide/recovery              | draft  |
| [07](07-ipc-cli.md)             | Full typed IPC/CLI and golden contracts                    | draft  |
| [08](08-tabs-drag-animation.md) | Native tabs/passive drag/animation                         | draft  |
| [09](09-jankyborders.md)        | Optional external JankyBorders                             | draft  |

Sequence 01 → 02 → 03 → 04 → 05 → 06 → 07 → 08 → 09. Part 10 moved to the [Tiling–Bar integration](../2026-11-08-tiling-bar/00-overview.md) spec, which merges after this spec and Bar 03–08; it keeps its number and `TI-10-*` IDs, so appendix references to part 10 point there. Each part compiles/tests/commits alone; internal seams compile before consumers. Part 05's production factory remains unregistered until 06 supplies workspace safety; 06 rejects not-yet-implemented optional features explicitly. Part 07 must not report success for unimplemented actions; later parts activate only compiled features, never placeholders. Unfinished parts block feature-branch merge.

Acceptance IDs `TI-NN-MM` have exactly one owning part; appendices contain contracts, not duplicate criteria. See [inventory](inventory.md), [CLI contract](cli-contract.md), [review lessons](review-lessons.md) and [validation](validation.md).

## Intentional compatibility corrections

Separate `tiling.jsonc`; transactional accepted reload instead of whole-app restart; unified dwindle default; rule appId fallback self-comparison fixed; grid max12 silent extras becomes max10 explicit overflow with append-only new eligibility; advertised floating positioning implemented (not dead preset-on-open behavior); advertised numeric focus implemented with exact-token safety; full-bounds max-normalized reservations instead of additive native/Bar exclusions; query failures exit1 instead of0; typed actual outcomes replace native-success assumptions; safe retained cleanup replaces drain-and-forget restoration; external borders require actual authority rather than blanket pause commands. Other successful CLI grammar/JSON/table/flag/order contracts and all Bar visuals remain unchanged. Generated transport names replace hand-coded events; no legacy socket/global plumbing is copied.

## Affected docs

The [feature notice](../../features/tiling.md) stays planned until implementation lands; the workspace strip lands only with the Tiling–Bar integration spec. Each part materializes only its shipped slice in architecture/config/sdk/ipc/ui, feature/testing/performance docs and additive index routes. Preserve all earlier catalog/schema/bindings/capability/roadmap additions. No unresolved product choice now; native feasibility, shared-service authority and independent baseline evidence are blocking gates, not assumed facts.
