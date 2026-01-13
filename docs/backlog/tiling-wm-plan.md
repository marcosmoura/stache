# Tiling Window Manager Implementation Plan

> This document tracks the implementation progress of the Stache tiling window manager.
> See [tiling-wm-requirements.md](./tiling-wm-requirements.md) for full requirements.

## Status: In Progress

**Last Updated**: 2026-01-13
**Current Phase**: Milestone 12 - Polish & Testing

---

## Module Structure

```text
app/native/src/
├── utils/
│   ├── accessibility.rs    # Shared accessibility permission utilities
│   ├── command.rs          # Shell command utilities
│   ├── ipc.rs              # NSDistributedNotificationCenter IPC
│   ├── ipc_socket.rs       # Unix socket IPC
│   ├── mod.rs              # Utils module root
│   ├── objc.rs             # Objective-C helpers
│   ├── path.rs             # Path expansion utilities
│   ├── thread.rs           # Thread utilities
│   └── window.rs           # Shared window utilities
│
└── tiling/
    ├── mod.rs              # Module root, init(), re-exports
    ├── state.rs            # TilingState, Screen, Workspace, TrackedWindow
    ├── manager.rs          # TilingManager singleton
    ├── screen.rs           # Screen detection via NSScreen
    ├── workspace.rs        # Workspace creation, switching, visibility
    ├── window.rs           # Window operations (get, move, resize, hide, show)
    ├── observer.rs         # AXObserver for window events
    ├── rules.rs            # Window rule matching logic
    ├── drag_state.rs       # Drag-and-drop state tracking
    ├── mouse_monitor.rs    # Mouse position monitoring
│
│     ├── layout/
│     │   ├── mod.rs          # Layout module root, calculate_layout()
│     │   ├── gaps.rs         # Gaps struct and configuration
│     │   ├── helpers.rs      # Shared layout helper functions
│     │   ├── dwindle.rs      # Dwindle layout (recursive BSP)
│     │   ├── floating.rs     # Floating layout (no repositioning)
│     │   ├── master.rs       # Master-stack layout
│     │   ├── monocle.rs      # Monocle layout (fullscreen stacking)
│     │   └── split.rs        # Split layouts (vertical/horizontal)
│     │
│     └── borders/
│         ├── mod.rs          # Borders module root, init()
│         ├── manager.rs      # BorderManager singleton (state tracking)
│         ├── janky.rs        # JankyBorders integration (CLI/Mach IPC)
│         └── mach_ipc.rs     # Low-latency Mach IPC for JankyBorders
```

---

## Implementation Milestones

### Milestone 1: Foundation & Configuration

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Move `menu_anywhere/accessibility.rs` to `utils/accessibility.rs`
- [x] Update `menu_anywhere/mod.rs` to use shared accessibility module
- [x] Create `tiling/mod.rs` module skeleton
- [x] Add tiling configuration types to `config/types.rs`:
  - [x] `TilingConfig` (root)
  - [x] `WorkspaceConfig`
  - [x] `WindowRule`
  - [x] `LayoutType` enum
  - [x] `AnimationConfig`
  - [x] `EasingType` enum
  - [x] `GapsConfigValue` (global/per-screen)
  - [x] `GapsConfig`
  - [x] `GapValue` (uniform/per-axis/per-side)
  - [x] `FloatingConfig`
  - [x] `FloatingPreset`
  - [x] `DimensionValue` (pixels/percentage)
  - [x] `FloatingPosition` enum
  - [x] `MasterConfig`
- [x] Add `TilingConfig` to `StacheConfig`
- [x] Regenerate JSON schema (`./scripts/generate-schema.sh`)
- [x] Add CLI command structure to `cli/commands.rs`:
  - [x] `TilingCommands` enum
  - [x] `TilingQueryCommands` enum
  - [x] `TilingWindowCommands` enum
  - [x] `TilingWorkspaceCommands` enum
  - [x] `Direction` enum
  - [x] `ResizeDimension` enum
- [x] Add stub implementations for all CLI commands
- [x] Add tiling event constants to `events.rs`:
  - [x] `WORKSPACE_CHANGED`
  - [x] `WORKSPACE_WINDOWS_CHANGED`
  - [x] `LAYOUT_CHANGED`
  - [x] `WINDOW_TRACKED`
  - [x] `WINDOW_UNTRACKED`
  - [x] `SCREENS_CHANGED`
  - [x] `INITIALIZED`
- [x] Add IPC notifications to `utils/ipc.rs`
- [x] Run tests, fix clippy warnings, ensure build passes

**Verification**: Config parses correctly, CLI commands show help, schema regenerated

---

### Milestone 2: State & Screen Detection

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Create `tiling/state.rs`:
  - [x] `TilingState` struct
  - [x] `Screen` struct
  - [x] `Workspace` struct
  - [x] `TrackedWindow` struct
  - [x] `Rect` and `Point` types
  - [x] Serialization for JSON output
- [x] Create `tiling/screen.rs`:
  - [x] NSScreen enumeration via objc
  - [x] Main screen detection
  - [x] Built-in screen detection
  - [x] Screen name extraction
  - [x] Frame and scale factor retrieval
