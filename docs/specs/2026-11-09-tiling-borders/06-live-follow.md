---
status: draft
---

# 06 — Live follow during user drags and resizes

Read when implementing after part 05 and separate approval. Paths are under `src/crates/`. Still behind `native-borders`.

## Scope

New `macos/src/skylight/window_events.rs` (gated private notifications, `dlsym`-resolved, reusing part 05's registration helper in `skylight/space_events.rs`) with a fake; the shared display-link adapter `macos/src/display_link.rs` (owned by whichever of this part and [animation part 03](../2026-11-09-tiling-animation/03-display-pacing-size.md) lands first); `modules/tiling/src/borders/engine.rs` (live mode) and `drag.rs` only to read the gesture state. Studied for approach (decision 5): JankyBorders `src/events.c` (`events_register`: `SLSRegisterNotifyProc` for move, resize, reorder, hide, unhide, close; ignoring its own windows by owner pid), `src/windows.c` (`windows_update_notifications`: `SLSRequestNotificationsForWindows` with the bordered window ids), `src/border.c` (`border_move`: reads the target bounds and moves the border in a transaction on a background queue), `src/animation.c` (display-link pacing).

## Requirements

- **Capability.** At engine start, resolve the needed private symbols (notification registration, per-window subscription, window bounds read) with runtime `dlsym` against SkyLight, the same policy as `macos/src/ax/window_id.rs`. Any missing symbol, a registration failure or a non-macOS-26 build makes live follow unavailable with the diagnostic `borders: live follow unavailable (<reason>); borders hide while a window moves`. Nothing else is disabled.
- **Owner confinement.** One owner thread `stache-tiling-borders-events` owns the registration and its run loop; callbacks only copy `(event, cg_window_id)` into a bounded latest-per-window slot and wake the follower; no allocation beyond fixed buffers, no lock held while calling out, no AX call. Events for windows that are not currently bordered, or owned by Stache, are dropped. Subscription is updated (bounded, coalesced) when the bordered set changes; the subscribed set never exceeds `MAX_OVERLAYS`.
- **Display-paced follow.** While at least one bordered window reports move or resize events, a display link for its display ticks; on each tick the follower reads the latest bounds of each moving window (one private bounds read per window per tick, never AX) and sends one coalesced `Move` batch to the overlay control. The link stops after a quiet period (proposed 100 ms without events); idle owns no link. Stop guarantees no callback after release (shared adapter rule from animation part 03).
- **Authority with Tiling.** Live bounds win only while no Tiling write for that window is in flight; a Tiling frame event ([part 04](04-tiling-coupling.md)) for the current plan always supersedes a live sample. Live follow never writes the target window, never feeds Tiling layout state and never changes Tiling's drag gesture outcome (`drag.rs` stays the only drag decision).
- **Hide and order events.** Private hide, unhide and reorder events, when available, only hide a border earlier (hide) or re-order it above its target (reorder). An unhide event never shows a border by itself; part 04's show rule and part 05's Space gate still decide. A live `Move` is computed under the current gate epoch, so it is dropped on main after a `HideAll` (part 05). Close events release the overlay.
- **Public fallback.** Without live follow: when Tiling observes a moved/resized sample for a bordered window that it did not cause (`events/geometry.rs` coalesced samples) or the drag gesture starts (mouse down on a bordered window), the border hides; it shows again at the next confirmed frame after the gesture ends (mouse up and settled snapshot) under part 04's show rule. No AX observation is added for borders.

## Acceptance criteria

- [ ] TB-06-01: Capability resolution is total: every missing symbol or failed registration yields the fallback with one diagnostic; fakes prove the owner thread, bounded slot, dropped foreign/own events, subscription cap and no lock across callbacks.
- [ ] TB-06-02: With a fake display link and fake events, a moving window's border moves once per tick using the latest bounds, the link starts on first event and stops after the quiet period, Tiling frame events supersede live samples, and live follow never produces a Tiling input or window write.
- [ ] TB-06-03: The fallback hides on an external move or drag start and shows at the settled frame after the gesture; private unhide events never show a border.

## Tests

Fakes for the notification source (inject move, resize, reorder, hide, unhide, close for own, foreign and unknown ids), the bounds reader and the display link; engine tests for drag, resize, concurrent Tiling animation, quiet-period stop, stop with a held callback, and fallback sequences. Default tests never call SkyLight or create a display link.

## Native gate (per-run approval)

Compile with symbols resolved on macOS 26; then on an owned test window: drag and resize for 30 s each at the display's refresh, record border lag per frame (border bounds vs target bounds read in the same tick), CPU and wakeups during and 10 s after; verify own-window filtering and that no event arrives after stop. Repeat with symbols forced absent to check the fallback.

## Review checklist

- [ ] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: notification use inspired by, not derived from, `events.c`/`windows.c`/`border.c`; private declarations written from observed signatures, not copied headers (`src/misc/extern.h` is not copied).

## Notes

- Rollback: remove live mode; the public fallback remains.
- Unverified: private event ids and payloads on macOS 26, callback thread, bounds read cost, ProMotion cadence.
- Docs: none lasting until part 08.
