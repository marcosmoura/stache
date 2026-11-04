---
status: draft
---

# 10 — Bar strip, reservations and final integration gates

Read when implementing after 09 and separate approval. Bar foundation and independent parity baseline must already have landed and been exercised.

## Scope

Tiling `src/published.rs` (reservation consumption itself landed in 05); app `bindings.rs`, concrete Tiling command/event bridge, capability authorization and host schema registration; UI `src/ui/src/bar/components/Spaces/` with App/AppList/Workspace/WorkspaceList directories, colocated tests/styles/index, SDK client/query/feed wrappers. Use existing Bar shell's first empty slot. Map inventory bridge/Spaces/icon/transitive rows. This is data/UI integration, not a second native Bar implementation.

## Requirements

- Reuse landed SDK provider ID + schema identity/version registration and generation-scoped publisher; Tiling owns original typed workspace/window/focus DTO snapshot. Envelope carries exact runtime owner/generation, accepted config revision, state sequence, schema identity; all u64 crossing JS are opaque decimal strings. Atomic payload/revision, publish cell before bounded notification, subscription-before-fetch/race-free resnapshot, stop tombstone rejects late writers. Never use lossy EventBus alone as state.
- Bar declares optional `observes` Tiling, not hard `requires`. App registers original Tiling schema codec and uses concrete generated tauri-specta commands/events; SDK/Bar never imports Tiling types/crate/config. Host projection passes controlled display data to UI, not arbitrary JSON pretending to be another provider. Commands target exact currently resolved identities/generation/sequence, revalidate at actual focus action. UI cannot inject raw native tokens/pointers or bypass module enable/quarantine. No generic arbitrary command dispatcher.
- Bar absent/disabled/stopped/failed and Tiling absent/disabled/paused/failed work independently: no circular lifecycle requirement; other status components keep running, workspace slot becomes original empty `<div />`. Tiling still manages windows without Bar; Bar still shows status without Tiling. Snapshot failure is explicit query/cache state, never fabricated stale active provider.
- End-to-end reservation lifecycle with landed 05 normalization: verify from the real host bridge that an active Bar's constant top inset applies while slid hidden, an exact-generation tombstone clears on stop/pause/disable/failure, a failed/pending panel never reserves as loaded, and a stale release cannot clear a successor. Reuse 05's per-edge max mathematics; do not re-implement or re-own it here, and keep no `bar.height`/config reads.
- **Visuals identical:** port actual legacy `Spaces/**`, AppList, icon maps and transitive primitives/hooks/styles, no redesign/extra badge/tooltip/loading treatment. Preserve workspace order terminal/coding/browser/music/design/communication/guitar/misc/files/tasks then unknown alphabetical, exact icons/maps including hidden-mark WhatsApp and Microsoft Edge Dev, fallback app icon, unknown workspace icon behavior. Preserve app order per focused workspace, visible/nonminimized filtering, duplicate-name title selection/truncation (40 characters + `...`, laptop25), focused-label-only appearance and laptop focused-app reduction at viewport width<=2036.
- Preserve grid-auto-flow/6px gap/0 10px padding, opacity0.68→1/crust focus, workspace surface1 indicator radius12 and motion spring bounce0/durationSlower, app width0↔auto/opacity0↔1 and length-scaled motion duration, loading fallback, reveal/exit/intermediate animation behavior. Reuse approved Bar fonts/colors/spacing/Surface/Button/Icon computed values; no dependency-induced icon changes.
- Extend **independent** Bar reference manifest with pinned legacy Spaces/AppList/maps/transitive source object/hash/provenance, font/browser/OS/scale/profile/data/animation timeline. Reference is extracted read-only markup/CSS/SVG reviewed against originals or user-approved screenshots with measurements; never derived from new components/helpers or baseline auto-updates. No legacy runtime/build/automation. Missing font/baseline is blocked/unverified, not similarity pass. Exact screenshots, computed geometry/styles/SVG and animation samples; narrowly justified raster tolerance requires explicit approval.
- React Query owns canonical snapshots with generation/sequence ordering, subscribe-before-fetch and stale-response rejection; zustand UI-local only. StrictMode/late unlisten disposal, provider disappearance and schema mismatch remain bounded/inert. Keep existing generated surface/capability least privilege and earlier UI unchanged.
- Final integration exercises **landed** accepted-watch, host shutdown ordering, input HQ>MA>KB with passive Tiling, Bar reservations/optional state, noTunes applications identity and sibling disabled/failure recovery. Publish actual native limitations/capabilities and performance evidence from [validation](validation.md), not an unsupported universal-window claim.

## Acceptance criteria

- [ ] TI-10-01: Typed versioned optional state/host-generated actions compile without Bar↔Tiling crate/config imports; snapshot races/tombstones/schema/identity tests keep unrelated modules active.
- [ ] TI-10-02: End-to-end Bar reservation lifecycle (constant while slid hidden, exact-generation tombstone, failed panel not reserving, stale release) passes on 05's landed normalization without config reads or duplicated math.
- [ ] TI-10-03: Independent legacy visual manifest and exact screenshot/style/geometry/SVG/animation matrix pass for Spaces/AppList/maps/responsive/loading/empty states without redesign.
- [ ] TI-10-04: Final fake safety/integration/CLI suites and approved native/performance evidence meet validation gates; every unexecuted gate remains explicit and lasting docs match observed behavior.

## Tests

Fake providers/reservations with absent/disabled/mismatch/stale/new generations, A–B–A accepted revisions, old initial response after newer event, overflow and cancel while waiting. Colocated Vitest/WebKit at1440/2560 and2035/2036/2037 viewport, scale1/2, known/unknown workspaces/apps, duplicate title/Unicode/long names, focus/no-focus, tab/minimized/hidden, loading/error/empty, reduced motion and t=0/mid/end transitions. Negative perturbation tests change icon/font/spacing/reference provenance to prove parity gate catches them. Router/client serde through real host bridge with fake modules; no native surface by default.

## Rollback and affected docs

Remove exact publisher/subscription/reservation consumers and strip slot integration after cancellation, preserving Bar shell/providers and earlier modules. Revert through supported lifecycle; no external geometry restoration promise. Materialize feature and `architecture/{sdk,ui,overview,config,ipc}.md`, testing/performance/index/roadmap for shipped facts only. No acceptance while independent baseline, native safety or performance gates are silently assumed.
