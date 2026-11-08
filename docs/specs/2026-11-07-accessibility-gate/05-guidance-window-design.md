---
status: done
---

# 05 — Guidance window design

## Scope

A visual redesign of the shipped guidance window from part 04, in the bar's look (Decision 9). Part 04 still describes the behavior. This part changes only the window's chrome and styling, and adds close-on-Escape, a close button and header dragging.

- `src/crates/app/src/windows.rs`: per-surface window chrome, the access window size, `TauriWindowHost::open` and the fake host in tests.
- `src/crates/app/Cargo.toml` (`tauri` feature `macos-private-api`), `src/crates/app/tauri.conf.json` (`app.macOSPrivateApi: true`), `src/crates/app/capabilities/access-window.json` (`core:window:allow-start-dragging`), `src/crates/app/tests/host_policy.rs`.
- `src/ui/src/sdk/tokens.ts` (adds tokens and leaves existing values unchanged).
- `src/ui/src/shell/windows/AccessibilityGuide/`: the component, `AccessibilityGuide.styles.tsx`, the colocated test, `index.ts`, and any small subcomponents with their colocated tests.
- Whatever makes the document background transparent for the `access` surface only (`src/ui/main.css`, the router or the component; the worker decides).
- Docs: [ui.md](../../architecture/ui.md), [capabilities/README.md](../../../src/crates/app/capabilities/README.md), and [tray.md](../../features/tray.md) only if its wording about the window changes.

Depends on part 04. The pinned dependencies `@hugeicons/react` 1.1.10 and `@hugeicons/core-free-icons` 4.4.0 are already in `package.json`. Do not change those pins.

## Requirements

### Window chrome (Rust)

1. A pure function maps a surface to its native chrome, for example `chrome_for(surface) -> WindowChrome { decorations, transparent, resizable, shadow }`. `NativeWindowSpec` carries the chrome and `TauriWindowHost::open` applies it through `WebviewWindowBuilder`. The values:
   - `access`: `decorations(false)`, `transparent(true)`, `resizable(false)`, `shadow(true)`. The window has no title bar and no traffic lights.
   - `test`: the same chrome the builder produces today (decorated, opaque, resizable, default shadow).
2. `access_window_request()` may change the size from 520×420 if the layout needs it, within the existing `1..=MAX_WINDOW_DIMENSION` backend bounds. Record the final size in `access_window_request()`, in the AG-04-03 test expectation and in ui.md. The window is not resizable, so the content must fit at that size (see step 12).
3. `src/crates/app/Cargo.toml` adds `macos-private-api` to the pinned `tauri` features, and `tauri.conf.json` sets `app.macOSPrivateApi: true`. `tauri-build` requires the two to match. No other private API is introduced, and no Rust code calls a private API directly.
4. `access-window.json` adds `core:window:allow-start-dragging` and nothing else. `test-window.json` stays unchanged. Do not grant `core:window:allow-internal-toggle-maximize` or any other window permission. A double-click on the header therefore does nothing. That is accepted, because the window is not resizable.
5. The lifecycle from part 04 stays the same: the auto-open and auto-close rules, the user close rule, the label, the key, the reuse rule and the `webviews=0` startup record. A close from the custom button or from Escape goes through the existing `getCurrentWindow().close()` path, and the host treats it exactly like the title-bar close did before.

### Typography, tokens and global surface

6. `tokens.ts` gains the following, and existing token values (used by `TestWindow`) stay unchanged:
   - `fonts.bar`: `'Maple Mono NF', 'SF Mono', monospace`.
   - `motion`: `easing: 'cubic-bezier(0.42, 0, 0.58, 1)'`, `duration: '150ms'`, `durationSlow: '400ms'`.
   - Radii for 12px (controls) and 16px (outer panel), added under new keys.
7. The guide's root uses `fonts.bar` at 13px, weight 500, `font-variant-numeric: tabular-nums` and line-height 1.6em. Fonts are system-installed only: no `@font-face` and no bundled font files.
8. Only for the `access` surface, `html`, `body` and `#root` are transparent and fill the window. The shell and the test window keep their current opaque rendering.

### Layout and styling

All styles live in `AccessibilityGuide.styles.tsx` (Linaria) and read the tokens. Do not hard-code hex values that `tokens.ts` provides. The component may be split into small subcomponents inside the `AccessibilityGuide` folder.

