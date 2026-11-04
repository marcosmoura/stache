//! Published Keep Awake runtime status.
//!
//! [`KeepAwakeRuntimeStatus`] is the module's single runtime observation:
//! assertion ownership, lock knowledge, observation mode and current
//! conditions. [`KeepAwakeSnapshot`] is the clonable read-only source a runtime
//! publishes to and readers copy from; reading clones one short-lived cell and
//! touches no policy state, native API, filesystem or configuration.
//!
//! The runtime publishes assertion and lock fields together, so a reader never
//! observes half of a transition. The app composes those runtime fields with
//! lifecycle, enabled, paused and diagnostic data sampled separately; a status
//! response is therefore an observation, not one globally atomic snapshot
//! across those cells.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use specta::Type;

/// Ownership state of the Keep Awake power assertion.
///
/// Assertion state is never inferred from lifecycle, enabled or paused state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AssertionState {
    /// Release is confirmed; no acquisition, candidate or unresolved ID remains.
    #[default]
    Inactive,
    /// Published before native creation or candidate validation, while unadopted
    /// work is in flight.
    Acquiring,
    /// The current generation holds a complete, adopted lease.
    ///
    /// This proves owned IOPM prevention only. It is not immunity from lid
    /// close, forced sleep or low-battery policy.
    Active,
    /// Published before native release or rollback while cleanup is in progress
    /// or unresolved, so sleep may still be prevented.
    ReleasePending,
}

/// Best-effort knowledge of the screen lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum LockState {
    /// The lock could not be read, or no runtime observation is current.
    #[default]
    Unknown,
    /// The session reports a locked screen.
    Locked,
    /// The session reports an unlocked screen, including a valid dictionary with
    /// no lock key.
    Unlocked,
}

/// How the runtime learns about lock changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum LockObservation {
    /// No observation is installed.
    #[default]
    Inactive,
    /// Distributed notifications refresh lock knowledge, backed by the owned
    /// reconciliation timer.
    NotificationsAndPolling,
    /// Notification installation failed for this generation, so the owned timer
    /// polls alone.
    PollingFallback,
}

/// The kind of a published runtime condition.
///
/// Variants are ordered so a stable, deduplicated condition list can be sorted
/// by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum KeepAwakeConditionKind {
    /// Lock observation could not be installed; polling fallback is in use.
    ObserverUnavailable,
    /// Reading the current lock state failed.
    LockProbeFailed,
    /// Creating or adopting the assertion lease failed.
    AssertionAcquireFailed,
    /// Releasing the assertion lease or rolling back failed.
    AssertionReleaseFailed,
    /// Screen lock observation could not be removed; the callback is inert and
    /// removal retries on the next lifecycle cleanup.
    ObservationDispatchFailed,
}

/// One actionable runtime condition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeepAwakeCondition {
    /// Which failure the condition reports.
    pub kind: KeepAwakeConditionKind,
    /// Actionable, user-facing description of the failure.
    pub message: String,
}

/// The module's published runtime observation.
///
/// The fields are sampled together under one short lock. Envelope data such as
/// lifecycle, enabled, paused and diagnostics belongs to the app, not here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeepAwakeRuntimeStatus {
    /// Ownership state of the power assertion.
    pub assertion_state: AssertionState,
    /// Best-effort screen-lock knowledge.
    pub lock_state: LockState,
    /// How lock changes are observed.
    pub observation: LockObservation,
    /// Current deduplicated conditions; empty when healthy.
    pub conditions: Vec<KeepAwakeCondition>,
}

/// Clonable read-only source of the published runtime status.
///
/// Clones share one cell, so every reader observes the runtime's latest
/// publication. Reading copies the whole status under a short lock and holds no
/// policy or native work.
#[derive(Clone, Debug, Default)]
pub struct KeepAwakeSnapshot {
    status: Arc<Mutex<KeepAwakeRuntimeStatus>>,
}

