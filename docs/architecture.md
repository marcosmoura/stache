# Architecture

This document describes the technical architecture of Stache, including its components, communication patterns, and implementation details.

## Overview

Stache is built with Tauri 2.x, combining a Rust backend with a React frontend. The application uses a single-binary architecture where the same executable serves as both a desktop app and a CLI tool.

## High-Level Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                       Stache Binary                          │
│  ┌─────────────────────┐    ┌─────────────────────────────┐ │
│  │   CLI Mode          │    │      Desktop App Mode       │ │
│  │   (with args)       │    │      (no args)              │ │
│  │                     │    │                             │ │
│  │  stache reload      │───►│  ┌─────────────────────┐   │ │
│  │  stache wallpaper   │    │  │  IPC Listener       │   │ │
│  │  stache audio       │    │  │  (NSDistributed     │   │ │
│  │  stache event       │    │  │   NotificationCenter)│   │ │
│  └─────────────────────┘    │  └──────────┬──────────┘   │ │
│                              │             │              │ │
│                              │  ┌──────────▼──────────┐   │ │
│                              │  │   Tauri Backend     │   │ │
│                              │  │   (Rust)            │   │ │
│                              │  └──────────┬──────────┘   │ │
│                              │             │              │ │
│                              │  ┌──────────▼──────────┐   │ │
│                              │  │   React Frontend    │   │ │
│                              │  │   (TypeScript)      │   │ │
│                              │  └─────────────────────┘   │ │
│                              └─────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

## Component Overview

### Backend (Rust)

Location: `app/native/src/`

| Module           | Purpose                            |
| ---------------- | ---------------------------------- |
| `main.rs`        | Entry point, CLI handling          |
| `lib.rs`         | Tauri app initialization           |
| `cli/`           | Command-line interface definitions |
| `config/`        | Configuration loading and types    |
| `bar/`           | Status bar widgets and logic       |
| `wallpaper/`     | Wallpaper management               |
| `audio/`         | Audio device handling              |
| `hotkey/`        | Global keyboard shortcuts          |
| `menu_anywhere/` | Menu summoning feature             |
| `notunes/`       | Apple Music blocking               |
| `cmd_q/`         | Hold-to-quit feature               |
| `widgets/`       | Widget window management           |
| `utils/`         | Shared utilities                   |
| `events.rs`      | Event name constants               |
| `cache.rs`       | Cache management                   |

### Frontend (React)

Location: `app/ui/`

| Directory        | Purpose                           |
| ---------------- | --------------------------------- |
| `renderer/`      | Window-specific UI (bar, widgets) |
| `components/`    | Shared React components           |
| `hooks/`         | Custom React hooks                |
| `stores/`        | Zustand state stores              |
| `design-system/` | Styling tokens and themes         |
| `types/`         | TypeScript type definitions       |
| `utils/`         | Utility functions                 |

## Communication Patterns

### 1. Tauri Commands

Frontend invokes Rust functions via Tauri's command system.

**Rust side:**

```rust
#[tauri::command]
pub fn get_battery_info() -> Result<BatteryInfo, String> {
    // Implementation
}

// Register in lib.rs
tauri::generate_handler![
    bar::components::battery::get_battery_info,
]
```

**TypeScript side:**

```typescript
const info = await invoke<BatteryInfo>('get_battery_info');
```

### 2. Tauri Events

Backend emits events that frontend listens to.

**Event naming convention:** `stache://<module>/<event-name>`

**Rust side:**

```rust
// events.rs
pub mod media {
    pub const PLAYBACK_CHANGED: &str = "stache://media/playback-changed";
}

// Emitting
app_handle.emit(events::media::PLAYBACK_CHANGED, &payload)?;
```

**TypeScript side:**

```typescript
// tauri-events.ts
export const MEDIA_PLAYBACK_CHANGED = 'stache://media/playback-changed';

// Listening
useTauriEvent<MediaPayload>(MEDIA_PLAYBACK_CHANGED, (event) => {
  // Handle event
});
```

### 3. CLI to App Communication

CLI commands communicate with the running desktop app via macOS `NSDistributedNotificationCenter`.

**Notification names:**

- `com.marcosmoura.stache.window-focus-changed`
- `com.marcosmoura.stache.workspace-changed`
- `com.marcosmoura.stache.reload`

**Flow:**

1. CLI sends notification via `NSDistributedNotificationCenter`
2. Desktop app's IPC listener receives notification
3. App processes the command or emits appropriate Tauri event

## Module Details

