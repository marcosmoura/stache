# Popover

Read when changing the tray popover window: its panel, placement, dismissal, UI shell or
Overview or settings editor. **Shipped** on `feat/tray-popover`
([spec](../specs/2026-11-13-tray-popover/00-overview.md)); native behaviour awaits the
user's check in the running app. Sources: `src/crates/app/src/popover.rs`, `src/crates/app/src/popover_actions.rs`,
`src/crates/app/src/windows.rs`, `src/crates/macos/src/panel.rs`,
`src/crates/macos/src/popover.rs`, `src/ui/src/windows/Popover/`, `src/ui/src/queries/configDocument.ts`.

## Window

- One host-owned window, label `stache-popover`, surface `popover`, 480×640 logical points,
  prewarmed hidden about 1 s after the host reports ready (off the startup path), then
  only shown and hidden; the webview stays alive. The prewarm uses the same creation path,
  shows nothing and emits no `popover-shown`; a click before or during it reuses the one
  window and shows it once built. It is skipped once shutdown is requested (and in the
  smoke build); a failed prewarm is logged and the first click creates the window.
- Tauri builds it hidden, frameless and transparent; `PanelAdapter::host_popover` moves its
  webview into `StacheNextPopoverPanel`, an `NSPanel` subclass (`Borderless |
  NonactivatingPanel`) whose only override is `canBecomeKeyWindow = YES`, so text fields get
  key without activating Stache or showing a Dock icon. Its content view is an
  `NSVisualEffectView` (`.popover` material, chosen over `.menu` to match Control Center's panels;
  behind-window blending, 12 pt corner radius like other menu-bar extras) behind
  the transparent webview; the panel has the native shadow, floats at the pop-up menu level
  (101), joins all Spaces and appears over full-screen apps (`CanJoinAllSpaces |
  FullScreenAuxiliary | IgnoresCycle`), and does not hide on deactivate.
- No module can open it: `WindowBackend::open` refuses the `popover` surface for every
  owner and kind (`UnsupportedSurface`); only `NativeWindows::toggle_popover`,
  `open_popover` and `hide_popover` drive it. No `WindowKind` was added: the window is
  tracked as a host `NonActivatingPanel` entry, so window focus requests are no-ops.
- Shutdown releases it with every other window through the `windows` owner (unhost, then
  Tauri close, acknowledged by `Destroyed`).

## Placement

`popover_origin(icon, size, visible)` (pure, top-left logical points of the primary
display) centres the panel on the icon, puts its top 8 pt below the lower of the icon's
bottom and the visible frame's top (just below the menu bar), and clamps it inside the
visible frame with an 8 pt margin; a frame smaller than the panel pins it to the left and
top margins. `locate_icon` converts the click's physical rectangle by trying each display's
scale and picks the display whose frame holds the icon's centre (else the first display),
so the popover opens on the icon's display. `Open Stache` reuses the rectangle of the last
right click (none: the origin of the first display, clamped).

## Lifecycle and dismissal

`PopoverToggle` holds `visible` and the time of the last resign-key hide:

- Left click: a shown popover hides; a hidden one shows. A click within 400 ms
  (`TOGGLE_GUARD`) of a resign-key hide is the click that took key away, so it keeps the
  popover closed instead of reopening it.
- Resign key (a click outside it, or on the tray icon): the panel's
  `NSWindowDidResignKeyNotification` observer reports it; a shown popover hides at once on
  the main thread.
- `hide_popover` (Esc in the UI): hides a shown popover; otherwise nothing.
- Opening the Test Window or the Accessibility guidance window (any `test` or `access`
  open through `NativeWindows`) hides the popover first.
- `Open Stache`: always shows (again), so the panel regains key.
- A failed creation resets it to hidden; the next click retries.

Every show emits `popover-shown` (empty payload) to `stache-popover`. While hidden the UI
neither polls nor runs timers: it refetches only on `popover-shown`, `config-changed` and
`status-changed`.

## UI shell

