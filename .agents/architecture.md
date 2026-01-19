# Architecture

## Binary Modes

Stache is a single binary that operates in two modes:

- **Desktop App** (no args): Tauri app with React frontend
- **CLI** (with args): Sends commands to running app via IPC

## IPC Communication

CLI commands communicate with the running app via `NSDistributedNotificationCenter`:

```rust
// CLI sends notification
send_notification("TilingFocusWorkspace", Some("workspace_name"));

// App handles in modules/bar/ipc_listener.rs
"TilingFocusWorkspace" => {
    if let Some(manager) = tiling::get_manager() {
        manager.write().focus_workspace(&workspace_name);
    }
}
```

## Key Directories

### Rust Backend (`app/native/src/`)

- `modules/` — Feature modules (audio, bar, tiling, wallpaper, etc.)
- `services/` — Shared services and traits
- `config/` — Configuration types and hot-reload
- `cli/` — Clap command definitions
- `platform/` — macOS-specific platform code
- `utils/` — Shared utilities (IPC, paths, cache)

### React Frontend (`app/ui/`)

- `renderer/bar/` — Status bar UI components
- `renderer/widgets/` — Widget overlay components
- `components/` — Shared UI components
- `hooks/` — React hooks (Tauri integration)
- `stores/` — Zustand state stores
- `design-system/` — Colors (Catppuccin Mocha), motion tokens
