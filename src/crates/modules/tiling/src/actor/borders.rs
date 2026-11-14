//! The actor's half of the native borders.
//!
//! The [`BorderEngine`] lives on the actor thread. After each applied input
//! the actor computes the pure [`border_model`] and updates the engine *before*
//! it dispatches that input's effects, so a window leaving the visible
//! workspace is hidden before its frame write is queued. Nothing here waits:
//! the engine sends to the overlay control without blocking and only reads
//! acknowledgements that already arrived. A border problem is a diagnostic; it
//! never changes Tiling's health and never produces an input.
//!
//! # Stopping
//!
//! Disabling the borders (a reload), the end of the actor and a panic in the
//! engine (unwind-enabled builds only; release aborts) all run the same ordered
//! retire: stop the live-follow owner, stop the Spaces owner (typed outcomes),
//! retire the engine's handle ledger, ask the control to `release_all` and
//! record the ticket in the shared [`BorderLifecycle`]. The actor never waits
//! for the main thread: the stop (off main) or a later turn reads the
//! acknowledgement.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::ApplicationId;
use stache_macos::border_overlay::StopOutcome;
use stache_macos::windows::WindowInstance;

use crate::borders::{BorderDiagnostics, BorderEngine, BorderLifecycle, border_model};
use crate::config::{BordersConfig, TilingConfig};
use crate::state::TilingState;

/// The border notes the status surface reads (messages, newest published by the
/// actor). A plain shared list: reading it never touches the engine.
pub type BorderNotes = Arc<Mutex<Vec<String>>>;

pub(super) struct BorderSide {
    engine: BorderEngine,
    notes: BorderNotes,
    /// The configuration the engine was last configured from.
    seen: Option<Arc<TilingConfig>>,
    enabled: bool,
    /// The focused window last seen, to re-order on a focus change.
    last_focus: Option<WindowInstance>,
    lifecycle: Arc<BorderLifecycle>,
    /// The borders were enabled at least once: overlays may exist.
    engaged: bool,
    /// A release was requested and nothing was shown since.
    released: bool,
    /// The newest time the actor handed in.
    last_now: Instant,
    /// The engine panicked; borders stay off for the rest of the run.
    fault: Option<String>,
    /// Owners whose last stop timed out (retried by the next stop).
    stuck_owners: Vec<&'static str>,
    /// Diagnostics of a pending or mismatched release.
    release_notes: Vec<String>,
    /// When a pending release becomes late.
    release_due: Option<Instant>,
}

/// The diagnostic for an engine that panicked.
pub const ENGINE_FAULT: &str =
    "borders: the border engine failed (panic); borders are off until the next restart";

impl BorderSide {
    pub(super) fn new(
        engine: BorderEngine,
        notes: BorderNotes,
        lifecycle: Arc<BorderLifecycle>,
    ) -> Self {
        lifecycle.attach();
        Self {
            engine,
            notes,
            seen: None,
            enabled: false,
            last_focus: None,
            lifecycle,
            engaged: false,
            released: false,
            last_now: Instant::now(),
            fault: None,
            stuck_owners: Vec::new(),
            release_notes: Vec::new(),
            release_due: None,
        }
    }

    /// Runs `f` unless the engine already failed; a panic turns the borders off
    /// (owners stopped, overlays released) and is reported, never propagated: the
    /// Tiling state machine keeps running (unwind-enabled builds only; release aborts).
    fn guarded<R>(&mut self, default: R, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.fault.is_some() {
            return default;
        }
        if let Ok(value) = catch_unwind(AssertUnwindSafe(|| f(self))) {
            return value;
        }
        self.fault = Some(ENGINE_FAULT.to_owned());
        self.enabled = false;
        self.last_focus = None;
        // The release is enqueued now and runs on the next main-thread turn.
        let _ = catch_unwind(AssertUnwindSafe(|| self.retire()));
        default
    }

    /// The ordered retire: owners first, then the ledger, then the release of
    /// every overlay on main. Never waits.
    fn retire(&mut self) -> (StopOutcome, StopOutcome) {
        let live = self.engine.stop_live();
        let spaces = self.engine.stop_spaces();
        self.engine.route().log().debug(|| {
            format!(
                "retire owners stopped live={live:?} spaces={spaces:?} engaged={}",
                self.engaged
            )
        });
        self.stuck_owners.clear();
        if matches!(live, StopOutcome::TimedOut { .. }) {
            self.stuck_owners.push("live follow owner");
        }
        if matches!(spaces, StopOutcome::TimedOut { .. }) {
            self.stuck_owners.push("spaces owner");
        }
        if self.engaged && !self.released {
            let expected = self.engine.retire_ledger();
            let ticket = self.engine.route().control().release_all();
            self.lifecycle.request_release(expected, ticket, self.last_now);
            self.released = true;
        }
        (live, spaces)
    }

    /// The final hand-over to the lifecycle (the actor is ending).
    fn finish_run(&mut self) {
        let stuck = catch_unwind(AssertUnwindSafe(|| {
            let (live, spaces) = self.retire();
            self.engine.take_stuck(live, spaces)
        }))
        .unwrap_or_default();
        self.lifecycle.finish(stuck);
    }

