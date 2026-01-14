# Window Animation Performance Improvements

This document tracks the implementation of performance improvements for window animations in the Stache tiling window manager.

## Overview

The goal is to make window animations smoother and more efficient by leveraging macOS private APIs and optimization techniques used by other window managers (Rift, yabai, AeroSpace).

**Key Constraints:**

- No SIP (System Integrity Protection) disabling required
- Must maintain compatibility with existing AX-based window management
- Changes should be reversible via feature flags where appropriate

---

## Architecture Flowcharts

### Master Decision Flow

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│                     WINDOW ANIMATION PERFORMANCE IMPROVEMENTS                    │
│                            Decision Tree & Implementation                        │
└─────────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────────┐
│ PHASE 1: SCREEN UPDATE BATCHING (SLSDisableUpdate)                              │
│ Priority: HIGH | Complexity: LOW | Impact: SIGNIFICANT                          │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │ Import SLSDisableUpdate and   │
                    │ SLSReenableUpdate from        │
                    │ SkyLight framework            │
                    └───────────────┬───────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │ Wrap animation loop with      │
                    │ disable/reenable calls?       │
                    └───────────────┬───────────────┘
                                    │
                       ┌────────────┴────────────┐
                       │    DECISION DIAMOND     │
                       │  Does SLSDisableUpdate  │
                       │  improve frame timing?  │
                       └───────────┬─────────────┘
                                   │
                    ┌──────────────┴──────────────┐
                    │                             │
                    ▼ YES                         ▼ NO
        ┌───────────────────────┐     ┌───────────────────────┐
        │ Apply per-frame:      │     │ Try per-batch:        │
        │ SLSDisableUpdate()    │     │ Call before layout    │
        │ <move all windows>    │     │ calculation, reenable │
        │ SLSReenableUpdate()   │     │ after all windows set │
        └───────────┬───────────┘     └───────────┬───────────┘
                    │                             │
                    └────────────┬────────────────┘
                                 │
                                 ▼
                    ┌───────────────────────────────┐
                    │ Benchmark: Compare frame      │
                    │ timing consistency            │
                    │ If <2ms variance → SUCCESS    │
                    │ If >5ms variance → ROLLBACK   │
                    └───────────────────────────────┘
                                 │
                                 ▼
              ┌─────────────────────────────────────────┐
              │ DEAD END CHECK: SLSDisableUpdate can    │
              │ cause visual glitches if held too long. │
              │ Max hold time: 16ms (1 frame @ 60Hz)    │
              └─────────────────────────────────────────┘
```

### SLS Transaction Flow

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│ PHASE 3: SLS TRANSACTION BATCHING                                               │
│ Priority: HIGH | Complexity: MEDIUM | Impact: HIGH                              │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │ Can we use SLSTransactions    │
                    │ without window ID resolution? │
                    └───────────────┬───────────────┘
                                    │
                       ┌────────────┴────────────┐
                       │    DECISION DIAMOND     │
                       │  Is window ID available │
                       │  without AX API call?   │
                       └───────────┬─────────────┘
                                   │
                    ┌──────────────┴──────────────┐
                    │                             │
                    ▼ YES (cached)                ▼ NO
        ┌───────────────────────┐     ┌───────────────────────┐
        │ Use SLS Transaction:  │     │ Add window ID cache   │
        │                       │     │ during tracking:      │
        │ tx = SLSTransaction   │     │                       │
        │   Create()            │     │ Store CGWindowID      │
        │ for each window:      │     │ from _AXUIElement-    │
        │   SLSTransaction-     │     │   GetWindow() at      │
        │     SetWindowAlpha()  │     │   window creation     │
        │   SLSTransaction-     │     └───────────┬───────────┘
        │     OrderWindow()     │                 │
        │ SLSTransactionCommit()│    ┌────────────┴────────────┐
        └───────────┬───────────┘    │    DECISION DIAMOND     │
                    │                │  Cache hit rate > 95%?  │
                    │                └───────────┬─────────────┘
                    │                            │
                    │                 ┌──────────┴──────────┐
                    │                 │                     │
                    │                 ▼ YES                 ▼ NO
                    │     ┌───────────────────┐  ┌───────────────────┐
                    │     │ Use SLS for       │  │ Keep AX fallback  │
                    │     │ cached windows    │  │ Add CGWindowList- │
                    │     │ AX for uncached   │  │   CopyWindowInfo  │
                    │     └─────────┬─────────┘  │   fallback        │
                    │               │            └─────────┬─────────┘
                    │               │                      │
                    └───────────────┴──────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────────────┐
                    │ LOOP: Transaction APIs require        │
                    │ window server connection ID (cid)     │
                    │                                       │
                    │ Pattern:                              │
                    │ 1. Get cid = SLSMainConnectionID()    │
                    │ 2. Cache cid in static variable       │
                    │ 3. Use for all SLS calls              │
                    └───────────────────────────────────────┘
```

