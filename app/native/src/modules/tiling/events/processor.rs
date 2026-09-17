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
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::modules::tiling::actor::{
    GeometryUpdate, GeometryUpdateType, StateActorHandle, StateMessage, WindowCreatedInfo,
};
use crate::modules::tiling::identity::{AppIdentity, WindowTarget};
use crate::modules::tiling::state::{Rect, Screen};

/// Default refresh rate if detection fails (60 Hz).
const DEFAULT_REFRESH_RATE: f64 = 60.0;

/// Minimum refresh rate to prevent too-fast batching.
const MIN_REFRESH_RATE: f64 = 30.0;

/// Maximum refresh rate to prevent too-slow batching.
const MAX_REFRESH_RATE: f64 = 360.0;

/// Differences below this are display-mode rounding noise, not a retiming request.
const REFRESH_RATE_EPSILON: f64 = 0.001;

/// Repeatable, resettable completion signal used to acknowledge that a screen
/// batch timer task has fully exited its loop.
#[derive(Default)]
struct TimerCompletion {
    done: parking_lot::Mutex<bool>,
    cv: parking_lot::Condvar,
}

impl TimerCompletion {
    fn reset(&self) { *self.done.lock() = false; }

    fn mark(&self) {
        *self.done.lock() = true;
        self.cv.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let mut done = self.done.lock();
        if !*done {
            self.cv.wait_for(&mut done, timeout);
        }
        *done
    }
}

/// A batch queue for a single screen.
struct ScreenBatch {
    /// Screen ID (`CGDirectDisplayID`).
    #[allow(dead_code)] // Stored for potential debugging use
    screen_id: u32,

    /// Refresh rate in Hz.
    refresh_rate: f64,

    /// Full display frame used to route geometry without querying CoreGraphics.
    frame: Rect,

    /// Pending geometry updates for windows on this screen.
    updates: HashMap<(AppIdentity, u32), GeometryUpdate>,

    /// Whether the timer for this screen is running.
    timer_running: AtomicBool,

    /// Wakes the timer when its configuration changes or it must stop.
    wakeup: Arc<tokio::sync::Notify>,

    /// Completion acknowledgment for the currently running timer task.
    timer_done: Arc<TimerCompletion>,
}

impl ScreenBatch {
    fn new(screen_id: u32, refresh_rate: f64, frame: Rect) -> Self {
        Self {
            screen_id,
            refresh_rate: refresh_rate.clamp(MIN_REFRESH_RATE, MAX_REFRESH_RATE),
            frame,
            updates: HashMap::new(),
            timer_running: AtomicBool::new(false),
            wakeup: Arc::new(tokio::sync::Notify::new()),
            timer_done: Arc::new(TimerCompletion::default()),
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

    /// (Identity, Window ID) → Screen ID mapping for routing geometry events.
    window_screen_map: Arc<DashMap<(AppIdentity, u32), u32>>,

    /// Last known window frame, used to remap routes after a display change.
    window_frames: Arc<DashMap<(AppIdentity, u32), Rect>>,

    /// Identity → Set of Window IDs mapping for destroy detection.
    /// When we get a destroy event but can't get the window ID, we compare
    /// against current windows from macOS to find which one was destroyed.
    pid_windows: Arc<Mutex<HashMap<AppIdentity, HashSet<u32>>>>,

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
            window_frames: Arc::new(DashMap::new()),
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
                if (batch.refresh_rate - old_rate).abs() > REFRESH_RATE_EPSILON {
                    batch.wakeup.notify_one();
                }
                tracing::debug!(
                    "Updated screen {} refresh rate: {} Hz → {} Hz",
                    screen_id,
                    old_rate,
                    batch.refresh_rate
                );
            }
            return;
        }

