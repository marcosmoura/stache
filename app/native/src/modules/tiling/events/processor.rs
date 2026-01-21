//! Event processor with per-screen refresh-rate-based batching.
//!
//! The `EventProcessor` is responsible for:
//! - Dispatching time-sensitive events immediately (focus, create, destroy)
//! - Batching geometry events (move, resize) per display refresh rate
//! - Coalescing multiple geometry updates for the same window
//!
//! # Multi-Monitor Support
//!
//! Each screen can have a different refresh rate. The processor maintains:
//! - A mapping of window ID → screen ID
//! - Per-screen batch queues with independent timers
//!
//! When a geometry event arrives, it's routed to the appropriate screen's batch queue.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::modules::tiling::actor::{
    GeometryUpdate, GeometryUpdateType, StateActorHandle, StateMessage, WindowCreatedInfo,
};
use crate::modules::tiling::state::Rect;

/// Default refresh rate if detection fails (60 Hz).
const DEFAULT_REFRESH_RATE: f64 = 60.0;

/// Minimum refresh rate to prevent too-fast batching.
const MIN_REFRESH_RATE: f64 = 30.0;

/// Maximum refresh rate to prevent too-slow batching.
const MAX_REFRESH_RATE: f64 = 360.0;

/// A batch queue for a single screen.
struct ScreenBatch {
    /// Screen ID (`CGDirectDisplayID`).
    #[allow(dead_code)] // Stored for potential debugging use
    screen_id: u32,

    /// Refresh rate in Hz.
    refresh_rate: f64,

    /// Pending geometry updates for windows on this screen.
    updates: HashMap<u32, GeometryUpdate>,

    /// Whether the timer for this screen is running.
    timer_running: AtomicBool,
}

impl ScreenBatch {
    fn new(screen_id: u32, refresh_rate: f64) -> Self {
        Self {
            screen_id,
            refresh_rate: refresh_rate.clamp(MIN_REFRESH_RATE, MAX_REFRESH_RATE),
            updates: HashMap::new(),
            timer_running: AtomicBool::new(false),
        }
    }

    fn batch_interval(&self) -> Duration { Duration::from_secs_f64(1.0 / self.refresh_rate) }
}

/// Event processor that batches geometry updates per-screen and dispatches to the state actor.
///
/// # Thread Safety
///
/// The processor is designed to be called from multiple threads:
/// - `AXObserver` callbacks come from the main thread
/// - App monitor callbacks come from the main thread
/// - Screen monitor callbacks may come from any thread
///
/// The batching mechanism uses `parking_lot::Mutex` for fast, uncontended locking.
pub struct EventProcessor {
    /// Handle to send messages to the state actor.
    actor_handle: StateActorHandle,

    /// Per-screen batch queues.
    screen_batches: Arc<Mutex<HashMap<u32, ScreenBatch>>>,

    /// Window ID → Screen ID mapping for routing geometry events.
    window_screen_map: Arc<DashMap<u32, u32>>,

    /// PID → Set of Window IDs mapping for destroy detection.
    /// When we get a destroy event but can't get the window ID, we compare
    /// against current windows from macOS to find which one was destroyed.
    pid_windows: Arc<Mutex<HashMap<i32, HashSet<u32>>>>,

    /// Default screen ID for windows with unknown screen assignment.
    default_screen_id: AtomicU32,

    /// Whether the processor is running.
    running: Arc<AtomicBool>,
}

