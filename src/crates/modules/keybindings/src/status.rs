//! The Keybindings runtime observation.
//!
//! [`KeybindingsRuntimeStatus`] is the module's single runtime observation:
//! per-binding outcomes, the tap and Caps remap state, dispatch counters and
//! retained cleanup. [`KeybindingsSnapshot`] is the clonable read-only source the
//! runtime publishes to. Reading copies one short-lived cell and reads atomics; it
//! takes no module, registry or lifecycle lock, waits on no native work and reads
//! no file.
//!
//! Lifecycle, held file diagnostics and this runtime observation are sampled from
//! separate cells, so a response is a point-in-time observation rather than one
//! globally atomic snapshot. The runtime fields are published together.
//!
//! Opaque counters that can exceed JavaScript's exact integer range (the accepted
//! revision and the dispatch generation) are decimal strings on the wire.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_sdk::input::TapStatus;

use crate::dispatch::{DispatchSnapshot, Dispatcher};
use crate::runtime::RouterCounters;

/// Longest binding list a status carries; the rest is counted in
/// [`KeybindingsRuntimeStatus::bindings_omitted`].
pub const MAX_BINDING_STATUSES: usize = 256;
/// Longest runtime diagnostic list a status carries.
pub const MAX_DIAGNOSTICS: usize = 32;

/// Whether the runtime currently owns any shortcut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeState {
    /// Nothing is registered: stopped, disabled, paused, waiting or an empty map.
    #[default]
    Inactive,
    /// Every requested binding is active.
    Active,
    /// Some requested bindings are active, rejected or pending cleanup.
    Degraded,
}

/// How a binding is delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BindingRoute {
    /// A physical chord through the Carbon hotkey adapter.
    Ordinary,
    /// A `§` chord resolved from the keyboard layout.
    Section,
    /// A `CapsLock+<key>` chord through the shared input tap.
    Caps,
}

/// The outcome of one binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum BindingState {
    /// Registered and routed.
    Active,
    /// Not registered; `detail` says why.
    Rejected,
    /// Accepted but not registered yet (for example the runtime is waiting).
    Pending,
}

/// One configured binding and what happened to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BindingStatus {
    /// The shortcut as configured.
    pub shortcut: String,
    /// The route, when the spelling parsed.
    pub route: Option<BindingRoute>,
    /// The outcome.
    pub state: BindingState,
    /// The reason for a rejection or a note such as a replaced duplicate.
    pub detail: Option<String>,
}

/// The shared tap as this module needs it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum TapState {
    /// No Caps binding is accepted, so no tap is used.
    #[default]
    NotRequired,
    /// Installed, enabled and acknowledged.
    Ready,
    /// The suppressing tap has no native resources.
    Absent,
    /// The OS disabled the tap and a re-enable is pending.
    Disabled,
    /// Re-enabling failed.
    ReenableFailed,
    /// A failed action left inert resources awaiting cleanup.
    Retained,
}

impl From<&TapStatus> for TapState {
    fn from(status: &TapStatus) -> Self {
        match status {
            TapStatus::Absent => Self::Absent,
            TapStatus::Ready => Self::Ready,
            TapStatus::Disabled => Self::Disabled,
            TapStatus::ReenableFailed { .. } => Self::ReenableFailed,
            TapStatus::Retained(_) => Self::Retained,
        }
    }
}

/// The phase of the global Caps Lock remap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum RemapPhase {
    /// Nothing is owned (a blocked or refused remap also reports this, with a
    /// `detail`).
    #[default]
    Inactive,
    /// A remap write is in progress.
    Applying,
    /// The remap is applied and verified.
    Owned,
    /// Restoration did not complete; our mapping may still exist.
    CleanupPending,
    /// A native write may have happened and is unverified.
    Uncertain,
    /// Caps Lock is mapped by someone else; nothing of theirs was changed.
    OwnershipLost,
}

/// The Caps remap observation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RemapStatus {
    /// The phase.
    pub phase: RemapPhase,
    /// Why, for any phase other than inactive-and-clean.
    pub detail: Option<String>,
    /// Physical Caps Lock may send F18 to applications.
    pub f18_exposed: bool,
}

/// Dispatch counters, copied from atomics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DispatchStatus {
    /// The current dispatcher generation, as a decimal string.
    pub generation: String,
    /// Sequences running now.
    pub active: u32,
    /// Sequences waiting for a slot.
    pub waiting: u32,
    /// Reservations held by the input router.
    pub reserved: u32,
    /// Submissions refused because the dispatcher was full.
    pub overflow_drops: u32,
    /// Submissions refused because retained child ownership blocked dispatch.
    pub blocked_drops: u32,
    /// Submissions refused as stale.
    pub stale_drops: u32,
    /// Waiting sequences discarded by invalidation.
    pub discarded: u32,
    /// Sequences that completed every step.
    pub completed: u32,
    /// Sequences that stopped at a failing step.
    pub failed: u32,
    /// Sequences stopped by cancellation.
    pub cancelled: u32,
    /// Presses refused between a publication and its observation, or from a
    /// revision the runtime has not adopted.
    pub window_drops: u32,
    /// Presses whose route was unknown (retired or not yet adopted).
    pub unrouted: u32,
    /// Accepted presses the dispatcher refused: lost invocations, never run late.
    pub lost: u32,
    /// Presses handed to the dispatcher.
    pub submitted: u32,
}

