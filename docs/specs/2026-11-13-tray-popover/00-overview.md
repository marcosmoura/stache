---
branch: feat/tray-popover
---

# Tray popover

Read when planning or implementing the popover that replaces the native tray menu: its window, the Overview controls and the live settings editor. Drafted 2026-11-13.

## Goal

Clicking the tray icon opens a popover window anchored under the icon instead of the native menu. It has a sidebar with an **Overview** tab (module enable, pause/resume, retry, the Accessibility gate, Test Window, Quit) and one tab per configuration file (`global`, `bar`, `commandQuit`, `keepAwake`, `keybindings`, `menuAnywhere`, `notunes`, `proxyAudio`, `tiling`, `wallpapers`). Each file tab is a form generated from that file's schema that covers every key, and it stays in sync with the file both ways: a save on disk updates the form, an edit in the form writes the file.

## Background

The tray today is a native `NSMenu` rebuilt from `tray::rows(&HostStatus)` ([tray](../../features/tray.md)). Config is file-backed with a directory watcher (200 ms debounce), per-file schemas under `schemas/`, and one comment-preserving edit: the root `enabled` boolean (`stache_sdk::set_enabled`, `app/src/config_edit.rs`, [config](../../architecture/config.md#tray-enable-edits)). Every `oneOf` in the committed schemas is a string enum, and every `anyOf` is an `Option`, so a generic form is practical.

## Design references

For mockup and visual direction only (user, 2026-11-13); do not reproduce them: [Menu Bar Actions](https://dribbble.com/shots/21256312-Menu-Bar-Actions), [System Status Tool Explorations](https://dribbble.com/shots/21908743-System-Status-Tool-Explorations-MacOS). The shipped look follows the app's own tokens and themes (`docs/architecture/ui.md`).

## Non-goals

- No undo/redo, no settings search, no import/export.
- No per-module hand-written settings screens: every file tab uses the same generic form. A custom widget for one field type (a color, a shortcut) is out of scope; such values are text fields.
- No rewrite of a file's formatting. Edits change only the touched value span; nothing is reserialized.
- No editing of a symlinked config file, and no editing of a file that does not currently parse and validate (read-only with its diagnostic).
- No change to the CLI, the socket protocol, module behavior or the Bar.
- The Test Window and the Accessibility guidance window stay as they are.

## Decisions

- **D1. Window.** One popover, about 480×640 logical points, frameless and transparent with the native shadow and rounded corners. It is a borderless `NSPanel` that can become key (text fields work) without making Stache a regular app or showing a Dock icon. It opens with its top edge just below the menu bar, horizontally centred on the tray icon, on the display that holds the icon, and clamped inside that display's visible frame. One instance at most; the webview is kept alive and hidden between openings so reopening is instant and drafts survive.
- **D2. Dismissal.** The popover closes on: a click outside it (it loses key), Esc (when no field is mid-edit; the first Esc cancels a text edit), a second left click on the tray icon, and Quit. Opening the Test Window or the Accessibility guidance window from it closes it.
- **D3. Tray clicks.** Left click toggles the popover. Right click shows a minimal native menu: `Open Stache`, separator, `Quit Stache Next`. All other former menu rows are gone; their content moves into the Overview tab. The status tooltip stays.
- **D4. Sidebar.** A narrow icon sidebar (icon plus tooltip and accessible name) lists Overview first, then the ten files in catalog order. A file whose current bytes are invalid, or that has a held diagnostic or warnings, shows a badge on its icon. The last selected tab is remembered for the session.
- **D5. Overview.** Per module: name, state label (same labels as the old tray), runtime detail, config diagnostic, an enable switch (configuration-backed, as the old tray), Pause/Resume (session only) and Retry with the old tray's eligibility rules. Above them: a host error line and, while the gate is blocked, an Accessibility banner with its three actions. Footer: Open Test Window and Quit.
- **D6. Form coverage.** Generated from the catalog schema at runtime, covering everything: booleans (switch), string enums (select, showing each variant's description), integers/numbers (number field honouring `minimum`/`maximum`), strings (text), `Option` (`anyOf` with `null`) as the inner control plus a "not set" state, nested tables (sections), maps (`additionalProperties`: key/value rows with add, rename and remove), arrays of scalars (chips or rows with add/remove/reorder) and arrays of tables (collapsible cards with add/remove/reorder, each a nested form). Schema `description` is the help text under each field. A schema construct the form does not recognise renders read-only with its TOML text and an "Open in editor" action instead of failing the tab.
- **D7. Defaults.** A key absent from the file shows its effective default (from the catalog default), visibly marked as default. Editing it inserts the key. Every set key has "Reset to default", which removes it from the file. A missing file shows defaults; the first edit creates it from the generated template (the same rule as the existing enable edit).
- **D8. Write timing.** Switches, selects, steppers and structural actions (add/remove/reorder/reset) write at once. Text and number fields write on blur or Enter; Esc reverts the field. A value is validated by the host against the whole file before any write; a refused value leaves the file untouched and shows the host's message inline on the field.
- **D9. Edits are surgical.** One generic host edit replaces only the targeted value span with `toml_edit`. Comments, `#:schema`, key order, line endings and every untouched byte stay. Insertion follows the existing `enabled` insertion rules generalised to any table. The commit protocol is the existing one: same-directory temp file, fsync, re-read-before-rename, no-clobber create, permissions kept, symlinks refused, then `ConfigStore::reload_one`.
- **D10. Sync and conflicts.** Every document read carries a content hash, and every edit sends the hash it was based on. If the file changed in the meantime the host refuses with `stale` and writes nothing; the UI refetches and the user's field keeps its draft. A host event fires after every watcher batch and every commit, naming the files; the open tab refetches. A field that is being edited is never overwritten by a refetch: it keeps the draft and shows "Changed on disk", with an action to take the disk value.
- **D11. Invalid on disk.** A file that fails to parse or validate is read-only: the tab shows the diagnostic (with line/column when known) and an "Open in editor" button that opens the file with the system default app. The form becomes editable again as soon as a valid save lands. A symlinked file is read-only with a note.
- **D12. Identity.** The new UI surface is `popover`. Window label `stache-popover`. User-facing name "Stache" in the popover header.

## Parts

| Part                         | Summary                                                                                      | Status |
| ---------------------------- | -------------------------------------------------------------------------------------------- | ------ |
| [01](01-config-documents.md) | Host: config document read, generic surgical edit, stale check, change event, commands       | draft  |
| [02](02-popover-window.md)   | Native popover panel, tray left/right click, `popover` surface shell with sidebar and tabs   | draft  |
| [03](03-overview-tab.md)     | Overview tab: module controls, Accessibility banner, footer; host commands for those actions | draft  |
| [04](04-settings-editor.md)  | Schema-driven settings form for every file tab with live two-way sync                        | draft  |

After part 04 the user tries it in the running app; the steps are in part 04's notes. Native behavior (anchoring, key focus, dismissal) stays unverified until then.

## Affected docs

`docs/features/tray.md` (rewritten: click behaviour, minimal menu, popover), a new `docs/features/popover.md` (Overview and settings editor behavior), `docs/architecture/config.md` (generic edits replace "Tray enable edits"; document reads, hashes and the change event), `docs/architecture/ui.md` (the `popover` surface, its layout, the form), `docs/architecture/overview.md` where it describes the tray, `docs/guides/testing.md` if suites are added, and the rows in `docs/index.md` and `docs/roadmap.md`.