        let batch = ScreenBatch::new(screen_id, refresh_rate, Rect::zero());
        tracing::debug!(
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

    /// Reconcile registered geometry batches with an authoritative display snapshot.
    ///
    /// The screen monitor captures this snapshot on the main thread, allowing
    /// geometry callbacks to route using cached frames without native work.
    pub fn reconcile_screens(&self, screens: &[Screen]) {
        if screens.is_empty() {
            tracing::warn!("tiling: ignoring empty screen topology snapshot");
            return;
        }

        let mut screen_iter = screens.iter();
        let Some(first_screen) = screen_iter.next() else {
            return;
        };
        let screen_ids: HashSet<u32> = screens.iter().map(|screen| screen.id).collect();
        let default_screen = screen_iter
            .filter(|screen| screen.is_main)
            .map(|screen| screen.id)
            .min()
            .unwrap_or(first_screen.id);

        let removed_updates = {
            let mut batches = self.screen_batches.lock();
            for screen in screens {
                let refresh_rate = screen.refresh_rate.clamp(MIN_REFRESH_RATE, MAX_REFRESH_RATE);
                match batches.get_mut(&screen.id) {
                    Some(batch) => {
                        batch.frame = screen.frame;
                        if (batch.refresh_rate - refresh_rate).abs() > REFRESH_RATE_EPSILON {
                            batch.refresh_rate = refresh_rate;
                            batch.wakeup.notify_one();
                        }
                    }
                    None => {
                        batches.insert(
                            screen.id,
                            ScreenBatch::new(screen.id, refresh_rate, screen.frame),
                        );
                    }
                }
            }

            let removed: Vec<u32> = batches
                .keys()
                .copied()
                .filter(|screen_id| !screen_ids.contains(screen_id))
                .collect();
            let mut updates = Vec::new();
            for screen_id in removed {
                if let Some(mut batch) = batches.remove(&screen_id) {
                    batch.timer_running.store(false, Ordering::SeqCst);
                    batch.wakeup.notify_waiters();
                    updates.extend(batch.updates.drain().map(|(_, update)| update));
                }
            }
            updates
        };

        self.default_screen_id.store(default_screen, Ordering::SeqCst);
        for mut entry in self.window_screen_map.iter_mut() {
            if !screen_ids.contains(entry.value()) {
                let key = *entry.key();
                let screen_id = self
                    .window_frames
                    .get(&key)
                    .and_then(|frame| self.screen_for_frame(*frame))
                    .unwrap_or(default_screen);
                *entry.value_mut() = screen_id;
            }
        }

        if !removed_updates.is_empty() {
            let _ = self.actor_handle.send(StateMessage::BatchedGeometryUpdates(removed_updates));
        }

        if self.running.load(Ordering::SeqCst) {
            for screen in screens {
                self.start_screen_timer(screen.id);
            }
        }
    }

    /// Unregister a screen.
    ///
    /// Any pending geometry updates for windows on this screen will be flushed.
    pub fn unregister_screen(&self, screen_id: u32) {
        let (updates, was_registered, new_default) = {
            let mut batches = self.screen_batches.lock();
            let removed = batches.remove(&screen_id);
            let was_registered = removed.is_some();
            let updates = removed.map_or_else(Vec::new, |mut batch| {
                batch.timer_running.store(false, Ordering::SeqCst);
                batch.wakeup.notify_waiters();
                batch.updates.drain().map(|(_, update)| update).collect()
            });
            let new_default = (self.default_screen_id.load(Ordering::SeqCst) == screen_id)
                .then(|| batches.keys().next().copied().unwrap_or_default());

            drop(batches);
            (updates, was_registered, new_default)
        };

        if let Some(new_default) = new_default {
            self.default_screen_id.store(new_default, Ordering::SeqCst);
        }

        if !updates.is_empty() {
            let _ = self.actor_handle.send(StateMessage::BatchedGeometryUpdates(updates));
        }

        if was_registered {
            tracing::debug!("Unregistered screen {screen_id}");
        }
    }

    /// Set the screen assignment for a window.
    ///
    /// Call this when a window is created or moves to a different screen.
    pub fn set_window_screen(&self, identity: AppIdentity, window_id: u32, screen_id: u32) {
        self.window_screen_map.insert((identity, window_id), screen_id);
    }

    /// Remove the screen assignment for a window.
    pub fn remove_window(&self, identity: AppIdentity, window_id: u32) {
        self.window_screen_map.remove(&(identity, window_id));
        self.window_frames.remove(&(identity, window_id));
    }

    /// Get the screen ID for a window.
    fn get_window_screen(&self, identity: AppIdentity, window_id: u32) -> u32 {
        self.window_screen_map
            .get(&(identity, window_id))
            .map_or_else(|| self.default_screen_id.load(Ordering::SeqCst), |entry| *entry)
    }

    fn screen_for_frame(&self, frame: Rect) -> Option<u32> {
        let (x, y) = frame.center();
        self.screen_batches
            .lock()
            .iter()
            .filter(|(_, batch)| batch.frame.is_valid() && batch.frame.contains_point(x, y))
            .map(|(screen_id, _)| *screen_id)
            .min()
    }

    fn route_window_frame(&self, identity: AppIdentity, window_id: u32, frame: Rect) -> u32 {
        let key = (identity, window_id);
        self.window_frames.insert(key, frame);

        let current = self.get_window_screen(identity, window_id);
        let (x, y) = frame.center();
        let remains_assigned = self
            .screen_batches
            .lock()
            .get(&current)
            .is_some_and(|batch| !batch.frame.is_valid() || batch.frame.contains_point(x, y));
        if remains_assigned {
            return current;
        }

        let screen_id = self
            .screen_for_frame(frame)
            .unwrap_or_else(|| self.default_screen_id.load(Ordering::SeqCst));
        self.window_screen_map.insert(key, screen_id);
        if screen_id != current {
            // A timer on the old display may flush after the new display's
            // timer. Move its coalesced entry with the routing assignment so
            // an old frame cannot roll the actor state backwards.
            let mut batches = self.screen_batches.lock();
            let pending = batches.get_mut(&current).and_then(|batch| batch.updates.remove(&key));
            if let Some(update) = pending
                && let Some(batch) = batches.get_mut(&screen_id)
            {
                batch
                    .updates
                    .entry(key)
                    .and_modify(|existing| {
                        // The frame that caused reassignment is newer than
                        // the old queued one; preserve only the combined kind.
                        existing.update_type = match (existing.update_type, update.update_type) {
                            (GeometryUpdateType::MoveResize, _)
                            | (_, GeometryUpdateType::MoveResize)
                            | (GeometryUpdateType::Move, GeometryUpdateType::Resize)
                            | (GeometryUpdateType::Resize, GeometryUpdateType::Move) => {
                                GeometryUpdateType::MoveResize
                            }
                            (kind, _) => kind,
                        };
                    })
                    .or_insert(update);
            }
        }
        screen_id
    }

    /// Start the timer for a specific screen.
    fn start_screen_timer(&self, screen_id: u32) {
        let batches = self.screen_batches.clone();
        let actor_handle = self.actor_handle.clone();
        let running = self.running.clone();

        {
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
                    batch.timer_done.reset();
                }
                None => return,
            }
        }