/// Retained ownership that is bounded and visible.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RetainedStatus {
    /// Native hotkey IDs that stay registered but inert, awaiting cleanup.
    pub hotkey_ids: Vec<u32>,
    /// The Carbon handler or layout observer could not be removed.
    pub carbon_handler_pending: bool,
    /// Child processes the command reaper still owns.
    pub children: u32,
    /// A Caps restore is unresolved.
    pub caps_cleanup: bool,
}

/// The module's single runtime observation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingsRuntimeStatus {
    /// The accepted revision the runtime has adopted, as an exact decimal
    /// string; `"0"` while none is adopted.
    pub revision: String,
    /// Whether any shortcut is owned.
    pub state: RuntimeState,
    /// Active bindings.
    pub active: u32,
    /// Bindings that are not active.
    pub rejected: u32,
    /// Per-binding outcomes, capped at [`MAX_BINDING_STATUSES`].
    pub bindings: Vec<BindingStatus>,
    /// Bindings beyond the cap.
    pub bindings_omitted: u32,
    /// The shared tap.
    pub tap: TapState,
    /// The Caps remap.
    pub remap: RemapStatus,
    /// Dispatch counters.
    pub dispatch: DispatchStatus,
    /// Retained cleanup.
    pub retained: RetainedStatus,
    /// Recent runtime diagnostics, oldest first. This is not the rejected-file
    /// channel: that stays `ModuleStatusView::diagnostic`.
    pub diagnostics: Vec<String>,
}

/// The fields the runtime publishes whole; counters are merged at read time.
#[derive(Clone, Debug, Default)]
pub(crate) struct Published {
    pub revision: u64,
    pub state: RuntimeState,
    pub bindings: Vec<BindingStatus>,
    pub bindings_omitted: u32,
    pub tap: TapState,
    pub remap: RemapStatus,
    pub hotkey_ids: Vec<u32>,
    pub carbon_handler_pending: bool,
}

impl Published {
    fn counts(&self) -> (u32, u32) {
        let mut active = 0_u32;
        let mut rejected = 0_u32;
        for binding in &self.bindings {
            match binding.state {
                BindingState::Active => active = active.saturating_add(1),
                BindingState::Rejected => rejected = rejected.saturating_add(1),
                BindingState::Pending => {}
            }
        }
        (active, rejected.saturating_add(self.bindings_omitted))
    }
}

/// A bounded, shareable log of runtime diagnostics.
///
/// Dispatcher workers, the coordinator and lifecycle calls push to it; it never
/// grows past [`MAX_DIAGNOSTICS`] and never blocks beyond one short lock.
#[derive(Clone, Default)]
pub struct DiagnosticLog {
    entries: Arc<Mutex<VecDeque<String>>>,
}

impl DiagnosticLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Appends a message, dropping the oldest at the bound.
    pub fn push(&self, message: impl Into<String>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= MAX_DIAGNOSTICS {
            entries.pop_front();
        }
        entries.push_back(message.into());
    }

    /// The dispatcher-compatible sink.
    #[must_use]
    pub fn sink(&self) -> crate::dispatch::DiagnosticSink {
        let log = self.clone();
        Arc::new(move |message: &str| log.push(message))
    }

    /// A copy, oldest first.
    #[must_use]
    pub fn entries(&self) -> Vec<String> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
}

impl std::fmt::Debug for DiagnosticLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagnosticLog").finish_non_exhaustive()
    }
}

/// The clonable read-only source of [`KeybindingsRuntimeStatus`].
#[derive(Clone)]
pub struct KeybindingsSnapshot {
    published: Arc<Mutex<Arc<Published>>>,
    dispatcher: Dispatcher,
    counters: Arc<RouterCounters>,
    log: DiagnosticLog,
}

impl std::fmt::Debug for KeybindingsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeybindingsSnapshot").finish_non_exhaustive()
    }
}

fn saturate(value: u64) -> u32 { u32::try_from(value).unwrap_or(u32::MAX) }

fn saturate_len(value: usize) -> u32 { u32::try_from(value).unwrap_or(u32::MAX) }

impl KeybindingsSnapshot {
    pub(crate) fn new(
        dispatcher: Dispatcher,
        counters: Arc<RouterCounters>,
        log: DiagnosticLog,
    ) -> Self {
        Self {
            published: Arc::new(Mutex::new(Arc::new(Published::default()))),
            dispatcher,
            counters,
            log,
        }
    }

    /// Replaces the published fields as one coherent observation.
    pub(crate) fn publish(&self, published: Published) {
        *self.published.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(published);
    }