### Window Bounds Query Flow

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│ PHASE 2: DIRECT WINDOW BOUNDS QUERY (Bypass AX for reads)                       │
│ Priority: MEDIUM | Complexity: LOW | Impact: MODERATE                           │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │ Current: AXUIElement-         │
                    │   CopyAttributeValue()        │
                    │ for kAXPositionAttribute,     │
                    │ kAXSizeAttribute              │
                    │ Latency: ~2-8ms per call      │
                    └───────────────┬───────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────┐
                    │ Alternative: CGSGetWindow-    │
                    │   Bounds(cid, wid, &frame)    │
                    │ Latency: ~0.1-0.5ms           │
                    └───────────────┬───────────────┘
                                    │
                       ┌────────────┴────────────┐
                       │    DECISION DIAMOND     │
                       │  Do we need accurate    │
                       │  window bounds during   │
                       │  animation frame calc?  │
                       └───────────┬─────────────┘
                                   │
                    ┌──────────────┴──────────────┐
                    │                             │
                    ▼ YES                         ▼ NO
        ┌───────────────────────┐     ┌───────────────────────┐
        │ Use SLSWindowQuery-   │     │ Continue using cached │
        │   Windows() +         │     │ positions from state  │
        │ SLSWindowIterator-    │     │ (current approach)    │
        │   GetBounds()         │     │                       │
        │                       │     │ LOOP: Only use CGS    │
        │ Batch query all       │     │ bounds for validation │
        │ windows at once       │     │ after animation ends  │
        └───────────┬───────────┘     └───────────────────────┘
                    │
                    ▼
                    ┌───────────────────────────────────────┐
                    │ DEAD END: CGSGetWindowBounds returns  │
                    │ server-side frame, may differ from    │
                    │ app's internal state during resize.   │
                    │                                       │
                    │ Solution: Use for position tracking   │
                    │ only, keep AX for resize operations.  │
                    └───────────────────────────────────────┘
