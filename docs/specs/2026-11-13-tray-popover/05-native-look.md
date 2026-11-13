---
status: done
---

# 05 — Native look

## Scope

Restyle the popover (shell, Overview, settings editor) so it reads as a native macOS menu-bar extra instead of using the Catppuccin palette (user request 2026-11-13, after part 04). Only the popover changes: the Bar, the Test Window and the Accessibility guide keep their current palette.

Files expected to change: `src/ui/src/theme/tokens.stylex.ts` (the `popover` group stops reading the Catppuccin palette), `src/ui/src/windows/Popover/**` styles, `src/crates/macos/src/panel.rs` (only if the material or corner radius changes), the popover screenshot baselines, and `docs/architecture/ui.md` / `docs/features/popover.md`.

## Requirements

- **Colours.** The popover uses the system's own semantic colours through WebKit's `-apple-system-*` CSS colour keywords (`-apple-system-label`, `-apple-system-secondary-label`, `-apple-system-tertiary-label`, `-apple-system-quaternary-label`, `-apple-system-separator`, `-apple-system-control-background`, `-apple-system-selected-content-background`, and so on) and the user's accent colour (`-apple-system-control-accent`, or `AccentColor`). A literal fallback is allowed only where WebKit has no keyword, and only with neutral greys. No Catppuccin token is used anywhere under `windows/Popover/`; a lint or unit test enforces it.
- **Material.** The page background is transparent, so the native `NSVisualEffectView` shows through. Use the `.popover` material (or `.menu` if `.popover` looks wrong next to Control Center; record the choice) with the native corner radius (about 10–12 pt, like other menu-bar extras). Inset groups use a subtle translucent fill (as System Settings and Control Center do), not opaque cards.
- **Typography.** The system font stack (`-apple-system`, `system-ui`). Body text is 13 px, secondary text 11 px, section headings 11 px semibold in the secondary label colour, the header 13 px semibold. Numbers use tabular figures. No monospace in the UI except for file paths.
- **Controls.** Native proportions and behaviour:
  - Switches look like `NSSwitch` (mini/small size, accent fill when on).
  - Selects look like a macOS pop-up button: a trailing value with a small up/down chevron, and a menu-styled floating list with a check on the selected option, highlighted in the accent colour.
  - Number fields are a text field plus a small stepper.
  - Buttons are the push-button style. The state badges become plain secondary text with a small coloured status dot (green running, orange paused, red failed, grey disabled/stopped) instead of filled pills.
  - Focus rings use the system focus colour.
- **Sidebar.** Icons are monochrome in the secondary label colour. The selected item has a rounded selection background in the accent colour (or the unemphasised selection colour) with a white icon, like a source list.
- **Light and dark.** Both follow the app appearance automatically through the system colours. There are no hand-picked dark values.
- **Motion and density.** Spacing and row heights match native lists (about 28 px rows for simple settings). Animations are subtle, and none run under reduced motion.
- **Screenshots.** Because the system colour keywords and the material differ between headless WebKit and the real app, the screenshot baselines are regenerated and the test harness supplies stable fallbacks. The real look is checked by the user in the running app.

## Acceptance criteria

- [x] TP-05-01: No Catppuccin palette import or token is used under `src/ui/src/windows/Popover/` or by the `popover` token group (enforced by a test).
- [x] TP-05-02: The popover tokens use system colour keywords and the system font; the documented exceptions are listed in `docs/architecture/ui.md`.
- [x] TP-05-03: Switch, pop-up select, stepper, push button, status dot and sidebar selection match the requirements, in both appearances (component tests plus screenshots).
- [x] TP-05-04: The Bar, Test Window and Accessibility guide screenshots are unchanged.
- [x] TP-05-05: Docs updated; `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- A unit test that scans the popover sources and tokens for palette imports.
- The existing popover component tests keep passing, updated only where markup changed (status dot, select).
- Regenerated popover screenshot baselines (light and dark) for the shell, Overview, `keepAwake`, `tiling` and the invalid-file tab.

Implementation decisions (2026-11-13): the material stays `NSVisualEffectMaterial::Popover` (matches Control Center); the panel radius is 12 pt (`POPOVER_CORNER_RADIUS`), matching `popover.panelRadius`. Fills without a WebKit keyword use `color-mix` of the label colour; the documented exceptions are in `docs/architecture/ui.md`. Status tones: green running; orange paused or waiting; red failed or unavailable; grey stopped or disabled. The screenshot harness pins accent, selection and focus ring to macOS defaults (blue) and paints a neutral stand-in for the material. Also fixed here: a committed text draft is not re-sent by the blur that follows Enter.