`?surface=popover` renders `windows/Popover`: a transparent panel over the native
material (opaque `popover.tintOpaque` under reduced transparency; rounded with a hairline
ring), with a native macOS look: system font and colours, the user's accent, no Catppuccin
palette ([tokens](../architecture/ui.md#popover-native-look)). It has the "Stache" header, the icon sidebar and the selected tab's pane.

- Tabs: Overview first, then one per catalog file in the order `config_files()` returns,
  each with a monochrome Hugeicons icon in the secondary label colour (the selected tab is
  a source-list selection: accent fill, white icon), its name as tooltip and accessible name. A file whose bytes
  are invalid, or that has a held diagnostic, or warnings, shows a badge dot (red,
  orange, yellow; most severe wins) that is also part of its accessible name
  (`tiling.toml, invalid`).
- Keyboard: the sidebar is a vertical `tablist`; only the selected tab is in the Tab order,
  ↑/↓ (wrapping), Home and End select and focus a tab, Tab moves to the pane
  (`tabpanel`, focusable).
- The selected tab lives in the scoped `popoverStore` and survives hiding.
- `config_files` is refetched on `config-changed`, `status-changed` and `popover-shown`;
  `popover-shown` also focuses the selected tab.
- Esc calls `hide_popover` unless a child already handled it (`preventDefault`).
- Each file pane is the [settings editor](#settings-editor) for that file; the Overview pane is below.

The `popover-window` capability grants `stache-popover` only `host_status`,
`allow-hide-popover`, the five Overview commands below, the settings editor's `config_files`,
`config_document`, `config_edit` and `open_config_file`, `request_accessibility`,
`open_accessibility_settings`, `quit_app` and the event listener. Every app command is in the
app manifest, so a command a capability does not grant is refused for that window (including
read-only ones); `tests/host_policy.rs` checks each surface's UI calls against its grants. No other window is granted the popover's own commands.

## Overview

Everything the former tray menu offered (`windows/Popover/components/OverviewTab/`),
rendered from `host_status` plus every `status-changed`:

- **Status line**: `starting…` before the host is ready, `checking Accessibility…` while the
  gate checks; the host error (full text, wrapped, with a tooltip) as an alert.
- **Accessibility banner**, only while the gate is blocked: `Accessibility required`, or
  `Accessibility was turned off` when revoked, an explanation and `Show Accessibility
  Help…` (`open_accessibility_guide`), `Request Accessibility…` (`request_accessibility`)
  and `Open Accessibility Settings…` (`open_accessibility_settings`).
- **Modules**, one row per registered module in registration order, on one translucent inset group: a
  monochrome icon, the readable name (`commandQuit` Hold to Quit, `keepAwake` Keep Awake,
  `menuAnywhere` Menu Anywhere, `notunes` noTunes, `proxyAudio` Proxy Audio, others
  capitalised), the former tray state label as secondary text after a status dot (green
  `running`; orange `paused`, `waiting for permission`; red `unavailable`, `failed`; grey
  `stopped`, `disabled`), the runtime
  detail and `configuration: <diagnostic>` as muted lines (wrapped, clamped to three lines,
  full text in a tooltip), and the controls:

  | Control                | Present when                                 | Command                                       |
  | ---------------------- | -------------------------------------------- | --------------------------------------------- |
  | Enable switch          | always; checked is the configuration flag    | `set_module_enabled(id, !enabled)`            |
  | Pause / Resume (round) | the module is enabled; Resume while paused   | `module_session_action(id, "pause"/"resume")` |
  | Retry (round)          | failed, or enabled, unpaused and unavailable | `retry_module(id)`                            |

  A module whose file has a tab shows its name as a button that selects that tab.

- **Configuration**: each catalog file with a diagnostic or warnings, with the full texts.
- **Footer**: `Open Test Window` (`open_test_window`), a separator, `Quit Stache Next`
  (power icon, `quit_app`).

Commands (`popover_actions.rs`) map to exactly the host command the former tray row used:
`set_module_enabled` to the runner's configuration-backed enable edit (the tray's
`HostHandle::set_enabled`), `retry_module` to `HostHandle::retry` (the host keeps the tray's
retry rule), `open_test_window` and `open_accessibility_guide` to the host's window commands
(both open through `NativeWindows`, which hides the popover first). Retry and the window
commands return at once. `set_module_enabled` returns a typed `ModuleEnableOutcome`
(`applied`, `unchanged`, `refused { message }`, `unavailable { message }`) after the runner
processed it; the wait is bounded (15 s), a shutdown wakes it, and a request still queued
then is cancelled and never runs. A refusal is also published as the host error.
`module_session_action` reuses the generic session action
([`session_action.rs`](../../src/crates/app/src/session_action.rs)) with its stale-click
rule and returns its typed outcome.

No control flips optimistically. A clicked control is disabled (`aria-busy` on its row)
until:

- Enable: its outcome; `refused`/`unavailable` show the host's message inline on the row
  (so two identical refusals in a row each settle and show it). Status events still
  update the row's switch and state meanwhile;
- Retry: the next `status-changed`, with a new host error inline if there is one;
- Pause/Resume: the outcome; any outcome other than `applied` or `stale` shows inline
  (`stale` means the host already moved on; the next status redraws the row).

A rejected `invoke` settles the control and shows inline. The next click on the row clears
its message.

## Settings editor

Each file tab (`windows/Popover/components/FileTab/`) is one generic form generated at
runtime from the file's catalog schema (`config_document`); no module has its own screen.

- **Coverage.** `schemaTree` walks the schema: `$ref` into `$defs` (sibling `properties`
  merged), booleans as switches, string enums (`oneOf` of string constants) as a macOS
  pop-up button (value plus an up/down chevron) opening a menu-styled list with each
  variant's description, a leading check on the selected one and the accent highlight,
  integers/numbers as a text field with the schema bounds plus a small up/down stepper, strings as filled text fields, `anyOf`/`type` with `null` as an optional
  field ("Not set" with **Set**), other `anyOf`/`oneOf` as a union with a kind menu (a
  table form is named by its constant, such as a Bar widget), nested tables as collapsible
  sections, `additionalProperties` as key/value entries (rename, remove, **Add entry**),
  lists of scalars as rows and arrays of tables as collapsible cards titled from a
  `name`/`id`/`widget`-like key (drag the handle, or **Move up/down**, **Remove**, **Add
  item**). All ten committed schemas map with no fallback; anything unrecognised shows its
  TOML text read-only with **Open in editor** for that field only. Labels are the
  humanised key. Help is the first sentence of the schema `description` on one line (ellipsis when it overflows), as are the descriptions in an enum menu; the full text is the tooltip and the field's accessible description (`summary` in `FileTab/summary.ts`).
- **Defaults.** A key absent from the file shows the catalog default (array elements in
  the file use the schema default) with a `default` marker; editing it inserts the key.
  A set key of a table has **Reset to default** (`remove`). An array that exists only as a
  default is written whole on its first change.
- **Write timing (D8).** Switches, menus, steppers, Set and structural actions write at
  once; text and numbers on blur or Enter (a draft already being saved is not sent again by the blur that follows Enter). Esc drops a draft (and only then is not a
  close). Numbers are checked against the bounds before sending: an out-of-range value
  shows the message inline and nothing is sent.
- **Edits.** One serialized editor per file (`createDocumentEditor`) sends each
  `config_edit` after the previous edit finished, with the hash of the document the op
  was built from (captured at click time, not at send time), then refetches the
  document. An op queued behind an edit that changed the file is therefore `stale`
  (for example a rename clicked while a move of the same card is in flight, or a second
  whole-array write from defaults); a rejected invoke does not block the next edit. While an edit is in flight its field is busy and
  disabled. `invalid`/`refused` show the host's message on the field and keep the
  draft; `stale` refetches, keeps the draft and says to save again.
- **Sync.** The tab refetches on `config-changed` naming its file and on
  `popover-shown`. Unfocused fields update in place (rows are keyed by path, so the scroll
  position stays). A focused field or one with a draft keeps it and shows **Changed on
  disk** with **Use disk value**.
- **States.** Header: the path (**Copy path**) and **Open in editor**
  (`open_config_file`, the one accent button). Unknown-key warnings are listed at the
  top. A missing file shows defaults and "File not created yet; the first change creates
  it." An invalid file shows its diagnostic (`Line L, column C: message`) and the form
  read-only on the defaults until a valid save lands; a symlink is read-only with a note.
- **Performance.** Sections below the first level start collapsed, as do array cards
  and lists longer than eight items; collapsed content is not rendered.

## Tests

- Rust: `macos::popover` (placement at the left edge, centre and right edge, a secondary
  display with a negative origin, a frame smaller than the panel, display lookup by scale);
  `app::popover` (toggle, guard, hide and resign transitions); `app::windows` (created once
  then shown and hidden, hide only when shown, `Open Stache` anchor, the resign-key and
  click race, hide before the test window and the guide, failed creation, release on
  shutdown, a queued show after a hide, module requests refused, the popover host and its
  resign callback); `tests/host_policy.rs` (the capability).
- Rust Overview: `host::popover_actions_tests` (each request queues the tray row's host
  command; the session action queues one job; Enable, Pause, a stale Pause, Resume and
  Disable over the real runner, `ConfigStore` and watcher in a temp dir, with the exact file
  bytes and module state; an unknown module becomes the host error).
- UI: `Popover.test.tsx`, `Popover.tabs.test.ts`, `Sidebar.test.tsx`,
  `stores/popover/popover.test.tsx`, `OverviewTab.model.test.ts` (names, labels, the
  Enable/Pause/Resume/Retry eligibility table for every state, header, banner, outcome texts,
  settling) and `OverviewTab.test.tsx` (rows, pending and inline errors, the banner and its
  commands, file notes, footer, name selects the file tab), `FileTab/schema.test.ts`
  (all ten schemas walk with no fallback, each construct, an unknown and a recursive
  reference, variant matching and seeds), `FileTab/values.test.ts` (paths, ops, array
  materialisation, labels) and `FileTab/FileTab.test.tsx` (each control's op, hash and
  moment, serialization, invalid and stale keep the draft, external changes, missing,
  invalid, symlink and warnings); Playwright
  `e2e/scenarios/popover-shell.spec.ts`, `e2e/scenarios/popover-overview.spec.ts`,
  `e2e/scenarios/popover-settings.spec.ts` (against `e2e/fakeConfig.ts`, an in-memory store
  of real TOML text from `e2e/configFixtures.ts`: tiling gap, layout enum, add/move/remove a
  workspace, reset, out-of-range number; rename and remove a keybinding; the Bar height;
  an outside save while focused; an invalid save and its fix; comments checked after each
  step) and the `e2e/screenshots/popover.spec.ts` baselines (shell, Overview, `keepAwake`,
  `tiling` and an invalid file, light and dark).
- Native behavior (key without activation, all Spaces, full-screen, the real toggle race,
  material and shadow) is verified by the user in the running app at the end of part 04.