```

### Window Proxy Animation Flow (Advanced)

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│ PHASE 6: WINDOW PROXY ANIMATION (Advanced - Rift/Yabai technique)               │
│ Priority: LOW | Complexity: HIGH | Impact: VERY HIGH                            │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────────────┐
                    │ CONCEPT: Instead of moving real      │
                    │ window on each frame, move a         │
                    │ lightweight "proxy" window with      │
                    │ captured screenshot content.         │
                    └───────────────────┬───────────────────┘
                                        │
                                        ▼
        ┌─────────────────────────────────────────────────────────────┐
        │                    ANIMATION SEQUENCE                        │
        │                                                             │
        │  ┌──────────┐    ┌──────────┐    ┌──────────┐              │
        │  │ CAPTURE  │───▶│ ANIMATE  │───▶│  SWAP    │              │
        │  │  PHASE   │    │  PHASE   │    │  PHASE   │              │
        │  └──────────┘    └──────────┘    └──────────┘              │
        │       │               │               │                     │
        │       ▼               ▼               ▼                     │
        │  ┌──────────┐    ┌──────────┐    ┌──────────┐              │
        │  │SLSHWCap- │    │Animate   │    │Show real │              │
        │  │tureWindow│    │proxy via │    │window at │              │
        │  │List()    │    │SLSSet-   │    │target,   │              │
        │  │          │    │WindowTra-│    │hide proxy│              │
        │  │Hide real │    │nsform()  │    │          │              │
        │  │window    │    │GPU accel │    │Release   │              │
        │  └──────────┘    └──────────┘    │proxy     │              │
        │                                   └──────────┘              │
        └─────────────────────────────────────────────────────────────┘
                                    │
                       ┌────────────┴────────────┐
                       │    DECISION DIAMOND     │
                       │ Is window capture fast  │
                       │ enough (<3ms)?          │
                       └───────────┬─────────────┘
                                   │
                    ┌──────────────┴──────────────┐
                    │                             │
                    ▼ YES                         ▼ NO
        ┌───────────────────────┐     ┌───────────────────────┐
        │ Implement full proxy  │     │ Skip proxy animation  │
        │ animation system:     │     │ for this window       │
        │                       │     │                       │
        │ 1. Capture in thread  │     │ LOOP BACK: Use direct │
        │ 2. Create proxy       │     │ AX animation instead  │
        │    window with        │     │                       │
        │    SLSNewWindow-      │     │ Some apps block HW    │
        │    WithOpaque...()    │     │ capture (security)    │
        │ 3. Animate transforms │     └───────────────────────┘
        │ 4. Swap at end        │
        └───────────────────────┘
                    │
                    ▼
        ┌───────────────────────────────────────────────────────┐
        │ COMPLEXITY WARNING:                                   │
        │ - Parallel capture threads for multiple windows       │
        │ - Memory management for CGImage buffers               │
        │ - Retina scaling (2x resolution)                      │
        │ - Z-order preservation during swap                    │
        │ - Edge case: window content changes during animation  │
        └───────────────────────────────────────────────────────┘
```

### Per-App Thread Isolation Flow

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│ PHASE 7: PER-APP THREAD ISOLATION (AeroSpace approach)                          │
│ Priority: MEDIUM | Complexity: HIGH | Impact: HIGH                              │
└─────────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
                    ┌───────────────────────────────────────┐
                    │ PROBLEM: AX API can block when app    │
                    │ is unresponsive, stalling all         │
                    │ window animations.                    │
                    └───────────────────┬───────────────────┘
                                        │
                                        ▼
                    ┌───────────────────────────────────────┐
                    │ SOLUTION: Isolate AX calls per-app    │
                    │ so one slow app doesn't block others. │
                    └───────────────────┬───────────────────┘
                                        │
                       ┌────────────────┴────────────────┐
                       │      DECISION DIAMOND           │
                       │ Is animation targeting multiple │
                       │ applications' windows?          │
                       └───────────────┬─────────────────┘
                                       │
                        ┌──────────────┴──────────────┐
                        │                             │
                        ▼ YES                         ▼ NO
            ┌───────────────────────┐     ┌───────────────────────┐
            │ Use per-app channels: │     │ Single-threaded is OK │
            │                       │     │ (current approach)    │
            │ for each app:         │     └───────────────────────┘
            │   spawn thread/task   │
            │   with timeout (50ms) │
            │   send frame updates  │
            │   via channel         │
            └───────────┬───────────┘
                        │
                        ▼
            ┌───────────────────────────────────────────────────────┐
            │ IMPLEMENTATION:                                       │
            │                                                       │
            │ ┌──────────┐   ┌──────────┐   ┌──────────┐           │
            │ │ Main     │   │ App A    │   │ App B    │           │
            │ │ Animation│──▶│ Thread   │   │ Thread   │           │
            │ │ Thread   │──▶│ (Firefox)│   │ (VSCode) │           │
            │ └──────────┘   └──────────┘   └──────────┘           │
            │      │              │              │                  │
            │      │         ┌────┴────┐    ┌────┴────┐            │
            │      │         │AX calls │    │AX calls │            │
            │      │         │with     │    │with     │            │
            │      │         │timeout  │    │timeout  │            │
            │      │         └─────────┘    └─────────┘            │
            │      │                                                │
            │      ▼                                                │
            │ If timeout: skip window this frame                   │
            │ (better than blocking all windows)                   │
            └───────────────────────────────────────────────────────┘
                        │
                        ▼
            ┌───────────────────────────────────────────────────────┐
            │ DEAD END WARNING: Thread-per-app adds complexity     │
            │ - Thread pool management                             │
            │ - Channel synchronization                            │
            │ - Timeout handling                                   │
            │                                                       │
            │ RECOMMENDATION: Only implement if AX blocking is     │
            │ a measured problem (profile first!)                  │
            └───────────────────────────────────────────────────────┘
