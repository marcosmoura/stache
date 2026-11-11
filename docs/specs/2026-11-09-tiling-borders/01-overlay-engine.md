---
status: done
---

# 01 — Overlay window engine

Read when implementing after separate approval (`fix/tiling-fixes` merged as `ba51f20`). Paths are under `src/crates/`. This part adds a native adapter only; nothing in Tiling uses it yet and production is unchanged.

## Scope

New `macos/src/border_overlay/` (`mod.rs`, `control.rs`, `native.rs`, `testing.rs`, `tests.rs`), exported from `macos/src/lib.rs`, behind the `native-borders` cargo feature of `stache-macos` (decision 1). Pattern: the Hold-to-Quit HUD (`macos/src/hold_hud.rs`): a `Send + Sync` control that holds plain state and a `MainThreadExecutor` (`sdk/src/threading.rs:321`), and a main-thread-local owner map of Cocoa objects keyed by opaque handles. Studied for approach (decision 5): JankyBorders `src/border.c` (`border_create_window`, `border_update_internal`, `border_hide`/`border_unhide`), `src/misc/window.h` (`window_create`, `window_level`, `window_sub_level`), `src/windows.c` (`windows_window_create`, corner radii).

## Requirements

- **Control API (plain data).** `OverlayControl` exposes `apply(batch)` where a batch is a list of `OverlayCommand { handle, kind }` with kinds `Show { target_cg_id, frame, appearance }`, `Move { frame }`, `Order { target_cg_id }`, `Hide`, `Release`, plus the control-wide `HideAll { reason }`. Frames are global top-left coordinates in points (Tiling's `Rect`); the main-thread side converts to AppKit bottom-left coordinates against the primary screen. `appearance` is a plain `BorderAppearance` value (part 01 needs only solid color, width, style and hidpi; part 02 extends it).
- **Never block main.** `apply` enqueues one job per batch through the executor and returns at once; callers off main may wait for the job's acknowledgement with a bounded deadline (`OverlayAck`), main never waits. A batch is applied in order inside one `CATransaction` with implicit actions disabled, so moves and color changes never animate implicitly.
- **Latest wins.** At most one pending batch per control; a newer batch merges per handle (latest command kind per handle, `Release` and `Hide` never dropped) instead of queueing. Pending handles are bounded by `MAX_OVERLAYS` (proposed 64); excess `Show` commands are refused with a typed outcome and a counter.
- **Overlay window.** One borderless, non-opaque, clear `NSPanel` per handle with `ignoresMouseEvents`, non-activating style, `canBecomeKey`/`canBecomeMain` false, no shadow, `isReleasedWhenClosed` false, `hidesOnDeactivate` false, `sharingType = .none` (best effort; whether screenshots and screen sharing still include it is a gate item). The collection behavior is the one [part 05](05-spaces-mission-control.md) specifies (Exposé-hidden, out of the window cycle, never joining all Spaces); this part sets it at creation and exposes it as a constant so part 05's tests can assert it. The border is drawn **outside** the target frame: the overlay frame is the target frame outset by `width` plus a small fixed padding, and the stroke's inner edge follows the target's corner radius.
- **Ordering.** The overlay takes the target window's level (normal level unless the target reports otherwise) and is ordered directly above the target with the public `orderWindow(_:relativeTo:)`, passing the target's `CGWindowID` (`WindowSnapshot.cg_window_id`, `macos/src/windows/events.rs:90`). Whether AppKit accepts another process's window number is **the** compile and native gate of this part. If it does not, the fallback orders the overlay at the target's level in front of the target's application only after Tiling raises the target, and the diagnostic `borders: overlays cannot be ordered relative to other applications' windows` is published; a gated private SkyLight ordering transaction may replace it only after its own approved native check.
- **Spaces.** An overlay belongs to the macOS Space where it was last ordered in and is never on all Spaces. Which overlays may be shown during Space changes, Mission Control and fullscreen transitions is decided by [part 05](05-spaces-mission-control.md); this part only provides `HideAll { reason }` as a batch command that is applied before any other pending command and is never merged away.
- **Corner radius.** `round` uses the target's corner radius: the private `SLSWindowIteratorGetCornerRadii` resolved by `dlsym` at runtime (macOS 26 only; resolved OQ-4), else a measured per-OS constant recorded by the native check; `square` uses 0. A missing symbol is a capability fact in status, not an error.
- **Scale.** Layer `contentsScale` is the display's `backingScaleFactor` when `hidpi` is on (default), 1.0 when off; it updates when the overlay changes display.
- **Pool.** Released overlays return to a bounded pool (ordered out, layers kept) instead of being destroyed; the pool is trimmed to zero on `release_all`. `release_all` orders out and closes every overlay and acknowledges how many it closed; it is the only teardown path ([part 07](07-lifecycle.md) calls it).
- Disabled or uninstalled, the adapter owns no panel, layer, timer or observer.

## Acceptance criteria

- [x] TB-01-01: With a fake executor and fake native backend, `apply` never runs native code off main, never waits on main, merges latest-wins per handle within `MAX_OVERLAYS`, and keeps `Hide`/`Release` ordering; `HideAll` is applied ahead of every other pending command and never merged away; acknowledgements are bounded and typed.
- [x] TB-01-02: Coordinate conversion, outset frame, corner radius (symbol present, absent, zero) and scale selection are pure functions with tests across two displays with different scales and a non-zero primary origin.
- [x] TB-01-03: Pool reuse, `release_all` and refusal at the cap leave no live overlay in the fake owner map; each capability gap (relative ordering, corner radius, Space) has one status diagnostic.

## Tests

Unit tests in `macos/src/border_overlay/tests.rs` with `testing.rs` fakes (`FakeOverlayBackend` recording commands, a queued fake executor): batch merge, cap refusal, hide-after-show in one batch, release during pending show, coordinate flips, outset with width 0 and large widths, pool trim. Default tests create no real panel.

## Native gate (per-run approval)

Compile on macOS 26, then on an owned test application window: `orderWindow(_:relativeTo:)` with a foreign `CGWindowID` (accepted, ignored or error), z-order relative to the target and to a third window, screenshot/screen-sharing inclusion with `sharingType = .none`, `SLSWindowIteratorGetCornerRadii` presence and values for at least Ghostty and Spotify, and that the overlay never takes focus or clicks. Space and Mission Control behavior is part 05's gate.

## Review checklist

- [x] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: inspired by, not derived from, JankyBorders; the files above were studied, none copied, translated or closely paraphrased.

## Notes

- Rollback: delete the module; nothing uses it.
- Overlays are Stache's own windows, so Tiling never tracks them (`modules/tiling/src/state/transition.rs:177`, self bundle id exclusion); keep that exclusion covered by a test.
- Unverified until the gate: everything native above.
- Implemented (2026-11-10): `OrderingMode::RelativeToTarget` by default; `RaiseFallback` is set through `OverlayControl::set_ordering_mode` from the gate's verdict, since AppKit gives no signal that a foreign window number was ignored. `SLSWindowIteratorGetCornerRadii` is only detected (`dlsym`), not called: its signature is not public, so the call is wired only after the native gate; until then `round` uses the provisional `fallback_corner_radius` (16 pt on macOS 26, unverified) and the corner-radius diagnostic is shown.
- A `Show` merged into the same pending batch after a `HideAll` is still applied after it; the stale-epoch drop is part 05 (TB-05-02).
- Native run 1 (2026-11-10, approved, build `f8754c5`, the user's own `tiling.toml` with animations on): borders render around the focused window. Findings: (1) ordered **above** its target, a border flips under the window whenever the user clicks the application (macOS raises the window after Stache's re-order); (2) the visible border looks about 1.5× as thick as JankyBorders with the same `width`; (3) the inner corner radius should be about 1 pt larger; (4) borders lag behind manual drags and resizes (expected until [part 06](06-live-follow.md)).
- Decisions from that run (user, 2026-11-10): placement is configurable, `below` (default) or `above` its target window; with `below` the ring extends under the window edge and the window hides the inner part. Two thirds of the configured width is visible outside the window. The inner radius is the window radius plus 1 pt. The `tiling.toml` key ships in [part 08](08-migration-removal.md) with the other additive fields; until then the engine uses the default.
- Docs: owner map in [architecture/overview.md](../../architecture/overview.md); fakes in [guides/testing.md](../../guides/testing.md).
- Native run 2 (2026-11-10): `below` placement confirmed (the border stayed under its window on every click). Two thirds of the width looked thinner than JankyBorders; the user chose five sixths (`VISIBLE_FRACTION = 5/6`).
- `VISIBLE_FRACTION` = 5/6 (user choice after native run 2, 2026-11-10; calibration history: run 1 drew the full width and looked 1.5 times too thick, run 2 drew two thirds and looked too thin). The decision above that says two thirds is superseded; the hidden part under the window is now one sixth of the width.
- Native run 3 (2026-11-10): five sixths of the width looked right to the user ("so much better").
- OQ-4 delivered (2026-11-11, user requirement: radii differ per application on macOS 26): `SystemCornerRadiusResolver` (`border_overlay/corner_radius.rs`) runs the private window query `SLSWindowQueryWindows` → `SLSWindowQueryResultCopyWindows` → `SLSWindowIteratorAdvance` → `SLSWindowIteratorGetWindowID` / `SLSWindowIteratorGetCornerRadii` on `SLSMainConnectionID`, all `dlsym`-resolved lazily (any missing one keeps `FallbackOnly` and its diagnostic; all present sets `RadiusCapability::Available` through `OverlayControl::set_radius_capability`). Signatures are written from the commonly described shapes, not from any border tool's headers, and are **unverified until a native check**; the first radius must be a `CFNumber` in a `CFArray`, for the queried id, finite and within 0–64 pt, else the 16 pt fallback is used. Cached per window id; `Order`, a new handle and `Release` forget it. The inner radius stays resolved + 1 pt below and + 2 pt above.
- Native run 15 (2026-11-11): the private corner-radius query worked; every window queried (Ghostty, Zed, Spotify and others) reported 16 pt, and the user judged the corners right. Distinct per-app radii were not observed on this setup.
