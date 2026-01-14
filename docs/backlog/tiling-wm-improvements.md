# Tiling Window Manager Improvements Plan

> This document tracks the implementation progress of improvements to the Stache tiling window manager.
> See [tiling-wm-plan.md](./tiling-wm-plan.md) for the original implementation plan.

## Status: In Progress

**Last Updated**: 2026-01-13
**Current Phase**: Milestone 4 complete, ready for Milestone 5-7

---

## Overview

This plan addresses code quality, performance, and maintainability improvements identified during a comprehensive review of the tiling window manager implementation (~15,000 lines of Rust across 26 files, 290 tests).

### Key Objectives

| Objective         | Current State                                    | Target State                               |
| ----------------- | ------------------------------------------------ | ------------------------------------------ |
| Error Handling    | Mix of `bool`, `Option`, silent failures         | Unified `Result<T, TilingError>`           |
| Code Organization | `manager.rs` (2800 lines), `mod.rs` (1470 lines) | No file >1500 lines (mod.rs now 607 lines) |
| Thread Safety     | Some race conditions possible                    | Deterministic event processing             |
| FFI Safety        | Raw pointers, scattered declarations             | Safe wrappers, consolidated FFI            |
| Testing           | Unit tests only                                  | Integration + fuzz + benchmarks            |
| Performance       | Good baseline                                    | Cached layouts, event coalescing           |

---

## Target Module Structure

```text
app/native/src/tiling/
├── mod.rs                    # 607 lines (down from 1470) - DONE
├── error.rs                  # DONE: TilingError enum
├── constants.rs              # DONE: Centralized magic numbers
├── event_handlers.rs         # DONE: All handle_* functions (1206 lines)
├── testing.rs                # FUTURE: Mock infrastructure
├── README.md                 # FUTURE: Architecture documentation
│
├── manager/                  # DONE: Split from manager.rs
│   ├── mod.rs               # Core TilingManager struct (2797 lines)
│   └── helpers.rs           # DONE: Layout ratio helpers (349 lines)
│   # NOTE: Further splitting into focus.rs, workspace_ops.rs, etc.
│   # was deferred due to tight coupling of methods via &self
│
├── ffi/                      # DEFERRED: FFI currently well-organized per-module
│   # Each module (window.rs, observer.rs, animation.rs) keeps
│   # its FFI declarations close to usage, which is idiomatic Rust
│
├── state.rs                  # (unchanged)
├── workspace.rs              # (unchanged)
├── window.rs                 # Updated: Result returns
├── observer.rs               # Updated: Error context
├── rules.rs                  # (unchanged)
├── screen.rs                 # (unchanged)
├── animation.rs              # (unchanged)
├── drag_state.rs             # (unchanged)
├── mouse_monitor.rs          # (unchanged)
├── app_monitor.rs            # (unchanged)
├── screen_monitor.rs         # (unchanged)
│
├── layout/                   # (unchanged structure)
│   └── ...
│
└── borders/                  # (unchanged structure)
    └── ...
```

---

## Configuration Decisions

| Setting               | Value        | Rationale                            |
| --------------------- | ------------ | ------------------------------------ |
| Breaking changes      | Allowed      | Functionality preserved; cleaner API |
| Event coalesce window | 4ms          | Close to typical frame time          |
| Worker thread model   | FIFO         | Simpler, predictable ordering        |
| FFI priority          | Safety first | Then optimize hot paths              |

---

## Dependencies to Add

```toml
# Cargo.toml additions
[dependencies]
smallvec = "1.13"

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
proptest = "1.5"
```

---

## Implementation Milestones

### Milestone 1: Error Handling & Constants ✅ COMPLETE

**Status**: [x] Complete

**Goal**: Create unified error handling system and centralize magic numbers.

#### Phase 1.1: Create TilingError Type ✅

