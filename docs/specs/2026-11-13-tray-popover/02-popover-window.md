---
status: done
---

# 02 — Native popover and tray clicks

## Scope

The popover window and the tray click handling, with a UI shell: the `popover` surface renders the sidebar and an empty pane per tab ("Overview", file names) so the window is testable. Overview content is part 03; file forms are part 04. The native menu shrinks to the right-click menu.

Files expected to change: `src/crates/app/src/tray.rs`, `src/crates/app/src/windows.rs`, `src/crates/sdk/src/windows.rs` if a window kind is added, `src/crates/macos/` (panel adapter for a key-capable borderless panel and its placement), `src/crates/app/src/host.rs`, `src/ui/src/router.ts`, a new `src/ui/src/windows/Popover/` (shell, sidebar), UI tests and Playwright fake host, and the docs below.

## Requirements

- **Panel.** A borderless, transparent `NSPanel` with shadow and rounded corners (D1) that can become key so text fields receive input, without activating Stache as a regular app and without a Dock icon. Prefer extending the existing panel adapter (the Bar uses `WindowKind::NonActivatingPanel`); add a new kind (working name `WindowKind::Popover`) if the semantics differ, accepted only for the `popover` surface and only from the host. It floats above normal windows, joins all Spaces and appears over full-screen apps like a menu extra.
- **Placement.** A pure function (working name `popover_origin(icon_rect, panel_size, visible_frame)`) puts the panel top just below the menu bar, centred on the icon, clamped to the visible frame with an 8 pt margin. The icon rect comes from the tray click event. Opening on the display that holds the clicked icon.
- **Lifecycle.** Created on first open, then hidden and shown; one instance; the label is `stache-popover`. Hidden on: resign key (click outside), Esc from the UI (a command, working name `hide_popover`), second left click on the tray, and before opening the Test Window or the Accessibility guide from the popover. A click on the tray icon while the panel is key must toggle it closed, not close-then-reopen (resign-key and the click race; handle it with a short guard). Shutdown releases it through the existing windows owner.
- **Tray.** Left click toggles the popover (`show_menu_on_left_click(false)`). Right click shows the native menu `Open Stache`, separator, `Quit Stache Next`; `Open Stache` opens the popover under the icon. `tray::rows` and its status/module/config rows are removed; `tray::update` only refreshes the tooltip (and the menu if needed). Quit keeps calling `HostHandle::request_shutdown`.
- **Visibility event.** The host emits a typed event when the popover is shown (working name `popover-shown`) so the UI can focus the selected tab and refetch.
- **UI shell.** `router.ts` maps `popover` to `windows/Popover`. The shell: rounded translucent material background following the app appearance, header "Stache", the icon sidebar (D4) with Overview plus the catalog files in order (from `config_files()` or `HostStatus.config`), badges for invalid/diagnostic/warnings, keyboard navigation (↑/↓ in the sidebar, Tab into the pane), the selected tab stored in a scoped zustand store for the session, Esc calls `hide_popover` unless a child handled it. Each tab pane is a placeholder title in this part.
- **Docs.** Rewrite `docs/features/tray.md` for the new click behaviour and minimal menu; add `docs/features/popover.md` (window, placement, dismissal); `docs/architecture/ui.md` gains the surface and layout entries.

## Acceptance criteria

- [x] TP-02-01: `popover_origin` is tested for an icon at the left edge, centre and right edge, on a secondary display with negative origin, and with a frame smaller than the panel.
- [x] TP-02-02: Left click toggles; right click shows exactly the two items; the old rows and their tests are removed or rewritten.
- [x] TP-02-03: The panel lifecycle (create once, show, hide on each trigger, toggle race, release on shutdown) is tested against a fake panel adapter.
- [x] TP-02-04: Only the host can open the `popover` surface; a module request for it is refused.
- [x] TP-02-05: The UI shell renders the sidebar with badges, navigates by keyboard, remembers the tab and calls `hide_popover` on Esc (Vitest browser + a Playwright scenario with the fake host).
- [x] TP-02-06: Docs updated; `bun run lint`, `bun run format:check`, `bun run check:generated` and `bun run test` pass.

## Tests

- Rust: placement table, tray click mapping, panel lifecycle with a fake adapter, surface/kind validation.
- UI: `Popover.test.tsx` (sidebar, badges, keyboard, Esc), a Playwright scenario `popover-shell.spec.ts`, and a screenshot baseline in light and dark.

## Notes

Native behaviour (key focus without activation, all Spaces, full-screen, the toggle race) is verified by the user in the running app at the end of part 04.

Implementation decisions (2026-11-13): no new `WindowKind`; the popover is a host-only entry recognised by its `popover` surface (fixed label `stache-popover`) with a key-capable panel path in the adapter (`host_popover`/`show_popover`/`hide_popover`, `NSPanel` subclass with `canBecomeKeyWindow`, `NSVisualEffectView`, 18 pt corners, level 101). `tray::update` is removed (the tooltip is fixed). The toggle guard is 400 ms and a left click acts on release; a right press records the icon rect used by `Open Stache`. Between this part and part 03 the branch has no UI entry for Enable/Pause/Retry/Test Window.

The popover is prewarmed hidden about 1 s after the host reports ready, so the first tray click only positions and shows it (user report 2026-11-13).
