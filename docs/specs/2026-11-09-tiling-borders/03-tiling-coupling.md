---
status: draft
---

# 03 — Tiling coupling

Read when implementing after parts 01–02 and separate approval. Paths are under `src/crates/`. This part contains the blink fix. Still behind `native-borders`; production keeps JankyBorders.

## Scope

New `modules/tiling/src/borders/{model.rs,engine.rs}` (pure border model and the engine that diffs it into overlay batches), `actor/mod.rs` (feed the engine after each published turn, next to `border_focus` at `actor/mod.rs:1000–1012`), `actor/frames.rs` and `animation/worker.rs` (frame events), the `HideApp`/`UnhideApp`/`Focus` arms of the effect runner (`actor/mod.rs:298–315`), `tracking.rs` (compose the engine instead of `BordersControl` when the feature is on) and `app/src/host.rs` (compose the overlay adapter under the feature). Studied for approach (decision 5): JankyBorders `src/events.c` (`window_modify_handler`, `space_handler`, `front_app_handler`) and `src/windows.c` (`windows_window_hide`/`windows_window_unhide`, `windows_determine_and_focus_active_window`): JankyBorders reacts to the window server after the fact; Stache instead orders border changes around its own effects.

## Requirements

- **Model.** A pure `border_model(&TilingState, &BordersConfig) -> BorderModel` lists, per exact `WindowInstance`, whether it is bordered, its frame, focused flag and appearance. Bordered means decision 2: tracked and managed, tiled or floating, on a visible workspace, not ignored (tiling `ignore` and `borders.ignore` rules, the system skip lists of `policy::blacklist`, matched per OQ-6 default against the application's bundle id and name), not minimized, fullscreen or hidden (`ObservedWindow`, `state/mod.rs:464–482`), not a suspended slot member or inactive tab (`state/slots.rs`), with a known `cg_window_id` and a frame. Focused window: `focused_state` (legacy precedence); others: `unfocused` (OQ-3 default). A disabled state means no overlay for that window.
- **Diff, not rebuild.** The engine keeps one overlay handle per bordered window and sends only differences: `Show` for new, `Hide`/`Release` for gone, appearance changes for focus flips (recolor only), `Move` for frame changes. A configuration reload recomputes appearances in place.
- **Frame source.** The border frame is the frame Tiling last wrote or confirmed for the window's current plan: a direct write's target when it is dispatched, each animation frame when it is written (same tick), and the readback or observed frame once known (a refused or partial write corrects the border to the observed frame). A frame event for an older plan sequence than the engine's current one for that window is ignored.
- **`BorderFrames` seam.** The frame paths report `(window, plan_sequence, frame, kind)` through one bounded, non-blocking `BorderFrames` trait: today from `FrameControl::execute` (`actor/frames.rs:167`) and `AnimationSink::write` (`animation/worker.rs:64`); after the [animation spec](../2026-11-09-tiling-animation/02-async-frame-pipeline.md) part 02, from the per-tick batch dispatch and acknowledgements. The frame path never waits for borders.
- **Hide before the application hides.** Before the effect runner issues `HideApp` for an application, the engine hides that application's overlays and waits for the main-thread acknowledgement for at most `BORDER_HIDE_WAIT` (proposed 32 ms, two frames at 60 Hz). On timeout the hide proceeds and a counter records a late border hide; the hide is never cancelled or delayed further for borders. Windows leaving the visible workspace without an application hide (same application on two workspaces) are hidden in the same turn before their frame write.
- **Show only after visible.** A window that becomes bordered by a reveal (workspace switch, display return, unhide, external show) stays hidden until all hold: its application is observed shown (`Observation::Shown`, `visibility.rs:674`, or never hidden), its reveal frame write has completed (the `revealed` set, `state/frames.rs:158–165`, or a fresh snapshot after an external show), and then one display frame has passed. Only then is `Show` sent. This ordering is the blink fix; whether one frame suffices for every application is a native gate item.
- **External hide and show.** A user ⌘H is observed after the fact; overlays of that application hide in the turn the `Hidden` observation arrives (bounded lag, documented). A user unhide or Cmd-Tab follows the show rule above.
- **Focus and raise.** Focus changes recolor overlays in place. When Tiling focuses or raises a window (`Effect::Focus`), its overlay is re-ordered above it after the raise; when another bordered window is observed focused, the same re-order runs. Without part 04's reorder notifications, a raise Tiling did not see may leave a border under another window until the next focus event (documented limit).
- **Tabs.** A slot's border follows its active member; on handover the old member's overlay is released and the new member gets one after the show rule. `slot_held` keeps the existing overlay in place.
- **Spaces and fullscreen.** On a macOS Space change every overlay hides until Tiling's next confirmed snapshot of its window; fullscreen windows have none.
- **Failure.** Overlay refusal, a missing `cg_window_id` or an ordering failure affects that border only, with a status diagnostic. The engine never submits Tiling inputs that change layout, focus or visibility.

## Acceptance criteria

- [ ] TB-03-01: The pure model includes exactly the decision 2 windows across tiled, floating, ignored, system, minimized, fullscreen, hidden, suspended, inactive-tab, invisible-workspace and unknown-`cg_window_id` fixtures, with today's precedence and OQ-1/OQ-3 defaults.
- [ ] TB-03-02: With fakes, a workspace switch hides the outgoing application's overlays and receives their acknowledgement before the fake `HideApp` send (or times out after `BORDER_HIDE_WAIT` with a counter); revealed windows show only after the shown observation, the completed reveal write and one fake frame tick, in that order.
- [ ] TB-03-03: Borders follow direct writes, every animation frame and the corrected readback for the current plan only; stale plan sequences, refused and partial writes are handled; focus changes recolor and reorder without recreating overlays; tab handover and `slot_held` behave as specified.

## Tests

Pure model tests in `borders/model.rs`; engine tests with the fake overlay backend from part 01, a fake clock and fake frame events; actor/runner tests using the existing fake backend for switch, reveal, external hide/show, Space change, focus and tab handover sequences. Default tests create no real overlay.

## Native gate (per-run approval)

On owned test instances of Ghostty and Spotify (exact instances named in the approval) with a temporary Next config and two workspaces: repeat 20 workspace switches and 20 ⌘H/Cmd-Tab cycles; record any border blink (user judgment plus overlay command log with timestamps), late-hide counters and border lag after unhide. Pass: no blink in either application.

## Review checklist

- [ ] [Hard rule checklist](00-overview.md#hard-rule-review-checklist-every-part) passed: event handling inspired by, not derived from, `events.c`/`windows.c`.

## Notes

- Rollback: compose `BordersControl` again; parts 01–02 stay unused.
- Unverified: whether one frame after the reveal write is enough, and Space-change timing.
- Docs: none lasting until part 06.
