//! Concise structured logging for the actor, through the host logger the module
//! was given.
//!
//! Everything here is bounded: phase, fault, failed-application, no-frame and
//! intent lines are written once per change, never per event; high-frequency
//! detail (frame outcomes, lifecycle) stays at debug. The actor is the only
//! caller, so no locking is needed.

use std::collections::{BTreeMap, BTreeSet};

use stache_macos::applications::LifecycleKind;
use stache_macos::windows::{IssuedKind, WindowActionOutcome};
use stache_macos::{ApplicationId, DisplayId};
use stache_sdk::ScopedLogger;

use crate::logclock::LogOrigin;
use crate::state::{CompletionResult, Input, NoFrameReason, Phase, TilingState};
use crate::visibility::Planned;

/// What the actor last reported, so only changes are logged.
#[derive(Default)]
pub struct Diag {
    logger: Option<ScopedLogger>,
    origin: LogOrigin,
    phase: Option<Phase>,
    faults: Vec<String>,
    failed: BTreeMap<ApplicationId, String>,
    truncated: BTreeSet<ApplicationId>,
    no_frame: BTreeMap<DisplayId, NoFrameReason>,
    intent_sequence: u64,
    /// The application last reported kept on screen.
    kept_last: std::cell::Cell<Option<ApplicationId>>,
}

/// `Name (bundle.id)` of a known application.
fn label(state: &TilingState, application: ApplicationId) -> String {
    state.apps.get(&application).map_or_else(
        || format!("{application:?}"),
        |record| format!("{} ({})", record.info.name, record.info.bundle_id),
    )
}

impl Diag {
    /// A reporter writing through `logger` (silent without one).
    #[must_use]
    pub fn new(logger: Option<ScopedLogger>, origin: LogOrigin) -> Self {
        Self {
            logger,
            origin,
            ..Self::default()
        }
    }

    /// Logs the inputs worth a line before the transition consumes them.
    pub fn input(&self, state: &TilingState, input: &Input) {
        let Some(logger) = &self.logger else { return };
        match input {
            Input::Completion {
                result: CompletionResult::Frame { window, outcome, .. },
                ..
            } => {
                let applied = matches!(outcome, WindowActionOutcome::Issued {
                    kind: IssuedKind::Applied,
                    ..
                });
                if !applied {
                    logger.debug(format!(
                        "{} tiling: frame write for {} not applied: {outcome:?}",
                        self.origin.stamp(),
                        label(state, window.application())
                    ));
                }
            }
            Input::Lifecycle {
                kind: LifecycleKind::SpaceChanged,
                ..
            } => {
                logger.debug(format!("{} tiling: active Space changed", self.origin.stamp()));
            }
            Input::Lifecycle { kind, info } => {
                // Only a launch carries a description; the other events name the
                // application the registry already knows.
                let who = if info.name.is_empty() && info.bundle_id.is_empty() {
                    label(state, info.application)
                } else {
                    format!("{} ({})", info.name, info.bundle_id)
                };
                logger.debug(format!(
                    "{} tiling: application {kind:?}: {who}",
                    self.origin.stamp()
                ));
            }
            Input::Window(event) => {
                if let stache_macos::windows::WindowEventKind::EnhancedUi { application, note } =
                    &event.kind
                {
                    self.enhanced_ui(state, *application, *note);
                }
            }
            Input::Activated(application) => {
                logger.debug(format!(
                    "{} tiling: activated {}",
                    self.origin.stamp(),
                    label(state, *application)
                ));
            }
            Input::Displays(layout) => {
                logger.debug(format!("tiling: display layout {layout:?}"));
            }
            _ => {}
        }
    }

    /// Logs one lease transition. Failures, re-enables and `VoiceOver` skips are
    /// warnings; plain leases and restores are debug. The shard emits these
    /// only on a transition, never per frame.
    fn enhanced_ui(
        &self,
        state: &TilingState,
        application: ApplicationId,
        note: stache_macos::windows::EnhancedUiNote,
    ) {
        use stache_macos::windows::EnhancedUiNote;
        let Some(logger) = &self.logger else { return };
        let line = format!(
            "{} tiling: enhanced ui of {}: {note:?}",
            self.origin.stamp(),
            label(state, application)
        );
        if matches!(note, EnhancedUiNote::Leased | EnhancedUiNote::Restored) {
            logger.debug(line);
        } else {
            logger.warn(line);
        }
    }