        tauri::async_runtime::spawn(async move {
            loop {
                let (interval, wakeup) = {
                    let batches = batches.lock();
                    match batches.get(&screen_id) {
                        Some(batch) if batch.timer_running.load(Ordering::SeqCst) => {
                            (batch.batch_interval(), Arc::clone(&batch.wakeup))
                        }
                        _ => break,
                    }
                };

                tokio::select! {
                    () = tokio::time::sleep(interval) => {
                        if !running.load(Ordering::SeqCst) {
                            break;
                        }

                        let updates: Vec<GeometryUpdate> = {
                            let mut batches = batches.lock();
                            match batches.get_mut(&screen_id) {
                                Some(batch) if batch.timer_running.load(Ordering::SeqCst) => {
                                    batch.updates.drain().map(|(_, v)| v).collect()
                                }
                                _ => break,
                            }
                        };

                        if !updates.is_empty() {
                            let _ = actor_handle.send(StateMessage::BatchedGeometryUpdates(updates));
                        }
                    }
                    () = wakeup.notified() => {}
                }
            }

            // Mark timer as stopped
            if let Some(batch) = batches.lock().get(&screen_id) {
                batch.timer_running.store(false, Ordering::SeqCst);
            }
            let timer_done = batches.lock().get(&screen_id).map(|b| Arc::clone(&b.timer_done));
            if let Some(timer_done) = timer_done {
                timer_done.mark();
            }

            tracing::trace!("Batch timer stopped for screen {screen_id}");
        });

