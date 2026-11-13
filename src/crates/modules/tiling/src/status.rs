//! The Tiling runtime observation.
//!
//! [`TilingRuntimeStatus`] is the module's single runtime observation. It is
//! sampled without taking a module, registry or lifecycle lock, waits on no
//! native work and reads no file. Lifecycle and held file diagnostics are
//! sampled from separate cells, so it is a point-in-time view.
//!
//! Lifecycle fields come from the module; the revision, plan sequence and health
//! are read from the actor's own published snapshot, so the revision shown is
//! the one the runtime actually adopted, never a module-local counter. Counters
//! that can exceed JavaScript's exact integer range are decimal strings.

use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::ApplicationId;

use crate::actor::SnapshotCell;
use crate::outcome::OperationOutcome;
use crate::state::Health;
use crate::visibility::{OwnedApp, Ownership};

/// Where the runtime is in its lifecycle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum TilingRuntimeState {
    /// Nothing is installed or spawned.
    #[default]
    Inactive,
    /// The infrastructure is installed and the action authority is published.
    Running,
    /// Stop began: the authority is withdrawn and cleanup is in progress.
    Stopping,
    /// Cleanup is incomplete; exact resources are retained and no restart is
    /// allowed until the owed stages finish.
    Quarantined,
}

/// The runtime observation published to the host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TilingRuntimeStatus {
    /// Lifecycle position.
    pub state: TilingRuntimeState,
    /// The accepted revision the actor adopted, as a decimal string.
    pub config_revision: String,
    /// The monotonic layout plan sequence, as a decimal string.
    pub plan_sequence: String,
    /// Whether the actor reports healthy infrastructure.
    pub healthy: bool,
    /// Cleanup stages still owed (empty unless quarantined or stopping).
    pub owed: Vec<String>,
    /// The last lifecycle or cleanup problem.
    pub detail: Option<String>,
    /// Explicit degraded or unsupported conditions: requested optional features
    /// the runtime keeps off and a display feed problem.
    pub diagnostics: Vec<String>,
    /// Applications whose hidden state the runtime cannot attribute. They are
    /// retained and never unhidden automatically.
    pub ambiguous_visibility: Vec<String>,
    /// Windows whose latest placement was only partly confirmed, uncertain or
    /// refused. The accepted configuration is separate from physical placement.
    pub partial_outcomes: u32,
    /// Applications with windows on inactive workspaces that stay visible
    /// because another visible workspace needs them (partial isolation).
    pub visibility_conflicts: u32,
}

#[derive(Default)]
struct Shared {
    stored: Mutex<TilingRuntimeStatus>,
    cell: Mutex<Option<Arc<SnapshotCell>>>,
    /// Optional features the accepted configuration requests but the runtime
    /// does not provide yet.
    features: Mutex<Vec<String>>,
    /// The latest display feed problem.
    display: Mutex<Option<String>>,
    /// Why the passive drag observer is not running, if it is not.
    drag: Mutex<Option<String>>,
    /// Ambiguous records retained past a stop.
    retained: Mutex<Vec<String>>,
    /// Which displays animate on the timer instead of a frame clock, read live.
    pacing: Mutex<Option<PacingSource>>,
    /// The live native border diagnostics (engine and overlay), when composed.
    native_borders: Mutex<Option<PacingSource>>,
    /// The live instant-fallback set and animation metrics.
    animation: Mutex<Option<AnimationSource>>,
}

/// Where the live animation pacing notes come from.
pub type PacingSource = Box<dyn Fn() -> Vec<String> + Send + Sync>;

/// Where the live instant-fallback set and animation metrics come from.
pub type AnimationSource = Box<dyn Fn() -> crate::animation::AnimationReport + Send + Sync>;

/// The application label of a diagnostic line: `<name> (<bundle id>)`.
fn who_is(snapshot: &crate::state::TilingSnapshot, application: ApplicationId) -> String {
    snapshot
        .apps
        .iter()
        .find(|record| record.info.application == application)
        .map_or_else(
            || "an unidentified application".to_owned(),
            |record| format!("{} ({})", record.info.name, record.info.bundle_id),
        )
}

