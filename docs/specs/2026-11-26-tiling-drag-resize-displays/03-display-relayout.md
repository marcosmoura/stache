---
status: draft
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

- [ ] Scenario: two displays (2560x1440 above/right of 1920x1200), workspaces with windows on both, some hidden; the external display disconnects → every window of every workspace, hidden ones included, gets a frame inside the built-in display's usable area (the live `coding` case: Grid with five windows, previous frames at y=-184).
- [ ] Scenario: the display returns → displaced workspaces go home and every window on them, visible or hidden, is written for that display.
- [ ] Scenario: resolution change of a single display → all workspaces replanned and written.
- [ ] Hidden-application writes issue no unhide, activate or focus effects (asserted on the effect log).
- [ ] Revealing a workspace after the relayout issues no frame write for windows already at target.
- [ ] `docs/features/tiling.md` describes display-change relayout.

## Tests

End-to-end scenarios over the real sharded adapter and fake AX arena with a fake display topology, asserting final arena frames and the effect log.

## Notes