- [x] Create `tiling/error.rs` with `TilingError` enum:
  - [x] `NotInitialized` - Manager not initialized
  - [x] `WorkspaceNotFound(String)` - Workspace lookup failed
  - [x] `WindowNotFound(u32)` - Window lookup failed
  - [x] `ScreenNotFound(String)` - Screen lookup failed
  - [x] `AccessibilityError { code, message }` - AX API errors
  - [x] `WindowOperation(String)` - Generic window op failure
  - [x] `Observer(String)` - Observer system errors
  - [x] `AnimationCancelled` - Animation was interrupted
- [x] Implement `std::error::Error` and `Display` traits
- [x] Add `From` implementations for common conversions
- [x] Export from `tiling/mod.rs`

#### Phase 1.2: Convert Window Operations to Result ✅

- [x] Update `window.rs` functions to return `Result<T, TilingError>`:
  - [x] `set_window_frame()` → `Result<(), TilingError>`
  - [x] `set_window_frame_with_retry()` → `Result<(), TilingError>`
  - [x] `focus_window()` → `Result<(), TilingError>`
  - [x] `hide_window()` / `show_window()` → `Result<(), TilingError>`
  - [x] `hide_app()` / `unhide_app()` → `Result<(), TilingError>`
  - [x] `minimize_window()` / `unminimize_window()` → `Result<(), TilingError>`
- [x] Update all call sites in `manager.rs`, `mod.rs`, `workspace.rs`
- [x] Add error logging with context at call sites

#### Phase 1.3: Add Error Context to Observer Callbacks ✅

- [x] Update `observer.rs` to propagate errors:
  - [x] `add_observer()` - already returns `Result`, improve messages
  - [x] `add_notification()` - log failures with context instead of ignoring
  - [x] Observer callback - wrap in error handling
- [x] Add retry logic for transient AX failures

#### Phase 1.4: Create Constants Module ✅

- [x] Create `tiling/constants.rs` with documented constants:
  - [x] `timing::FOCUS_COOLDOWN_MS` (25)
  - [x] `timing::WORKSPACE_SWITCH_COOLDOWN_MS` (25)
  - [x] `timing::HIDE_SHOW_DELAY_MS` (10)
  - [x] `timing::SCREEN_CHANGE_DELAY_MS` (100)
  - [x] `timing::WINDOW_READY_TIMEOUT_MS` (25)
  - [x] `timing::WINDOW_READY_POLL_INTERVAL_MS` (5)
  - [x] `timing::EVENT_COALESCE_MS` (4)
  - [x] `window_size::MIN_TRACKABLE_SIZE` (50.0)
  - [x] `window_size::MAX_PANEL_HEIGHT` (200.0)
  - [x] `window_size::MAX_PANEL_WIDTH` (450.0)
  - [x] `window_size::MIN_UNTITLED_WINDOW_SIZE` (320.0)
  - [x] `layout::REPOSITION_THRESHOLD_PX` (1.0)
  - [x] `animation::DEFAULT_FPS` (60)
  - [x] `animation::VSYNC_TIMEOUT_MULTIPLIER` (2.0)
  - [x] `animation::SPRING_POSITION_THRESHOLD` (0.01)
- [x] Update all files to use constants module
- [x] Remove hardcoded values from `manager.rs`, `workspace.rs`, `animation.rs`, `mod.rs`, `screen_monitor.rs`

- [x] Run tests, fix clippy warnings, ensure build passes

**Verification**: All tiling operations return `Result`, constants centralized, tests pass

---

### Milestone 2: Code Structure Refactoring ✅ COMPLETE

**Status**: [x] Complete (Phase 2.3 deferred by design)

**Goal**: Break down large files into focused, maintainable modules.

#### Phase 2.1: Extract Event Handlers ✅ COMPLETE