    /// Follows the accepted configuration: enabled starts borders, disabled
    /// releases every overlay, anything else is recomputed in place.
    fn follow_config(&mut self, config: Option<&Arc<TilingConfig>>, now: Instant) {
        let Some(config) = config else { return };
        if self.seen.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, config)) {
            return;
        }
        self.seen = Some(Arc::clone(config));
        let borders: &BordersConfig = &config.borders;
        if borders.enabled {
            if self.enabled {
                if borders != self.engine.config() {
                    self.engine.configure(borders);
                }
            } else {
                // Starting: the first model after this shows the borders.
                self.engine.configure(borders);
                self.enabled = true;
                self.engaged = true;
                self.released = false;
                self.stuck_owners.clear();
                self.last_now = now;
                // Registers nothing until here; borders stay hidden until the
                // source reports that it is registered.
                self.engine.start_spaces();
                // Likewise for live follow: its owner registers on its own thread
                // and reports a status; until it is active the fallback applies.
                self.engine.start_live();
            }
        } else if self.enabled {
            self.enabled = false;
            self.last_focus = None;
            // The sources stop before the overlays are released; the release is
            // fire and forget for the actor (the ticket goes to the lifecycle).
            self.last_now = now;
            let _ = self.retire();
            self.engine.configure(borders);
        }
    }

    /// Diffs the new state into the overlays. Call before dispatching the
    /// input's effects.
    pub(super) fn update(&mut self, now: Instant, state: &TilingState) {
        self.guarded((), |side| {
            side.follow_config(state.config.as_ref(), now);
            if !side.enabled {
                return;
            }
            side.engine.poll_spaces(now);
            let model = border_model(state, side.engine.config());
            side.engine.update(now, model);
            side.engine.reorder_raised(now);
            let focus = state.focus.window;
            if focus != side.last_focus {
                side.last_focus = focus;
                // One `Order` for the border of the window that took the focus.
                side.engine.note_focus(now, focus);
            }
        });
    }

    /// Drains the Space and Mission Control facts into the gate. Called at the
    /// start of every turn, before any border update.
    pub(super) fn poll_spaces(&mut self, now: Instant) {
        self.guarded((), |side| {
            if side.enabled {
                side.engine.poll_spaces(now);
            }
        });
    }

    /// Wakes the actor when a source queues a Space fact.
    pub(super) fn set_space_waker(&self, waker: stache_macos::border_overlay::SpaceWaker) {
        self.engine.set_space_waker(Some(waker));
    }

    /// Wakes the actor when live follow queued a note (hide, unhide, reorder,
    /// close) for the engine.
    pub(super) fn set_live_waker(&self, waker: crate::borders::NoteWaker) {
        self.engine.set_live_waker(Some(waker));
    }

    /// Shows the borders whose display frame has passed.
    pub(super) fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.last_now = now;
        let polled = self.lifecycle.poll(now);
        self.release_notes = polled.notes;
        self.release_due = polled.deadline;
        let engine_due = self.guarded(None, |side| {
            if !side.enabled {
                return None;
            }
            side.engine.poll_spaces(now);
            side.engine.reorder_raised(now);
            side.engine.tick(now);
            side.engine.next_deadline()
        });
        self.publish_notes();
        // Idle (nothing pending, nothing moving, nothing unacknowledged)
        // schedules no deadline at all: zero wakeups.
        [engine_due, self.release_due].into_iter().flatten().min()
    }

    /// Publishes the diagnostics when they changed. Disabled borders report
    /// nothing.
    fn publish_notes(&self) {
        let mut messages = if self.enabled && self.fault.is_none() {
            self.diagnostics().messages()
        } else {
            Vec::new()
        };
        messages.extend(self.fault.clone());
        for name in &self.stuck_owners {
            messages.push(format!(
                "borders: the {name} did not stop; it is retried by the next stop"
            ));
        }
        for note in &self.release_notes {
            if !messages.contains(note) {
                messages.push(note.clone());
            }
        }
        let mut notes = self.notes.lock().unwrap_or_else(PoisonError::into_inner);
        if *notes != messages {
            *notes = messages;
        }
    }

    pub(super) fn application_shown(&mut self, now: Instant, application: ApplicationId) {
        self.guarded((), |side| side.engine.note_application_shown(now, application));
    }

    pub(super) fn application_hidden(&mut self, application: ApplicationId) {
        // The returned acknowledgement is not awaited on the actor thread.
        self.guarded((), |side| drop(side.engine.hide_application(application)));
    }

    pub(super) const fn set_frame_period(&mut self, period: Duration) {
        self.engine.set_frame_period(period);
    }

    pub(super) fn diagnostics(&self) -> BorderDiagnostics { self.engine.diagnostics() }
}

impl Drop for BorderSide {
    fn drop(&mut self) {
        // No source callback may outlive the engine; what cannot stop is handed
        // to the lifecycle, never dropped while stuck.
        self.finish_run();
    }
}