```

### Implementation Priority Sequence

```text
  ┌─────────────────────────────────────────────────────────────────────────────┐
  │ RECOMMENDED SEQUENCE:                                                       │
  │                                                                             │
  │ ┌───┐  ┌─────────────────────────────────────────────────────────────────┐ │
  │ │ 1 │  │ SLSDisableUpdate/ReenableUpdate                                 │ │
  │ └───┘  │ Effort: 2 hours | Risk: Low | Reversible: Yes                   │ │
  │        └─────────────────────────────────────────────────────────────────┘ │
  │                                     │                                       │
  │                                     ▼                                       │
  │ ┌───┐  ┌─────────────────────────────────────────────────────────────────┐ │
  │ │ 2 │  │ CGSGetWindowBounds for read operations                          │ │
  │ └───┘  │ Effort: 3 hours | Risk: Low | Reversible: Yes                   │ │
  │        └─────────────────────────────────────────────────────────────────┘ │
  │                                     │                                       │
  │                                     ▼                                       │
  │ ┌───┐  ┌─────────────────────────────────────────────────────────────────┐ │
  │ │ 3 │  │ SLS Transaction batching for multi-window updates               │ │
  │ └───┘  │ Effort: 1 day | Risk: Medium | Reversible: Yes                  │ │
  │        └─────────────────────────────────────────────────────────────────┘ │
  │                                     │                                       │
  │                                     ▼                                       │
  │ ┌───┐  ┌─────────────────────────────────────────────────────────────────┐ │
  │ │ 4 │  │ Cache window server IDs (CGWindowID) at tracking time           │ │
  │ └───┘  │ Effort: 4 hours | Risk: Low | Reversible: Yes                   │ │
  │        └─────────────────────────────────────────────────────────────────┘ │
  │                                     │                                       │
  │                                     ▼                                       │
  │ ┌───┐  ┌─────────────────────────────────────────────────────────────────┐ │
  │ │ 5 │  │ Window proxy animation system (optional, high complexity)       │ │
  │ └───┘  │ Effort: 1-2 weeks | Risk: High | Reversible: Feature flag       │ │
  │        └─────────────────────────────────────────────────────────────────┘ │
  │                                     │                                       │
  │                                     ▼                                       │
  │ ┌───┐  ┌─────────────────────────────────────────────────────────────────┐ │
  │ │ 6 │  │ Per-app thread isolation (only if blocking is measured issue)   │ │
  │ └───┘  │ Effort: 1 week | Risk: Medium | Reversible: Feature flag        │ │
  │        └─────────────────────────────────────────────────────────────────┘ │
  └─────────────────────────────────────────────────────────────────────────────┘
