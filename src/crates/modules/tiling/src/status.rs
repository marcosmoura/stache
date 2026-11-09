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
    /// The latest borders delivery fault (borders only; never the runtime).
    borders: Mutex<Option<String>>,
    /// Ambiguous records retained past a stop.
    retained: Mutex<Vec<String>>,
}

/// The optional features `config` requests that this runtime keeps off.
///
/// Each entry would be an explicit unsupported diagnostic, never a silent
/// erasure. Every optional feature of the configuration now has a runtime, so
/// none is listed; borders report their own delivery faults through
/// `diagnostics`.
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
            status
                .diagnostics
                .extend(self.shared.borders.lock().unwrap_or_else(PoisonError::into_inner).clone());
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

    /// Records (or clears) the latest display feed problem.
    pub(crate) fn display_failure(&self, failure: Option<String>) {
        *self.shared.display.lock().unwrap_or_else(PoisonError::into_inner) = failure;
    }

    /// Records (or clears) why the passive drag observer is not running. Drag
    /// alone is degraded; ordinary tiling stays healthy.
    pub(crate) fn drag_failure(&self, failure: Option<String>) {
        *self.shared.drag.lock().unwrap_or_else(PoisonError::into_inner) = failure;
    }

    /// Records (or clears) the latest borders delivery fault. Borders alone are
    /// degraded; layout, commands and other modules are unaffected.
    pub(crate) fn borders_failure(&self, failure: Option<String>) {
        *self.shared.borders.lock().unwrap_or_else(PoisonError::into_inner) = failure;
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
