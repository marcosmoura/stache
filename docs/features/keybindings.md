# Keybindings, Caps Lock remap and § shortcuts

Read when changing global shortcut registration, shortcut parsing/normalization, command execution, the Caps Lock pseudo-modifier, or `§` key handling.

Rust paths are relative to `app/native/src/`.

## Purpose

`config.keybindings` maps a shortcut string to one command or a list of commands. Stache registers each as a system-wide hotkey and runs the commands when it fires. Three mechanisms cooperate because one library cannot express all keys:

| Mechanism                                              | Keys                                      | File                        |
| ------------------------------------------------------ | ----------------------------------------- | --------------------------- |
| `tauri-plugin-global-shortcut` (Carbon under the hood) | ordinary chords, e.g. `Command+Control+R` | `modules/hotkey/mod.rs`     |
| `hidutil` HID remap + `CGEventTap` state machine       | `CapsLock+<single key>`                   | `modules/hotkey/caps_lock/` |
| Carbon `RegisterEventHotKey`                           | `§` (ISO section key)                     | `modules/hotkey/section.rs` |

## Entry points

- `hotkey::create_hotkey_plugin()` — added to the Tauri builder (`lib.rs`). It builds an empty plugin; registration is separate.
- `hotkey::register_configured_hotkeys(app.handle())` — called from the Tauri `setup` closure. Returns early when `keybindings` is empty.
- `hotkey::execute_shortcut_commands(&ShortcutCommands)` — also used for `execOnStartup` in `lib.rs`.
- `hotkey::shutdown()` — called from `app_shutdown::cleanup_once` to restore the Caps Lock mapping.

## Planning and registration

`collect_planned_shortcuts` sorts keys for determinism and routes each binding: CapsLock parser first, then `§`, then `Shortcut::parse`. Duplicates after normalization are last-wins with a warning; malformed shortcuts are warned about and skipped, never fatal.

`normalize_shortcut` rewrites per `+`-separated part: `Ctrl`→`Control`, `Cmd`/`Super`/`Meta`→`Command`, `Alt`/`Opt`→`Option`, `` ` ``→`Backquote`. `§` is passed through untouched.

Ordinary shortcuts are registered one at a time via `app.global_shortcut().on_shortcut`; failures are counted and logged so one unavailable chord cannot abort startup. The callback ignores `ShortcutState::Released`.

## Command execution

`execute_shortcut_commands` returns immediately for an empty command list — an empty string or empty array is a deliberate "capture and block this shortcut" binding.

Otherwise it spawns a thread and runs commands sequentially, stopping at the first failure. Each command goes through `split_command` (shell-like quoting: `'…'`, `"…"`, backslash escapes; unterminated quotes are an error) and then `platform/command::resolve_binary`, which searches: absolute path, debug `target/debug` for `stache` (debug builds only), `STACHE_EXTRA_PATHS` (colon-separated), `PATH`, `/usr/local/bin`, `/opt/homebrew/bin`, `/opt/homebrew/sbin`, `~/.cargo/bin`, `~/.local/bin`. The child is spawned directly — no shell, so pipes/redirections are not interpreted.

## Caps Lock pseudo-modifier

Config supports `CapsLock+<single key>` only (`CapsLock+Command+S` is rejected).

1. `caps_lock::start(bindings)` spawns a thread that creates a `CGEventTap` (HID, head-insert) and waits up to 2 s for it to be ready.
2. Only after the tap is confirmed running does `remap::apply()` map HID usage `0x7_0000_0039` (Caps Lock) → `0x7_0000_006D` (F18) with `hidutil property --set UserKeyMapping`. Ordering is deliberate: without Accessibility the physical key stays untouched.
3. macOS then never sees Caps Lock, so the LED stays off and capitalization never toggles. F18 events are treated as the pseudo-modifier by `state.rs` (`CapsMode::{Idle, CapsHeld, ChordUsed}`); a bound chord executes once, a bare tap does nothing, and unbound keys pass through.
4. `remap::apply()` first restores any mapping left behind by a crash, and persists ownership in `~/Library/Application Support/stache/caps_lock_remap.json` (`dirs::config_dir()/stache/caps_lock_remap.json`). `restore()` only undoes the mapping if the F18 mapping is still ours.

Because the remap is global and survives until reboot if Stache dies uncleanly, the next launch recovers it from that file. `hotkey::shutdown()` restores it on a normal exit.

## § shortcuts

`global-hotkey` cannot parse or map `§`, so `section::parse_section_shortcut` accepts `[Command|Shift|Option|Control+]…§` (`§` must be last) and `section::start` registers Carbon hotkeys with signature `stac`. The physical key is resolved from the active layout with `TISCopyCurrentKeyboardLayoutInputSource` + `UCKeyTranslate`, so ANSI layouts (where `§` shares the `6` key, sometimes needing Shift) work; the ISO section key `0x0A` is the fallback. Foreign Carbon hotkeys are forwarded via `CallNextEventHandler`.

## Config

Root key `keybindings` (`HashMap<String, ShortcutCommands>`); values are a string or an array, executed in order. See [config](../architecture/config.md).

## Permissions and threading

- `CapsLock+…` bindings need Accessibility: `CGEventTapCreate` fails without it, and the module logs "check accessibility permissions" and leaves Caps Lock alone.
- The plugin's Carbon hotkeys and `§` registration need no permission.
- The Caps Lock event tap owns a dedicated thread and run loop; `§` Carbon handlers are installed on the application event target. The command thread is spawned per invocation.

## Invariants and pitfalls

- Bindings are read once at startup. Editing `keybindings` requires an app restart (`config/watcher.rs` restarts in release, only logs in debug) — see [config](../architecture/config.md).
- Accessibility is checked once at startup and cached in a `OnceLock`; granting it later needs a restart.
- Shortcut parsing happens after normalization, so duplicate detection only catches duplicates that normalize identically.
- `execute_shortcut_commands` spawns a new thread per key press; commands from rapid presses can overlap, and ordering is only guaranteed within one binding.
- Caps Lock bindings are not tray-toggleable; there is no `LifecycleModule` for hotkeys.

## Tests

`modules/hotkey/mod.rs` (normalization table, `collect_planned_shortcuts` routing/duplicates, `split_command` quoting, command execution), `caps_lock/mod.rs` (parser keycode table, chord state machine, suppression after release, duplicate down), `section.rs` (parsing, candidate selection, foreign-hotkey forwarding).