- [x] Create `tiling/event_handlers.rs` (1206 lines):
  - [x] Move `handle_window_event()` from `mod.rs`
  - [x] Move `handle_window_moved()` from `mod.rs`
  - [x] Move `handle_window_resized()` from `mod.rs`
  - [x] Move `handle_window_created()` from `mod.rs`
  - [x] Move `handle_window_destroyed()` from `mod.rs`
  - [x] Move `handle_window_focused()` from `mod.rs`
  - [x] Move `handle_app_launch()` from `mod.rs`
  - [x] Move `handle_screen_change()` from `mod.rs`
  - [x] Move `on_mouse_up()` from `mod.rs`
  - [x] Move helper functions: `start_drag_operation()`, `try_handle_tab_swap_inline()`, etc.
  - [x] Move all drag-and-drop tests
- [x] Update `mod.rs` to use event handlers module
- [x] `mod.rs` reduced from 1768 to 607 lines (-65%)

#### Phase 2.2: Split Manager into Directory ✅ PARTIAL

- [x] Create `tiling/manager/` directory structure
- [x] Move `manager.rs` to `tiling/manager/mod.rs`
- [x] Create `tiling/manager/helpers.rs` (349 lines):
  - [x] `frames_approximately_equal()`
  - [x] `calculate_ratios_from_frames()`
  - [x] `cumulative_ratios_to_proportions()`
  - [x] `proportions_to_cumulative_ratios()`
  - [x] `calculate_proportions_adjusting_adjacent()`
  - [x] Related tests

**Note**: Further splitting into `focus.rs`, `workspace_ops.rs`, `window_ops.rs`, `layout_ops.rs`
was evaluated but deferred. All these methods operate on `&self` or `&mut self` of
`TilingManager`, making them tightly coupled. Splitting would require:

- Extension traits (adds complexity)
- Passing explicit state refs (breaks encapsulation)
- Macro-based file inclusion (non-idiomatic)

The current structure keeps related code together while extracting standalone helpers.

#### Phase 2.3: Consolidate FFI Declarations ⏸️ DEFERRED

FFI declarations are currently distributed across modules, with each module
defining its own FFI close to where it's used. This is idiomatic Rust and
provides good locality of reference. Consolidation would:

- Move FFI away from usage sites
- Require cross-module type sharing
- Add complexity without clear benefit

Files with FFI (kept as-is):

- `window.rs` - AX (Accessibility) and CG (Core Graphics)
- `observer.rs` - `AXObserver` functions
- `animation.rs` - `CVDisplayLink` and `CATransaction`
- `screen.rs` - `NSScreen`
- `mouse_monitor.rs` - `CGEvent`
- `screen_monitor.rs` - `CGDisplayRegister`
- `app_monitor.rs` - `NSNotification`
- `borders/mach_ipc.rs` - Mach IPC

**Verification**:

- [x] `mod.rs` reduced to 607 lines (target was ~300, achieved 607)
- [x] `manager/mod.rs` is 2797 lines with helpers extracted
- [ ] FFI consolidation deferred (current approach is acceptable)
- [x] All 931 tests pass
- [x] Clippy passes

---

### Milestone 3: Thread Safety Improvements ✅ COMPLETE

**Status**: [x] Complete (Phase 3.1 deferred by design)

**Goal**: Eliminate race conditions and improve lock patterns.

#### Phase 3.1: Worker Channel for Event Processing ⏸️ DEFERRED

The current implementation uses isolated thread spawns for window polling in `handle_window_created()` and `handle_app_launch()`. These spawns are necessary because:

- The accessibility API needs time to register new windows
- Polling in the main event handler would block other events
- Each spawn is isolated and doesn't share state unsafely

A worker channel architecture would serialize event processing but adds
significant complexity. The current approach is working correctly and the
thread spawns are well-contained. Deferring to a future milestone if issues arise.

#### Phase 3.2: Fix Redundant Workspace Lookups ✅ COMPLETE

- [x] Fixed redundant lookup in `set_focused_window()`:

  ```rust
  // Before: two lookups to get old focused window ID
  workspace_by_name(name).and_then(|ws| ws.focused_window_index)
      .and_then(|idx| workspace_by_name(name).and_then(|ws| ws.window_ids.get(idx)))
  // After: single lookup with chained operations
  workspace_by_name(name).and_then(|ws| ws.focused_window_index.and_then(|idx| ws.window_ids.get(idx).copied()))
  ```

