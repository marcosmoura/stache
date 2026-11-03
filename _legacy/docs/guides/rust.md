# Rust conventions (`app/native`)

Read when you write or review Rust in `app/native/`.

## Toolchain and lints

- `rust-toolchain.toml` pins `nightly-2026-08-17` with rustfmt, clippy and rust-analyzer.
  Edition 2024.
- Workspace lints in `Cargo.toml`: clippy `all` = deny, `pedantic`/`nursery`/`cargo` =
  warn, plus a few targeted allows (`multiple_crate_versions`, `assert_is_empty`,
  `suboptimal_flops`). `pnpm lint:native` and CI add `-D warnings`, so a warning is a build
  failure.
- `pnpm lint` also runs `cargo sort --workspace` and clippy with `--fix`; the pre-commit
  hook repeats it. Clippy rewrites code, so review what it changed.
- `rustfmt.toml` sets `fn_single_line`, `imports_granularity = "Module"` and
  `group_imports = "StdExternalCrate"`, with the default 100-column width. Run
  `cargo fmt --all` rather than hand-formatting.

## Layout

- Feature code in `modules/<name>/`, macOS primitives in `platform/`, CLI in `cli/`.
- Every new file starts with a `//!` module doc saying what it is for.
- `lib.rs` exposes the feature and infrastructure modules publicly; `platform` is private
  and reached through `crate::platform::…`.
- Prefer `#[must_use]` on pure getters and `const fn` where it compiles; the pedantic set
  will ask for them anyway.

## Errors

- `error.rs::StacheError` (`thiserror`) is the shared error type for CLI commands and
  `#[tauri::command]` returns. Variants are domain-scoped (`AudioError`,
  `WallpaperError`, `TilingError`, `ConfigError`, `IpcError`, …); `From<io::Error>`,
  `From<String>` and `From<&str>` are implemented for quick `?` conversions.
- It derives `Serialize` with `#[serde(tag = "kind", content = "message")]`, which is what
  lets a command return it to the frontend unchanged.
- Modules with richer failure modes keep a local enum (`WallpaperManagerError`) and convert
  at the boundary.
- `main.rs` prints `stache: {err}` and exits 1. Do not `unwrap` on user input or OS state.

## Logging

`logging.rs` installs the `tracing` subscriber: `RUST_LOG` wins, otherwise
`warn,stache={debug|info}` (debug builds get `debug`), compact, on stderr.

- Use structured fields: `tracing::warn!(error = %err, path = %p.display(), "…")`.
- `error!` for a broken feature, `warn!` for a degraded path (missing permission, failed
  listener, ignored notification), `debug!` for lifecycle steps, `trace!` inside hot loops
  such as tiling event handling.
- `println!` only for CLI output, ideally through `cli/output.rs`.

## Threads

- `platform::thread::spawn_named_thread("name", closure)` spawns `stache-<name>` and logs a
  spawn failure instead of panicking.
- `dispatch_on_main_sync(closure)` runs on the main thread and blocks the caller; it
  executes inline when already there, so it is safe to call from either side.
  `dispatch_on_main` is the async variant; `dispatch_on_high_priority` is reserved.
- AppKit, the Accessibility API, `NSWindow`/`NSStatusBar` and the Caps Lock remap require
  the main thread. `tiling::init` is reached through `dispatch_on_main_sync` for this
  reason.
- Tauri's async runtime hosts the tiling actor, the effect subscriber and background
  startup. A synchronous caller that must await an actor reply builds a short-lived
  current-thread runtime instead (`modules/bar/ipc_listener.rs::build_tiling_runtime`,
  used from the socket thread).

## Objective-C and FFI

- `platform/objc.rs` provides `nsstring(&str) -> *mut Object`, `nsstring_to_string` and
  `get_app_bundle_id`. Both directions are `unsafe`: check for null, and comment why the
  call is valid.
- The `objc` crate (`msg_send!`, `class!`, `declare::ClassDecl`) bridges AppKit, Wi-Fi and
  the notification center; `objc2-core-audio` covers audio. Keep FFI inside `platform/` or
  a module's own `ffi/` directory, never in code that also renders UI.
- `app/native/build.rs` links `SkyLight` from `/System/Library/PrivateFrameworks` and
  `CoreLocation`. Private API use depends on `macOSPrivateApi` in `tauri.conf.json`.

## Paths

- Never pass a config path string straight to the filesystem.
  `platform::path::expand` does `~` expansion; `expand_and_resolve(path, base_dir)` also
  resolves relative paths against a base directory (normally the config file's directory).
- Cache paths come from `cache::get_cache_dir()` and `cache::get_cache_subdir("name")`.

## State and ownership

- Global singletons are `OnceLock` (`config::CONFIG`, the wallpaper manager) or
  `parking_lot::Mutex` / atomics (`tiling::LIFECYCLE`, the IPC server flag). Prefer
  `parking_lot` over `std::sync::Mutex`.
- Never hold a lock across an OS call. `LifecycleRegistry::toggle` clones the `Arc` out and
  drops the guard first; copy that pattern.
- Tauri shared state is registered with `app.manage(...)` (`LifecycleRegistry`,
  `TrayMenuState`, `KeepAwakeController`) and read with `app.state::<T>()` or
  `try_state::<T>()` when absence is possible.

## Tests

Inline `#[cfg(test)] mod tests` at the bottom of the file under test; there is no
`app/native/tests/` directory. See [testing.md](testing.md).