/// The animation diagnostics of one published snapshot: one line per
/// application that moves without animation,
/// `animation: <name> (<bundle id>) moves without animation for <kinds>: <reason>`
/// (`moves`, `resizes` or `moves and resizes`), then one
/// metrics summary line per application with data.
#[must_use]
pub fn animation_diagnostics(
    snapshot: &crate::state::TilingSnapshot,
    report: &crate::animation::AnimationReport,
) -> Vec<String> {
    let mut out = Vec::new();
    // One line per application and reason, naming the kinds it holds (a lease
    // reason holds both).
    let mut lines: Vec<(ApplicationId, crate::animation::FallbackReason, Vec<&str>)> = Vec::new();
    for entry in &report.fallbacks {
        let kind = match entry.kind {
            crate::animation::MotionKind::Move => "moves",
            crate::animation::MotionKind::Resize => "resizes",
        };
        match lines.iter_mut().find(|(a, r, _)| *a == entry.application && *r == entry.reason) {
            Some((_, _, kinds)) => kinds.push(kind),
            None => lines.push((entry.application, entry.reason, vec![kind])),
        }
    }
    for (application, reason, kinds) in lines {
        out.push(format!(
            "animation: {} moves without animation for {}: {reason}",
            who_is(snapshot, application),
            kinds.join(" and ")
        ));
    }
    for (application, summary) in &report.metrics {
        out.push(crate::animation::describe_metrics(
            &who_is(snapshot, *application),
            summary,
        ));
    }
    out
}

/// The optional features `config` requests that this runtime keeps off.
///
/// Each entry would be an explicit unsupported diagnostic, never a silent
/// erasure. Every optional feature of the configuration now has a runtime, so
/// none is listed; borders report their own problems through `diagnostics`.
#[must_use]
pub const fn unsupported_features(_config: &crate::config::TilingConfig) -> Vec<String> {
    Vec::new()
}

/// The tab capability diagnostics for one published snapshot. Tab support is
/// never claimed: reads off or failing is stated, and ordinary windows are
/// always tiled.
#[must_use]
pub fn tab_diagnostics(tabs: &crate::tabs::TabsStatus) -> Vec<String> {
    let mut out = Vec::new();
    if !tabs.reading {
        out.push(
            "native tabs are not read: tab groups are not detected, every window is tiled as an \
             ordinary window, and tab handling is unverified"
                .to_owned(),
        );
    } else if tabs.unavailable_apps > 0 {
        out.push(format!(
            "native tab information is unavailable for {} application(s): their windows are \
             tiled as ordinary windows",
            tabs.unavailable_apps
        ));
    }
    out
}

/// How a lease problem reads in a diagnostic line.
fn describe_enhanced_ui(note: stache_macos::windows::EnhancedUiNote) -> Option<String> {
    use stache_macos::windows::EnhancedUiNote;
    Some(match note {
        EnhancedUiNote::Leased | EnhancedUiNote::Restored => return None,
        EnhancedUiNote::ReadFailed(error) => format!(
            "AXEnhancedUserInterface could not be read ({error:?}); frames are written without \
             suppressing it"
        ),
        EnhancedUiNote::SetFailed(error) => format!(
            "AXEnhancedUserInterface could not be turned off ({error:?}); frames are written \
             without suppressing it"
        ),
        EnhancedUiNote::ReEnabled { consecutive } => format!(
            "AXEnhancedUserInterface was turned back on during animation ({consecutive} in a \
             row); it is not fought"
        ),
        EnhancedUiNote::RestoreFailed(error) => format!(
            "AXEnhancedUserInterface could not be restored ({error:?}); the restore is retried"
        ),
        EnhancedUiNote::VoiceOverSkipped => {
            "VoiceOver is running; AXEnhancedUserInterface is left untouched".to_owned()
        }
    })
}