- [x] Fixed redundant lookup in `untrack_window_internal()`:
  - Combined visibility check with workspace modification in single lookup
- [x] Fixed redundant lookup in `track_window_internal()`:
  - `add_window_to_state()` now returns visibility status
  - Removed separate lookup for visibility check
- [x] Fixed redundant lookup in `track_existing_windows()`:
  - Uses return value from `add_window_to_state()`

#### Phase 3.3: Relax Memory Ordering in drag_state.rs ✅ COMPLETE

- [x] Changed `OPERATION_IN_PROGRESS`:
  - `store()` → `Ordering::Release`
  - `load()` → `Ordering::Acquire`
- [x] Changed `OPERATION_DRAG_SEQUENCE`:
  - `store()` → `Ordering::Release`
  - `load()` → `Ordering::Acquire`
- [x] Added comprehensive documentation explaining:
  - Memory ordering rationale
  - Happens-before relationships
  - Why `Acquire`/`Release` is sufficient (mutex provides main sync)

#### Phase 3.4: Add Debug Lock Contention Monitoring ✅ COMPLETE

- [x] Created `track_lock_time()` helper in `manager/helpers.rs`:

  ```rust
  #[cfg(debug_assertions)]
  pub fn track_lock_time<T, F: FnOnce() -> T>(name: &str, f: F) -> T
  ```

- [x] Debug-only implementation times operations and warns if >5ms
- [x] Release build has zero-overhead inline no-op
- [x] Exported as `debug_track_lock_time` from manager module
- [x] Added tests for the helper

**Verification**:

- [x] All 933 tests pass
- [x] Clippy passes
- [x] Thread spawns in event handlers are necessary and well-contained

---

### Milestone 4: FFI Safety Improvements ✅ COMPLETE

**Status**: [x] Complete

**Goal**: Improve safety and documentation around unsafe FFI code.

#### Phase 4.1: Create Safe AXElement Wrapper ✅ COMPLETE

- [x] Implement `AXElement` struct in `ffi/accessibility.rs`:
  - [x] `application(pid: i32) -> Option<Self>`
  - [x] `windows() -> Vec<AXElement>`
  - [x] `focused_window() -> Option<AXElement>`
  - [x] `title() -> Option<String>`
  - [x] `role() -> Option<String>`
  - [x] `frame() -> Option<Rect>`
  - [x] `position() -> Option<(f64, f64)>`
  - [x] `size() -> Option<(f64, f64)>`
  - [x] `set_position(x, y) -> TilingResult<()>`
  - [x] `set_size(width, height) -> TilingResult<()>`
  - [x] `set_frame(frame) -> TilingResult<()>`
  - [x] `raise() -> TilingResult<()>`
- [x] Implement `Drop` for automatic `CFRelease`
- [x] Implement `Clone` using `CFRetain`
- [x] Add `Send + Sync` with safety documentation
- [ ] ~~Update `window.rs` to use `AXElement` wrapper~~ DEFERRED
- [ ] ~~Update `observer.rs` to use `AXElement` wrapper~~ DEFERRED

**Note**: Full migration of window.rs/observer.rs deferred due to:

- Extensive refactoring required (40+ usages of raw pointers)
- Risk of breaking performance-optimized animation code
- Existing code is tested and working (933 tests pass)

The `AXElement` wrapper is available for new code via `tiling::ffi::AXElement`.

#### Phase 4.2: Document Safety Invariants ✅ COMPLETE

- [x] Add `# Safety` sections to all `unsafe impl` blocks:
  - [x] `DisplayLink` Send/Sync (`animation.rs`)
  - [x] `CATransactionSelectors` Send/Sync (`animation.rs`)
  - [x] `SendableAXElement` Send/Sync (`window.rs`)
  - [x] `AppObserver` Send/Sync (`observer.rs`)
