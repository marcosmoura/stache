---
status: done
---

# 02 — Bar shell and shared components

## Scope

Everything the widgets share: the Bar's root rules and slide-away, the `BarItem` and `ScrollingLabel` components, compact mode, and the command mutations. Depends on part 01. No widget is registered yet, so the Bar stays empty.

## Requirements

### Tokens

- Extend the semantic tokens with what today's Bar uses and the foundation lacks, without adding one-off values to components: the Bar text style (13px, weight 500, line height 1.6, tabular numbers), the item radius (12px), item padding (10px inline) and inner gap (6px), the gap between items (4px), the item rest and hover surfaces and hover border, and the status colors used by widgets (red, yellow, green, sky, and text) for both themes. Dark values equal today's Mocha values; light values are the Latte counterparts.
- Motion values gain today's durations and easing for item fade-in, hover transitions, the Bar slide and the Spaces spring, in `theme/motion.ts` and the matching StyleX consts.

### Bar window

- Root rules for the Bar window only: the font stack and text style above, no text selection, no dragging, images and SVGs as blocks, hidden overflow, full height.
- The context menu is disabled in the Bar window.
- **Slide-away.** When the snapshot is `available` and `menu.state` is `visible`, the Bar translates down by its own height and fades to transparent; any other outcome or menu state shows it. The transition uses the motion values and is instant under reduced motion.
- Zones keep the foundation's behavior. Items in a zone are separated by the item gap.

### Components

- `windows/Bar/components/BarItem/` (moved there by the user on 2026-11-11): the rounded item. Props: `onClick` (optional; with it the item is a `button`, without it a `div`), `active` (optional), `label` (accessible name for icon-only items), `children`, restricted `style`. Rest and hover looks, hover and active transitions, the fade-in on mount (opacity only) and full Bar height follow today's item. Keyboard focus is visible. Disabled-motion and reduced-motion behavior come from the Motion provider.
- `components/ScrollingLabel/`: shows text in a width-limited box; when the text overflows, it runs as a one-way marquee with a start pause and faded edges, as today; when it fits, it is static. It re-measures when the text or the box width changes, and does not animate under reduced motion (the text is truncated with an ellipsis instead).
- `hooks/useMediaQuery` (subscribes with `useSyncExternalStore`) and a `compact` media query const for `width <= 2036px`.
- A widget-level error fallback is already in `WidgetSlot`; add nothing visible for errors.

### Commands

- `queries/barActions.ts`: mutation option factories for `bar_action` (closed action enum), `bar_session_action` (Keep Awake pause or resume) and `bar_tiling_action` (focus workspace or window with the rendered generation and sequence). Each resolves with the typed outcome; a hook per command logs any outcome other than success with `console.debug` and draws nothing. A click while a previous call of the same action is pending is ignored.

## Acceptance criteria

- [x] MIG-02-01: Every new token resolves to today's Mocha value in dark and its Latte counterpart in light.
- [x] MIG-02-02: The Bar slides away only for `available` with `menu.state = "visible"`, returns for every other state, and does both instantly under reduced motion.
- [x] MIG-02-03: `BarItem` renders a button only when clickable, shows rest, hover, active and focus-visible looks from tokens in both themes, and fades in once on mount.
- [x] MIG-02-04: `ScrollingLabel` is static when the text fits, scrolls when it overflows, re-measures on text and width changes, and truncates under reduced motion.
- [x] MIG-02-05: Each mutation sends exactly the typed request, ignores a second click while pending, logs non-success outcomes and changes no DOM.
- [x] MIG-02-06: The context menu does not open in the Bar window; text cannot be selected.
- [x] MIG-02-07: `docs/architecture/ui.md` documents `BarItem`, `ScrollingLabel`, compact mode and the command mutations; `docs/features/bar.md` restores the shell and click-action sections as shipped.
- [x] MIG-02-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Token tests for the additions in both themes.
- `Bar.test.tsx`: slide-away matrix over outcomes and menu states, reduced motion, context menu.
- `BarItem.test.tsx`, `ScrollingLabel.test.tsx` (with real layout in WebKit), `useMediaQuery.test.tsx`.
- `barActions.test.tsx` against the mocked IPC boundary for every outcome kind.
- End-to-end `bar-shell.spec.ts`: boot with a fixture widget built on `BarItem`; emit menu visible then hidden and assert the Bar leaves and returns; click the fixture and assert one recorded command.
- Screenshot baselines for `BarItem` (rest, hover, active) in dark and light, under the font rule in overview decision 9.

## Notes

- Read today's values from `main` (`Bar.styles.tsx`, `Bar.root.css`, the Surface, Button, Stack and ScrollingLabel styles, `sdk/tokens.ts`) as numbers to reproduce, not as code to port.
- Outcome: a `defineConsts` media query failed as a style key in dev and tests because the StyleX plugin could serve CSS before the token file was transformed. `server.warmup` for `theme/*.stylex.ts` fixes it, so style files can use `[media.compact]`; the slide-away stays on Motion.
- Outcome: `ScrollingLabel` was first built as back-and-forth scrolling, which this part wrongly described as today's behavior. It was rebuilt as today's one-way marquee (user decision 2026-11-11).
- Outcome (user decision 2026-11-11): the marquee no longer waits for the whole label to leave. The label enters from the right with no start pause and a copy follows it at a gap of 50% of the box width, looping seamlessly.