### Configuration System

Location: `app/native/src/config/`

- `types.rs` - Configuration structs with `schemars` for JSON Schema generation
- `env.rs` - Environment file parsing
- `watcher.rs` - File system watcher for hot reload

Configuration is loaded at startup and can be reloaded via `stache reload`.

### Status Bar

Location: `app/native/src/bar/`

Components:

- `battery.rs` - Battery status monitoring
- `cpu.rs` - CPU usage tracking
- `media.rs` - Media playback info
- `weather.rs` - Weather data fetching
- `keepawake.rs` - Sleep prevention
- `menubar.rs` - Menu bar visibility

### Wallpaper System

Location: `app/native/src/wallpaper/`

- `manager.rs` - Wallpaper selection and cycling
- `processing.rs` - Image effects (blur, rounded corners)
- `macos.rs` - macOS wallpaper APIs

Processed wallpapers are cached in `~/Library/Caches/com.marcosmoura.stache/wallpapers/`.

### Audio Management

Location: `app/native/src/audio/`

- `device.rs` - Device matching logic
- `list.rs` - Device enumeration
- `priority.rs` - Priority-based switching

Uses CoreAudio APIs for device management.

### Global Hotkeys

Location: `app/native/src/hotkey/`

Uses CoreGraphics event taps to capture keyboard events system-wide.

### MenuAnywhere

Location: `app/native/src/menu_anywhere/`

- `accessibility.rs` - Accessibility API integration
- `event_monitor.rs` - Mouse/keyboard event monitoring
- `menu_builder.rs` - NSMenu construction

### noTunes

Location: `app/native/src/notunes/`

Monitors app launch events and terminates Apple Music/iTunes.

### Hold-to-Quit

Location: `app/native/src/cmd_q/`

Intercepts Cmd+Q events and requires holding for 1.5 seconds.

## Frontend Architecture

### Technology Stack

- **React 19** - UI framework
- **TypeScript** - Type safety
- **Linaria** - Zero-runtime CSS-in-JS
- **TanStack Query** - Server state management
- **Zustand** - Client state management

### Component Structure

```text
ComponentName/
├── index.ts                  # Re-export
├── ComponentName.tsx         # Component
├── ComponentName.styles.ts   # Linaria styles
├── ComponentName.types.ts    # TypeScript types (optional)
├── ComponentName.state.ts    # Business logic (optional)
└── ComponentName.test.tsx    # Tests
```

### Key Hooks

| Hook                 | Purpose                   |
| -------------------- | ------------------------- |
| `useTauriEvent`      | Subscribe to Tauri events |
| `useTauri`           | Access Tauri APIs         |
| `useMediaQuery`      | Responsive design         |
| `useCrossWindowSync` | Cross-window state        |
| `useWidgetToggle`    | Widget visibility         |

### Design System

Location: `app/ui/design-system/`

- `colors.ts` - Catppuccin Mocha color palette
- `motion.ts` - Animation constants

## Window System

### Bar Window

- **Position:** Top of screen
- **Behavior:** Sticky (all spaces), below menu bar
- **Features:** Auto-repositions on screen changes

### Widgets Window

- **Position:** Overlay
- **Behavior:** Always on top, sticky
- **Features:** Click-outside detection, auto-dismiss

## Caching

**Location:** `~/Library/Caches/com.marcosmoura.stache/`

**Subdirectories:**

- `media_artwork/` - Resized album art (128x128)
- `wallpapers/` - Processed wallpaper images

## macOS APIs Used

| API                               | Purpose                  |
| --------------------------------- | ------------------------ |
| `NSDistributedNotificationCenter` | IPC between CLI and app  |
| `NSWorkspace`                     | App launching, detection |
| `NSRunningApplication`            | App termination          |
| `CoreAudio`                       | Audio device management  |
| `CoreGraphics`                    | Event taps (hotkeys)     |
| `Accessibility APIs`              | Menu bar access          |
| `NSScreen`                        | Display management       |

## Sidecar Binaries

Bundled executables in `app/native/binaries/`:

| Binary          | Purpose                   |
| --------------- | ------------------------- |
| `media-control` | Media playback monitoring |
| `caffeinate`    | System sleep prevention   |

## Error Handling

- Rust uses `Result<T, String>` for command errors
- Frontend displays errors via toast notifications
- Critical errors are logged to Console.app

## Security Considerations

- API keys stored in `.env` files, not in config
- Accessibility permissions required for sensitive features
- No network requests except weather API (user-provided key)