- [x] Create `tiling/manager.rs`:
  - [x] `TilingManager` struct
  - [x] `OnceLock<Arc<RwLock<TilingManager>>>` singleton pattern
  - [x] `get_manager()` function
  - [x] `init()` function (checks config.enabled and permissions)
- [x] Add `tiling::init()` call to `lib.rs`
- [x] Implement `stache tiling query screens` command
- [x] Add unit tests for screen detection
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: `stache tiling query screens` returns real screen data as JSON

---

### Milestone 3: Window Tracking

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Create `tiling/window.rs`:
  - [x] `AXUIElement` wrapper for windows
  - [x] `get_all_windows()` - enumerate all windows
  - [x] `get_focused_window()` - get currently focused window
  - [x] `set_window_frame(id, frame)` - move/resize window
  - [x] `focus_window(id)` - bring window to front
  - [x] `hide_window(id)` / `hide_app(pid)` - hide via `NSRunningApplication`
  - [x] `show_window(id)` / `unhide_app(pid)` - unhide window
  - [x] `get_running_apps()` - enumerate running applications
  - [x] `get_cg_window_list()` - get window list from `CGWindowList`
  - [x] `WindowInfo` struct with bundle ID, app name, PID, frame, etc.
- [x] Create `tiling/rules.rs`:
  - [x] `WindowRule` matching logic
  - [x] AND-logic for multiple properties
  - [x] `matches_window(rule, window)` function
  - [x] `find_matching_workspace(window)` function
- [x] Create `tiling/observer.rs`:
  - [x] `AXObserver` creation and management
  - [x] Window created notification
  - [x] Window destroyed notification
  - [x] Window focused notification
  - [x] Window moved notification
  - [x] Window resized notification
  - [x] Window minimized/unminimized notifications
  - [x] Title changed notification
  - [x] Callback dispatch to manager
- [x] Integrate observer with manager:
  - [x] Track windows on startup
  - [x] Handle window create events
  - [x] Handle window destroy events
  - [x] Assign windows to workspaces based on rules
- [x] Implement `stache tiling query windows` command
- [x] Add unit tests for window operations
- [x] Add unit tests for rule matching
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: All windows enumerated correctly, rules matching works

---

### Milestone 4: Workspace Management

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Create `tiling/workspace.rs`:
  - [x] Create workspaces from config
  - [x] Create default workspaces (1 per screen) when no config
  - [x] Workspace-to-screen assignment
  - [x] Track visible workspace per screen
  - [x] Track focused workspace globally
- [x] Implement window assignment:
  - [x] Assign windows to workspaces on startup
  - [x] Assign new windows based on rules
  - [x] Fall back to focused workspace when no rule matches
- [x] Implement workspace switching:
  - [x] `hide_workspace_windows(workspace)` - hide all windows
  - [x] `show_workspace_windows(workspace)` - show all windows
  - [x] `switch_workspace(name)` - full switch logic
  - [x] Track focus history per workspace
  - [x] Restore focus when switching back
- [x] Implement focus-follows-workspace:
  - [x] Detect when focused window is in different workspace
  - [x] Auto-switch to that workspace
  - [x] Only affect the screen containing the window
- [x] Implement startup behavior:
  - [x] Detect focused window on launch
  - [x] Switch to containing workspace
  - [x] Set first workspace visible on other screens
- [x] Implement `stache tiling workspace --focus` command
- [x] Implement `stache tiling query workspaces` command
- [x] Emit `WORKSPACE_CHANGED` events
- [x] Add unit tests for workspace operations
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Workspace switching hides/shows correct windows

---

### Milestone 5: Basic Layouts

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Create `tiling/layout.rs`:
  - [x] `LayoutResult` type alias for layout calculations
  - [x] `calculate_layout()` function with all layout types
  - [x] `calculate_layout_with_gaps()` for gap-aware layouts
- [x] Implement `Gaps` struct in `tiling/layout.rs`:
  - [x] Parse `GapsConfigValue` (global vs per-screen)
  - [x] `Gaps::from_config()` to resolve gaps for a screen
  - [x] `Gaps::apply_outer()` to calculate available area
  - [x] Inner gaps (horizontal/vertical) between windows
  - [x] Outer gaps (top/right/bottom/left) from screen edges
- [x] Implement Floating layout:
  - [x] Returns empty result (no repositioning)
  - [x] Windows keep their current positions
- [x] Implement Monocle layout:
  - [x] All windows maximized to available area
  - [x] All windows at same position (stacked)
- [x] Implement Dwindle layout:
  - [x] Recursive binary space partitioning
  - [x] Alternating horizontal/vertical splits
  - [x] Balanced window distribution
  - [x] Gap-aware splitting
- [x] Implement Split layouts:
  - [x] `Split` - auto-detect based on screen orientation
  - [x] `SplitVertical` - windows stacked top to bottom
  - [x] `SplitHorizontal` - windows side by side
  - [x] Even distribution with inner gaps
- [x] Implement Master layout:
  - [x] Master window gets configurable ratio (from config)
  - [x] Stack windows share remaining space vertically
  - [x] Gap between master and stack
  - [x] Gaps between stack windows
- [x] Integrate layouts with workspace switching:
  - [x] Apply layout after showing windows (`switch_workspace()`)
  - [x] Recalculate on window add (`track_window()`)
  - [x] Recalculate on window remove (`untrack_window()`)
