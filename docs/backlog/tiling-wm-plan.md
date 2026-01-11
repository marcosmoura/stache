# Tiling Window Manager Implementation Plan

> This document tracks the implementation progress of the Stache tiling window manager.
> See [tiling-wm-requirements.md](./tiling-wm-requirements.md) for full requirements.

## Status: In Progress

**Last Updated**: 2026-01-11
**Current Phase**: Milestone 7 - Grid Layout & Layout Enhancements

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
│         ├── manager.rs      # BorderManager singleton
│         ├── window.rs       # BorderWindow (NSWindow overlay)
│         └── renderer.rs     # Border rendering (CALayer, gradients)
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

**Status**: [ ] Not Started / [ ] In Progress / [ ] Complete

**Goal**: Implement animations for window transitions, drag-and-drop window swapping, and performance optimizations.

#### Phase 9.1: Animations

- [ ] Create `tiling/animation.rs`:
  - [ ] `AnimationSystem` struct
  - [ ] Frame interpolation logic
  - [ ] Easing functions (linear, ease-in, ease-out, ease-in-out, spring)
    - [ ] Evaluate usage of existing easing crate or implement manually
  - [ ] Animation thread/timer management
  - [ ] `animate_window_frames(transitions)` function
- [ ] Integrate animations:
  - [ ] Animate workspace switches
  - [ ] Animate layout changes
  - [ ] Animate window moves
  - [ ] Respect `animations.enabled` config
- [ ] Add unit tests for animations
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Window animations are smooth and respect configuration

---

#### Phase 9.2: Drag and Drop

- [ ] Implement drag-and-drop swapping:
  - [ ] Detect window drag start
  - [ ] Track drag position
  - [ ] Detect drop on another window
  - [ ] Trigger swap operation
- [ ] Add visual feedback during drag (optional highlight)
- [ ] Handle edge cases:
  - [ ] Drag to different workspace
  - [ ] Drag to different screen
  - [ ] Cancelled drags
- [ ] Add unit tests for drag-and-drop logic
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Drag-swap works reliably

---

#### Phase 9.3: Performance Optimizations

- [ ] Profile window operations:
  - [ ] Measure layout calculation time
  - [ ] Measure window move/resize time
  - [ ] Identify bottlenecks
- [ ] Batch frame updates where possible
- [ ] Optimize observer callbacks:
  - [ ] Consider debouncing rapid events
  - [ ] Minimize redundant layout recalculations
- [ ] Reduce AX API call frequency
- [ ] Run performance tests, fix clippy warnings and ensure build passes

**Verification**: Tiling operations feel snappy, no lag during window management

---

### Milestone 10: Window Borders

**Status**: [ ] Not Started / [ ] In Progress / [ ] Complete

**Goal**: Implement configurable borders around tiled windows with support for different states, gradients, rounded corners, and animations.

#### Phase 10.1: Configuration Types

- [ ] Add `BordersConfig` to `TilingConfig` in `config/types.rs`:
  - [ ] `enabled: bool` - Whether borders are enabled (default: false)
  - [ ] `width: u32` - Border width in pixels (default: 4)
  - [ ] `animation: BorderAnimationConfig` - Animation settings
  - [ ] `colors: BorderColors` - State-based color configuration
  - [ ] `ignore: Vec<WindowRule>` - Rules for windows that should not have borders
- [ ] Add `BorderAnimationConfig` struct:
  - [ ] `duration_ms: u32` - Animation duration for appear/disappear (default: 200)
  - [ ] `easing: EasingType` - Easing function (reuse existing enum)
- [ ] Add `BorderColor` enum (supports solid colors and gradients):
  - [ ] `Solid(String)` - Hex color string (e.g., "#FF0000")
  - [ ] `Gradient { from: String, to: String, angle: Option<f64> }` - Linear gradient
- [ ] Add `BorderColors` struct for state-based colors:
  - [ ] `focused: BorderColor` - Color for focused window
  - [ ] `unfocused: BorderColor` - Color for unfocused windows
  - [ ] `monocle: BorderColor` - Color for monocle layout windows
  - [ ] `floating: BorderColor` - Color for floating windows
- [ ] Implement hex color parsing to RGBA
- [ ] Regenerate JSON schema (`./scripts/generate-schema.sh`)
- [ ] Add unit tests for configuration parsing and color conversion
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Configuration parses correctly, schema includes border types

---

#### Phase 10.2: Border Window Foundation

- [ ] Create `tiling/borders/mod.rs`:
  - [ ] Module root with `init()` function and re-exports