        tracing::trace!("Batch timer started for screen {screen_id}");
    }

    /// Stop the batch flush timers.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);

        {
            let batches = self.screen_batches.lock();
            for batch in batches.values() {
                batch.timer_running.store(false, Ordering::SeqCst);
                batch.wakeup.notify_waiters();
            }
        }

        tracing::debug!("EventProcessor stopped");
    }

    /// Atomically reject new events, stop all timers, wait for each timer
    /// task's completion acknowledgment, then clear all pending batches and
    /// routing maps.
    ///
    /// Returns `false` if any timer task failed to acknowledge within
    /// `timeout`; routing maps are still cleared in that case.
    pub fn stop_and_wait(&self, timeout: Duration) -> bool {
        self.running.store(false, Ordering::SeqCst);

        let completions: Vec<Arc<TimerCompletion>> = {
            let mut batches = self.screen_batches.lock();
            batches
                .values_mut()
                .filter_map(|batch| {
                    batch.timer_running.swap(false, Ordering::SeqCst).then(|| {
                        batch.wakeup.notify_waiters();
                        Arc::clone(&batch.timer_done)
                    })
                })
                .collect()
        };

        let deadline = Instant::now() + timeout;
        let all_done = completions.iter().all(|completion| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            completion.wait(remaining)
        });

        if !all_done {
            tracing::warn!("tiling: EventProcessor timers did not stop within {timeout:?}");
        }

        self.clear_routing_maps();
        all_done
    }

    /// Discard pending geometry updates and all routing/tracking maps.
    ///
    /// Idempotent: safe to call when already stopped or on retry.
    fn clear_routing_maps(&self) {
        {
            let mut batches = self.screen_batches.lock();
            for batch in batches.values_mut() {
                batch.updates.clear();
            }
        }
        self.window_screen_map.clear();
        self.window_frames.clear();
        self.pid_windows.lock().clear();
    }

    /// Start the batch flush timers for all registered screens.
    pub fn start(&self) {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::warn!("EventProcessor already running");
            return;
        }

        // Pending updates enqueued before start belong to this generation;
        // stale routes from a prior generation are cleared by stop_and_wait.
        let screen_ids: Vec<u32> = self.screen_batches.lock().keys().copied().collect();
        for screen_id in screen_ids {
            self.start_screen_timer(screen_id);
        }

        tracing::debug!("EventProcessor started");
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
        tracing::trace!("Window created: {window_id:?}");

        // Track this window for destroy detection (identity-keyed).
        let Some(identity) = info.identity else {
            tracing::trace!("tiling: dropping window created event without identity");
            return;
        };
        self.route_window_frame(identity, info.window_id, info.frame);
        self.pid_windows.lock().entry(identity).or_default().insert(info.window_id);

        let _ = self.actor_handle.send(StateMessage::WindowCreated(info));
    }

    /// Dispatch a window destroyed event immediately.
    ///
    /// Also removes any pending geometry updates for this window.
    pub fn on_window_destroyed(&self, window_id: u32, identity: AppIdentity) {
        tracing::debug!("tiling: processor.on_window_destroyed called for window_id={window_id}");

        // Remove from window-screen mapping
        let screen_id = self.window_screen_map.remove(&(identity, window_id)).map(|(_, id)| id);
        self.window_frames.remove(&(identity, window_id));

        // Remove from geometry batch
        if let Some(screen_id) = screen_id
            && let Some(batch) = self.screen_batches.lock().get_mut(&screen_id)
        {
            batch.updates.remove(&(identity, window_id));
        }

        // Remove from pid_windows tracking
        {
            let mut m = self.pid_windows.lock();
            if let Some(set) = m.get_mut(&identity) {
                set.remove(&window_id);
            }
        }

        tracing::debug!(
            "tiling: sending WindowDestroyed message to actor for window_id={window_id}"
        );
        let _ = self.actor_handle.send(StateMessage::WindowDestroyed { window_id, identity });
    }

    /// Handle window destruction when we only know the identity.
    ///
    /// Uses the window element cache to efficiently check which tracked
    /// windows are no longer valid, avoiding expensive AX enumeration.
    pub fn on_window_destroyed_for_identity(&self, identity: AppIdentity) {
        tracing::debug!("tiling: on_window_destroyed_for_identity called for {identity:?}");

        // Get tracked windows for this identity from our local cache
        let tracked: Vec<u32> = self
            .pid_windows
            .lock()
            .get(&identity)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();

        tracing::debug!("tiling: tracked windows for {identity:?}: {tracked:?}");

        if tracked.is_empty() {
            tracing::debug!("tiling: no tracked windows for {identity:?}, nothing to do");
            return;
        }

        // Use window cache to efficiently find invalid windows
        // This uses O(1) validity checks on cached elements where possible
        let cache = crate::modules::tiling::effects::get_window_cache();
        let targets: Vec<_> =
            tracked.iter().map(|&window_id| WindowTarget { identity, window_id }).collect();
        let invalid = cache.find_invalid_windows(&targets);

        tracing::debug!(
            "tiling: found {} invalid windows for {identity:?}",
            invalid.len()
        );

        // Destroy invalid windows
        for window_id in invalid {
            tracing::debug!(
                "tiling: window {window_id} no longer valid for {identity:?}, destroying"
            );
            self.on_window_destroyed(window_id, identity);
        }
    }

    /// Dispatch a window focused event immediately.
    pub fn on_window_focused(&self, window_id: u32, identity: AppIdentity) {
        tracing::debug!("tiling: Window focused event received: {window_id}");
        let _ = self.actor_handle.send(StateMessage::WindowFocused { window_id, identity });
    }

    /// Dispatch a window unfocused event immediately.
    pub fn on_window_unfocused(&self, window_id: u32, identity: AppIdentity) {
        tracing::trace!("Window unfocused: {window_id}");
        let _ = self.actor_handle.send(StateMessage::WindowUnfocused { window_id, identity });
    }

    /// Dispatch a window minimized event immediately.
    pub fn on_window_minimized(&self, window_id: u32, identity: AppIdentity, minimized: bool) {
        tracing::trace!("Window minimized: {window_id} = {minimized}");
        let _ = self.actor_handle.send(StateMessage::WindowMinimized {
            window_id,
            identity,
            minimized,
        });
    }

    /// Dispatch a window title changed event immediately.
    pub fn on_window_title_changed(&self, window_id: u32, identity: AppIdentity, title: String) {
        tracing::trace!("Window title changed: {window_id} = '{title}'");
        let _ =
            self.actor_handle
                .send(StateMessage::WindowTitleChanged { window_id, identity, title });
    }

    /// Dispatch a window fullscreen changed event immediately.
    pub fn on_window_fullscreen_changed(
        &self,
        window_id: u32,
        identity: AppIdentity,
        fullscreen: bool,
    ) {
        tracing::trace!("Window fullscreen changed: {window_id} = {fullscreen}");
        let _ = self.actor_handle.send(StateMessage::WindowFullscreenChanged {
            window_id,
            identity,
            fullscreen,
        });
    }

    // ========================================================================
    // Batched Dispatch (geometry events)
    // ========================================================================

    /// Queue a window moved event for batched dispatch.
    ///
    /// The event is routed to the appropriate screen's batch queue.
    pub fn on_window_moved(&self, window_id: u32, identity: AppIdentity, frame: Rect) {
        let screen_id = self.route_window_frame(identity, window_id, frame);
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
                    .entry((identity, window_id))
                    .and_modify(|e| {
                        e.frame = frame;
                        e.update_type = match e.update_type {
                            GeometryUpdateType::Resize | GeometryUpdateType::MoveResize => {
                                GeometryUpdateType::MoveResize
                            }
                            GeometryUpdateType::Move => GeometryUpdateType::Move,
                        };
                    })
                    .or_insert(GeometryUpdate {
                        window_id,
                        identity,
                        frame,
                        update_type: GeometryUpdateType::Move,
                    });
            }
        } else {
            // No screens registered, dispatch immediately
            drop(batches);
            let _ =
                self.actor_handle.send(StateMessage::WindowMoved { window_id, identity, frame });
        }
    }

    /// Queue a window resized event for batched dispatch.
    ///
    /// The event is routed to the appropriate screen's batch queue.
    pub fn on_window_resized(&self, window_id: u32, identity: AppIdentity, frame: Rect) {
        let screen_id = self.route_window_frame(identity, window_id, frame);
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
                    .entry((identity, window_id))
                    .and_modify(|e| {
                        e.frame = frame;
                        e.update_type = match e.update_type {
                            GeometryUpdateType::Move | GeometryUpdateType::MoveResize => {
                                GeometryUpdateType::MoveResize
                            }
                            GeometryUpdateType::Resize => GeometryUpdateType::Resize,
                        };
                    })
                    .or_insert(GeometryUpdate {
                        window_id,
                        identity,
                        frame,
                        update_type: GeometryUpdateType::Resize,
                    });
            }
        } else {
            drop(batches);
            let _ =
                self.actor_handle
                    .send(StateMessage::WindowResized { window_id, identity, frame });
        }
    }

    // ========================================================================
    // App Events (immediate dispatch)
    // ========================================================================

    /// Dispatch an app launched event.
    pub fn on_app_launched(
        &self,
        identity: AppIdentity,
        pid: i32,
        bundle_id: String,
        name: String,
    ) {
        tracing::trace!("App launched: pid={pid}, bundle={bundle_id}");
        let _ =
            self.actor_handle
                .send(StateMessage::AppLaunched { identity, pid, bundle_id, name });
    }

    /// Dispatch an app terminated event.
    ///
    /// No pre-actor forget: the actor removes exact ownership on revalidation.
    pub fn on_app_terminated(&self, identity: AppIdentity, pid: i32) {
        tracing::trace!("App terminated: identity={identity:?}, pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppTerminated { identity, pid });
    }

    /// Dispatch an app hidden event.
    pub fn on_app_hidden(&self, identity: AppIdentity, pid: i32) {
        tracing::trace!("App hidden: identity={identity:?}, pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppHidden { identity, pid });
    }

    /// Dispatch an app shown event (raw forwarding — the actor revalidates).
    pub fn on_app_shown(&self, identity: AppIdentity, pid: i32) {
        tracing::trace!("App shown: identity={identity:?}, pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppShown { identity, pid });
    }

    /// Dispatch an app activated event.
    pub fn on_app_activated(&self, identity: AppIdentity, pid: i32) {
        tracing::trace!("App activated: pid={pid}");
        let _ = self.actor_handle.send(StateMessage::AppActivated { identity, pid });
    }

    // ========================================================================
    // Screen Events (immediate dispatch)
    // ========================================================================

    /// Dispatch a screens changed event.
    ///
    /// NOTE: This sends `ScreensChanged` which requires the actor to call macOS APIs.
    /// Prefer `on_set_screens` with pre-detected screens when possible.
    pub fn on_screens_changed(&self) {
        tracing::trace!("Screens changed");
        let _ = self.actor_handle.send(StateMessage::ScreensChanged);
    }

    /// Dispatch screens with pre-detected screen data.
    ///
    /// This is the preferred method when screens have been detected on the main thread.
    pub fn on_set_screens(&self, screens: Vec<crate::modules::tiling::state::Screen>) {
        tracing::trace!("Setting {} screens", screens.len());
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
    pub fn track_window_for_destroy_detection(&self, window_id: u32, identity: AppIdentity) {
        self.pid_windows.lock().entry(identity).or_default().insert(window_id);
    }

    /// Tracks multiple windows for destroy detection.
    ///
    /// This is the batch version of `track_window_for_destroy_detection`.
    #[allow(clippy::significant_drop_tightening)]
    pub fn track_windows_for_destroy_detection(&self, windows: &[(u32, AppIdentity, Rect)]) {
        let mut pid_windows = self.pid_windows.lock();
        for (window_id, identity, frame) in windows {
            self.route_window_frame(*identity, *window_id, *frame);
            pid_windows.entry(*identity).or_default().insert(*window_id);
        }
        tracing::debug!(
            "tiling: tracked {} windows for destroy detection ({} identities)",
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
        tracing::debug!("Display {display_id} has no display mode, using default");
        return DEFAULT_REFRESH_RATE;
    };

    let rate = mode.refresh_rate();

    // Some displays (especially LCD panels) report 0
    if rate <= 0.0 {
        tracing::debug!("Display {display_id} reported 0 Hz refresh rate, using default");
        return DEFAULT_REFRESH_RATE;
    }

    tracing::debug!("Display {display_id} refresh rate: {rate} Hz");

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
    use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};
    use crate::modules::tiling::state::Screen;

    fn test_identity() -> AppIdentity {
        AppIdentity {
            pid: 1000,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(1.0).unwrap(),
        }
    }

    fn screen(id: u32, x: f64, refresh_rate: f64, is_main: bool) -> Screen {
        Screen {
            id,
            name: format!("Screen {id}"),
            frame: Rect::new(x, 0.0, 1_000.0, 800.0),
            visible_frame: Rect::new(x, 0.0, 1_000.0, 800.0),
            scale_factor: 1.0,
            is_main,
            is_builtin: false,
            refresh_rate,
        }
    }

    fn window_info(identity: AppIdentity, window_id: u32, frame: Rect) -> WindowCreatedInfo {
        WindowCreatedInfo {
            window_id,
            pid: identity.pid,
            identity: Some(identity),
            app_id: "com.example.Test".to_string(),
            app_name: "Test".to_string(),
            title: "Test window".to_string(),
            frame,
            is_minimized: false,
            is_fullscreen: false,
            minimum_size: None,
            tab_group_id: None,
            is_active_tab: true,
        }
    }

    #[tokio::test]
    async fn test_processor_creation() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        assert!(!processor.is_running());
        assert_eq!(processor.pending_geometry_count(), 0);
        assert_eq!(processor.screen_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_screen_registration() {
        let (handle, _stopped) = StateActor::spawn();
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
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.register_screen(2, 144.0);

        // Assign windows to screens
        processor.set_window_screen(test_identity(), 100, 1);
        processor.set_window_screen(test_identity(), 200, 2);

        // Queue geometry events
        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_moved(200, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));

        // Check per-screen pending counts
        assert_eq!(processor.pending_geometry_count_for_screen(1), 1);
        assert_eq!(processor.pending_geometry_count_for_screen(2), 1);
        assert_eq!(processor.pending_geometry_count(), 2);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_window_admission_routes_geometry_to_its_display() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());
        let identity = test_identity();

        processor
            .reconcile_screens(&[screen(1, 0.0, 60.0, true), screen(2, 1_000.0, 144.0, false)]);
        processor.on_window_created(window_info(
            identity,
            100,
            Rect::new(1_100.0, 100.0, 500.0, 500.0),
        ));
        processor.on_window_moved(100, identity, Rect::new(1_120.0, 100.0, 500.0, 500.0));

        assert_eq!(processor.pending_geometry_count_for_screen(1), 0);
        assert_eq!(processor.pending_geometry_count_for_screen(2), 1);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn moving_between_displays_moves_the_pending_geometry_entry() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());
        let identity = test_identity();

        processor
            .reconcile_screens(&[screen(1, 0.0, 60.0, true), screen(2, 1_000.0, 144.0, false)]);
        processor.on_window_moved(100, identity, Rect::new(100.0, 100.0, 500.0, 500.0));
        processor.on_window_moved(100, identity, Rect::new(1_100.0, 100.0, 500.0, 500.0));

        assert_eq!(processor.pending_geometry_count_for_screen(1), 0);
        assert_eq!(processor.pending_geometry_count_for_screen(2), 1);
        let frame = processor
            .screen_batches
            .lock()
            .get(&2)
            .and_then(|batch| batch.updates.get(&(identity, 100)))
            .map(|update| update.frame);
        assert_eq!(frame, Some(Rect::new(1_100.0, 100.0, 500.0, 500.0)));

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_screen_reconciliation_rearms_running_timer_for_new_refresh_rate() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());
        let identity = test_identity();

        processor.reconcile_screens(&[screen(1, 0.0, 30.0, true)]);
        processor.start();
        tokio::time::sleep(Duration::from_millis(5)).await;
        processor.set_window_screen(identity, 100, 1);
        processor.on_window_moved(100, identity, Rect::new(100.0, 100.0, 500.0, 500.0));
        processor.reconcile_screens(&[screen(1, 0.0, 360.0, true)]);

        tokio::time::timeout(Duration::from_millis(20), async {
            while processor.pending_geometry_count() != 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the retimed batch timer must flush using the new interval");

        assert!(processor.stop_and_wait(Duration::from_secs(1)));
        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_removed_screen_remaps_tracked_window_to_remaining_display() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());
        let identity = test_identity();

        processor
            .reconcile_screens(&[screen(1, 0.0, 60.0, true), screen(2, 1_000.0, 144.0, false)]);
        processor.on_window_created(window_info(
            identity,
            100,
            Rect::new(1_100.0, 100.0, 500.0, 500.0),
        ));
        processor.on_window_moved(100, identity, Rect::new(1_120.0, 100.0, 500.0, 500.0));

        processor.reconcile_screens(&[screen(1, 0.0, 60.0, true)]);
        processor.on_window_moved(100, identity, Rect::new(120.0, 100.0, 500.0, 500.0));

        assert_eq!(processor.screen_count(), 1);
        assert_eq!(processor.pending_geometry_count_for_screen(1), 1);
        assert_eq!(processor.pending_geometry_count_for_screen(2), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_geometry_batching_per_screen() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(test_identity(), 100, 1);

        // Queue multiple moves for the same window
        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_moved(100, test_identity(), Rect::new(10.0, 10.0, 100.0, 100.0));
        processor.on_window_moved(100, test_identity(), Rect::new(20.0, 20.0, 100.0, 100.0));

        // Should only have 1 pending update (coalesced)
        assert_eq!(processor.pending_geometry_count(), 1);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_window_destroyed_clears_batch() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(test_identity(), 100, 1);

        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(processor.pending_geometry_count(), 1);

        processor.on_window_destroyed(100, test_identity());
        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_screen_unregistration_flushes() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(test_identity(), 100, 1);
        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));

        assert_eq!(processor.pending_geometry_count(), 1);

        // Unregistering screen should flush its pending updates
        processor.unregister_screen(1);

        assert_eq!(processor.screen_count(), 0);
        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_screen_unregistration_without_updates_does_not_deadlock() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = std::sync::Arc::new(EventProcessor::new(handle.clone()));
        processor.register_screen(1, 60.0);

        let processor_for_unregister = std::sync::Arc::clone(&processor);
        let completion = tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || processor_for_unregister.unregister_screen(1)),
        )
        .await
        .expect("unregistering an empty batch must not block");
        completion.expect("unregister task must not panic");

        assert_eq!(processor.screen_count(), 0);
        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_unknown_screen_unregistration_does_not_deadlock() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = std::sync::Arc::new(EventProcessor::new(handle.clone()));

        let processor_for_unregister = std::sync::Arc::clone(&processor);
        let completion = tokio::time::timeout(
            Duration::from_secs(1),
            tokio::task::spawn_blocking(move || processor_for_unregister.unregister_screen(99)),
        )
        .await
        .expect("unregistering an unknown screen must not block");
        completion.expect("unregister task must not panic");

        assert_eq!(processor.screen_count(), 0);
        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_geometry_coalescing_keeps_move_resize_classification() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());
        let identity = test_identity();
        processor.register_screen(1, 60.0);
        processor.set_window_screen(identity, 100, 1);

        processor.on_window_resized(100, identity, Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_moved(100, identity, Rect::new(10.0, 20.0, 120.0, 100.0));
        processor.on_window_moved(100, identity, Rect::new(20.0, 30.0, 120.0, 100.0));
        processor.on_window_moved(101, identity, Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_resized(101, identity, Rect::new(0.0, 0.0, 120.0, 100.0));
        processor.on_window_resized(101, identity, Rect::new(0.0, 0.0, 140.0, 100.0));

        let update_types: Vec<GeometryUpdateType> = processor
            .screen_batches
            .lock()
            .get(&1)
            .map(|batch| {
                [100, 101]
                    .into_iter()
                    .filter_map(|window_id| {
                        batch.updates.get(&(identity, window_id)).map(|update| update.update_type)
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(update_types, vec![GeometryUpdateType::MoveResize; 2]);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_no_screens_dispatches_immediately() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        // No screens registered - should dispatch immediately
        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));

        // Should not be batched (no screens to batch to)
        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_processor_start_stop() {
        let (handle, _stopped) = StateActor::spawn();
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
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.register_screen(2, 144.0);
        processor.set_window_screen(test_identity(), 100, 1);
        processor.set_window_screen(test_identity(), 200, 2);

        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.on_window_moved(200, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));

        assert_eq!(processor.pending_geometry_count(), 2);

        processor.flush_all();

        assert_eq!(processor.pending_geometry_count(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_stop_and_wait_clears_routes_and_batches() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(test_identity(), 100, 1);
        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));
        processor.start();
        assert!(processor.is_running());
        assert_eq!(processor.pending_geometry_count(), 1);

        let stopped = processor.stop_and_wait(Duration::from_secs(1));
        assert!(stopped, "timer should acknowledge completion");
        assert!(!processor.is_running());
        assert_eq!(processor.pending_geometry_count(), 0);
        assert_eq!(processor.window_screen_map.len(), 0);
        assert_eq!(processor.pid_windows.lock().len(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_stop_and_wait_when_not_running_is_idempotent() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(test_identity(), 100, 1);
        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));

        // Never started: no timer tasks exist, so nothing to wait on.
        let stopped = processor.stop_and_wait(Duration::from_millis(100));
        assert!(stopped);
        assert_eq!(processor.pending_geometry_count(), 0);
        assert_eq!(processor.window_screen_map.len(), 0);

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_start_discards_stale_routes() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = EventProcessor::new(handle.clone());

        processor.register_screen(1, 60.0);
        processor.set_window_screen(test_identity(), 100, 1);
        processor.on_window_moved(100, test_identity(), Rect::new(0.0, 0.0, 100.0, 100.0));

        let _ = processor.stop_and_wait(Duration::from_secs(1));
        assert_eq!(processor.pending_geometry_count(), 0);

        // Restart must not resurrect the stale move.
        processor.start();
        assert_eq!(processor.pending_geometry_count(), 0);
        assert_eq!(processor.window_screen_map.len(), 0);

        handle.shutdown().unwrap();
    }
}