/// The `AXEnhancedUserInterface` lease diagnostics for one published snapshot:
/// failures, re-enables and `VoiceOver` skips, one line each as
/// `enhanced ui: <name> (<bundle id>): <note>`. Plain leases and restores only
/// update counters and never appear here.
#[must_use]
pub fn enhanced_ui_diagnostics(snapshot: &crate::state::TilingSnapshot) -> Vec<String> {
    use stache_macos::windows::EnhancedUiNote;
    let mut out = Vec::new();
    for (application, stats) in &snapshot.enhanced_ui {
        let who = snapshot
            .apps
            .iter()
            .find(|record| record.info.application == *application)
            .map_or_else(
                || "an unidentified application".to_owned(),
                |record| format!("{} ({})", record.info.name, record.info.bundle_id),
            );
        let notes = stats
            .latest_problem
            .into_iter()
            .chain(stats.voice_over_skipped.then_some(EnhancedUiNote::VoiceOverSkipped));
        for note in notes {
            if let Some(text) = describe_enhanced_ui(note) {
                out.push(format!("enhanced ui: {who}: {text}"));
            }
        }
    }
    out
}

/// The clonable, read-only source and its module-side writer.
#[derive(Clone, Default)]
pub struct TilingStatusSource {
    shared: Arc<Shared>,
}

impl TilingStatusSource {
    /// One point-in-time observation.
    #[must_use]
    pub fn read(&self) -> TilingRuntimeStatus {
        let mut status = self.shared.stored.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let cell = self.shared.cell.lock().unwrap_or_else(PoisonError::into_inner).clone();
        status
            .ambiguous_visibility
            .clone_from(&self.shared.retained.lock().unwrap_or_else(PoisonError::into_inner));
        if matches!(
            status.state,
            TilingRuntimeState::Running | TilingRuntimeState::Stopping
        ) {
            status
                .diagnostics
                .clone_from(&self.shared.features.lock().unwrap_or_else(PoisonError::into_inner));
            status
                .diagnostics
                .extend(self.shared.display.lock().unwrap_or_else(PoisonError::into_inner).clone());
            status
                .diagnostics
                .extend(self.shared.drag.lock().unwrap_or_else(PoisonError::into_inner).clone());
            if let Some(source) =
                self.shared.pacing.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            {
                status.diagnostics.extend(source());
            }
            // Border problems are diagnostics only: the health above is the
            // runtime's, not the borders'.
            if let Some(source) = self
                .shared
                .native_borders
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            {
                status.diagnostics.extend(source());
            }
        }
        if status.state == TilingRuntimeState::Running
            && let Some(cell) = cell
        {
            let snapshot = cell.load();
            // Records retained from earlier runs stay reported beside this run's.
            for described in snapshot
                .hidden_apps
                .iter()
                .filter(|app| app.ownership == Ownership::Ambiguous)
                .map(|app| {
                    let label = snapshot
                        .apps
                        .iter()
                        .find(|record| record.info.application == app.application)
                        .map(|record| AppLabel::from_info(&record.info))
                        .unwrap_or_default();
                    describe_ambiguous(app, &label)
                })
            {
                if !status.ambiguous_visibility.contains(&described) {
                    status.ambiguous_visibility.push(described);
                }
            }
            status.partial_outcomes = u32::try_from(
                snapshot
                    .frames
                    .iter()
                    .filter(|frame| {
                        matches!(
                            frame.outcome,
                            Some(
                                OperationOutcome::Partial { .. }
                                    | OperationOutcome::Uncertain { .. }
                                    | OperationOutcome::Failed { .. }
                            )
                        )
                    })
                    .count(),
            )
            .unwrap_or(u32::MAX);
            status.visibility_conflicts =
                u32::try_from(snapshot.visibility.conflicts.len()).unwrap_or(u32::MAX);
            status.diagnostics.extend(tab_diagnostics(&snapshot.tabs));
            status.diagnostics.extend(enhanced_ui_diagnostics(&snapshot));
            if let Some(source) =
                self.shared.animation.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            {
                status.diagnostics.extend(animation_diagnostics(&snapshot, &source()));
            }
            status.config_revision = snapshot.config_revision.to_string();
            status.plan_sequence = snapshot.plan_sequence.to_string();
            status.healthy = matches!(snapshot.health, Health::Healthy);
            if let Health::Failed(fault) = snapshot.health {
                status.detail = Some(format!("the runtime failed: {fault:?}"));
            }
        }
        status
    }