- [ ] Create `tiling/borders/window.rs`:
  - [ ] `BorderWindow` struct representing a single border overlay
  - [ ] NSWindow creation via Objective-C FFI with properties:
    - [ ] `NSWindowStyleMaskBorderless` - No title bar
    - [ ] `NSWindowLevelFloating` - Stay above normal windows
    - [ ] `ignoresMouseEvents: YES` - Click-through
    - [ ] `opaque: NO`, `backgroundColor: clearColor` - Transparent
    - [ ] `hasShadow: NO` - No shadow
    - [ ] `collectionBehavior: canJoinAllSpaces` - Follow to all spaces
  - [ ] `set_frame(rect: Rect)` - Position and size the border window
  - [ ] `set_corner_radius(radius: f64)` - Match target window corner radius
  - [ ] `set_visible(visible: bool)` - Show/hide border
  - [ ] `destroy()` - Clean up NSWindow resources
- [ ] Add macOS API bindings in `utils/objc.rs` or inline:
  - [ ] NSWindow creation and configuration
  - [ ] CALayer setup for border drawing
  - [ ] CGColor/NSColor handling
- [ ] Add unit tests for border window lifecycle
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Border windows can be created, positioned, and destroyed

---

#### Phase 10.3: Border Rendering

- [ ] Create `tiling/borders/renderer.rs`:
  - [ ] `BorderRenderer` for drawing borders using Core Animation
  - [ ] `draw_solid_border(color: RGBA, width: f64, corner_radius: f64)`
  - [ ] `draw_gradient_border(from: RGBA, to: RGBA, angle: f64, width: f64, corner_radius: f64)`
  - [ ] Use `CAShapeLayer` with stroke for border path
  - [ ] Use `CAGradientLayer` for gradient colors
- [ ] Handle corner radius detection from target window:
  - [ ] Query window corner radius via Accessibility API if available
  - [ ] Fall back to macOS default (~10-12px on modern versions)
- [ ] Ensure borders render correctly at Retina scale factors
- [ ] Add unit tests for color conversion and gradient calculations
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Borders render with correct colors, widths, and corner radii

---

#### Phase 10.4: Border Manager

- [ ] Create `tiling/borders/manager.rs`:
  - [ ] `BorderManager` struct (singleton pattern like `TilingManager`)
  - [ ] `borders: HashMap<u32, BorderWindow>` - Map window_id to border
  - [ ] `create_border(window_id: u32, frame: Rect, state: BorderState)`
  - [ ] `remove_border(window_id: u32)`
  - [ ] `update_border_frame(window_id: u32, frame: Rect)`
  - [ ] `update_border_state(window_id: u32, state: BorderState)`
  - [ ] `show_borders_for_workspace(workspace: &str)`
  - [ ] `hide_borders_for_workspace(workspace: &str)`
  - [ ] `set_enabled(enabled: bool)` - Enable/disable all borders
  - [ ] `refresh_all()` - Rebuild all borders from current state
- [ ] Add `BorderState` enum:
  - [ ] `Focused` - Currently focused window
  - [ ] `Unfocused` - Visible but not focused
  - [ ] `Monocle` - In monocle layout
  - [ ] `Floating` - In floating layout or marked as floating
- [ ] Implement ignore rule matching (reuse `WindowRule` logic from `rules.rs`)
- [ ] Add unit tests for border manager operations
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Border manager can create, update, and remove borders correctly

---

#### Phase 10.5: Integration with Tiling Manager

- [ ] Integrate border creation in `TilingManager::track_window()`:
  - [ ] Check if borders enabled and window not ignored
  - [ ] Create border with initial state based on focus/layout
- [ ] Integrate border removal in `TilingManager::untrack_window()`:
  - [ ] Remove border when window untracked
- [ ] Integrate border updates in window event handlers:
  - [ ] `WindowEventType::Moved` → `update_border_frame()`
  - [ ] `WindowEventType::Resized` → `update_border_frame()`
  - [ ] Focus changes → `update_border_state()` for both old and new focused windows
- [ ] Integrate with workspace switching:
  - [ ] `switch_workspace()` → show/hide borders along with windows
- [ ] Integrate with layout changes:
  - [ ] Detect monocle layout → update all borders to `Monocle` state
  - [ ] Detect floating layout/window → update border to `Floating` state
- [ ] Ensure borders don't interfere with layout calculations (pure visual overlay)
- [ ] Add integration tests
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Borders appear/update correctly when windows are managed

---

#### Phase 10.6: Animation Support

- [ ] Add animation support to `BorderWindow`:
  - [ ] `animate_show()` - Fade in border with configured duration
  - [ ] `animate_hide()` - Fade out border with configured duration
  - [ ] `animate_color_change(from: BorderColor, to: BorderColor)` - Smooth color transition
- [ ] Use `CABasicAnimation` for opacity and color animations
- [ ] Implement easing functions mapping to `CAMediaTimingFunction`:
  - [ ] `linear` → `kCAMediaTimingFunctionLinear`
  - [ ] `ease-in` → `kCAMediaTimingFunctionEaseIn`
  - [ ] `ease-out` → `kCAMediaTimingFunctionEaseOut`
  - [ ] `ease-in-out` → `kCAMediaTimingFunctionEaseInEaseOut`
