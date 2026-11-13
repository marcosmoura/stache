---
status: done
---

# 02 — Bar: the dimmed workspace item and the refusal flash

## Scope

The Spaces widget in `src/ui/src/windows/Bar/widgets/Spaces/`: an inactive workspace is dimmed, and a refused focus flashes its item. Depends on part 01. No Rust change.

## Requirements

- **Data.** `Spaces.state.ts` gains two selectors over `BarView.tiling`: whether a workspace is inactive, and the identity of the last refused focus for a workspace (the strip's `generation` plus the refusal counter, or nothing). Each `Workspace` item selects only its own values, as it does for its focus today.
- **Inactive look.** The item's icon is drawn at reduced opacity through a theme token (no hard-coded value), the item has no hover background and the default cursor, and it carries `aria-disabled="true"` and a `data-inactive` attribute. It stays focusable and clickable. The focused workspace is never drawn inactive.
- **Click.** A click on an inactive item sends the same `focusWorkspace` request as any other item. The UI does not decide; Tiling refuses and the flash follows from the published refusal.
- **Flash.** When an item's refusal identity changes while it is mounted, the item flashes once: a tint in the theme's attention colour fades in and out over the item, and the icon is at full opacity for the flash. Duration and easing come from the theme's motion values. A repeated refusal restarts the flash. The identity present when the item first renders does not flash, so a reloaded Bar does not replay an old refusal.
- **Reduced motion.** The tint appears and disappears without interpolation.
- **Performance rules.** `m.*` components from `motion/react-m` only, no manual memoization, only the properties the animation lint allows. A refusal re-renders only the refused item; an `inactive` change re-renders only that item.
- **Docs.** `docs/features/bar.md`, "Tiling strip UI": the inactive look, the click and the flash.

## Acceptance criteria

- [x] IW-02-01: An inactive workspace renders dimmed with `aria-disabled` and `data-inactive`; an active one is unchanged; the focused one is never inactive.
- [x] IW-02-02: Clicking an inactive item sends exactly the `focusWorkspace` request with the rendered generation and sequence.
- [x] IW-02-03: A new refusal identity flashes the item once; a second refusal flashes again; the identity at first render does not; another workspace's refusal does nothing to this item.
- [x] IW-02-04: Under reduced motion nothing interpolates and the tint still shows and goes.
- [x] IW-02-05: Render counts hold as specified.
- [x] IW-02-06: Screenshot baselines show a strip with inactive workspaces in dark and light, wide and compact.
- [x] IW-02-07: `docs/features/bar.md` describes the shipped behaviour.
- [x] IW-02-08: `bun run lint`, `bun run format:check`, `bun run test` and `bun run build` pass.

## Tests

- Selector tests in `Spaces.state.test.ts`.
- Item tests in `Workspace.test.tsx` and `Spaces.test.tsx` for the look, the click, the flash (animations on and reduced), and the render counts.
- End-to-end in `src/ui/e2e/scenarios/spaces.spec.ts`, through the fake host as the native host would drive it: a strip modelled on the user's nine workspaces with `tasks` and `misc` inactive; a click on `tasks` records the request, the host answers `refused` and emits the changed strip with the refused focus, and the item flashes; a second refusal flashes again; a reload with the same refusal does not; the strip then marks `tasks` active and a click focuses it. No console error and no unanswered command.
- Screenshot spec for IW-02-06 in `src/ui/e2e/screenshots/`, with the Maple Mono skip the other baselines use.

## Notes

- What the user tries in the running app after this part, on one display:
  1. With a Finder window open in `files` and every other running application showing a window: `tasks` and the other empty workspaces are dimmed. Pressing their key leaves everything as it is and flashes the item. Ghostty keeps keyboard focus.
  2. Close the Finder window: the dimming goes away. The key now switches and only the desktop is left; coming back focuses Ghostty.
  3. Open the Finder window again from `files` and leave: the empty workspaces dim again.
  4. Send a window to a dimmed workspace (`Option+Shift+<n>`): it becomes active.
- The opacity value and the flash are judged by eye on the screenshot baselines and in the app; the user may ask for different values.
- Outcome: tokens `bar.inactiveOpacity` (0.4) and `duration.flash` / `durations.flash` (500 ms), transition `transitions.flash`. The flash peaks at full opacity in `color.attention`, behind the icon.
- Outcome: the item keeps the refusal identity it first rendered with in state and compares it during render; a new identity starts the flash and a `key` restarts it. Under reduced motion a timer removes the tint after the flash's duration.
- Outcome: Playwright does not click an `aria-disabled` element, so the end-to-end test forces the click; a real click is unaffected.