    /// Logs every frame write a transition planned (debug): which window, from
    /// which observed frame to which target, so a needless rewrite is visible.
    pub fn effects(&self, state: &TilingState, effects: &[crate::state::Effect]) {
        let Some(logger) = &self.logger else { return };
        for effect in effects {
            if let crate::state::Effect::WriteFrame {
                window,
                from,
                target,
                components,
                animate,
                plan_sequence,
                ..
            } = effect
            {
                logger.debug(format!(
                    "{} tiling: write {} from {from:?} to {target:?} ({components:?}, \
                     animate {animate}, plan {plan_sequence})",
                    self.origin.stamp(),
                    label(state, window.application())
                ));
            }
        }
    }

    /// Logs (info) each application that entered or left the animation instant
    /// fallback: name, bundle id, kind and reason.
    pub fn animation_fallback(
        &self,
        state: &TilingState,
        events: &[crate::animation::FallbackEvent],
    ) {
        use crate::animation::FallbackEvent;
        let Some(logger) = &self.logger else { return };
        for event in events {
            let line = match event {
                FallbackEvent::Entered(entry) => format!(
                    "animation: {} moves without animation for {}s ({}): {}",
                    label(state, entry.application),
                    entry.kind.as_str(),
                    if entry.until.is_some() {
                        "until a cool-down passes"
                    } else {
                        "for this run"
                    },
                    entry.reason,
                ),
                FallbackEvent::Left(entry) => format!(
                    "animation: {} animates {}s again (its cool-down passed): {}",
                    label(state, entry.application),
                    entry.kind.as_str(),
                    entry.reason,
                ),
            };
            logger.info(format!("{} tiling: {line}", self.origin.stamp()));
        }
    }

    /// Logs a tab group handover (debug): the window that now represents the
    /// group took the slot of the one it replaced.
    pub fn handover(
        &self,
        state: &TilingState,
        old: stache_macos::windows::WindowInstance,
        new: stache_macos::windows::WindowInstance,
    ) {
        let Some(logger) = &self.logger else { return };
        logger.debug(format!(
            "tiling: tab group of {} handed over {old:?} -> {new:?}",
            label(state, new.application())
        ));
    }

    /// Logs a slot event (debug): a suspension, a return or the release of the
    /// tile of a suspended window.
    pub fn slot_note(&self, state: &TilingState, note: crate::state::TabNote) {
        use crate::state::TabNote;
        let Some(logger) = &self.logger else { return };
        let (what, window) = match note {
            TabNote::Suspended(window) => ("window suspended (alive, unlisted)".to_owned(), window),
            TabNote::Returned(window, held) => (
                format!("suspended window listed again, took the slot of {held:?}"),
                window,
            ),
            TabNote::Resumed(window) => ("suspended window listed again".to_owned(), window),
            TabNote::SingleHandover(suspended, new) => (
                format!("new tabbed window took the slot of suspended {suspended:?}"),
                new,
            ),
            TabNote::Released(window) => (
                "slot released: no window took the suspended one's place".to_owned(),
                window,
            ),
        };
        // The fullscreen flag the return read: a native fullscreen exit lists
        // the window again before macOS clears it.
        // The frame it reads and its planned tile tell whether a missing write
        // was skipped because it already sits on its tile.
        let fullscreen = state.windows.get(&window).map_or_else(String::new, |held| {
            format!(
                " (reads fullscreen={}, frame={:?}, tile={:?}, focused={})",
                held.observed.fullscreen,
                held.observed.frame,
                state.frames.desired.get(&window),
                state.focus.window == Some(window),
            )
        });
        logger.debug(format!(
            "tiling: {} {what}{fullscreen}: {window:?}",
            label(state, window.application())
        ));
    }

    /// Logs a settle listing that has to be repeated (debug): a window of the
    /// application was still in transition, or the retries ran out.
    pub fn settle_retry(
        &self,
        state: &TilingState,
        application: ApplicationId,
        attempt: u8,
        expired: bool,
    ) {
        let Some(logger) = &self.logger else { return };
        let what = if expired { "gave up" } else { "retry" };
        logger.debug(format!(
            "tiling: tab settle {what} for {} (attempt {attempt})",
            label(state, application)
        ));
    }