impl KeepAwakeSnapshot {
    /// Copies the current published status.
    #[must_use]
    pub fn read(&self) -> KeepAwakeRuntimeStatus {
        self.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Replaces the published status as one coherent observation.
    pub(crate) fn publish(&self, status: KeepAwakeRuntimeStatus) {
        *self.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn condition(kind: KeepAwakeConditionKind, message: &str) -> KeepAwakeCondition {
        KeepAwakeCondition {
            kind,
            message: message.to_owned(),
        }
    }

    #[test]
    fn defaults_are_inactive_unknown_and_empty() {
        let status = KeepAwakeSnapshot::default().read();
        assert_eq!(status, KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::Inactive,
            lock_state: LockState::Unknown,
            observation: LockObservation::Inactive,
            conditions: Vec::new(),
        });
        assert_eq!(status, KeepAwakeRuntimeStatus::default());
    }

    #[test]
    fn fields_and_conditions_use_exact_camel_case_names() {
        let status = KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::Active,
            lock_state: LockState::Unlocked,
            observation: LockObservation::NotificationsAndPolling,
            conditions: vec![condition(
                KeepAwakeConditionKind::AssertionAcquireFailed,
                "The power assertion could not be created.",
            )],
        };
        assert_eq!(
            serde_json::to_value(&status).expect("encode"),
            json!({
                "assertionState": "active",
                "lockState": "unlocked",
                "observation": "notificationsAndPolling",
                "conditions": [{
                    "kind": "assertionAcquireFailed",
                    "message": "The power assertion could not be created."
                }]
            })
        );
    }

    #[test]
    fn every_assertion_state_serializes_exactly() {
        for (state, expected) in [
            (AssertionState::Inactive, "inactive"),
            (AssertionState::Acquiring, "acquiring"),
            (AssertionState::Active, "active"),
            (AssertionState::ReleasePending, "releasePending"),
        ] {
            assert_eq!(
                serde_json::to_value(state).expect("encode"),
                Value::String(expected.to_owned())
            );
            assert_eq!(
                serde_json::from_value::<AssertionState>(Value::String(expected.to_owned()))
                    .expect("decode"),
                state
            );
        }
    }

    #[test]
    fn every_lock_and_observation_state_serializes_exactly() {
        for (state, expected) in [
            (LockState::Unknown, "unknown"),
            (LockState::Locked, "locked"),
            (LockState::Unlocked, "unlocked"),
        ] {
            assert_eq!(serde_json::to_value(state).expect("encode"), json!(expected));
            assert_eq!(
                serde_json::from_value::<LockState>(json!(expected)).expect("decode"),
                state
            );
        }
        for (state, expected) in [
            (LockObservation::Inactive, "inactive"),
            (
                LockObservation::NotificationsAndPolling,
                "notificationsAndPolling",
            ),
            (LockObservation::PollingFallback, "pollingFallback"),
        ] {
            assert_eq!(serde_json::to_value(state).expect("encode"), json!(expected));
            assert_eq!(
                serde_json::from_value::<LockObservation>(json!(expected)).expect("decode"),
                state
            );
        }
    }

    #[test]
    fn every_condition_kind_serializes_in_stable_order() {
        let expected = [
            (
                KeepAwakeConditionKind::ObserverUnavailable,
                "observerUnavailable",
            ),
            (KeepAwakeConditionKind::LockProbeFailed, "lockProbeFailed"),
            (
                KeepAwakeConditionKind::AssertionAcquireFailed,
                "assertionAcquireFailed",
            ),
            (
                KeepAwakeConditionKind::AssertionReleaseFailed,
                "assertionReleaseFailed",
            ),
            (
                KeepAwakeConditionKind::ObservationDispatchFailed,
                "observationDispatchFailed",
            ),
        ];
        let kinds = expected.map(|(kind, _)| kind);
        assert!(
            kinds.windows(2).all(|pair| pair[0] < pair[1]),
            "conditional kinds stay in stable order"
        );
        for (kind, text) in expected {
            assert_eq!(serde_json::to_value(kind).expect("encode"), json!(text));
            assert_eq!(
                serde_json::from_value::<KeepAwakeConditionKind>(json!(text)).expect("decode"),
                kind
            );
        }
    }

