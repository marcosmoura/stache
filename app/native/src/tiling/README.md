# Tiling Window Manager Architecture

This document provides a concise overview of the tiling window manager implementation.

## System Overview

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                              CLI / IPC                                  │
│                    (stache tiling workspace --focus)                    │
└─────────────────────────────────┬───────────────────────────────────────┘
                                  │
┌─────────────────────────────────▼───────────────────────────────────────┐
│                          TilingManager                                  │
│                         (manager/mod.rs)                                │
│  - Window tracking    - Workspace switching    - Layout application     │
└──────┬─────────────────────┬─────────────────────┬──────────────────────┘
       │                     │                     │
       ▼                     ▼                     ▼
┌──────────────┐    ┌────────────────┐    ┌──────────────────┐
│   Monitors   │    │     Layout     │    │    Borders       │
│              │    │   Algorithms   │    │  (JankyBorders)  │
│ - observer   │    │ - dwindle      │    │                  │
│ - app_monitor│    │ - master       │    │ - janky.rs       │
│ - screen_mon │    │ - grid/split   │    │ - mach_ipc.rs    │
│ - mouse_mon  │    │ - monocle      │    │ - manager.rs     │
└──────────────┘    └────────────────┘    └──────────────────┘
```

## Module Overview

| Module              | Lines | Purpose                                                            |
| ------------------- | ----- | ------------------------------------------------------------------ |
| `manager/`          | 3.4k  | Core singleton: window tracking, workspace ops, layout application |
| `window.rs`         | 3.0k  | AX API wrappers: get/set window frames, focus, hide/show           |
| `animation.rs`      | 1.9k  | CVDisplayLink animation with spring physics                        |
| `event_handlers.rs` | 1.2k  | All handle\_\* functions for window/app/screen events              |
| `state.rs`          | 1.2k  | Data types: Screen, Workspace, TrackedWindow, Rect                 |
| `observer.rs`       | 900   | AXObserver setup for window event notifications                    |
| `layout/`           | 2.5k  | Layout algorithms: dwindle, master, grid, split, monocle, floating |
| `borders/`          | 1.7k  | JankyBorders integration via Mach IPC                              |

## Event Flow

```text
1. Window Event (AXObserver callback)
   └─► observer.rs::observer_callback()
       └─► event_handlers.rs::handle_window_event()
           └─► manager.rs (update state, apply layout)

2. App Launch (NSWorkspace notification)
   └─► app_monitor.rs::handle_app_launch_notification()
       └─► event_handlers.rs::handle_app_launch()
           └─► observer.rs::add_observer() + track windows

3. Screen Change (CGDisplayReconfigurationCallback)
   └─► screen_monitor.rs::display_reconfiguration_callback()
       └─► event_handlers.rs::handle_screen_change()
           └─► manager.rs (reassign workspaces, reapply layouts)

4. Mouse Up (CGEventTap callback)
   └─► mouse_monitor.rs::mouse_event_callback()
       └─► event_handlers.rs::on_mouse_up()
           └─► drag_state.rs::finish_operation() (reapply layout or update ratios)
```

## Thread Model

| Thread        | Components            | Notes                      |
| ------------- | --------------------- | -------------------------- |
| Main          | TilingManager, State  | All state mutations        |
| AXObserver    | observer.rs callbacks | Posts to main via callback |
| CVDisplayLink | animation.rs          | 60fps render loop          |
| CGEventTap    | mouse_monitor.rs      | Global mouse events        |

**Synchronization:** `parking_lot::RwLock` on TilingManager, atomics for flags.

## State Management

```text
TilingState
├── screens: Vec<Screen>           # Physical displays
├── workspaces: Vec<Workspace>     # Virtual desktops
│   ├── window_ids: Vec<u32>       # Tracked windows
│   ├── layout: LayoutType         # Current layout
│   └── layout_cache: LayoutCache  # Cached positions
└── workspace_index: HashMap       # O(1) name lookup
```

**Caches:** Layout results, AX elements (5s TTL), screen list (1s TTL), CG window list (50ms TTL).

## JankyBorders Integration

Stache delegates border rendering to [JankyBorders](https://github.com/FelixKratz/JankyBorders):

1. **Mach IPC** (preferred): Direct messaging via `git.felix.borders` bootstrap service (~0.5ms)
2. **CLI fallback**: Spawns `borders` command when IPC unavailable (~20-50ms)

Color updates sent on: focus change, layout change, config reload.

## Performance Considerations

- **Event coalescing**: 4ms window for rapid move/resize events
- **Layout caching**: Hash-based cache invalidation
- **SmallVec**: Inline storage for up to 16 windows (avoids heap allocation)
- **Parallel layout**: `rayon` for multi-window position updates
- **Observer filtering**: Skip system apps (Dock, Spotlight, Control Center)

## Debugging Tips

```bash
# Check if tiling is enabled
stache tiling query workspaces

# List tracked windows
stache tiling query windows

# Watch for events (run stache with RUST_LOG=debug)
RUST_LOG=debug stache

# Test layout without animation
stache tiling workspace --layout dwindle
```

**Common issues:**

- Windows not tracking: Check accessibility permissions, window rules
- Layout not applying: Verify workspace has windows, check `query windows`
- Borders not showing: Ensure JankyBorders is installed and running