impl EventProcessor {
    /// Create a new event processor.
    ///
    /// # Arguments
    ///
    /// * `actor_handle` - Handle to send messages to the state actor
    #[must_use]
    pub fn new(actor_handle: StateActorHandle) -> Self {
        Self {
            actor_handle,
            screen_batches: Arc::new(Mutex::new(HashMap::new())),
            window_screen_map: Arc::new(DashMap::new()),
            pid_windows: Arc::new(Mutex::new(HashMap::new())),
            default_screen_id: AtomicU32::new(0),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Register a screen with its refresh rate.
    ///
    /// This creates a batch queue for the screen and starts its timer.
    pub fn register_screen(&self, screen_id: u32, refresh_rate: f64) {
        let mut batches = self.screen_batches.lock();

        if batches.contains_key(&screen_id) {
            // Update refresh rate if screen already registered
            if let Some(batch) = batches.get_mut(&screen_id) {
                let old_rate = batch.refresh_rate;
                batch.refresh_rate = refresh_rate.clamp(MIN_REFRESH_RATE, MAX_REFRESH_RATE);
                log::debug!(
                    "Updated screen {} refresh rate: {} Hz → {} Hz",
                    screen_id,
                    old_rate,
                    batch.refresh_rate
                );
            }
            return;
        }

        let batch = ScreenBatch::new(screen_id, refresh_rate);
        log::debug!(
            "Registered screen {} with refresh rate {} Hz (batch interval {:?})",
            screen_id,
            batch.refresh_rate,
            batch.batch_interval()
        );

        batches.insert(screen_id, batch);

        // Set as default if it's the first screen
        if batches.len() == 1 {
            self.default_screen_id.store(screen_id, Ordering::SeqCst);
        }

        // Start timer for this screen if processor is running
        if self.running.load(Ordering::SeqCst) {
            drop(batches); // Release lock before starting timer
            self.start_screen_timer(screen_id);
        }
    }

    /// Unregister a screen.
    ///
    /// Any pending geometry updates for windows on this screen will be flushed.
    pub fn unregister_screen(&self, screen_id: u32) {
        let mut batches = self.screen_batches.lock();

        if let Some(mut batch) = batches.remove(&screen_id) {
            // Stop the timer
            batch.timer_running.store(false, Ordering::SeqCst);

            // Flush any pending updates
            if !batch.updates.is_empty() {
                let updates: Vec<GeometryUpdate> = batch.updates.drain().map(|(_, v)| v).collect();
                drop(batches); // Release lock before sending
                let _ = self.actor_handle.send(StateMessage::BatchedGeometryUpdates(updates));
            }

            log::debug!("Unregistered screen {screen_id}");
        }

        // Update default screen if needed
        let batches = self.screen_batches.lock();
        if self.default_screen_id.load(Ordering::SeqCst) == screen_id
            && let Some((&new_default, _)) = batches.iter().next()
        {
            self.default_screen_id.store(new_default, Ordering::SeqCst);
        }
    }

    /// Set the screen assignment for a window.
    ///
    /// Call this when a window is created or moves to a different screen.
    pub fn set_window_screen(&self, window_id: u32, screen_id: u32) {
        self.window_screen_map.insert(window_id, screen_id);
    }

    /// Remove the screen assignment for a window.
    pub fn remove_window(&self, window_id: u32) { self.window_screen_map.remove(&window_id); }

    /// Get the screen ID for a window.
    fn get_window_screen(&self, window_id: u32) -> u32 {
        self.window_screen_map
            .get(&window_id)
            .map(|entry| *entry)
            .unwrap_or_else(|| self.default_screen_id.load(Ordering::SeqCst))
    }

    /// Start the batch flush timers for all registered screens.
    pub fn start(&self) {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            log::warn!("EventProcessor already running");
            return;
        }

        let screen_ids: Vec<u32> = self.screen_batches.lock().keys().copied().collect();
        for screen_id in screen_ids {
            self.start_screen_timer(screen_id);
        }

        log::debug!("EventProcessor started");
    }

    /// Start the timer for a specific screen.
    fn start_screen_timer(&self, screen_id: u32) {
        let batches = self.screen_batches.clone();
        let actor_handle = self.actor_handle.clone();
        let running = self.running.clone();

        // Get the batch interval for this screen
        let interval = {
            let batches = batches.lock();
            match batches.get(&screen_id) {
                Some(batch) => {
                    if batch
                        .timer_running
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        return; // Timer already running
                    }
                    batch.batch_interval()
                }
                None => return,
            }
        };

        tauri::async_runtime::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                ticker.tick().await;

                // Check if we should stop
                if !running.load(Ordering::SeqCst) {
                    break;
                }

                // Collect and clear pending updates for this screen
                let updates: Vec<GeometryUpdate> = {
                    let mut batches = batches.lock();
                    match batches.get_mut(&screen_id) {
                        Some(batch) => {
                            if !batch.timer_running.load(Ordering::SeqCst) {
                                break;
                            }
                            if batch.updates.is_empty() {
                                continue;
                            }
                            batch.updates.drain().map(|(_, v)| v).collect()
                        }
                        None => break, // Screen was unregistered
                    }
                };

                // Send batched updates to actor
                if !updates.is_empty() {
                    let _ = actor_handle.send(StateMessage::BatchedGeometryUpdates(updates));
                }
            }