    #[test]
    fn status_round_trips_with_conditions() {
        let status = KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::ReleasePending,
            lock_state: LockState::Locked,
            observation: LockObservation::PollingFallback,
            conditions: vec![
                condition(
                    KeepAwakeConditionKind::ObserverUnavailable,
                    "Lock notifications are unavailable; polling instead.",
                ),
                condition(
                    KeepAwakeConditionKind::AssertionReleaseFailed,
                    "The power assertion could not be released yet.",
                ),
            ],
        };
        let text = serde_json::to_string(&status).expect("encode");
        let decoded: KeepAwakeRuntimeStatus = serde_json::from_str(&text).expect("decode");
        assert_eq!(decoded, status);
    }

    #[test]
    fn snapshots_are_independent_sources() {
        let first = KeepAwakeSnapshot::default();
        let second = KeepAwakeSnapshot::default();
        first.publish(KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::Active,
            ..KeepAwakeRuntimeStatus::default()
        });
        assert_eq!(first.read().assertion_state, AssertionState::Active);
        assert_eq!(second.read(), KeepAwakeRuntimeStatus::default());
    }

    #[test]
    fn published_transitions_are_read_whole() {
        let snapshot = KeepAwakeSnapshot::default();
        snapshot.publish(KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::Acquiring,
            lock_state: LockState::Unlocked,
            observation: LockObservation::NotificationsAndPolling,
            conditions: Vec::new(),
        });
        assert_eq!(snapshot.read(), KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::Acquiring,
            lock_state: LockState::Unlocked,
            observation: LockObservation::NotificationsAndPolling,
            conditions: Vec::new(),
        });
        snapshot.publish(KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::ReleasePending,
            lock_state: LockState::Locked,
            observation: LockObservation::NotificationsAndPolling,
            conditions: vec![condition(
                KeepAwakeConditionKind::AssertionReleaseFailed,
                "The power assertion could not be released yet.",
            )],
        });
        let read = snapshot.read();
        assert_eq!(read.assertion_state, AssertionState::ReleasePending);
        assert_eq!(read.lock_state, LockState::Locked);
        assert_eq!(read.conditions.len(), 1);
        assert_eq!(
            read.conditions[0].kind,
            KeepAwakeConditionKind::AssertionReleaseFailed
        );
    }

    #[test]
    fn concurrent_readers_never_observe_a_mixed_status() {
        let snapshot = KeepAwakeSnapshot::default();
        let inactive = KeepAwakeRuntimeStatus::default();
        let released = KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::ReleasePending,
            lock_state: LockState::Locked,
            observation: LockObservation::NotificationsAndPolling,
            conditions: vec![condition(
                KeepAwakeConditionKind::AssertionReleaseFailed,
                "The power assertion could not be released yet.",
            )],
        };
        snapshot.publish(inactive.clone());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..2_000 {
                    let read = snapshot.read();
                    assert!(
                        read == inactive || read == released,
                        "a read must copy one whole publication, not a mix: {read:?}"
                    );
                }
            });
            for index in 0..2_000 {
                snapshot.publish(if index % 2 == 0 {
                    released.clone()
                } else {
                    inactive.clone()
                });
            }
        });
    }

    #[test]
    fn a_cloned_reader_sees_later_publications() {
        let snapshot = KeepAwakeSnapshot::default();
        let reader = snapshot.clone();
        assert_eq!(reader.read(), KeepAwakeRuntimeStatus::default());
        snapshot.publish(KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::Active,
            lock_state: LockState::Unlocked,
            ..KeepAwakeRuntimeStatus::default()
        });
        let published = reader.read();
        assert_eq!(published.assertion_state, AssertionState::Active);
        assert_eq!(published.lock_state, LockState::Unlocked);
    }
}