    /// Records the unsupported optional features of the adopted configuration.
    pub(crate) fn set_features(&self, features: Vec<String>) {
        *self.shared.features.lock().unwrap_or_else(PoisonError::into_inner) = features;
    }

    /// Sets (or clears) the live source of fallback and metrics diagnostics.
    pub(crate) fn set_animation_source(&self, source: Option<AnimationSource>) {
        *self.shared.animation.lock().unwrap_or_else(PoisonError::into_inner) = source;
    }

    /// Sets (or clears) the live source of animation pacing diagnostics.
    pub(crate) fn set_pacing_source(&self, source: Option<PacingSource>) {
        *self.shared.pacing.lock().unwrap_or_else(PoisonError::into_inner) = source;
    }

    /// Sets (or clears) the live source of native border diagnostics.
    pub(crate) fn set_native_borders_source(&self, source: Option<PacingSource>) {
        *self.shared.native_borders.lock().unwrap_or_else(PoisonError::into_inner) = source;
    }

    /// Records (or clears) the latest display feed problem.
    pub(crate) fn display_failure(&self, failure: Option<String>) {
        *self.shared.display.lock().unwrap_or_else(PoisonError::into_inner) = failure;
    }

    /// Records (or clears) why the passive drag observer is not running. Drag
    /// alone is degraded; ordinary tiling stays healthy.
    pub(crate) fn drag_failure(&self, failure: Option<String>) {
        *self.shared.drag.lock().unwrap_or_else(PoisonError::into_inner) = failure;
    }

    /// Replaces the retained ambiguous records (the caller merges and prunes).
    pub(crate) fn retain_ambiguous(&self, apps: &[(OwnedApp, AppLabel)]) {
        *self.shared.retained.lock().unwrap_or_else(PoisonError::into_inner) =
            apps.iter().map(|(app, label)| describe_ambiguous(app, label)).collect();
    }

    pub(crate) fn running(&self, cell: Arc<SnapshotCell>) {
        // `retained` survives: an ambiguous record is never forgotten by a start.
        *self.shared.display.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.shared.drag.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.shared.cell.lock().unwrap_or_else(PoisonError::into_inner) = Some(cell);
        *self.shared.stored.lock().unwrap_or_else(PoisonError::into_inner) = TilingRuntimeStatus {
            state: TilingRuntimeState::Running,
            healthy: true,
            ..TilingRuntimeStatus::default()
        };
    }

    pub(crate) fn stopping(&self) {
        *self.shared.cell.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let mut stored = self.shared.stored.lock().unwrap_or_else(PoisonError::into_inner);
        stored.state = TilingRuntimeState::Stopping;
        stored.healthy = false;
    }

    pub(crate) fn inactive(&self, detail: Option<String>) {
        *self.shared.cell.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.shared.stored.lock().unwrap_or_else(PoisonError::into_inner) = TilingRuntimeStatus {
            detail,
            ..TilingRuntimeStatus::default()
        };
    }

    pub(crate) fn quarantined(&self, owed: Vec<String>, detail: String) {
        *self.shared.cell.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.shared.stored.lock().unwrap_or_else(PoisonError::into_inner) = TilingRuntimeStatus {
            state: TilingRuntimeState::Quarantined,
            owed,
            detail: Some(detail),
            ..TilingRuntimeStatus::default()
        };
    }
}

/// Human-readable facts about an application, for diagnostics only. They are
/// never authority: the opaque application id and its token are never shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppLabel {
    pub(crate) name: Option<String>,
    pub(crate) bundle_id: Option<String>,
    pub(crate) pid: Option<i32>,
}

impl AppLabel {
    pub fn from_info(info: &crate::state::AppInfo) -> Self {
        let text = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        Self {
            name: text(&info.name),
            bundle_id: text(&info.bundle_id),
            pid: Some(info.pid),
        }
    }