- [x] Add `# Safety` sections to all extern C callbacks:
  - [x] `display_link_callback` (`animation.rs`)
  - [x] `observer_callback` (`observer.rs`)
  - [x] `display_reconfiguration_callback` (`screen_monitor.rs`)
  - [x] `mouse_event_callback` (`mouse_monitor.rs`)
  - [x] `handle_app_launch_notification` (`app_monitor.rs`)

#### Phase 4.3: Add FFI Null Check Helpers ✅ COMPLETE

- [x] Create `ffi_try!` macro in `ffi/mod.rs`:
  - `ffi_try!(ptr)` - returns `Err(TilingError::window_op("Null pointer"))`
  - `ffi_try!(ptr, error)` - returns `Err(error)` if null
- [x] Create `ffi_try_opt!` macro for `Option` returns
- [x] Added 5 unit tests for the macros

#### Phase 4.4: Apply FFI Improvements ✅ COMPLETE

- [x] Apply `ffi_try_opt!` macro to `window.rs` helper functions:
  - `get_ax_string()`, `get_ax_bool()`, `get_ax_position()`, `get_ax_size()`
- [x] Document `AXElement` wrapper interop in `window.rs` module docs
- [x] Review `observer.rs` - null checks are part of larger logic, macros not applicable
- [x] Keep raw pointers in animation hot paths for performance
- [x] All 944 tests pass, clippy clean

**Note**: Full migration of window.rs/observer.rs to `AXElement` wrapper was evaluated but deferred due to tight coupling with raw pointer animation code. The macros and wrapper are available for new code.

**Verification**: All unsafe code documented ✅, safe wrappers for AX API ✅, macros applied ✅

---

### Milestone 5: Performance Optimization

**Status**: [x] In Progress

**Goal**: Optimize critical paths for smoother operation.

#### Phase 5.1: Workspace Name Lookup Cache ✅ COMPLETE

- [x] Add `workspace_index: HashMap<String, usize>` to `TilingState`
- [x] Add `add_workspace()` method for indexed insertion
- [x] Add `rebuild_workspace_index()` for bulk rebuilds
- [x] Update `workspace_by_name()` to use O(1) index lookup
- [x] Update `workspace_by_name_mut()` to use O(1) index lookup
- [x] Update `TilingManager` to use `add_workspace()` instead of `vec.push()`
- [x] Added 3 new tests for index functionality
- [x] 947 tests pass, clippy clean

#### Phase 5.2: Batch JankyBorders Commands ✅ COMPLETE

- [x] Create `janky::set_multiple()` function for batching arbitrary settings
- [x] Add `janky::set_colors()` for batching active + inactive colors
- [x] Batch config updates in `apply_config()` (already implemented)
- [x] Reduce CLI/IPC round trips via existing caching mechanism
- [x] Fixed test isolation issue with unique test keys
- [x] 949 tests pass, clippy clean

#### Phase 5.3: Pre-allocated Animation Buffers ✅ COMPLETE

- [x] Add `buffers` module with thread-local pre-allocated vectors:
  - `WINDOW_IDS`, `ANIMATABLE`, `POSITION_FRAMES`, `DELTA_FRAMES`
  - `PREV_FRAMES`, `FINAL_FRAMES`, `SPRING_STATES`
- [x] `take_*(capacity)` and `return_*()` API for buffer lifecycle
- [x] Larger capacity buffers preserved across calls
- [x] Note: Animation code already uses `.clear()` for per-frame reuse
- [x] Added 4 new tests for buffer functionality
- [x] 953 tests pass, clippy clean

#### Phase 5.4: Layout Result Caching ✅ COMPLETE

- [x] Add `LayoutCache` struct to `Workspace`:
  - [x] `input_hash: u64` - hash of layout inputs
  - [x] `positions: Vec<(u32, Rect)>` - cached layout positions
  - [x] `is_valid()`, `update()`, `invalidate()` methods
- [x] Implement `compute_layout_hash()` function:
  - Hashes: layout type, window IDs, screen frame, master ratio, split ratios, gaps hash