- [x] Implement `stache tiling workspace --layout` command:
  - [x] IPC notification `TilingSetLayout` handler
  - [x] Parse layout string (kebab-case)
  - [x] Call `manager.set_workspace_layout()`
- [x] Implement `stache tiling workspace --balance` command:
  - [x] IPC notification `TilingWorkspaceBalance` handler
  - [x] Re-apply layout to reset ratios
- [x] Add unit tests (26 tests total):
  - [x] Gaps tests (uniform, is_zero, apply_outer, from_config)
  - [x] Floating layout tests
  - [x] Monocle layout tests
  - [x] Dwindle layout tests (1-4 windows, with gaps)
  - [x] Split layout tests (vertical, horizontal, auto, with gaps)
  - [x] Master layout tests (1-3 windows, ratio clamping, with gaps)
  - [x] calculate_layout routing tests
  - [x] Helper function tests
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Windows arrange according to layout type, gaps applied correctly

---

### Milestone 6: Window Commands

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Implement focus navigation:
  - [x] `focus_direction(direction)` - up/down/left/right
  - [x] `focus_previous()` / `focus_next()` - cycle through windows
  - [x] Wrap around at edges
  - [x] Spatial navigation based on window positions
- [x] Implement `stache tiling window --focus` command
- [x] Implement window swapping:
  - [x] `swap_direction(direction)` - swap with neighbor
  - [x] Re-apply layout after swap
- [x] Implement `stache tiling window --swap` command
- [x] Implement resize:
  - [x] `resize_window(dimension, delta)` - adjust ratios
  - [x] Uses existing ratio calculation logic
  - [x] Re-apply layout after resize
- [x] Implement `stache tiling window --resize` command
- [x] Implement send-to-workspace:
  - [x] Remove from current workspace
  - [x] Add to target workspace
  - [x] Hide if target workspace not visible
  - [x] Update both workspace layouts
- [x] Implement `stache tiling window --send-to-workspace` command
- [x] Implement send-to-screen:
  - [x] Move to visible workspace on target screen
  - [x] Update layouts on both screens
- [x] Implement `stache tiling window --send-to-screen` command
- [x] CLI sends IPC notifications, IPC handler calls manager methods
- [x] Run tests, fix clippy warnings and ensure build passes

**Bug Fixes** (during Milestone 6):

