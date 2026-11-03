# IPC and frontend contracts

Read when you add or debug a CLI command, the CLI ↔ app channel, a Tauri command, or a
Tauri event.

## Two channels, different jobs

| Channel                           | Carries                            | Code                     |
| --------------------------------- | ---------------------------------- | ------------------------ |
| Unix domain socket                | state queries and control commands | `platform/ipc_socket.rs` |
| `NSDistributedNotificationCenter` | benign UI refresh events only      | `platform/ipc.rs`        |

Control commands were moved off distributed notifications because any process in the user
session can observe or spoof them. The socket is `0600` and only reachable by the user who
owns the cache directory.

## Socket protocol

- Path: `<cache dir>/stache.sock`, i.e.
  `~/Library/Caches/com.marcosmoura.stache/stache.sock` (`ipc_socket::get_socket_path`,
  `cache::get_cache_dir`).
- One JSON object per line. `IpcMessage` is untagged, so a request is either `IpcQuery` or
  `IpcCommand`; both are tagged by `type` in camelCase, for example
  `{"type":"tilingFocusWorkspace","workspace":"coding"}`.
- Response: `IpcResponse`, either `{"data": …}` or `{"error": "…"}`.
- Server limits (`ipc_socket.rs`): 8 concurrent connections, 5 s read/write timeouts,
  64 KiB maximum request line. The client retries 3× but only on `AppNotRunning`.
- Queries: `screens`, `workspaces`, `windows`, `apps`, `ping`, `v2State`, `v2Screens`,
  `v2Workspaces`, `v2Windows`, `v2Enabled`.
- Commands: `reload`, `tilingPause`, `tilingResume`, `tilingFocusWorkspace`,
  `tilingSetLayout`, `tilingWindowFocus`, `tilingWindowSwap`, `tilingWindowResize`,
  `tilingWindowToggleFloating`, `tilingWindowPreset`, `tilingWindowSendToWorkspace`,
  `tilingWindowSendToScreen`, `tilingWorkspaceBalance`, `tilingWorkspaceSendToScreen`.

Routing is in `lib.rs::load_base_modules`: the socket `init` handler sends
`IpcMessage::Query` to `tiling::init::handle_ipc_query` and `IpcMessage::Command` to
`modules/bar/ipc_listener.rs::handle_command`.

## Who actually uses the socket

Only tiling commands and `stache reload` (`cli/commands/tiling.rs`,
`cli/commands/mod.rs`). Wallpaper, audio, cache, config and schema commands run entirely
inside the CLI process; they never talk to the app.

The CLI never launches the app. With no app running, tiling queries print
`Stache app is not running.` and still exit 0, while tiling control commands fail with
`StacheError::IpcError` and exit 1.

`stache reload` and a config-file change both end in `app_shutdown::restart`; debug builds
only log. See [config.md](config.md).

## Distributed notifications

Prefix `com.marcosmoura.stache.` (`platform/ipc::NOTIFICATION_PREFIX`). The app observes
only `window-focus-changed` and `workspace-changed` (`observed_notification_names`) and
re-emits them as the `stache://spaces/*` events. `send_notification` is the supported API
for external tooling that wants to nudge the UI. Never put a control operation on this
channel.

## Tauri commands

- Declared with `#[tauri::command]` next to the feature code — mostly
  `modules/bar/components/*.rs` and `modules/bar/window.rs`.
- Registered in `lib.rs` inside `tauri::generate_handler![…]`. A command that is not
  registered is invisible to `invoke` and fails silently on the TS side.
- Return `Result<T, StacheError>`; `StacheError` serializes as
  `{"kind": "…", "message": "…"}` (`error.rs`).
- Payload and result structs use `#[serde(rename_all = "camelCase")]`, so TypeScript sees
  camelCase fields even when Rust uses snake_case.

## Events

- Naming: `stache://<module>/<event-name>`, kebab-case event name.
- Rust source of truth: `app/native/src/events.rs`, one `pub mod` per module. Its tests
  assert every constant has the `stache://` prefix and matches the naming convention.
- TS mirror: `app/ui/types/tauri-events.ts`, re-exported from `app/ui/types/index.ts` as
  `MediaEvents`, `TilingEvents`, `WidgetsEvents`, `MenubarEvents`, `KeepAwakeEvents`,
  `SpacesEvents`, `CmdQEvents`, `AppEvents`. The strings must match byte for byte.
- Emit with `app_handle.emit(events::<module>::NAME, payload)`. Payloads built with
  `serde_json::json!` use camelCase keys (`windowId`, `previousWorkspace`).
- Consume with `useTauriEvent(name, callback)` for one-off listeners, or with
  `useTauri`/`useTauriSuspense` and an `eventName` option to keep a React Query cache in
  sync (see [../guides/react.md](../guides/react.md)).
- Window to window: `emitTauriEvent({ eventName, target, payload })` emits to another
  window label; the widget toggle uses it to reach the `widgets` window.

## Cache

`cache.rs` is not IPC but it shares the directory: `get_cache_dir()` returns
`~/Library/Caches/com.marcosmoura.stache` (fallback `/tmp/…`) and `clear_cache` calls
`remove_dir_all` on it. `stache cache clear` therefore deletes the live socket file of a
running app.