            // Mark timer as stopped
            if let Some(batch) = batches.lock().get(&screen_id) {
                batch.timer_running.store(false, Ordering::SeqCst);
            }

            log::trace!("Batch timer stopped for screen {screen_id}");
        });

        log::trace!("Batch timer started for screen {screen_id} ({interval:?})");
    }

    /// Stop the batch flush timers.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);

        // Stop all screen timers
        {
            let batches = self.screen_batches.lock();
            for batch in batches.values() {
                batch.timer_running.store(false, Ordering::SeqCst);
            }
        }

        log::debug!("EventProcessor stopped");
    }

    /// Check if the processor is running.
    #[must_use]
    pub fn is_running(&self) -> bool { self.running.load(Ordering::SeqCst) }

    // ========================================================================
    // Immediate Dispatch (time-sensitive events)
    // ========================================================================

    /// Dispatch a window created event immediately.
    pub fn on_window_created(&self, info: WindowCreatedInfo) {
        let window_id = info.window_id;
        log::trace!("Window created: {window_id:?}");

        // Track this window for destroy detection
        self.pid_windows.lock().entry(info.pid).or_default().insert(info.window_id);

        let _ = self.actor_handle.send(StateMessage::WindowCreated(info));
    }

    /// Dispatch a window destroyed event immediately.
    ///
    /// Also removes any pending geometry updates for this window.
    pub fn on_window_destroyed(&self, window_id: u32) {
        log::debug!("tiling: processor.on_window_destroyed called for window_id={window_id}");

        // Remove from window-screen mapping
        let screen_id = self.window_screen_map.remove(&window_id).map(|(_, id)| id);

        // Remove from geometry batch
        if let Some(screen_id) = screen_id
            && let Some(batch) = self.screen_batches.lock().get_mut(&screen_id)
        {
            batch.updates.remove(&window_id);
        }

        // Remove from pid_windows tracking
        {
            let mut pid_windows = self.pid_windows.lock();
            for windows in pid_windows.values_mut() {
                windows.remove(&window_id);
            }
        }

        log::debug!("tiling: sending WindowDestroyed message to actor for window_id={window_id}");
        let _ = self.actor_handle.send(StateMessage::WindowDestroyed { window_id });
    }

    /// Handle window destruction when we only know the PID.
    ///
    /// Uses the window element cache to efficiently check which tracked
    /// windows are no longer valid, avoiding expensive AX enumeration.
    pub fn on_window_destroyed_for_pid(&self, pid: i32) {
        log::debug!("tiling: on_window_destroyed_for_pid called for pid={pid}");

        // Get tracked windows for this PID from our local cache
        let tracked_window_ids: Vec<u32> = {
            let pid_windows = self.pid_windows.lock();
            pid_windows.get(&pid).map(|s| s.iter().copied().collect()).unwrap_or_default()
        };

        log::debug!("tiling: tracked windows for pid={pid}: {tracked_window_ids:?}");

        if tracked_window_ids.is_empty() {
            log::debug!("tiling: no tracked windows for pid={pid}, nothing to do");
            return;
        }

        // Use window cache to efficiently find invalid windows
        // This uses O(1) validity checks on cached elements where possible
        let cache = crate::modules::tiling::effects::get_window_cache();
        let invalid_windows = cache.find_invalid_windows(&tracked_window_ids);

        log::debug!(
            "tiling: found {} invalid windows for pid={pid}",
            invalid_windows.len()
        );

        // Destroy invalid windows
        for window_id in invalid_windows {
            log::debug!("tiling: window {window_id} no longer valid for pid={pid}, destroying");
            self.on_window_destroyed(window_id);
        }
    }

    /// Dispatch a window focused event immediately.
    pub fn on_window_focused(&self, window_id: u32) {
        log::debug!("tiling: Window focused event received: {window_id}");
        let _ = self.actor_handle.send(StateMessage::WindowFocused { window_id });
    }

    /// Dispatch a window unfocused event immediately.
    pub fn on_window_unfocused(&self, window_id: u32) {
        log::trace!("Window unfocused: {window_id}");
        let _ = self.actor_handle.send(StateMessage::WindowUnfocused { window_id });
    }

    /// Dispatch a window minimized event immediately.
    pub fn on_window_minimized(&self, window_id: u32, minimized: bool) {
        log::trace!("Window minimized: {window_id} = {minimized}");
        let _ = self.actor_handle.send(StateMessage::WindowMinimized { window_id, minimized });
    }

    /// Dispatch a window title changed event immediately.
    pub fn on_window_title_changed(&self, window_id: u32, title: String) {
        log::trace!("Window title changed: {window_id} = '{title}'");
        let _ = self.actor_handle.send(StateMessage::WindowTitleChanged { window_id, title });
    }

    /// Dispatch a window fullscreen changed event immediately.
    pub fn on_window_fullscreen_changed(&self, window_id: u32, fullscreen: bool) {
        log::trace!("Window fullscreen changed: {window_id} = {fullscreen}");
        let _ = self
            .actor_handle
            .send(StateMessage::WindowFullscreenChanged { window_id, fullscreen });
    }

    // ========================================================================
    // Batched Dispatch (geometry events)
    // ========================================================================

    /// Queue a window moved event for batched dispatch.
    ///
    /// The event is routed to the appropriate screen's batch queue.
    pub fn on_window_moved(&self, window_id: u32, frame: Rect) {
        let screen_id = self.get_window_screen(window_id);
        let mut batches = self.screen_batches.lock();

        // Find the target screen, falling back to any available screen
        let target_screen = if batches.contains_key(&screen_id) {
            Some(screen_id)
        } else {
            batches.keys().next().copied()
        };

        if let Some(target) = target_screen {
            if let Some(batch) = batches.get_mut(&target) {
                batch
                    .updates
                    .entry(window_id)
                    .and_modify(|e| {
                        e.frame = frame;
                        e.update_type = match e.update_type {
                            GeometryUpdateType::Resize => GeometryUpdateType::MoveResize,
                            _ => GeometryUpdateType::Move,
                        };
                    })
                    .or_insert(GeometryUpdate {
                        window_id,
                        frame,
                        update_type: GeometryUpdateType::Move,
                    });
            }
        } else {
            // No screens registered, dispatch immediately
            drop(batches);
            let _ = self.actor_handle.send(StateMessage::WindowMoved { window_id, frame });
        }
    }

    /// Queue a window resized event for batched dispatch.
    ///
    /// The event is routed to the appropriate screen's batch queue.
    pub fn on_window_resized(&self, window_id: u32, frame: Rect) {
        let screen_id = self.get_window_screen(window_id);
        let mut batches = self.screen_batches.lock();

        // Find the target screen, falling back to any available screen
        let target_screen = if batches.contains_key(&screen_id) {
            Some(screen_id)
        } else {
            batches.keys().next().copied()
        };

        if let Some(target) = target_screen {
            if let Some(batch) = batches.get_mut(&target) {
                batch
                    .updates
                    .entry(window_id)
                    .and_modify(|e| {
                        e.frame = frame;
                        e.update_type = match e.update_type {
                            GeometryUpdateType::Move => GeometryUpdateType::MoveResize,
                            _ => GeometryUpdateType::Resize,
                        };
                    })
                    .or_insert(GeometryUpdate {
                        window_id,
                        frame,
                        update_type: GeometryUpdateType::Resize,
                    });
            }
        } else {
            drop(batches);
            let _ = self.actor_handle.send(StateMessage::WindowResized { window_id, frame });
        }
    }

    // ========================================================================
    // App Events (immediate dispatch)
    // ========================================================================

    /// Dispatch an app launched event.
    pub fn on_app_launched(&self, pid: i32, bundle_id: String, name: String) {
        log::trace!("App launched: pid={pid}, bundle={bundle_id}");
        let _ = self.actor_handle.send(StateMessage::AppLaunched { pid, bundle_id, name });
    }

    /// Dispatch an app terminated event.
    pub fn on_app_terminated(&self, pid: i32) {
        log::trace!("App terminated: pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppTerminated { pid });
    }

    /// Dispatch an app hidden event.
    pub fn on_app_hidden(&self, pid: i32) {
        log::trace!("App hidden: pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppHidden { pid });
    }

    /// Dispatch an app shown event.
    pub fn on_app_shown(&self, pid: i32) {
        log::trace!("App shown: pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppShown { pid });
    }

    /// Dispatch an app activated event.
    pub fn on_app_activated(&self, pid: i32) {
        log::trace!("App activated: pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppActivated { pid });
    }

    // ========================================================================
    // Screen Events (immediate dispatch)
    // ========================================================================

    /// Dispatch a screens changed event.
    ///
    /// NOTE: This sends `ScreensChanged` which requires the actor to call macOS APIs.
    /// Prefer `on_set_screens` with pre-detected screens when possible.
    pub fn on_screens_changed(&self) {
        log::trace!("Screens changed");
        let _ = self.actor_handle.send(StateMessage::ScreensChanged);
    }

    /// Dispatch screens with pre-detected screen data.
    ///
    /// This is the preferred method when screens have been detected on the main thread.
    pub fn on_set_screens(&self, screens: Vec<crate::modules::tiling::state::Screen>) {
        log::trace!("Setting {} screens", screens.len());
        let _ = self.actor_handle.send(StateMessage::SetScreens { screens });
    }

    // ========================================================================
    // Utility
    // ========================================================================

    /// Get the total number of pending geometry updates across all screens.
    #[must_use]
    pub fn pending_geometry_count(&self) -> usize {
        self.screen_batches.lock().values().map(|b| b.updates.len()).sum()
    }

    /// Get pending geometry count for a specific screen.
    #[must_use]
    pub fn pending_geometry_count_for_screen(&self, screen_id: u32) -> usize {
        self.screen_batches.lock().get(&screen_id).map_or(0, |b| b.updates.len())
    }

    /// Flush all pending geometry updates immediately.
    pub fn flush_all(&self) {
        let mut all_updates = Vec::new();

        {
            let mut batches = self.screen_batches.lock();
            for batch in batches.values_mut() {
                all_updates.extend(batch.updates.drain().map(|(_, v)| v));
            }
        }

        if !all_updates.is_empty() {
            let _ = self.actor_handle.send(StateMessage::BatchedGeometryUpdates(all_updates));
        }
    }

    /// Tracks a window for destroy detection.
    ///
    /// This should be called for windows that are tracked at startup via
    /// `BatchWindowsCreated`, since those bypass the normal `on_window_created` path.
    pub fn track_window_for_destroy_detection(&self, window_id: u32, pid: i32) {
        self.pid_windows.lock().entry(pid).or_default().insert(window_id);
    }

    /// Tracks multiple windows for destroy detection.
    ///
    /// This is the batch version of `track_window_for_destroy_detection`.
    #[allow(clippy::significant_drop_tightening)]
    pub fn track_windows_for_destroy_detection(&self, windows: &[(u32, i32)]) {
        let mut pid_windows = self.pid_windows.lock();
        for (window_id, pid) in windows {
            pid_windows.entry(*pid).or_default().insert(*window_id);
        }
        log::debug!(
            "tiling: tracked {} windows for destroy detection ({} PIDs)",
            windows.len(),
            pid_windows.len()
        );
    }

    /// Get registered screen count.
    #[must_use]
    pub fn screen_count(&self) -> usize { self.screen_batches.lock().len() }

    /// Get the batch interval for a specific screen.
    #[must_use]
    pub fn batch_interval_for_screen(&self, screen_id: u32) -> Option<Duration> {
        self.screen_batches.lock().get(&screen_id).map(ScreenBatch::batch_interval)
    }
}

impl Drop for EventProcessor {
    fn drop(&mut self) { self.stop(); }
}

// ============================================================================
// Refresh Rate Detection
// ============================================================================

/// Get the refresh rate of a specific display.
///
/// Falls back to 60 Hz if detection fails.
#[must_use]
pub fn get_display_refresh_rate(display_id: u32) -> f64 {
    use core_graphics::display::CGDisplay;

    let display = CGDisplay::new(display_id);
    let Some(mode) = display.display_mode() else {
        log::debug!("Display {display_id} has no display mode, using default");
        return DEFAULT_REFRESH_RATE;
    };

    let rate = mode.refresh_rate();

    // Some displays (especially LCD panels) report 0
    if rate <= 0.0 {
        log::debug!("Display {display_id} reported 0 Hz refresh rate, using default");
        return DEFAULT_REFRESH_RATE;
    }

    log::debug!("Display {display_id} refresh rate: {rate} Hz");

    rate
}

/// Get the refresh rate of the main display.
#[must_use]
pub fn get_main_display_refresh_rate() -> f64 {
    use core_graphics::display::CGDisplay;
    get_display_refresh_rate(CGDisplay::main().id)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::actor::StateActor;

    #[tokio::test]
    async fn test_processor_creation() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        assert!(!processor.is_running());
        assert_eq!(processor.pending_geometry_count(), 0);
        assert_eq!(processor.screen_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_screen_registration() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        // Register screens with different refresh rates
        processor.register_screen(1, 60.0);
        processor.register_screen(2, 144.0);
        processor.register_screen(3, 240.0);

        assert_eq!(processor.screen_count(), 3);

        // Check batch intervals
        let interval_60 = processor.batch_interval_for_screen(1).unwrap();
        let interval_144 = processor.batch_interval_for_screen(2).unwrap();
        let interval_240 = processor.batch_interval_for_screen(3).unwrap();

        assert!(interval_60 > interval_144);
        assert!(interval_144 > interval_240);

        // 60 Hz ≈ 16.67ms
        assert!(interval_60.as_millis() >= 16 && interval_60.as_millis() <= 17);
        // 144 Hz ≈ 6.94ms
        assert!(interval_144.as_millis() >= 6 && interval_144.as_millis() <= 7);
        // 240 Hz ≈ 4.17ms
        assert!(interval_240.as_millis() >= 4 && interval_240.as_millis() <= 5);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_window_screen_assignment() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.register_screen(2, 144.0);

        // Assign windows to screens
        processor.set_window_screen(100, 1);
        processor.set_window_screen(200, 2);

        // Queue geometry events
        processor.on_window_moved(100, Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_moved(200, Rect::new(0.0, 0.0, 100.0, 100.0));

        // Check per-screen pending counts
        assert_eq!(processor.pending_geometry_count_for_screen(1), 1);
        assert_eq!(processor.pending_geometry_count_for_screen(2), 1);
        assert_eq!(processor.pending_geometry_count(), 2);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_geometry_batching_per_screen() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(100, 1);

        // Queue multiple moves for the same window
        processor.on_window_moved(100, Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_moved(100, Rect::new(10.0, 10.0, 100.0, 100.0));
        processor.on_window_moved(100, Rect::new(20.0, 20.0, 100.0, 100.0));

        // Should only have 1 pending update (coalesced)
        assert_eq!(processor.pending_geometry_count(), 1);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_window_destroyed_clears_batch() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(100, 1);

        processor.on_window_moved(100, Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(processor.pending_geometry_count(), 1);

        processor.on_window_destroyed(100);
        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_screen_unregistration_flushes() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(100, 1);
        processor.on_window_moved(100, Rect::new(0.0, 0.0, 100.0, 100.0));

        assert_eq!(processor.pending_geometry_count(), 1);

        // Unregistering screen should flush its pending updates
        processor.unregister_screen(1);

        assert_eq!(processor.screen_count(), 0);
        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_no_screens_dispatches_immediately() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        // No screens registered - should dispatch immediately
        processor.on_window_moved(100, Rect::new(0.0, 0.0, 100.0, 100.0));

        // Should not be batched (no screens to batch to)
        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_processor_start_stop() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.register_screen(2, 144.0);

        assert!(!processor.is_running());

        processor.start();
        assert!(processor.is_running());

        // Wait a bit for timers to tick
        tokio::time::sleep(Duration::from_millis(50)).await;

        processor.stop();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!processor.is_running());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_flush_all() {
        let handle = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.register_screen(2, 144.0);
        processor.set_window_screen(100, 1);
        processor.set_window_screen(200, 2);

        processor.on_window_moved(100, Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_moved(200, Rect::new(0.0, 0.0, 100.0, 100.0));

        assert_eq!(processor.pending_geometry_count(), 2);

        processor.flush_all();

        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }
}