- [ ] Respect `borders.animation` config (skip if duration is 0)
- [ ] Handle animation interruption gracefully (e.g., rapid focus changes)
- [ ] Add unit tests for animation timing
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: Borders animate smoothly when appearing/disappearing/changing state

---

#### Phase 10.7: CLI Commands & Events

- [ ] Add CLI command structure to `cli/commands.rs`:
  - [ ] `BordersCommands` enum with `Enable`, `Disable`, `Refresh` variants
- [ ] Add `stache tiling borders` command:
  - [ ] `--enable` - Enable borders at runtime
  - [ ] `--disable` - Disable borders at runtime
  - [ ] `--refresh` - Rebuild all borders from current state
- [ ] Add IPC notification handlers in `ipc_listener.rs`:
  - [ ] `TilingBordersEnable`
  - [ ] `TilingBordersDisable`
  - [ ] `TilingBordersRefresh`
- [ ] Add events to `events.rs`:
  - [ ] `stache://tiling/borders-changed` - Border state changed (enabled/disabled)
- [ ] Ensure borders rebuild correctly after config reload
- [ ] Add tests for CLI commands
- [ ] Run tests, fix clippy warnings and ensure build passes

**Verification**: CLI commands control borders as expected

---

#### Phase 10.8: Edge Cases & Polish

- [ ] Handle edge cases:
  - [ ] Windows near screen edges (clamp borders to screen bounds)
  - [ ] Multi-monitor setups (borders on correct screen)
  - [ ] Minimized windows (hide borders)
  - [ ] Full-screen windows (hide borders)
  - [ ] Rapidly moving windows (throttle frame updates if needed)
- [ ] Performance optimization:
  - [ ] Profile border updates during window drag
  - [ ] Batch updates when multiple windows change simultaneously
  - [ ] Consider lazy border creation (only when window becomes visible)
- [ ] Memory management:
  - [ ] Ensure borders are cleaned up when windows close
  - [ ] Handle application termination (clean up all borders)
- [ ] Add comprehensive unit tests for edge cases
- [ ] Update `docs/sample-config.jsonc` with border configuration examples
- [ ] Run full test suite, fix any regressions

**Verification**: Borders work correctly in all scenarios, no performance degradation

---

### Milestone 11: Floating Presets

**Status**: [ ] Not Started / [ ] In Progress / [ ] Complete

- [ ] Create `tiling/presets.rs`:
  - [ ] Parse `DimensionValue` (pixels vs percentage)
  - [ ] `calculate_preset_frame(preset, screen, gaps)` function
  - [ ] Handle `center: true` positioning
  - [ ] Clamp to screen bounds
  - [ ] Respect gaps in calculations
- [ ] Implement `apply_preset(window_id, preset_name)`:
  - [ ] Look up preset from config
  - [ ] Calculate frame for window's screen
  - [ ] Apply frame to window
- [ ] Implement `stache tiling window --preset` command
- [ ] Support `preset-on-open` workspace config:
  - [ ] Apply preset when window opens
  - [ ] Only for floating layout or floating windows
- [ ] Add unit tests for preset calculations
- [ ] Run tests, fix clippy warnings and ensure build passes

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

| Risk                                   | Status | Mitigation                                            |
| -------------------------------------- | ------ | ----------------------------------------------------- |
| Accessibility API changes in new macOS | Open   | Use documented APIs, wrap undocumented in abstraction |
| Window hiding affects app state        | Open   | Test thoroughly, document known issues                |
| Animation performance issues           | Open   | Make animations optional, configurable quality        |
| Complex multi-monitor edge cases       | Open   | Start single-monitor, add multi incrementally         |
| AXObserver reliability                 | Open   | Add reconnection logic, fallback polling              |
| Border overlay z-ordering issues       | Open   | Use NSWindowLevel.floating, test with various apps    |
| Border performance during window drag  | Open   | Throttle updates, use Core Animation for GPU accel    |
| Corner radius detection unreliable     | Open   | Fall back to system default, make configurable        |

---

## Notes

- **Window Hiding**: Using `NSRunningApplication.hide()` approach (not corner placement)
- **No State Persistence**: Rely on macOS auto-unhide on crash/quit
- **Disabled by Default**: `tiling.enabled` defaults to `false`
- **Borders Disabled by Default**: `tiling.borders.enabled` defaults to `false`
- **Separate from Hyprspace**: User will integrate later
- **Layout Implementation**: All layouts in single `layout.rs` file (simpler than planned directory structure)
- **Gaps Implementation**: Integrated into `layout.rs` as `Gaps` struct with `from_config()` method
- **Borders Implementation**: Per-window NSWindow overlays with Core Animation rendering (Milestone 10)
- **Test Count**: 770 tests total (4 workspace screen movement tests added in Milestone 8)

---

## Change Log

| Date       | Change                                                                                                               |
| ---------- | -------------------------------------------------------------------------------------------------------------------- |
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