- [x] Add `Gaps::compute_hash()` for gap configuration hashing
- [x] Update `apply_layout_internal()` to check cache first:
  - Compute hash, check `layout_cache.is_valid(hash)`, return cached if valid
  - Calculate and update cache on miss or force=true
- [x] Add cache invalidation on state changes:
  - Window add/remove, layout change, ratio changes, window swaps, send-to-workspace
- [x] Added 18 new tests for cache and hash functionality
- [x] 971 tests pass, clippy clean

#### Phase 5.5: AXUIElement Resolution Caching ✅ COMPLETE

- [x] Create `AXElementCache` struct in `window.rs`:
  - [x] `entries: RwLock<HashMap<u32, CachedAXEntry>>` for thread-safe access
  - [x] `CachedAXEntry` with `CachedAXPtr` wrapper (Send+Sync) and timestamp
  - [x] TTL: 5 seconds (configurable via `constants::cache::AX_ELEMENT_TTL_SECS`)
- [x] Add global cache via `OnceLock<AXElementCache>` singleton
- [x] Update `resolve_window_ax_elements()` to use cache:
  - Check cache first via `get_multiple()`
  - Only query `get_all_windows()` for cache misses
  - Update cache with newly resolved elements
- [x] Add `invalidate_ax_element_cache()` called from `untrack_window_internal()`
- [x] Added 15 new tests for cache functionality
- [x] 986 tests pass, clippy clean

#### Phase 5.6: Event Coalescing ✅ COMPLETE

- [x] Create `EventCoalescer` struct in `event_coalescer.rs`:
  - [x] `entries: RwLock<HashMap<CoalesceKey, CoalesceEntry>>` for thread-safe tracking
  - [x] `CoalesceKey = (pid, event_type_discriminant)` for efficient lookups
  - [x] `coalesce_window: Duration` (4ms from `constants::timing::EVENT_COALESCE_MS`)
- [x] Add coalescer to event handling path:
  - [x] `should_process_move()` and `should_process_resize()` public API
  - [x] Integrated into `handle_window_moved()` and `handle_window_resized()`
- [x] Filter rapid move/resize events within coalesce window
- [x] Final position always applied via existing `on_mouse_up()` handler
- [x] Added 14 new tests for coalescer functionality
- [x] 1000 tests pass, clippy clean

#### Phase 5.7: Screen and Window List Caching

- [ ] Create `SystemInfoCache` struct:
  - [ ] `screens: Option<(Vec<Screen>, Instant)>` - TTL: 1s
  - [ ] `windows: Option<(Vec<CGWindowInfo>, Instant)>` - TTL: 50ms
- [ ] Add cache to `TilingManager` or global
- [ ] Update `get_all_screens()` to use cache
- [ ] Update `get_cg_window_list()` to use cache
- [ ] Add `invalidate_screen_cache()` for screen changes

#### Phase 5.8: SmallVec for Hot Paths

- [ ] Add `smallvec` dependency
- [ ] Update `Workspace::window_ids` to `SmallVec<[u32; 16]>`
- [ ] Update `Workspace::ratios` to `SmallVec<[f64; 16]>`
- [ ] Update layout return types to `SmallVec<[(u32, Rect); 16]>`
- [ ] Update animation transition vectors

#### Phase 5.9: Parallel Screen Layout Application

- [ ] Use `rayon` for multi-screen layout application
- [ ] Parallelize in `apply_layout_internal()` when multiple screens affected
- [ ] Ensure thread-safe access to window operations

#### Phase 5.10: Lazy Gap Resolution

- [ ] Add `gaps_cache: HashMap<String, Gaps>` to `TilingManager`
- [ ] Compute gaps on initialization and screen change
- [ ] Update `get_gaps_for_screen()` to use cache
- [ ] Invalidate cache on config reload

#### Phase 5.11: Observer Notification Filtering