9. **Panel.** One rounded panel fills the window edge to edge, with no transparent margin: `crust` background, 16px radius, a 1px inset `surface0` ring (inset `box-shadow`, as on legacy widgets) and `text` as the color. The native window shadow provides the depth. The panel carries `data-state` set to `checking`, `blocked`, `revoked` or `open`.
10. **Header.** A drag-region element (`data-tauri-drag-region`) holds a small app glyph and "Stache Next" in `subtext0`. It is not a heading, so the single `h1` stays in the hero. The close icon button (accessible name "Close") is a sibling of the drag region, never inside it. Clicks on the glyph and the text must start a drag: they either carry the attribute or have `pointer-events: none`.
11. **Hero.** The state icon sits in a tinted circular badge with `data-tone`:
    - checking: `overlay`, with a subtle pulse
    - blocked: `mauve`, a lock or shield icon
    - revoked: `peach`, a warning icon
    - open: `green`, a check icon

    In the blocked and revoked states the hero is a row: a 44px badge on the left, and the title and body left-aligned to its right. In the checking and open states the hero is a stacked, centered block (64px badge above the title) vertically centered in the body, which carries `data-layout="centered"` (`top` otherwise). Body copy is left-aligned except the short checking and granted status lines. The badge background is the tone color mixed into `crust`; the icon uses the tone color. The title is about 17px, weight 600, in `text`; the body text is in `subtext0`.

12. **Scrolling.** The header and the actions footer stay fixed. The middle section scrolls inside the panel when the expanded troubleshooting or a long path needs more height. With the troubleshooting disclosure collapsed and a typical path, the blocked and revoked states fit at 480×640 without scrolling, including the identity hint and the Troubleshooting summary row. The window itself never shows a scrollbar, and nothing overflows horizontally.
13. **Steps.** Three rows, each with a numbered circular chip (`surface0` chip, `mauve` number) and the step text. The chips are decorative, so the text and accessible order stay the same as an ordered list. Key and menu names (**Request Access**, **Open System Settings**, **Stache Next**) are emphasised.
14. **Identity card.** `mantle` background, a 1px inset `surface0` ring and label/value rows **Name**, **Bundle ID** and **Path**. Labels are in `overlay1` and values in `text`. Long paths wrap (`overflow-wrap: anywhere`) and never overflow. When `appPath` is null, the Path value reads "not an app bundle". The line "Make sure you turn on Stache Next, not Stache." is a hint accented in `yellow`, with an info icon.
15. **Troubleshooting.** A disclosure ("Troubleshooting", collapsed by default) whose chevron icon rotates when it opens. Both copy variants from part 04 stay, including the `appPath`-null variant.
16. **Actions footer.** **Request Access** (primary), **Open System Settings** and **Quit Stache Next** (quiet/ghost style), in that DOM and tab order, in a right-aligned or full-width row. Buttons follow legacy `Button`:
    - Base: `crust` or `surface0` background, 12px radius and a 1px border.
    - Hover: `surface1` background and `base` border.
    - Transitions: background and border color, `motion.duration` with `motion.easing`.
    - Primary: `mauve` fill with `crust` text.
    - Focus: a `lavender` `:focus-visible` ring.
    - In flight: the button is disabled, sets `aria-busy="true"` and shows a small spinner icon. Its accessible name does not change.
17. **Error alert.** The existing `role="alert"` message sits outside the scrolling body, directly above the actions footer, so it is always visible. It has a red-tinted surface (`red` mixed at about 12% into `crust`), an alert icon and text in `red`.
18. **Icons.** Use `HugeiconsIcon` from `@hugeicons/react` with icons from `@hugeicons/core-free-icons`. The worker picks icons that exist in 4.4.0. All icons are decorative (`aria-hidden`), except the close button, which gets its name from `aria-label`.
19. **Motion.**
    - The panel fades and scales in on mount (`motion.durationSlow` or shorter).
    - The hero and body crossfade on state changes (`motion.duration`).
    - The checking badge pulses, and the spinner and chevron animate.
    - Under `prefers-reduced-motion: reduce`, all of these are off: no keyframe animation runs and transitions are instant.
20. **Keyboard.**
    - Escape anywhere in the window calls `close()` once per press and ignores auto-repeat.
    - Tab order: Close, Troubleshooting, Request Access, Open System Settings, Quit Stache Next. Every control has a visible focus ring.
    - Nothing takes focus automatically on open.

### Copy and behavior

21. All copy from part 04 steps 10–11 stays word for word: the titles, body lines, steps, identity hint, both troubleshooting variants, the checking and granted lines, and the button names. The only new visible text is the field labels **Name**, **Bundle ID** and **Path**, and the close button's accessible name "Close". There are no new commands, events, queries or host status fields. Behavior beyond Escape, the close button and dragging is unchanged: the open state closes itself, actions stay hidden in the open state, and errors join with `; `.

## Acceptance criteria