    /// Logs what changed after a transition: phase, global faults, failed or
    /// truncated applications, displays without frames and the latest intent.
    pub fn observe(&mut self, state: &TilingState) {
        let Some(logger) = self.logger.clone() else { return };
        let phase = state.phase();
        if self.phase != Some(phase) {
            let line = format!(
                "tiling: phase {} ({} apps, {} windows)",
                phase.as_str(),
                state.apps.len(),
                state.windows.len()
            );
            // Reconciling and Complete alternate with every routine refresh;
            // only the first completion and the real trouble are worth `info`.
            let routine = matches!(
                (self.phase, phase),
                (Some(Phase::Reconciling), Phase::Complete)
                    | (Some(Phase::Complete), Phase::Reconciling)
            );
            if routine {
                logger.debug(line);
            } else {
                logger.info(line);
            }
            self.phase = Some(phase);
        }
        let faults = state.fault_reasons();
        if faults != self.faults {
            if faults.is_empty() {
                logger.info("tiling: global faults cleared");
            } else {
                logger.warn(format!("tiling: frame writes held back: {}", faults.join("; ")));
            }
            self.faults = faults;
        }
        let failed: BTreeMap<ApplicationId, String> = state
            .startup
            .failed
            .iter()
            .map(|application| {
                let reason = state
                    .startup
                    .failure_reasons
                    .get(application)
                    .cloned()
                    .unwrap_or_else(|| "tracking failed".to_owned());
                (*application, reason)
            })
            .collect();
        for (application, reason) in &failed {
            if self.failed.get(application) != Some(reason) {
                let retries = state.startup.retries.get(application).copied().unwrap_or(0);
                logger.warn(format!(
                    "tiling: {} excluded, only its own windows wait: {reason} (retries {retries})",
                    label(state, *application)
                ));
            }
        }
        for application in self.failed.keys().filter(|app| !failed.contains_key(app)) {
            logger.info(format!("tiling: {} recovered", label(state, *application)));
        }
        self.failed = failed;
        for application in state.startup.truncated.difference(&self.truncated) {
            logger.warn(format!(
                "tiling: {} enumeration truncated, only its own windows wait",
                label(state, *application)
            ));
        }
        self.truncated.clone_from(&state.startup.truncated);
        if self.no_frame != state.frames.no_frame {
            for (display, reason) in &state.frames.no_frame {
                if self.no_frame.get(display) != Some(reason) {
                    let name = state
                        .screens
                        .iter()
                        .find(|screen| screen.id == *display)
                        .and_then(|screen| screen.name.clone())
                        .unwrap_or_else(|| format!("screen-{}", display.raw()));
                    logger.warn(format!("tiling: no frames on {name}: {reason:?}"));
                }
            }
            self.no_frame.clone_from(&state.frames.no_frame);
        }
        if let Some(record) = state.last_intent()
            && record.sequence != self.intent_sequence
        {
            self.intent_sequence = record.sequence;
            logger.info(format!(
                "tiling: intent {:?} -> {:?}",
                record.intent, record.outcome
            ));
        }
    }

    /// Logs a transient track failure whose retry is scheduled (debug).
    pub fn track_retry_armed(
        &self,
        state: &TilingState,
        application: ApplicationId,
        attempt: u8,
        delay: std::time::Duration,
    ) {
        let Some(logger) = &self.logger else { return };
        logger.debug(format!(
            "{} tiling: track of {} failed, retry {attempt} in {delay:?}",
            self.origin.stamp(),
            label(state, application)
        ));
    }

    /// Logs that the time-based track retries ran out (warn): the application
    /// is left to the activation and relaunch retries.
    pub fn track_gave_up(&self, state: &TilingState, application: ApplicationId, retries: u8) {
        let Some(logger) = &self.logger else { return };
        logger.warn(format!(
            "tiling: {} still not trackable after {retries} timed retries; waiting for activation",
            label(state, application)
        ));
    }

    /// Logs once when an empty desktop cannot be shown (see
    /// `VisibilityRequirement::kept_last`).
    pub fn kept_last(&self, state: &TilingState, kept: Option<ApplicationId>) {
        if self.kept_last.replace(kept) == kept {
            return;
        }
        let (Some(logger), Some(application)) = (&self.logger, kept) else {
            return;
        };
        logger.info(format!(
            "{} tiling: cannot show an empty desktop: {} is the last visible application \
             (Finder is hidden because it has windows on a hidden workspace and no running \
             application without windows can host); it stays, retried at the next switch",
            self.origin.stamp(),
            label(state, application)
        ));
    }

    /// Logs the visibility requests just planned, with why.
    pub fn planned(&self, state: &TilingState, planned: &[Planned]) {
        let Some(logger) = &self.logger else { return };
        for request in planned {
            let (verb, application) = match request {
                Planned::Hide(application) => ("hide", *application),
                Planned::Unhide(application) => ("unhide", *application),
                Planned::Activate(application) => ("desktop host", *application),
            };
            logger.info(format!(
                "{} tiling: {verb} {}",
                self.origin.stamp(),
                label(state, application)
            ));
        }
    }
}