    pub fn from_running(app: &stache_macos::FrontmostApplication) -> Self {
        Self {
            name: app.name.clone().filter(|name| !name.is_empty()),
            bundle_id: app.bundle_identifier.clone().filter(|id| !id.is_empty()),
            pid: app.pid,
        }
    }

    /// `Safari (com.apple.Safari, pid 123)` with graceful fallbacks.
    fn render(&self) -> String {
        let pid = self.pid.map(|pid| format!("pid {pid}"));
        match (&self.name, &self.bundle_id) {
            (Some(name), Some(bundle)) => {
                format!(
                    "{name} ({bundle}{})",
                    pid.map(|p| format!(", {p}")).unwrap_or_default()
                )
            }
            (Some(one), None) | (None, Some(one)) => {
                format!("{one}{}", pid.map(|p| format!(" ({p})")).unwrap_or_default())
            }
            (None, None) => pid.map_or_else(
                || "an unidentified application".to_owned(),
                |p| format!("an application ({p})"),
            ),
        }
    }
}

fn describe_ambiguous(app: &OwnedApp, label: &AppLabel) -> String {
    format!(
        "{}: {}",
        label.render(),
        app.reason.as_deref().unwrap_or("hidden by an unattributable cause")
    )
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn labels_render_with_fallbacks() {
        let full = AppLabel {
            name: Some("Safari".into()),
            bundle_id: Some("com.apple.Safari".into()),
            pid: Some(123),
        };
        assert_eq!(full.render(), "Safari (com.apple.Safari, pid 123)");
        let bundle = AppLabel {
            name: None,
            bundle_id: Some("a.b".into()),
            pid: Some(7),
        };
        assert_eq!(bundle.render(), "a.b (pid 7)");
        let pid = AppLabel {
            pid: Some(7),
            ..AppLabel::default()
        };
        assert_eq!(pid.render(), "an application (pid 7)");
        assert_eq!(AppLabel::default().render(), "an unidentified application");
    }

    fn owned() -> OwnedApp {
        OwnedApp {
            application: crate::test_support::app(1),
            ownership: Ownership::Ambiguous,
            reason: Some("a show was observed".into()),
            unhide_failures: 0,
        }
    }

    #[test]
    fn live_and_retained_records_show_human_identity_and_never_the_token() {
        let expected = "Safari (com.apple.Safari, pid 101): a show was observed";
        // Live: from the run's application catalog.
        let info = crate::test_support::info(1, "com.apple.Safari", "Safari");
        assert_eq!(
            describe_ambiguous(&owned(), &AppLabel::from_info(&info)),
            expected
        );
        // Retained: captured at merge time and published through the source.
        let source = TilingStatusSource::default();
        source.retain_ambiguous(&[(owned(), AppLabel::from_info(&info))]);
        let lines = source.read().ambiguous_visibility;
        assert_eq!(lines, vec![expected.to_owned()]);
        assert!(!lines[0].contains("ApplicationId") && !lines[0].contains("token"));
    }
}

#[cfg(test)]
mod enhanced_ui_tests {
    use stache_macos::ax::AxError;
    use stache_macos::windows::EnhancedUiNote;

    use super::*;
    use crate::state::{AppRecord, TilingState};
    use crate::test_support::{app, info};

    fn state_with_safari() -> TilingState {
        let mut state = TilingState::new("");
        let safari = info(1, "com.apple.Safari", "Safari");
        state.apps.insert(safari.application, AppRecord {
            info: safari,
            hidden: false,
            skipped: false,
            tracked: true,
        });
        state
    }

    fn record(state: &mut TilingState, notes: &[EnhancedUiNote]) {
        let counters = state.enhanced_ui.entry(app(1)).or_default();
        for note in notes {
            counters.record(*note);
        }
    }

