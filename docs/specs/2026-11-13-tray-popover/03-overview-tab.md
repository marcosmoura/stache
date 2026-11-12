---
status: draft
---

# 03 — Overview tab

## Scope

Everything the old tray menu offered, in the Overview tab (D5), plus the host commands the webview needs for those actions.

Files expected to change: `src/crates/app/src/bindings.rs`, `src/crates/app/src/host.rs`, the generated bindings, `src/ui/src/windows/Popover/` (an `OverviewTab` and its components), `src/ui/src/queries/`, UI tests, the Playwright fake host, and the docs below.

## Requirements

- **Commands.** Typed commands, each mapping to the exact host command the old tray row used, returning immediately with the result arriving through `status-changed`, or a typed outcome where the action already has one:
  - `set_module_enabled(id, enabled)`: the configuration-backed enable (now through part 01's generic edit).
  - Pause/Resume: reuse the existing session action (`session_action.rs`) and its stale-click rule.
  - `retry_module(id)`: with the old tray's eligibility rule.
  - `open_test_window()` and `open_accessibility_guide()`: both hide the popover first.
  - Existing `request_accessibility`, `open_accessibility_settings` and `quit_app` are reused.
- **Rendering.** From `host_status` plus `status-changed`:
  - Header line: `starting…`, `checking Accessibility…` or the host error, when present.
  - Accessibility banner while the gate is blocked (or revoked): explanation plus the three actions.
  - One row per registered module, in registration order: a readable name (`commandQuit` → "Hold to Quit", `keepAwake` → "Keep Awake", `menuAnywhere` → "Menu Anywhere", `notunes` → "noTunes", `proxyAudio` → "Proxy Audio", `bar`, `tiling`, `keybindings`, `wallpapers` capitalised), a state badge with the old labels, an enable switch, Pause/Resume and Retry when eligible, and the runtime detail and config diagnostic under the row. Clicking a module's name selects its file tab.
  - Footer: Open Test Window, Quit Stache Next.
- **Pending state.** A clicked control shows pending until the next status that reflects it or an outcome; no optimistic flip of the switch that the host might refuse. An error outcome shows inline on the row.
- **Removed tray content.** Every row in the old tray table has a home here; nothing is dropped. Long texts wrap or truncate with the full text in a tooltip instead of the old 80-character budget.
- **Docs.** `docs/features/popover.md` gets the Overview section (rows, eligibility, actions); `docs/features/tray.md` points to it.

## Acceptance criteria

- [ ] TP-03-01: Each command maps to the same host command as the old tray row (Rust tests against the host fake).
- [ ] TP-03-02: Enable, Pause/Resume, Retry eligibility matches the old `rows` rules for every module state (table test in the UI).
- [ ] TP-03-03: The gate banner appears only while blocked/revoked and its actions call the right commands.
- [ ] TP-03-04: Opening the Test Window or the guide hides the popover.
- [ ] TP-03-05: Docs updated; `bun run lint`, `bun run format:check`, `bun run check:generated` and `bun run test` pass.

## Tests

- Vitest browser: `OverviewTab.test.tsx` for each module state, pending and error rendering, the banner.
- **End-to-end scenario** (Playwright, fake host modelled on the user's setup: tiling and bar running, keepAwake disabled, proxyAudio failed): enable keepAwake and see it turn running after the status event; pause tiling and resume it; retry proxyAudio; block the gate and use the banner; quit.
- Rust: a host-level test that drives the new commands through a real `ConfigStore` in a temp dir and checks the file edit and module state for enable.
