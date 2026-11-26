---
status: done
---

# 03 — Display changes re-lay out every workspace

## Scope

On any display change, replan every workspace (visible and hidden) and write every eligible window right away, so no window keeps a frame from a previous display arrangement.

Files: `state/frames.rs` (`plan_and_issue`, `note_revealed`, eligibility), `state/transition.rs` (`apply_displays`), `visibility.rs` (hidden-application handling), scenario tests.

## Requirements

1. A display change (connect, disconnect, resolution or scale change, arrangement change, visible-frame or host-reservation change) marks every workspace for planning; `plan_and_issue` then plans hidden workspaces as well as visible ones for that pass, against each workspace's current display (after `apply_displays` moved displaced workspaces to main and restored returning ones).
2. Windows of hidden workspaces are written through the normal frame path (readback, outcome bookkeeping, in-flight limits). The write never unhides, activates, focuses or raises the application, and never changes a window's minimized state. Animations are off for these writes.
3. Windows that are minimized, inactive native tabs, suspended, floating or of unsettled applications keep their existing eligibility rules.
4. A later reveal of a workspace whose windows are already at their targets writes nothing extra (no jump); a window whose write was refused or uncertain is retried on reveal as today.
5. Ordinary plans with no display change keep planning visible workspaces only (no new cost per relayout).
6. A transient empty topology keeps today's behaviour (no plan, `no_screens`).

## Acceptance criteria

- [x] Scenario: two displays (2560x1440 above/right of 1920x1200), workspaces with windows on both, some hidden; the external display disconnects → every window of every workspace, hidden ones included, gets a frame inside the built-in display's usable area (the live `coding` case: Grid with five windows, previous frames at y=-184).
- [x] Scenario: the display returns → displaced workspaces go home and every window on them, visible or hidden, is written for that display.
- [x] Scenario: resolution change of a single display → all workspaces replanned and written.
- [x] Hidden-application writes issue no unhide, activate or focus effects (asserted on the effect log).
- [x] Revealing a workspace after the relayout issues no frame write for windows already at target.
- [x] `docs/features/tiling.md` describes display-change relayout.

## Tests

End-to-end scenarios over the real sharded adapter and fake AX arena with a fake display topology, asserting final arena frames and the effect log.

## Notes

- A display change (`apply_displays` with a layout that differs from the last non-empty one once screens are known; `apply_reservations` when a display's host resolution changed) puts every workspace id in `FrameBook::relayout`. `desired_frames` plans visible workspaces plus the marked ones, each with `usable_for` of its current `workspace.screen` (one area per display, shared by all its workspaces; `frames.bindings` and host reservations are indexed per display, so a hidden workspace on main uses main's area and binding like the visible one). The startup layout is not a change.
- A marked workspace leaves the set after a pass in which the in-flight limit was not hit and none of its windows has an in-flight attempt; until then its desired frames stay in the plan, so a queued write is never cancelled for having lost its target and each completion's plan issues the next write as a slot frees. Visible workspaces drop out at once (they are planned anyway). Windows skipped by eligibility (unsettled applications and so on) do not keep a workspace marked; they are written on reveal as before.
- Hidden-workspace windows are never animated. A window with `observed.hidden` is written only when its workspace is marked and hidden **and** its application is in `owned_hidden` (Stache hid it); a window of an application someone else hid is already outside the layout (`flags.hidden`) and untouched. A window of a hidden workspace whose application also has a window on a visible workspace is not hidden at all (visibility keeps the application shown), so it is written like any other marked window.
- Tests: `runtime_tests/animation/displays.rs` (real sharded adapter, fake AX arena, 2560x1440 external above-right of a 1920x1200 built-in: disconnect with a hidden Grid of five and a hidden 20-window workspace, display return, resolution change, reveal writes nothing) and `runtime_tests/scenarios.rs::a_display_change_writes_hidden_workspaces_without_showing_their_applications` (visibility ledger: a connect writes Code and Spotify while held hidden, direct, no unhide, activation or focus). All three arena scenarios and the scenario test fail on the unmodified code.
