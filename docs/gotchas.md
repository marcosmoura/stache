# Gotchas

Read before debugging something that "should work". Cross-cutting pitfalls only; anything
specific to one feature lives in that feature's doc.

## An event that never fires

Rust and TypeScript event names must be byte-identical: `app/native/src/events.rs` and
`app/ui/types/tauri-events.ts`. The Rust tests only assert the prefix and the
`stache://<module>/<name>` shape — nothing checks the TS mirror, so a typo there fails
silently at runtime.

## A command that is not callable

`#[tauri::command]` alone does nothing. The function must be listed in
`tauri::generate_handler![…]` in `app/native/src/lib.rs`. Frontend `invoke` then rejects with a
command-not-found error, which is easy to miss inside a Suspense boundary.

## Config keys change shape at the root

`StacheConfig` has no struct-level `rename_all`. `bar`, `wallpapers`, `keybindings`,
`notunes` and `tiling` are the literal field names, while `commandQuit`, `menuAnywhere`,
`proxyAudio` and `execOnStartup` are explicit renames. "Normalizing" the root to camelCase
breaks every existing config; nested structs (`BarConfig`, `WeatherConfig`) _are_
camelCase, which makes the asymmetry easy to trip over.

## One option, three files

A new config option has to land in `config/types/…`, in the hardcoded
`config/template.rs::generate_config_template`, and in `docs/samples/config.jsonc`; then
`./scripts/generate-schema.sh` must rewrite `stache.schema.json`. Miss the template and
`stache config init` omits the option; miss the schema and editors validate against the old
shape.

## "Hot reload" is a restart

`config/watcher.rs` and `stache reload` both call `app_shutdown::restart` — in release
builds only. Debug builds log a line and change nothing, so a config edit during
`pnpm tauri:dev` appears ignored. No module supports in-place reload. Details in
[architecture/config.md](architecture/config.md).

## Paths from config are not paths yet

`~` and relative paths are expanded only by `platform::path::expand` /
`expand_and_resolve`. A raw `config.bar.weather.api_keys` or wallpaper path handed to the
filesystem resolves against the process CWD, which is not the config directory.

## AppKit and Accessibility need the main thread

Window, menu bar, status item and AX calls must run on the main thread. From a background
thread use `platform::thread::dispatch_on_main_sync` (blocks, runs inline when already on
the main thread) or `dispatch_on_main`. Calling AppKit off-thread crashes or corrupts state
in ways that look random.

## Accessibility permission is cached for the process lifetime

`lib.rs::is_accessibility_granted` stores the first answer in a `OnceLock`. Granting the
permission in System Settings does not take effect until Stache restarts. In the meantime
MenuAnywhere reports `ModuleStatus::Unavailable("Accessibility permission required")` in the
tray, while tiling only logs a warning and never starts — its tray entry looks paused because
the runtime is stopped, and resuming it starts a runtime that cannot observe windows.

## CLI commands are not all IPC

Wallpaper, audio, cache, config and schema commands run inside the CLI process and work with
the app closed. Only tiling commands and `reload` go through the socket. With the app
closed, tiling queries print "Stache app is not running." and still exit 0, while tiling
control commands return an `IpcError` and exit 1.

## Clearing the cache kills the IPC socket

The socket lives at `~/Library/Caches/com.marcosmoura.stache/stache.sock`, and
`stache cache clear` calls `remove_dir_all` on that directory. Run it while Stache is
running and new CLI connections fail until the app restarts.

## Distributed notifications are public

Any process in the user session can post `com.marcosmoura.stache.*` notifications. Only
frontend refresh events belong on that channel; control operations go over the `0600` Unix
socket. Adding a privileged notification re-opens the hole the socket was built to close.

## A new window needs three places

A window must exist in `app/native/tauri.conf.json` (label, hidden, transparent,
non-focusable) and have a branch in `app/ui/renderer/Renderer.tsx`. Size and position are
not shared: the bar is placed in Rust (`modules/bar/window.rs::set_window_position`), while
the widgets overlay places itself from the frontend using `get_bar_window_frame` plus
`setSize`/`setPosition` (`renderer/widgets/Widgets.state.ts`). Non-focusable windows never
receive focus events — the widget overlay detects click-outside with a Core Graphics event
tap in `modules/widgets/window.rs` instead.

## Clippy is part of the build

`pedantic` + `nursery` with `-D warnings` means ordinary code is rejected for style.
`pnpm lint` runs clippy with `--fix`, which rewrites your code; read the diff before
committing. When an allow is genuinely needed, add it with a reason comment.