    #[test]
    fn plain_leases_and_restores_only_update_counters() {
        let mut state = state_with_safari();
        record(&mut state, &[EnhancedUiNote::Leased, EnhancedUiNote::Restored]);
        let snapshot = state.snapshot();
        assert!(enhanced_ui_diagnostics(&snapshot).is_empty());
        let counters = snapshot.enhanced_ui_of(app(1)).expect("stats");
        assert_eq!((counters.leases, counters.restores), (1, 1));
        assert!(!counters.cannot_keep_up() && !counters.set_failed());
    }

    #[test]
    fn failures_re_enables_and_voiceover_show_the_application_and_the_note() {
        let mut state = state_with_safari();
        record(&mut state, &[
            EnhancedUiNote::SetFailed(AxError::CannotComplete),
            EnhancedUiNote::VoiceOverSkipped,
        ]);
        assert_eq!(enhanced_ui_diagnostics(&state.snapshot()), [
            "enhanced ui: Safari (com.apple.Safari): AXEnhancedUserInterface could not be turned \
             off (CannotComplete); frames are written without suppressing it",
            "enhanced ui: Safari (com.apple.Safari): VoiceOver is running; \
             AXEnhancedUserInterface is left untouched",
        ]);
        record(&mut state, &[EnhancedUiNote::ReEnabled { consecutive: 1 }]);
        record(&mut state, &[EnhancedUiNote::ReEnabled { consecutive: 2 }]);
        let snapshot = state.snapshot();
        assert_eq!(
            enhanced_ui_diagnostics(&snapshot)[0],
            "enhanced ui: Safari \
                    (com.apple.Safari): AXEnhancedUserInterface was turned back on during \
                    animation (2 in a row); it is not fought"
        );
        let counters = snapshot.enhanced_ui_of(app(1)).expect("stats");
        assert!(
            counters.cannot_keep_up(),
            "part 06 reads two consecutive re-enables"
        );
        assert!(counters.set_failed(), "part 06 reads a failed set");
        assert_eq!((counters.re_enables, counters.set_failures), (2, 1));
    }

    #[test]
    fn a_restore_clears_a_restore_failure_and_a_clean_lease_resets_the_streak() {
        let mut state = state_with_safari();
        record(&mut state, &[
            EnhancedUiNote::ReEnabled { consecutive: 1 },
            EnhancedUiNote::RestoreFailed(AxError::CannotComplete),
        ]);
        assert!(enhanced_ui_diagnostics(&state.snapshot())[0].contains("could not be restored"));
        record(&mut state, &[EnhancedUiNote::Restored]);
        let snapshot = state.snapshot();
        assert!(enhanced_ui_diagnostics(&snapshot).is_empty());
        assert_eq!(
            snapshot.enhanced_ui_of(app(1)).expect("stats").consecutive_re_enables,
            0
        );
    }

    #[test]
    fn the_status_source_shows_the_lines_while_running() {
        let mut state = state_with_safari();
        record(&mut state, &[EnhancedUiNote::ReadFailed(
            AxError::CannotComplete,
        )]);
        let cell = Arc::new(SnapshotCell::default());
        cell.publish(state.snapshot());
        let source = TilingStatusSource::default();
        source.running(cell);
        let diagnostics = source.read().diagnostics;
        assert!(
            diagnostics.iter().any(|line| line
                .starts_with("enhanced ui: Safari (com.apple.Safari): ")
                && line.contains("could not be read")),
            "{diagnostics:?}"
        );
    }
}

#[cfg(test)]
mod animation_diagnostics_tests {
    use std::time::Duration;

    use super::*;
    use crate::animation::{
        AnimationReport, FallbackEntry, FallbackReason, MetricsSummary, MotionKind,
    };
    use crate::state::{AppRecord, TilingState};
    use crate::test_support::{app, info};

    fn state_with_safari() -> TilingState {
        let mut state = TilingState::new("");
        let safari = info(1, "com.apple.Safari", "Safari");
        state.apps.insert(safari.application, AppRecord {
            info: safari,
            hidden: false,
            skipped: false,
            tracked: true,
        });
        state
    }