```

---

## Private APIs Reference (No SIP Required)

| API                                   | Purpose                | Usage Pattern              |
| ------------------------------------- | ---------------------- | -------------------------- |
| `SLSMainConnectionID()`               | Get connection ID      | Cache once at init         |
| `SLSDisableUpdate(cid)`               | Pause screen refresh   | Wrap batch operations      |
| `SLSReenableUpdate(cid)`              | Resume screen refresh  | After batch complete       |
| `SLSTransactionCreate(cid)`           | Begin atomic batch     | Before multi-window update |
| `SLSTransactionCommit(tx, sync)`      | Apply atomic batch     | After queuing all changes  |
| `CGSGetWindowBounds(cid, wid, &rect)` | Fast bounds query      | Replace AX reads           |
| `SLSWindowQueryWindows()`             | Batch window query     | Layout recalculation       |
| `_AXUIElementGetWindow(ax, &wid)`     | Get CGWindowID from AX | Cache at window track time |

---

## Phase 1: Screen Update Batching (SLSDisableUpdate)

**Priority:** HIGH | **Complexity:** LOW | **Impact:** SIGNIFICANT

Wrap animation frame updates with `SLSDisableUpdate`/`SLSReenableUpdate` to prevent intermediate screen refreshes during batch window operations.

### Tasks

- [ ] **1.1** Add SkyLight FFI bindings
  - [ ] Add `SLSMainConnectionID()` binding
  - [ ] Add `SLSDisableUpdate(cid: i32) -> i32` binding
  - [ ] Add `SLSReenableUpdate(cid: i32) -> i32` binding
  - [ ] Create safe wrapper module in `app/native/src/tiling/ffi/skylight.rs`

- [ ] **1.2** Cache connection ID
  - [ ] Create `static G_CONNECTION: OnceLock<i32>` for cached connection ID
  - [ ] Initialize on first use via `SLSMainConnectionID()`

- [ ] **1.3** Integrate with animation loop
  - [ ] Add `sls_disable_update()` call before frame batch
  - [ ] Add `sls_reenable_update()` call after frame batch
  - [ ] Ensure reenable is called even on early returns (use RAII guard)

- [ ] **1.4** Add feature flag
  - [ ] Add `use_sls_update_batching: bool` to animation config
  - [ ] Default to `true` for testing
  - [ ] Add config option to disable if issues arise

- [ ] **1.5** Benchmark and validate
  - [ ] Measure frame timing consistency before/after
  - [ ] Test with different window counts (1, 5, 10, 20 windows)
  - [ ] Verify no visual glitches on animation completion
  - [ ] Document max safe hold time (~16ms at 60Hz)

### Code Location

- FFI: `app/native/src/tiling/ffi/skylight.rs` (new file)
- Integration: `app/native/src/tiling/animation.rs`

### Acceptance Criteria

- [ ] Frame timing variance reduced by >30%
- [ ] No visual artifacts during or after animation
- [ ] Feature can be disabled via config

---

## Phase 2: Fast Window Bounds Query (CGSGetWindowBounds)

**Priority:** MEDIUM | **Complexity:** LOW | **Impact:** MODERATE

Replace slow AX attribute reads with fast CGS bounds queries for window position validation.

### Tasks

- [ ] **2.1** Add CGS bounds FFI
  - [ ] Add `CGSGetWindowBounds(cid: i32, wid: u32, frame: *mut CGRect) -> i32` binding
  - [ ] Add to `ffi/skylight.rs` module

- [ ] **2.2** Create safe wrapper
  - [ ] Create `get_window_bounds_fast(wid: u32) -> Option<Rect>` function
  - [ ] Handle error cases (window closed, invalid ID)
  - [ ] Convert CGRect to internal Rect type

- [ ] **2.3** Integrate for validation
  - [ ] Use CGS bounds for post-animation validation
  - [ ] Use CGS bounds for layout dirty detection
  - [ ] Keep AX for actual window manipulation (write operations)

- [ ] **2.4** Add Window Server ID caching
  - [ ] Store `CGWindowID` in `TrackedWindow` struct
  - [ ] Populate via `_AXUIElementGetWindow()` at track time
  - [ ] Use cached ID for CGS calls

- [ ] **2.5** Benchmark
  - [ ] Compare CGS vs AX read latency (expect ~10x improvement)
  - [ ] Measure cache hit rate for window IDs
  - [ ] Document any edge cases (windows without server IDs)

### Code Location

- FFI: `app/native/src/tiling/ffi/skylight.rs`
- Integration: `app/native/src/tiling/window.rs`
- State: `app/native/src/tiling/state.rs` (TrackedWindow)

### Acceptance Criteria

- [ ] Window bounds read latency <1ms (vs ~2-8ms with AX)
- [ ] > 95% cache hit rate for window server IDs
- [ ] No functionality regression

---

## Phase 3: SLS Transaction Batching

**Priority:** HIGH | **Complexity:** MEDIUM | **Impact:** HIGH

Use SLS transactions to batch multiple window operations atomically, reducing round-trips to the window server.

### Tasks

- [ ] **3.1** Add SLS Transaction FFI
  - [ ] Add `SLSTransactionCreate(cid: i32) -> *mut CFType` binding
  - [ ] Add `SLSTransactionCommit(tx: *mut CFType, sync: i32) -> CGError` binding
  - [ ] Add `SLSTransactionOrderWindow(tx, wid, order, rel_wid) -> CGError` binding
  - [ ] Add `SLSTransactionSetWindowAlpha(tx, wid, alpha) -> CGError` binding

- [ ] **3.2** Create transaction abstraction
  - [ ] Create `SLSTransaction` RAII wrapper struct
  - [ ] Implement `Drop` to auto-commit or rollback
  - [ ] Add builder pattern for queuing operations

- [ ] **3.3** Integrate with animation system
  - [ ] Use transactions for multi-window z-order updates
  - [ ] Use transactions for batch alpha changes (show/hide)
  - [ ] Evaluate if position updates can use transactions

- [ ] **3.4** Hybrid approach implementation
  - [ ] Keep AX for position/size (transactions don't support this well)
  - [ ] Use SLS for alpha, z-order, tags
  - [ ] Document which operations use which API

- [ ] **3.5** Testing
  - [ ] Test transaction commit (async vs sync)
  - [ ] Test rollback on error
  - [ ] Verify z-order consistency after batch updates

### Code Location

- FFI: `app/native/src/tiling/ffi/skylight.rs`
- Abstraction: `app/native/src/tiling/ffi/transaction.rs` (new file)
- Integration: `app/native/src/tiling/animation.rs`

### Acceptance Criteria

- [ ] Batch operations complete in single window server round-trip
- [ ] No z-order bugs after transaction commit
- [ ] Clean error handling with rollback

---

## Phase 4: Window Query Optimization (SLSWindowQuery)

**Priority:** MEDIUM | **Complexity:** MEDIUM | **Impact:** MODERATE

Use efficient SLS window query APIs for batch window enumeration instead of individual AX queries.

### Tasks

- [ ] **4.1** Add SLS Window Query FFI
  - [ ] Add `SLSWindowQueryWindows(cid, windows, count) -> *mut CFType`
  - [ ] Add `SLSWindowQueryResultCopyWindows(query) -> *mut CFType`
  - [ ] Add `SLSWindowIteratorAdvance(iter) -> bool`
  - [ ] Add `SLSWindowIteratorGetWindowID(iter) -> u32`
  - [ ] Add `SLSWindowIteratorGetBounds(iter) -> CGRect`
  - [ ] Add `SLSWindowIteratorGetPID(iter) -> i32`
  - [ ] Add `SLSWindowIteratorGetLevel(iter) -> i32`
  - [ ] Add `SLSWindowIteratorGetTags(iter) -> u64`

- [ ] **4.2** Create WindowQuery abstraction
  - [ ] Create `WindowQuery` struct with iterator interface
  - [ ] Implement `Iterator` trait for ergonomic usage
  - [ ] Handle memory management (CFRelease)

- [ ] **4.3** Integrate for batch queries
  - [ ] Use for initial window enumeration at startup
  - [ ] Use for layout recalculation validation
  - [ ] Use for animation target verification

- [ ] **4.4** Performance comparison
  - [ ] Benchmark vs CGWindowListCopyWindowInfo
  - [ ] Benchmark vs individual AX queries
  - [ ] Document when to use which API

### Code Location

- FFI: `app/native/src/tiling/ffi/skylight.rs`
- Abstraction: `app/native/src/tiling/ffi/window_query.rs` (new file)
- Integration: `app/native/src/tiling/window.rs`

### Acceptance Criteria

- [ ] Batch query 50+ windows in <5ms
- [ ] Correct results matching CGWindowList
- [ ] Memory-safe with proper cleanup

---

## Phase 5: Animation Cache Improvements

**Priority:** HIGH | **Complexity:** LOW | **Impact:** MODERATE

Optimize caching behavior during active animations to reduce AX API calls.

### Tasks

- [ ] **5.1** Animation-aware cache TTL
  - [ ] Add `ANIMATION_ACTIVE: AtomicBool` flag
  - [ ] Extend AX element cache TTL during animation (5s -> 30s)
  - [ ] Reset TTL after animation completes

- [ ] **5.2** Pre-warm cache before animation
  - [ ] Collect all window IDs from transitions
  - [ ] Batch-resolve AX elements before animation loop
  - [ ] Update cache entries for all resolved elements
  - [ ] (Note: partially implemented, verify completeness)

- [ ] **5.3** Cache metrics
  - [ ] Add hit/miss counters for AX element cache
  - [ ] Add hit/miss counters for CG window list cache
  - [ ] Log cache stats in debug builds

- [ ] **5.4** Persistent window ID mapping
  - [ ] Cache `AXUIElement -> CGWindowID` mapping
  - [ ] Use `_AXUIElementGetWindow()` result
  - [ ] Invalidate on window close notification

### Code Location

- Cache: `app/native/src/tiling/window.rs` (existing cache code)
- Metrics: `app/native/src/tiling/constants.rs`

### Acceptance Criteria

- [ ] Cache hit rate >98% during animations
- [ ] No cache-related animation stutter
- [ ] Metrics visible in debug logs

---

## Phase 6: Window Proxy Animation (Advanced)

**Priority:** LOW | **Complexity:** HIGH | **Impact:** VERY HIGH

Implement proxy window technique for butter-smooth animations by animating a screenshot instead of the actual window.

### Prerequisites

- [ ] Phase 1 complete (SLS update batching)
- [ ] Phase 3 complete (SLS transactions)
- [ ] Phase 4 complete (SLS window queries)

### Tasks

- [ ] **6.1** Add window capture FFI
  - [ ] Add `SLSHWCaptureWindowList(cid, windows, count, options) -> *mut CFArray<CGImage>`
  - [ ] Add capture options constants ((1 << 11) | (1 << 8))
  - [ ] Handle Retina scaling (2x resolution)

- [ ] **6.2** Add proxy window creation FFI
  - [ ] Add `SLSNewWindowWithOpaqueShapeAndContext()` binding
  - [ ] Add `SLSReleaseWindow(cid, wid)` binding
  - [ ] Add `SLWindowContextCreate(cid, wid, options)` binding
  - [ ] Add `SLSSetWindowResolution(cid, wid, scale)` binding

- [ ] **6.3** Add window transform FFI
  - [ ] Add `SLSSetWindowTransform(cid, wid, transform) -> CGError`
  - [ ] Add `SLSGetWindowTransform(cid, wid, *transform) -> CGError`
  - [ ] Add CGAffineTransform helpers

- [ ] **6.4** Implement proxy animation system
  - [ ] Create `WindowProxy` struct
  - [ ] Implement capture phase (parallel per window)
  - [ ] Implement animate phase (transform-based)
  - [ ] Implement swap phase (show real, hide proxy)

- [ ] **6.5** Handle edge cases
  - [ ] Window content changes during animation
  - [ ] Z-order preservation
  - [ ] Multi-monitor with different scales
  - [ ] Apps that block hardware capture

- [ ] **6.6** Feature flag and fallback
  - [ ] Add `use_proxy_animation: bool` config
  - [ ] Fallback to direct animation on capture failure
  - [ ] Per-app disable list for problematic apps

### Code Location

- FFI: `app/native/src/tiling/ffi/skylight.rs`
- Proxy: `app/native/src/tiling/animation/proxy.rs` (new file)
- Integration: `app/native/src/tiling/animation.rs`

### Acceptance Criteria

- [ ] Proxy capture <3ms per window
- [ ] Transform animation at native refresh rate (60/120Hz)
- [ ] Seamless swap with no visible flicker
- [ ] Graceful fallback for unsupported windows

---

## Phase 7: Per-App Thread Isolation (Optional)

**Priority:** LOW | **Complexity:** HIGH | **Impact:** HIGH (for problem cases)

Isolate AX calls per-application to prevent one unresponsive app from blocking all window animations.

### Prerequisites

- [ ] Profiling shows AX blocking is a real problem
- [ ] Specific apps identified as problematic

### Tasks

- [ ] **7.1** Design thread architecture
  - [ ] Define message types for window operations
  - [ ] Design channel-based communication
  - [ ] Define timeout behavior (50ms default)

- [ ] **7.2** Implement app worker threads
  - [ ] Create thread pool per tracked application
  - [ ] Implement operation queue with timeout
  - [ ] Handle thread cleanup on app termination

- [ ] **7.3** Integrate with animation system
  - [ ] Route window operations to appropriate app thread
  - [ ] Aggregate results with timeout handling
  - [ ] Skip timed-out windows (animate next frame)

- [ ] **7.4** Testing and tuning
  - [ ] Test with intentionally slow apps
  - [ ] Tune timeout values
  - [ ] Measure overhead vs single-threaded

### Code Location

- Worker: `app/native/src/tiling/worker.rs` (new file)
- Integration: `app/native/src/tiling/animation.rs`

### Acceptance Criteria

- [ ] One hung app doesn't block other windows
- [ ] Timeout overhead <1ms per frame
- [ ] Clean thread cleanup on app quit

---

## Testing Plan

### Unit Tests

- [ ] FFI binding smoke tests
- [ ] Transaction commit/rollback tests
- [ ] Cache TTL behavior tests
- [ ] Window query result parsing tests

### Integration Tests

- [ ] Animation with SLS update batching
- [ ] Multi-window batch transactions
- [ ] Cache behavior under rapid layout changes
- [ ] Proxy animation end-to-end (if implemented)

### Performance Benchmarks

- [ ] Frame timing consistency (variance measurement)
- [ ] AX vs CGS read latency comparison
- [ ] Transaction vs individual call overhead
- [ ] Window enumeration: CGWindowList vs SLSWindowQuery

### Manual Testing Scenarios

- [ ] Rapid workspace switches (spam test)
- [ ] Animations with 20+ windows
- [ ] Mixed app responsiveness (some apps slow)
- [ ] Multi-monitor with different refresh rates
- [ ] ProMotion displays (variable refresh rate)

---

## Risk Assessment

| Phase                  | Risk Level | Mitigation                         |
| ---------------------- | ---------- | ---------------------------------- |
| Phase 1 (SLS Update)   | Low        | Feature flag, short hold time      |
| Phase 2 (CGS Bounds)   | Low        | Keep AX fallback                   |
| Phase 3 (Transactions) | Medium     | Hybrid approach, rollback on error |
| Phase 4 (Window Query) | Medium     | Parallel CGWindowList validation   |
| Phase 5 (Cache)        | Low        | Metrics monitoring                 |
| Phase 6 (Proxy)        | High       | Feature flag, per-app disable      |
| Phase 7 (Threading)    | Medium     | Only implement if needed           |

---

## References

- [Rift WM](https://github.com/acsandmann/rift) - Private API usage patterns
- [yabai](https://github.com/koekeishiya/yabai) - SLS transaction and proxy techniques
- [AeroSpace](https://github.com/nikitabobko/AeroSpace) - Thread isolation plans
- Apple Core Video documentation (CVDisplayLink)
- macOS private headers (SkyLight.framework)

---

## Changelog

| Date       | Change                             |
| ---------- | ---------------------------------- |
| 2026-01-14 | Initial plan created               |
| 2026-01-14 | Added architecture flowcharts      |
| 2026-01-14 | Added private APIs reference table |