- [ ] AG-05-01 A Rust unit test shows that the chrome for `access` is undecorated, transparent, not resizable and shadowed, and that the chrome for `test` matches today's builder defaults. The fake window host records the chrome passed in `NativeWindowSpec` for both surfaces.
- [ ] AG-05-02 The AG-04-03 test asserts the final access window size, which is within backend bounds.
- [ ] AG-05-03 A config test shows that `tauri.conf.json` has `app.macOSPrivateApi: true` and that `src/crates/app/Cargo.toml` enables the `macos-private-api` feature on `tauri`.
- [ ] AG-05-04 The capability test asserts the exact `access-window.json` list (the part 04 list plus `core:window:allow-start-dragging`). The test-window capability still has its exact three permissions and gains neither `start-dragging` nor any guidance command.
- [ ] AG-05-05 WebKit: the header contains an element with `data-tauri-drag-region`, the "Close" button has no `[data-tauri-drag-region]` ancestor, and it does not carry the attribute itself.
- [ ] AG-05-06 WebKit: clicking Close calls the mocked window `close()` once. One Escape keydown calls it once, and an auto-repeat Escape does not call it again.
- [ ] AG-05-07 WebKit: the checking, blocked, revoked, unbundled (`appPath: null`) and open fixtures set the panel's `data-state` and the hero `data-tone` from step 11. All part 04 copy assertions still pass unchanged. The identity card shows Name, Bundle ID and Path with the fixture values, or "not an app bundle".
- [ ] AG-05-08 WebKit: the action buttons appear in DOM order Request Access, Open System Settings, Quit Stache Next. Each one calls its mocked command once. While a command is pending, its button is disabled, has `aria-busy="true"` and contains the spinner. A rejected command and `HostStatus.error` render in the `role="alert"` element.
- [ ] AG-05-09 WebKit: on the access surface the document background is transparent. Rendering `TestWindow` (or the `test` route) leaves it opaque as before.
- [ ] AG-05-10 Reduced motion: if the browser suite can emulate `prefers-reduced-motion: reduce` (for example through a Playwright-backed Vitest browser command), a test asserts `animation-name: none` on the panel, hero badge and spinner, and zero transition durations. If it cannot, record that in Notes during implementation and cover it with AG-05-M04.
- [ ] AG-05-11 `tokens.ts` keeps every existing value and adds `fonts.bar`, `motion` and the new radii. No `@font-face` or font asset is added.
- [ ] AG-05-12 ui.md and the capabilities README describe the frameless transparent access chrome, the `macos-private-api` note, the new capability entry and the final size. `bun run lint`, `bun run format:check`, `bun run test`, `bun run test:ui:coverage` (thresholds unchanged) and `bun run generate-bindings -- --check` pass.

## Tests

- **Rust:** unit tests on the chrome function and the fake host's recorded specs in `windows.rs`, plus the config and capability tests in `tests/host_policy.rs`. Update the existing AG-04-03 and AG-04-05 tests rather than duplicating them.
- **UI:** extend the colocated `AccessibilityGuide.test.tsx` (and add tests for any new subcomponents) with the existing `@tauri-apps/api` module mocks, `tests/hostFixtures.ts` and `renderWithClient`. Leave the part 04 copy assertions unchanged.
- No test opens a real window, calls a real command or depends on Maple Mono being installed.

## Manual verification (approval needed for each run)

- AG-05-M01 Launch the bundle on macOS 26 without the grant. The guidance window has no title bar or traffic lights, has rounded 16px corners with nothing visible outside them, and shows the native shadow.
- AG-05-M02 Drag the window by the header (glyph and text). Clicking the close button closes the window instead of dragging it. Escape closes the window. Neither reopens it during the same episode, and the tray row reopens it.
- AG-05-M03 Maple Mono NF renders when installed, and SF Mono is used when it is not. Hover, focus-visible, in-flight and alert states look right, and keyboard focus works in the frameless window.
- AG-05-M04 With Reduce Motion on in System Settings, nothing animates.
- AG-05-M05 Record a screenshot of each state (checking, blocked, revoked, unbundled if reachable, open, alert) and of the expanded troubleshooting section.

## Notes

- Do not port legacy `Button` or `Surface` wholesale. Bar part 04 (`2026-11-03-bar/04-ui-shell-parity.md`) owns that port. This part reproduces their look locally in the guide's styles. When the Bar port lands, the guide may switch to the shared components in a later change.
- The `macos-private-api` feature is needed for a transparent WebKit background on macOS. It applies to the whole app, it is the same setting the legacy app used, and the Bar panel needs it too. It does not authorize any other private API, such as the SkyLight or SLS adapters, which still need their own approval under the Bar spec.
- macOS can compute the native shadow of a transparent window from the window's content when the window is first shown. If the shadow comes out rectangular or stale, report it from AG-05-M01 instead of adding native code.
- Tauri's drag script reacts only to the element that is clicked. A child of the drag region without the attribute does not start a drag (see step 10).