- [ ] Add `should_observe_app()` check before creating observer
- [ ] Skip observers for apps matching ignore rules
- [ ] Reduce event volume from system apps (Spotlight, Dock, etc.)

- [ ] Run benchmarks to verify improvements
- [ ] Run tests, fix clippy warnings, ensure build passes

**Verification**: Benchmarks show improvement, no functionality regression

---

### Milestone 6: Documentation

**Status**: [ ] Not Started / [ ] In Progress / [ ] Complete

**Goal**: Improve developer experience through better documentation.

#### Phase 6.1: Add Module-Level Documentation

- [ ] Add module docs to files missing them:
  - [ ] `drag_state.rs` - Drag operation state tracking
  - [ ] `mouse_monitor.rs` - CGEventTap mouse monitoring
  - [ ] `screen_monitor.rs` - Display reconfiguration monitoring
  - [ ] `app_monitor.rs` - NSWorkspace app launch monitoring
  - [ ] `borders/janky.rs` - JankyBorders CLI/IPC integration
  - [ ] `borders/mach_ipc.rs` - Mach IPC for JankyBorders
  - [ ] `event_handlers.rs` - Window event handling (new file)
  - [ ] `constants.rs` - Internal tuning constants (new file)
  - [ ] `error.rs` - Error types (new file)
- [ ] Ensure all modules have `//!` doc comments explaining purpose

#### Phase 6.2: Create Architecture Documentation

- [ ] Create `tiling/README.md`:
  - [ ] System overview diagram (ASCII art)
  - [ ] Module dependency graph
  - [ ] Event flow documentation
  - [ ] Thread model explanation
  - [ ] State machine for workspace switching
  - [ ] JankyBorders integration explanation
  - [ ] Performance considerations
  - [ ] Debugging tips

- [ ] Run `cargo doc` and fix any warnings

**Verification**: `cargo doc` produces no warnings, README provides clear overview

---

### Milestone 7: Testing Infrastructure

**Status**: [ ] Not Started / [ ] In Progress / [ ] Complete

**Goal**: Improve test coverage with integration tests, fuzz tests, and benchmarks.

#### Phase 7.1: Add Integration Tests

- [ ] Create `tests/tiling_integration.rs`:
  - [ ] `#[ignore]` attribute (requires accessibility permissions)
  - [ ] Helper to check accessibility permissions
  - [ ] Helper to create test windows via osascript/AppleScript
  - [ ] Helper to cleanup test windows
- [ ] Implement integration test cases:
  - [ ] `test_window_tracking_lifecycle` - create, track, close
  - [ ] `test_workspace_switching` - switch workspaces, verify visibility
  - [ ] `test_layout_application` - apply layout, verify positions
  - [ ] `test_window_focus_navigation` - focus direction commands
  - [ ] `test_drag_and_drop_swap` - simulate drag, verify swap
- [ ] Add CI configuration to run integration tests (optional, manual trigger)

#### Phase 7.2: Add Fuzz Testing for Layouts

- [ ] Add `proptest` dependency
- [ ] Create property tests in layout modules:
  - [ ] `dwindle.rs`: No overlapping windows, all windows within bounds
  - [ ] `master.rs`: Master window has correct ratio
  - [ ] `split.rs`: Windows evenly distributed
  - [ ] `grid.rs`: Grid dimensions correct for window count
  - [ ] `monocle.rs`: All windows same size as screen
- [ ] Test edge cases:
  - [ ] 0 windows (empty result)
  - [ ] 1 window (fills screen)
  - [ ] Many windows (100+)
  - [ ] Extreme aspect ratios (10:1, 1:10)
  - [ ] Very small gaps, very large gaps
  - [ ] Zero-size screen (should handle gracefully)

#### Phase 7.3: Add Benchmark Suite