- [x] Fixed focus commands operating on wrong workspace (set_focused_window now updates focused_workspace)
- [x] Fixed focus command race condition (added cooldown to ignore stale macOS focus events)
- [x] Replaced fixed window creation delay with polling for AX readiness
- [x] Fixed send-to-workspace: window now remains focused (switches to target workspace and focuses moved window)
- [x] Fixed send-to-workspace: source workspace layout now updates correctly
- [x] Fixed send-to-workspace: target workspace layout now updates correctly (via switch_workspace)
- [x] Fixed send-to-workspace: focus cycling now works for ALL windows (target workspace's focused_window_index is set to moved window)

**Verification**: All `stache tiling window` commands functional

---

### Milestone 7: Grid Layout & Layout Enhancements

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

> **Note**: Split and Master layouts were implemented in Milestone 5. This milestone focuses on Grid layout and layout enhancements.

- [x] Implement Grid layout in `tiling/layout/grid.rs`:
  - [x] Calculate rows and columns based on window count
  - [x] Adapt grid orientation to screen aspect ratio (landscape vs portrait)
  - [x] Handle odd counts by distributing windows to balance grid
  - [x] Gap-aware grid calculations
  - [x] 18 unit tests for grid layout
- [x] Enhance Master layout:
  - [x] Configurable master position (left/right/top/bottom/auto)
  - [x] Auto mode: left for landscape screens, top for portrait
  - [x] ~20 new unit tests for master positions
- [x] Add `MasterPosition` enum to config types
- [x] Add `position` field to `MasterConfig`
- [x] Regenerate JSON schema with new types
- [x] Run tests (766 total), fix clippy warnings and ensure build passes

**Verification**: Grid layout arranges windows correctly, Master layout supports all 4 positions

---

### Milestone 8: Workspace Screen Movement

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Implement workspace-to-screen movement:
  - [x] `send_workspace_to_screen(workspace, screen)` function in manager.rs
  - [x] Track original screen for restoration via `original_screen_id` field
  - [x] Update screen's workspace list (screen_id field)
  - [x] Handle visibility (hide on source, show on target)
- [x] Implement restoration logic:
  - [x] Remember original screen_id when first moved
  - [x] Clear original_screen_id when returning home
- [x] Implement `stache tiling workspace --send-to-screen` command via IPC
- [x] Handle edge cases:
  - [x] Moving to same screen (rejected with message)
  - [x] Fallback workspace becomes visible on source screen
- [x] Add unit tests for workspace movement (4 new tests)
- [x] Run tests (770 total), fix clippy warnings and ensure build passes

**Verification**: Workspaces move between screens correctly

---

### Milestone 9: Advanced Features

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

**Goal**: Implement animations for window transitions, drag-and-drop window swapping, and performance optimizations.

#### Phase 9.1: Animations

- [x] Create `tiling/animation.rs`:
  - [x] `AnimationSystem` struct
  - [x] Frame interpolation logic
  - [x] Easing functions (linear, ease-in, ease-out, ease-in-out, spring)
    - [x] Evaluate usage of existing easing crate or implement manually
  - [x] Animation thread/timer management
  - [x] `animate_window_frames(transitions)` function
- [x] Integrate animations:
  - [x] Animate workspace switches
  - [x] Animate layout changes
  - [x] Animate window moves
  - [x] Respect `animations.enabled` config
- [x] Add unit tests for animations
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Window animations are smooth and respect configuration

---

#### Phase 9.2: Drag and Drop

- [x] Implement drag-and-drop swapping:
  - [x] Detect window drag start (via `AXWindowMoved` + mouse down)
  - [x] Track drag position (using mouse monitor)
  - [x] Detect drop on another window (center-point-in-bounds check)
  - [x] Trigger swap operation (`swap_windows_by_id`)
- [ ] Add visual feedback during drag (optional highlight) - Deferred to borders milestone
- [x] Handle edge cases:
  - [x] Drag to different workspace (handled - swap only within same workspace)
  - [x] Drag to different screen (handled - swap only within same workspace)
  - [x] Cancelled drags (handled - window snaps back to layout position)
- [x] Add unit tests for drag-and-drop logic (8 new tests)
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Drag-swap works reliably

---

#### Phase 9.3: Performance Optimizations

- [x] Profile window operations and review existing optimizations:
  - [x] Layout diff mode - only repositions windows that actually moved (2px threshold)
  - [x] Floating layout early return - no repositioning needed
  - [x] Frame caching in `TrackedWindow` structs
- [x] Optimize observer callbacks:
  - [x] Focus event debouncing (`last_programmatic_focus`)
  - [x] Workspace switch debouncing (`last_workspace_switch`)
  - [x] Drag operation tracking ignores redundant move/resize events
  - [x] Mouse button tracking distinguishes programmatic from user moves
- [x] Minimize redundant operations:
  - [x] Animation cancellation for rapid commands
  - [x] Debug logging wrapped in `#[cfg(debug_assertions)]` for release builds
- [x] Run tests (804 total), fix clippy warnings and ensure build passes

**Note**: The codebase already had extensive optimizations in place. This phase focused on:

1. Documenting existing optimizations
2. Removing debug logging overhead in release builds
3. Verifying debouncing/caching strategies are working correctly

**Verification**: Tiling operations feel snappy, no lag during window management

---

### Milestone 10: Window Borders

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

**Goal**: Implement configurable borders around tiled windows with support for different states, gradients, rounded corners, and animations.

#### Phase 10.1: Configuration Types

- [x] Add `BordersConfig` to `TilingConfig` in `config/types.rs`:
  - [x] `enabled: bool` - Whether borders are enabled (default: false)
  - [x] `width: u32` - Border width in pixels (default: 4)
  - [x] `animation: BorderAnimationConfig` - Animation settings
  - [x] `colors: BorderColors` - State-based color configuration
  - [x] `ignore: Vec<WindowRule>` - Rules for windows that should not have borders
- [x] Add `BorderAnimationConfig` struct:
  - [x] `duration_ms: u32` - Animation duration for appear/disappear (default: 200)
  - [x] `easing: EasingType` - Easing function (reuse existing enum)
- [x] Add `BorderColor` enum (supports solid colors and gradients):
  - [x] `Solid(String)` - Hex color string (e.g., "#FF0000")
  - [x] `Gradient { from: String, to: String, angle: Option<f64> }` - Linear gradient
- [x] Add `BorderColors` struct for state-based colors:
  - [x] `focused: BorderColor` - Color for focused window
  - [x] `unfocused: BorderColor` - Color for unfocused windows
  - [x] `monocle: BorderColor` - Color for monocle layout windows
  - [x] `floating: BorderColor` - Color for floating windows
- [x] Implement hex color parsing to RGBA (`Rgba` struct, `parse_hex_color()` function)
- [x] Regenerate JSON schema (`./scripts/generate-schema.sh`)
- [x] Add unit tests for configuration parsing and color conversion (24 tests)
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Configuration parses correctly, schema includes border types

---

#### Phase 10.2: Border Window Foundation

- [x] Create `tiling/borders/mod.rs`:
  - [x] Module root with `init()` function and re-exports
- [x] Create `tiling/borders/window.rs`:
  - [x] `BorderWindow` struct representing a single border overlay
  - [x] NSWindow creation via Objective-C FFI with properties:
    - [x] `NSWindowStyleMaskBorderless` - No title bar
    - [x] `NSWindowLevelFloating` - Stay above normal windows
    - [x] `ignoresMouseEvents: YES` - Click-through
    - [x] `opaque: NO`, `backgroundColor: clearColor` - Transparent
    - [x] `hasShadow: NO` - No shadow
    - [x] `collectionBehavior: canJoinAllSpaces` - Follow to all spaces
  - [x] `set_frame(rect: Rect)` - Position and size the border window
  - [x] `set_corner_radius(radius: f64)` - Match target window corner radius
  - [x] `set_visible(visible: bool)` - Show/hide border
  - [x] `set_color(r, g, b, a)` - Set border color
  - [x] `set_border_width(width)` - Set border width
  - [x] `destroy()` - Clean up NSWindow resources
- [x] Add macOS API bindings inline in `window.rs`:
  - [x] NSWindow creation and configuration
  - [x] CAShapeLayer setup for border drawing
  - [x] CGColor creation via Core Graphics FFI
- [x] Implement `Send + Sync` for `BorderWindow` (unsafe, main-thread-only operations)
- [x] Add unit tests for border window constants (4 tests)
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Border windows can be created, positioned, and destroyed

---

#### Phase 10.3: Border Rendering

- [x] Create `tiling/borders/renderer.rs`:
  - [x] `BorderRenderer` struct with static methods for border rendering
  - [x] `apply_solid_color(border, rgba)` - Apply solid color to border
  - [x] `apply_gradient(border, from, to, angle)` - Apply gradient to border
  - [x] `apply_border_color(border, color)` - Apply `BorderColor` (solid or gradient)
  - [x] `angle_to_gradient_points(angle)` - Convert angle to CAGradientLayer start/end points
- [x] Add `BorderState` enum:
  - [x] `Focused` - Currently focused window
  - [x] `Unfocused` (default) - Visible but not focused
  - [x] `Monocle` - In monocle layout
  - [x] `Floating` - In floating layout or marked as floating
- [x] Handle corner radius detection from target window:
  - [x] `detect_corner_radius(window_id)` - Query window corner radius (returns default for now)
  - [x] `get_corner_radius(window_id)` - Get or detect corner radius
  - [x] `DEFAULT_CORNER_RADIUS = 10.0` - macOS default fallback
- [x] Add unit tests for gradient calculations and border state (11 tests)
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Borders render with correct colors, widths, and corner radii

---

#### Phase 10.4: Border Manager

- [x] Create `tiling/borders/manager.rs`:
  - [x] `BorderManager` struct (singleton pattern with `OnceLock<Arc<RwLock<>>>`)
  - [x] `borders: HashMap<u32, BorderInfo>` - Map window_id to border info
  - [x] `BorderInfo` struct with border, state, workspace, visibility
  - [x] `get_border_manager()` - Get global manager instance
  - [x] `init_border_manager()` - Initialize manager from config
  - [x] `create_border(window_id, frame, state, workspace, visible)` - Create border for window
  - [x] `remove_border(window_id)` - Remove border for window
  - [x] `update_border_frame(window_id, frame)` - Update border position/size
  - [x] `update_border_state(window_id, state)` - Update border color based on state
  - [x] `update_border_workspace(window_id, workspace)` - Update workspace tracking
  - [x] `show_borders_for_workspace(workspace)` - Show borders for workspace
  - [x] `hide_borders_for_workspace(workspace)` - Hide borders for workspace
  - [x] `set_enabled(enabled)` - Enable/disable all borders
  - [x] `refresh_all()` - Refresh all border colors from config
  - [x] `should_have_border(window)` - Check if window should have border
  - [x] Helper methods: `border_count()`, `get_border_state()`, `has_border()`
- [x] `BorderState` enum moved to `renderer.rs` (Focused, Unfocused, Monocle, Floating)
- [x] Implement ignore rule matching (reuses `matches_window()` from `rules.rs`)
- [x] Add `WindowInfo::new_for_test_with_app()` helper for tests with bundle_id/app_name
- [x] Add unit tests for border manager operations (10 tests)
- [x] Run tests, fix clippy warnings and ensure build passes

**Verification**: Border manager can create, update, and remove borders correctly

---

#### Phase 10.5: Integration with Tiling Manager

- [x] Integrate border creation in `TilingManager::track_window()`:
  - [x] `create_border_for_window()` helper method
  - [x] `determine_border_state()` to get initial state based on layout/focus
  - [x] Create border with visibility based on workspace visibility
- [x] Integrate border removal in `TilingManager::untrack_window()`:
  - [x] `remove_border_for_window()` helper method called when window untracked
- [x] Integrate border updates in window event handlers:
  - [x] `update_window_frame()` now calls `update_border_frame()` via static helper
  - [x] Focus changes via `set_focused_window()` → `update_focus_border_states()`
  - [x] Updates both old focused (→ Unfocused) and new focused (→ Focused) windows
- [x] Integrate with workspace switching:
  - [x] `switch_workspace()` → `hide_borders_for_workspace()` for old workspace
  - [x] `switch_workspace()` → `show_borders_for_workspace()` for new workspace
- [x] Integrate with layout changes:
  - [x] `set_workspace_layout()` → `update_all_border_states_for_layout()`
  - [x] Monocle layout → all borders get `Monocle` state
  - [x] Floating layout → all borders get `Floating` state
  - [x] Other layouts → focused gets `Focused`, others get `Unfocused`
- [x] Borders are pure visual overlays (don't interfere with layout calculations)
- [x] All 864 tests pass, clippy clean

**Verification**: Borders appear/update correctly when windows are managed

---

#### Phase 10.6: JankyBorders Integration

> **Architecture Change**: After extensive testing, the custom SkyLight-based border implementation was replaced with JankyBorders integration. JankyBorders is a battle-tested, high-performance border rendering tool that handles all the complex window server interactions. Stache now acts as a controller, updating JankyBorders configuration based on window state.

- [x] Remove custom SkyLight border implementation:
  - [x] Remove `tiling/borders/window.rs` (BorderWindow)
  - [x] Remove `tiling/borders/skylight.rs` (SkyLight bindings)
  - [x] Remove `tiling/borders/renderer.rs` (border rendering)
  - [x] Simplify `tiling/borders/manager.rs` (state tracking only)
- [x] Create `tiling/borders/janky.rs`:
  - [x] `is_available()` - Check if `borders` command is in PATH
  - [x] `is_running()` - Check if JankyBorders process is running (Mach IPC + pgrep fallback)
  - [x] `send_command(args)` - Execute via Mach IPC (fast) or CLI (fallback)
  - [x] `set_active_color(color)` - Update active border color
  - [x] `set_inactive_color(color)` - Update inactive border color
  - [x] `set_background_color(color)` - Update background color
  - [x] `set_width(width)` - Update border width
  - [x] `set_style(style)` - Update border style (round/square)
  - [x] `set_order(above)` - Update border order (above/below windows)
  - [x] `set_hidpi(enabled)` - Update HiDPI setting
  - [x] `set_ax_focus(enabled)` - Update accessibility focus tracking
  - [x] `set_blacklist(apps)` - Set apps to exclude from borders
  - [x] `set_whitelist(apps)` - Set apps to include (exclusive mode)
  - [x] `apply_config(config)` - Apply full border configuration
  - [x] `update_colors_for_state(is_monocle, is_floating)` - Update colors based on state
  - [x] `refresh()` - Re-apply configuration from current settings
  - [x] Color conversion functions: `rgba_to_hex()`, `hex_to_janky()`, `border_color_to_janky()`
  - [x] Support for solid colors, gradients, and glow effects
  - [x] 11 unit tests for color conversion
- [x] Create `tiling/borders/mach_ipc.rs`:
  - [x] Low-latency Mach IPC client for JankyBorders communication
  - [x] `JankyConnection` struct with bootstrap service lookup
  - [x] `send(args)` - Send arguments via Mach IPC (~0.1-0.5ms latency)
  - [x] `send_one(arg)` - Send single argument
  - [x] `send_batch(args)` - Send multiple arguments in one message
  - [x] `is_connected()` - Check if Mach IPC connection is active
  - [x] `connect()` - Establish connection to JankyBorders
  - [x] `invalidate()` - Force reconnection on next send
  - [x] Automatic reconnection on connection loss
  - [x] Correct struct alignment (`#[repr(C, packed(4))]`) matching JankyBorders' C structs
  - [x] Compile-time size assertion for MachMessage (44 bytes)
  - [x] 5 unit tests for Mach IPC
- [x] Update `tiling/borders/mod.rs`:
  - [x] `init()` - Check for JankyBorders, establish Mach IPC, apply initial config
  - [x] Export `janky`, `mach_ipc`, `manager` modules
  - [x] Re-export commonly used types
- [x] Integrate with TilingManager:
  - [x] On focus change → `update_focus_border_states()` → `janky::update_colors_for_state()`
  - [x] On layout change (monocle/floating) → `update_border_colors_for_workspace()`
  - [x] On workspace switch → `update_border_colors_for_workspace()`
  - [x] On config reload → `janky::refresh()`
- [x] Update configuration types (`config/types.rs`):
  - [x] `BorderStateConfig` enum (Disabled, SolidColor, GradientColor, GlowColor)
  - [x] `BordersConfig` with `style`, `hidpi`, `focused`, `unfocused`, `monocle`, `floating`, `ignore`
  - [x] `BorderColor` enum for JankyBorders rendering (Solid, Gradient, Glow)
  - [x] Regenerate JSON schema
- [x] Add unit tests for JankyBorders integration (37 border tests total)
- [x] Run tests (867 total), fix clippy warnings and ensure build passes

**Verification**: JankyBorders colors update correctly based on focus and layout state

---

#### Phase 10.7: Polish & Documentation

- [x] Handle edge cases:
  - [x] JankyBorders not installed (graceful degradation, log warning)
  - [x] JankyBorders process crashes (detect via Mach IPC, auto-reconnect, CLI fallback)
  - [x] Config reload while JankyBorders is running (app restarts, re-initializes borders)
- [x] Add command caching to prevent border flickering:
  - [x] `LAST_SENT` cache stores last sent key=value pairs
  - [x] `filter_changed_args()` skips duplicate commands
  - [x] `clear_cache()` forces re-send on config refresh
  - [x] 5 unit tests for caching functionality
- [x] Add comprehensive unit tests (42 border tests total)
- [x] Update `docs/sample-config.jsonc` with border configuration examples
- [x] Run full test suite (872 tests), fix any regressions

**Verification**: Borders work correctly with JankyBorders, graceful degradation when unavailable

---

### Milestone 11: Floating Presets

**Status**: [ ] Not Started / [ ] In Progress / [x] Complete

- [x] Add preset functions to `tiling/layout/floating.rs`:
  - [x] Parse `DimensionValue` (pixels vs percentage)
  - [x] `calculate_preset_frame(preset, screen, gaps)` function
  - [x] Handle `center: true` positioning
  - [x] Clamp to screen bounds
  - [x] Respect outer gaps in calculations
  - [x] Respect inner gaps for 50% dimensions (half-screen layouts)
  - [x] `find_preset(name)` - Look up preset by name (case-insensitive)
  - [x] `list_preset_names()` - Get available preset names
- [x] Implement `apply_preset(preset_name)` in TilingManager:
  - [x] Look up preset from config
  - [x] Get focused window and its screen
  - [x] Calculate frame using preset and screen's visible frame
  - [x] Resolve gaps for screen
  - [x] Apply frame to window
  - [x] Update tracked window state
- [x] Implement `stache tiling window --preset` command:
  - [x] CLI sends IPC notification `TilingWindowPreset`
  - [x] IPC handler calls `manager.apply_preset()`
- [x] Add unit tests for preset calculations (21 tests including inner gap tests)
- [x] Run tests (893 total), fix clippy warnings and ensure build passes

**Verification**: Presets position windows correctly

---

### Milestone 12: Polish & Testing

**Status**: [ ] Not Started / [ ] In Progress / [ ] Complete

- [ ] Performance optimizations:
  - [ ] Measure memory usage
  - [ ] Measure CPU usage during idle/active
  - [ ] Profile overall tiling operations
  - [ ] Optimize layout calculations
  - [ ] Optimize observer event handling
  - [ ] Minimize AX API calls
  - [ ] Evaluate caching strategies
- [ ] Code structure review:
  - [ ] Ensure modularity
  - [ ] Verify single responsibility principle
  - [ ] Refactor large functions
  - [ ] Improve naming consistency
  - [ ] Make sure code is being reused where possible
  - [ ] Ensure proper error propagation
- [ ] Comprehensive error handling:
  - [ ] Handle unresponsive windows
  - [ ] Handle rapid screen connect/disconnect
- [ ] Add integration tests:
  - [ ] Full workflow tests
  - [ ] Multi-monitor scenarios
  - [ ] Config reload scenarios
- [ ] Documentation:
  - [ ] Update `docs/features/tiling.md` with usage guide
  - [ ] Update `docs/sample-config.jsonc` with tiling examples
  - [ ] Add inline documentation to all public functions
  - [ ] Update the README if necessary
  - [ ] Document things with `cargo doc --no-deps`
- [ ] Final cleanup:
  - [ ] Remove any dead code
  - [ ] Ensure consistent naming
  - [ ] Fix all clippy warnings (pedantic + nursery)
  - [ ] Unit test analysis:
    - [ ] Add missing tests
    - [ ] Analyse if tests are meaningful and cover functionality
    - [ ] Ensure edge cases are covered
    - [ ] Increase test coverage where needed

**Verification**: All tests pass, documentation complete, performance acceptable

---

## Event Definitions

Events emitted by the tiling module for status bar integration:

| Event                                       | Payload                                    | Description                  |
| ------------------------------------------- | ------------------------------------------ | ---------------------------- |
| `stache://tiling/workspace-changed`         | `{ workspace: string, screen: string }`    | Focused workspace changed    |
| `stache://tiling/workspace-windows-changed` | `{ workspace: string, windows: number[] }` | Windows in workspace changed |
| `stache://tiling/layout-changed`            | `{ workspace: string, layout: string }`    | Workspace layout changed     |
| `stache://tiling/window-tracked`            | `{ windowId: number, workspace: string }`  | New window tracked           |
| `stache://tiling/window-untracked`          | `{ windowId: number }`                     | Window no longer tracked     |
| `stache://tiling/screens-changed`           | `{ screens: Screen[] }`                    | Screen configuration changed |
| `stache://tiling/initialized`               | `{ enabled: boolean }`                     | Tiling system ready          |
| `stache://tiling/borders-changed`           | `{ enabled: boolean }`                     | Borders enabled/disabled     |

---

## IPC Notifications

CLI-to-app communication via `NSDistributedNotificationCenter`:

| Notification                  | Payload            | Description                     |
| ----------------------------- | ------------------ | ------------------------------- |
| `TilingFocusWorkspace`        | `workspace_name`   | Focus a workspace               |
| `TilingSetLayout`             | `layout_type`      | Change focused workspace layout |
| `TilingWindowFocus`           | `direction_or_id`  | Focus window                    |
| `TilingWindowSwap`            | `direction`        | Swap focused window             |
| `TilingWindowResize`          | `dimension,amount` | Resize focused window           |
| `TilingWindowPreset`          | `preset_name`      | Apply floating preset           |
| `TilingWindowSendToWorkspace` | `workspace_name`   | Send window to workspace        |
| `TilingWindowSendToScreen`    | `screen_name`      | Send window to screen           |
| `TilingWorkspaceBalance`      | (none)             | Balance focused workspace       |
| `TilingWorkspaceSendToScreen` | `screen_name`      | Send workspace to screen        |
| `TilingBordersEnable`         | (none)             | Enable window borders           |
| `TilingBordersDisable`        | (none)             | Disable window borders          |
| `TilingBordersRefresh`        | (none)             | Rebuild all borders             |

---

## Risk Log

| Risk                                   | Status | Mitigation                                             |
| -------------------------------------- | ------ | ------------------------------------------------------ |
| Accessibility API changes in new macOS | Open   | Use documented APIs, wrap undocumented in abstraction  |
| Window hiding affects app state        | Open   | Test thoroughly, document known issues                 |
| Animation performance issues           | Open   | Make animations optional, configurable quality         |
| Complex multi-monitor edge cases       | Open   | Start single-monitor, add multi incrementally          |
| AXObserver reliability                 | Open   | Add reconnection logic, fallback polling               |
| Border overlay z-ordering issues       | Closed | Resolved by using JankyBorders (handles z-ordering)    |
| Border performance during window drag  | Closed | Resolved by using JankyBorders (optimized C impl)      |
| Corner radius detection unreliable     | Closed | JankyBorders handles corner radius automatically       |
| Mach IPC struct alignment mismatch     | Closed | Fixed with `#[repr(C, packed(4))]` to match C structs  |
| JankyBorders not installed             | Open   | Graceful degradation, log warning, document dependency |

---

## Notes

- **Window Hiding**: Using `NSRunningApplication.hide()` approach (not corner placement)
- **No State Persistence**: Rely on macOS auto-unhide on crash/quit
- **Disabled by Default**: `tiling.enabled` defaults to `false`
- **Borders Disabled by Default**: `tiling.borders.enabled` defaults to `false`
- **Separate from Hyprspace**: User will integrate later
- **Layout Implementation**: All layouts in single `layout.rs` file (simpler than planned directory structure)
- **Gaps Implementation**: Integrated into `layout.rs` as `Gaps` struct with `from_config()` method
- **Borders Implementation**: JankyBorders integration (replaced custom SkyLight implementation in Phase 10.6)
- **JankyBorders Dependency**: Borders require JankyBorders to be installed (`brew install FelixKratz/formulae/borders`)
- **Mach IPC Performance**: ~0.1-0.5ms latency vs ~20-50ms for CLI fallback
- **Command Caching**: Duplicate commands are skipped to prevent border flickering
- **Test Count**: 893 tests total
- **Floating Presets**: Preset code lives in `layout/floating.rs` with inner gap support for 50% dimensions

---

## Change Log

| Date       | Change                                                                                                               |
| ---------- | -------------------------------------------------------------------------------------------------------------------- |
| 2026-01-13 | Milestone 11: Moved presets to layout/floating.rs, added inner gap support for 50% dimensions, 893 tests total       |
| 2026-01-13 | Milestone 11 complete: Floating presets with apply_preset(), IPC handler, 21 tests                                   |
| 2026-01-13 | Milestone 10 complete: Window borders via JankyBorders, command caching, sample config, 872 tests                    |
| 2026-01-13 | Phase 10.7 complete: Command caching to prevent flickering, sample config updated, edge cases handled                |
| 2026-01-13 | Phase 10.6 complete: JankyBorders integration with Mach IPC, struct alignment fix, 867 tests                         |
| 2026-01-12 | Phase 10.6: Replaced custom SkyLight border implementation with JankyBorders integration for better performance      |
| 2026-01-12 | Phase 10.5 complete: Integration with TilingManager (track/untrack, focus, workspace switch, layout change)          |
| 2026-01-12 | Phase 10.4 complete: BorderManager singleton with create/remove/update operations, 864 tests                         |
| 2026-01-12 | Phase 10.3 complete: BorderRenderer with solid/gradient colors, BorderState enum, corner radius detection            |
| 2026-01-12 | Phase 10.2 complete: BorderWindow with NSWindow overlay, CAShapeLayer rendering, Send+Sync impl                      |
| 2026-01-12 | Phase 10.1 complete: BordersConfig, BorderColor, BorderColors, Rgba, parse_hex_color, JSON schema updated            |
| 2026-01-12 | Milestone 9 complete: Animations, drag-and-drop, performance optimizations documented                                |
| 2026-01-12 | Phase 9.3 complete: Performance review, debug logging optimized for release builds                                   |
| 2026-01-12 | Phase 9.2 complete: Drag-and-drop window swapping with `swap_windows_by_id`, 804 tests                               |
| 2026-01-12 | Bug fix: Window swap now preserves window sizes (ratios swapped along with window IDs)                               |
| 2026-01-11 | Milestone 8 complete: Workspace screen movement with original_screen_id tracking, 770 tests                          |
| 2026-01-11 | Bug fix: send-to-workspace now switches to target workspace, focuses moved window, and sets correct focus index      |
| 2026-01-11 | Milestone 7 complete: Grid layout, Master position configuration (left/right/top/bottom/auto), 766 tests             |
| 2026-01-11 | Milestone 6 complete: Window commands (focus, swap, resize, send-to-workspace, send-to-screen), bug fixes            |
| 2026-01-11 | Milestone 5 complete: All layouts (Dwindle, Monocle, Split, Master, Floating), gaps, --layout and --balance commands |
| 2026-01-10 | Milestone 4 complete: Workspace management, focus-follows, IPC                                                       |
| 2026-01-10 | Milestone 4 in progress: Workspace management, CLI commands                                                          |
| 2026-01-10 | Milestone 3 complete: Window tracking, rules, and observer                                                           |
| 2026-01-10 | Milestone 3 in progress: Window tracking implemented                                                                 |
| 2026-01-10 | Milestone 2 complete: State & Screen Detection                                                                       |
| 2026-01-10 | Milestone 1 complete: Foundation & Configuration                                                                     |
| 2026-01-10 | Initial plan created                                                                                                 |