    pub(crate) fn current(&self) -> Arc<Published> {
        Arc::clone(&self.published.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Copies the current observation. Reads atomics and one short cell lock.
    #[must_use]
    pub fn read(&self) -> KeybindingsRuntimeStatus {
        let published = self.current();
        let dispatch = self.dispatcher.snapshot();
        let (active, rejected) = published.counts();
        KeybindingsRuntimeStatus {
            revision: published.revision.to_string(),
            state: published.state,
            active,
            rejected,
            bindings: published.bindings.clone(),
            bindings_omitted: published.bindings_omitted,
            tap: published.tap,
            remap: published.remap.clone(),
            dispatch: self.dispatch_status(&dispatch),
            retained: RetainedStatus {
                hotkey_ids: published.hotkey_ids.clone(),
                carbon_handler_pending: published.carbon_handler_pending,
                children: saturate_len(dispatch.retained),
                caps_cleanup: matches!(
                    published.remap.phase,
                    RemapPhase::CleanupPending | RemapPhase::Uncertain
                ),
            },
            diagnostics: self.log.entries(),
        }
    }

    fn dispatch_status(&self, dispatch: &DispatchSnapshot) -> DispatchStatus {
        let counters = self.counters.read();
        DispatchStatus {
            generation: dispatch.generation.to_string(),
            active: saturate_len(dispatch.active),
            waiting: saturate_len(dispatch.waiting),
            reserved: saturate_len(dispatch.reserved),
            overflow_drops: saturate(dispatch.overflow_drops),
            blocked_drops: saturate(dispatch.blocked_drops),
            stale_drops: saturate(dispatch.stale_drops),
            discarded: saturate(dispatch.discarded),
            completed: saturate(dispatch.completed),
            failed: saturate(dispatch.failed),
            cancelled: saturate(dispatch.cancelled),
            window_drops: saturate(counters.window_drops),
            unrouted: saturate(counters.unrouted),
            lost: saturate(counters.lost),
            submitted: saturate(counters.submitted),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn status_uses_exact_camel_case_names_and_decimal_strings() {
        let status = KeybindingsRuntimeStatus {
            revision: u64::MAX.to_string(),
            state: RuntimeState::Degraded,
            active: 1,
            rejected: 1,
            bindings: vec![BindingStatus {
                shortcut: "Cmd+K".to_owned(),
                route: Some(BindingRoute::Section),
                state: BindingState::Rejected,
                detail: Some("occupied".to_owned()),
            }],
            tap: TapState::ReenableFailed,
            remap: RemapStatus {
                phase: RemapPhase::CleanupPending,
                detail: Some("busy".to_owned()),
                f18_exposed: true,
            },
            retained: RetainedStatus {
                hotkey_ids: vec![7],
                carbon_handler_pending: true,
                children: 2,
                caps_cleanup: true,
            },
            ..KeybindingsRuntimeStatus::default()
        };
        let value = serde_json::to_value(&status).expect("encode");
        assert_eq!(value["revision"], json!("18446744073709551615"));
        assert_eq!(value["state"], json!("degraded"));
        assert_eq!(value["bindings"][0]["route"], json!("section"));
        assert_eq!(value["bindings"][0]["state"], json!("rejected"));
        assert_eq!(value["tap"], json!("reenableFailed"));
        assert_eq!(value["remap"]["phase"], json!("cleanupPending"));
        assert_eq!(value["remap"]["f18Exposed"], json!(true));
        assert_eq!(value["retained"]["hotkeyIds"], json!([7]));
        assert_eq!(value["retained"]["carbonHandlerPending"], json!(true));
        assert_eq!(value["bindingsOmitted"], json!(0));
        let decoded: KeybindingsRuntimeStatus = serde_json::from_value(value).expect("decode");
        assert_eq!(decoded, status);
    }

    #[test]
    fn every_remap_phase_has_a_stable_spelling() {
        let spelled: Vec<_> = [
            RemapPhase::Inactive,
            RemapPhase::Applying,
            RemapPhase::Owned,
            RemapPhase::CleanupPending,
            RemapPhase::Uncertain,
            RemapPhase::OwnershipLost,
        ]
        .iter()
        .map(|phase| serde_json::to_value(phase).expect("encode"))
        .collect();
        assert_eq!(spelled, [
            json!("inactive"),
            json!("applying"),
            json!("owned"),
            json!("cleanupPending"),
            json!("uncertain"),
            json!("ownershipLost"),
        ]);
    }

    #[test]
    fn the_diagnostic_log_is_bounded_and_ordered() {
        let log = DiagnosticLog::new();
        for index in 0..(MAX_DIAGNOSTICS + 5) {
            log.push(format!("message {index}"));
        }
        let entries = log.entries();
        assert_eq!(entries.len(), MAX_DIAGNOSTICS);
        assert_eq!(entries.first().map(String::as_str), Some("message 5"));
        assert_eq!(entries.last().map(String::as_str), Some("message 36"));
    }
}