- [ ] Add `criterion` dependency
- [ ] Create `benches/tiling_bench.rs`:
  - [ ] `bench_dwindle_layout` - 1, 5, 10, 20 windows
  - [ ] `bench_master_layout` - 1, 5, 10, 20 windows
  - [ ] `bench_split_layout` - 1, 5, 10, 20 windows
  - [ ] `bench_grid_layout` - 1, 5, 10, 20 windows
  - [ ] `bench_gaps_from_config` - parse gaps configuration
  - [ ] `bench_workspace_lookup` - by name lookup
  - [ ] `bench_window_rule_matching` - rule evaluation
- [ ] Add `[[bench]]` section to `Cargo.toml`
- [ ] Establish baseline measurements
- [ ] Add benchmark comparison to CI (optional)

#### Phase 7.4: Create Mock Infrastructure

- [ ] Create `tiling/testing.rs` (cfg(test) only):
  - [ ] `MockWindowManager` struct
  - [ ] `MockScreen` struct
  - [ ] `MockWindow` struct
- [ ] Implement mock methods:
  - [ ] `with_screens(screens: Vec<MockScreen>) -> Self`
  - [ ] `with_windows(windows: Vec<MockWindow>) -> Self`
  - [ ] `focus_window(id: u32)`
  - [ ] `get_focused_window() -> Option<u32>`
  - [ ] `move_window(id: u32, frame: Rect)`
  - [ ] `get_window_frame(id: u32) -> Option<Rect>`
- [ ] Update existing tests to use mocks where applicable
- [ ] Add new unit tests using mock infrastructure

- [ ] Run tests, fix clippy warnings, ensure build passes

**Verification**: Integration tests pass (with permissions), fuzz tests find no issues, benchmarks established

---

## Risk Log

| Risk                                     | Likelihood | Impact | Mitigation                                       |
| ---------------------------------------- | ---------- | ------ | ------------------------------------------------ |
| Breaking changes cause regressions       | Medium     | High   | Comprehensive test coverage, incremental changes |
| FFI wrapper introduces bugs              | Medium     | High   | Careful safety documentation, thorough testing   |
| Performance optimizations add complexity | Medium     | Medium | Benchmark before/after, revert if no improvement |
| Thread model change causes deadlocks     | Low        | High   | Careful lock ordering, debug monitoring          |
| Cache invalidation bugs                  | Medium     | Medium | Clear invalidation triggers, conservative TTLs   |

---

## Notes

- **Breaking Changes**: API changes from `bool` to `Result` are allowed
- **Dependencies**: Adding `smallvec`, `criterion`, `proptest`
- **Event Coalescing**: 4ms window chosen to be close to frame time
- **Worker Thread**: FIFO processing for predictable ordering
- **FFI Priority**: Safety first, then optimize hot paths
- **Test Coverage**: Target >85% for tiling module after improvements

---

## Change Log

| Date       | Change                                                             |
| ---------- | ------------------------------------------------------------------ |
| 2026-01-13 | Initial improvement plan created                                   |
| 2026-01-13 | Milestones 1-3 completed, fixed REPOSITION_THRESHOLD test          |
| 2026-01-13 | Milestone 4 Phase 4.1: AXElement wrapper complete (7 tests)        |
| 2026-01-13 | Milestone 4 Phase 4.2: Safety documentation complete               |
| 2026-01-13 | Milestone 4 Phase 4.3: ffi_try! macros complete (5 tests)          |
| 2026-01-13 | Milestone 4 Phase 4.4: Applied macros to window.rs                 |
| 2026-01-13 | Milestone 4 complete - 944 tests passing                           |
| 2026-01-13 | Milestone 5 Phase 5.1: Workspace name index (947 tests)            |
| 2026-01-13 | Milestone 5 Phase 5.2: Batch JankyBorders commands (949 tests)     |
| 2026-01-14 | Milestone 5 Phase 5.3: Animation buffer infrastructure (953 tests) |
| 2026-01-14 | Milestone 5 Phase 5.4: Layout result caching (971 tests)           |
| 2026-01-14 | Milestone 5 Phase 5.5: AXUIElement resolution caching (986 tests)  |
| 2026-01-14 | Milestone 5 Phase 5.6: Event coalescing (1000 tests)               |