    fn entry(
        application: stache_macos::ApplicationId,
        kind: MotionKind,
        reason: FallbackReason,
    ) -> FallbackEntry {
        FallbackEntry {
            application,
            kind,
            reason,
            until: None,
        }
    }

    fn summary() -> MetricsSummary {
        MetricsSummary {
            animations: 8,
            rate_p50_hz: 58.93,
            dropped: 3,
            write_p95: Duration::from_micros(4100),
            end_lag_p95: Duration::from_micros(6000),
        }
    }

    #[test]
    fn the_fallback_line_is_exact_for_every_reason() {
        let snapshot = state_with_safari().snapshot();
        for (reason, text) in [
            (
                FallbackReason::BelowMinimumRate,
                "writes fewer than 15 frames per second in 2 of its last 3 layout changes",
            ),
            (
                FallbackReason::LateEnd,
                "ends more than 250 ms late in 2 of its last 3 layout changes",
            ),
            (FallbackReason::ReEnablesEnhancedUi, "turns Enhanced UI back on"),
            (
                FallbackReason::EnhancedUiStuck,
                "Enhanced UI could not be turned off",
            ),
            (
                FallbackReason::Overflow,
                "shares the overflow AX thread and cannot keep up",
            ),
            (
                FallbackReason::NoFrameClock,
                "its display has no frame clock and it cannot keep up",
            ),
        ] {
            let report = AnimationReport {
                // A lease reason holds both kinds: still one line.
                fallbacks: vec![
                    entry(app(1), MotionKind::Move, reason),
                    entry(app(1), MotionKind::Resize, reason),
                ],
                metrics: Vec::new(),
            };
            assert_eq!(animation_diagnostics(&snapshot, &report), [format!(
                "animation: Safari (com.apple.Safari) moves without animation for moves and resizes: \
                 {text}"
            )]);
        }
    }

    #[test]
    fn the_metrics_line_has_a_stable_format() {
        let snapshot = state_with_safari().snapshot();
        let report = AnimationReport {
            fallbacks: Vec::new(),
            metrics: vec![(app(1), summary())],
        };
        assert_eq!(animation_diagnostics(&snapshot, &report), [
            "animation metrics: Safari (com.apple.Safari): 8 animations, rate p50 58.9 Hz, \
             dropped 3, write p95 4.1 ms, end lag p95 6.0 ms"
        ]);
        let one = MetricsSummary { animations: 1, ..summary() };
        assert!(
            crate::animation::describe_metrics("Safari (com.apple.Safari)", &one)
                .contains(": 1 animation, rate")
        );
        assert!(animation_diagnostics(&snapshot, &AnimationReport::default()).is_empty());
    }

    #[test]
    fn an_unknown_application_is_named_without_internals() {
        let snapshot = state_with_safari().snapshot();
        let report = AnimationReport {
            fallbacks: vec![entry(app(7), MotionKind::Resize, FallbackReason::LateEnd)],
            metrics: Vec::new(),
        };
        let lines = animation_diagnostics(&snapshot, &report);
        assert!(lines[0].starts_with("animation: an unidentified application moves"));
        assert!(!lines[0].contains("ApplicationId"));
    }

    #[test]
    fn the_status_source_lists_fallbacks_then_metrics_while_running() {
        let cell = Arc::new(SnapshotCell::default());
        cell.publish(state_with_safari().snapshot());
        let source = TilingStatusSource::default();
        source.running(cell);
        source.set_animation_source(Some(Box::new(|| AnimationReport {
            fallbacks: vec![entry(app(1), MotionKind::Move, FallbackReason::Overflow)],
            metrics: vec![(app(1), summary())],
        })));
        let diagnostics = source.read().diagnostics;
        let position = |prefix: &str| diagnostics.iter().position(|line| line.starts_with(prefix));
        let fallback =
            position("animation: Safari (com.apple.Safari) moves without animation for moves: ");
        let metrics = position("animation metrics: Safari (com.apple.Safari): ");
        assert!(fallback.is_some() && metrics.is_some(), "{diagnostics:?}");
        assert!(fallback < metrics);
    }
}
