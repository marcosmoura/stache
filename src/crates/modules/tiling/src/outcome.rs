//! The internal operation outcome and per-component frame evidence.
//!
//! An [`OperationOutcome`] says exactly how far an operation got:
//!
//! * [`OperationOutcome::Enqueued`] is actor admission only. It never means a
//!   window moved.
//! * [`OperationOutcome::Applied`] requires a readback that confirms **every**
//!   requested component of the stated scope.
//! * [`OperationOutcome::Partial`] and [`OperationOutcome::Uncertain`] record an
//!   issued send whose result is not a full confirmation, with the actual frame
//!   when it could be read.
//! * [`OperationOutcome::Failed`] is an issued send the system definitively
//!   refused.
//! * [`OperationOutcome::NotAttempted`] and [`OperationOutcome::Stale`] prove no
//!   send happened. They are never reported as `Uncertain`.
//!
//! [`classify_frame`] maps the native [`WindowActionOutcome`] onto this
//! vocabulary. It is pure.

use stache_macos::ax::{AxError, AxReading, FrameRead, WriteStatus};
use stache_macos::windows::{
    DispatchError, IssuedKind, NativeStatus, NotIssuedReason, StaleReason, WindowActionOutcome,
    WindowInstance,
};

use crate::geometry::Rect;
use crate::minimum_size::INFERENCE_TOLERANCE;

/// Which frame components an attempt writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameComponents {
    /// The window origin.
    pub position: bool,
    /// The window size.
    pub size: bool,
}

impl FrameComponents {
    /// Whether nothing is requested.
    #[must_use]
    pub const fn is_empty(&self) -> bool { !self.position && !self.size }
}

/// Why nothing was sent. Every reason proves the native write did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotAttemptedReason {
    /// Cancelled before the send committed.
    Cancelled,
    /// The one-shot issuance was already used.
    AlreadyUsed,
    /// The planned generation is not the live one.
    GenerationInvalid,
    /// The accepted configuration revision changed.
    RevisionChanged,
    /// A newer plan superseded the attempt.
    Superseded,
    /// The runtime is sealed or stopping.
    Sealed,
    /// The owner thread is gone.
    OwnerUnavailable,
    /// The request asked for nothing.
    EmptyRequest,
    /// Accessibility permission is missing.
    PermissionDenied,
    /// The messaging timeout could not be set.
    TimeoutSetFailed(AxError),
    /// Window membership could not be verified.
    MembershipUnverified(AxError),
    /// The owner queue refused the command.
    Dispatch(DispatchError),
    /// The caller's wait ended before the send.
    AbandonedBeforeSend,
    /// The plan deadline passed before the send.
    Expired,
    /// The Tiling effect queue was full.
    QueueFull,
}

/// Why a token is no longer a live window (nothing was sent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleCause {
    /// The backend reported the token dead.
    Window(StaleReason),
    /// A newer plan replaced this one in the actor before issuance.
    Plan,
}

/// Why an issued write failed outright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    /// Every requested component was refused by the application or system.
    Refused,
}

/// The evidence for one requested component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentEvidence {
    /// The component was not requested.
    NotRequested,
    /// The readback matches the target within tolerance.
    Confirmed,
    /// The setter succeeded but the readback differs from the target.
    Mismatch,
    /// The setter definitively failed.
    Refused(AxError),
    /// The setter ran (or may have run) but no usable readback exists.
    Unobserved,
    /// Nothing was sent.
    NotSent,
}

impl ComponentEvidence {
    const fn requested(self) -> bool { !matches!(self, Self::NotRequested) }
}

/// Per-target evidence of one frame attempt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetEvidence {
    /// The exact window token.
    pub window: WindowInstance,
    /// The layout plan sequence the attempt belongs to.
    pub plan_sequence: u64,
    /// The frame the attempt wanted.
    pub expected: Rect,
    /// The position component.
    pub position: ComponentEvidence,
    /// The size component.
    pub size: ComponentEvidence,
    /// The frame the window actually has, when a complete readback exists.
    pub actual: Option<Rect>,
}

/// How far an operation got.
#[derive(Clone, Debug, PartialEq)]
pub enum OperationOutcome {
    /// Admitted by the actor. Nothing is known about the windows.
    Enqueued,
    /// Every requested component of every target was confirmed by readback.
    Applied {
        /// Per-target evidence.
        targets: Vec<TargetEvidence>,
    },
    /// Some, but not all, requested components were confirmed.
    Partial {
        /// Per-target evidence.
        targets: Vec<TargetEvidence>,
    },
    /// The send was issued but its result is not known.
    Uncertain {
        /// Per-target evidence.
        targets: Vec<TargetEvidence>,
    },
    /// The send was issued and definitively refused.
    Failed {
        /// Why.
        reason: FailureReason,
        /// Per-target evidence.
        targets: Vec<TargetEvidence>,
    },
    /// Provably nothing was sent.
    NotAttempted {
        /// Why.
        reason: NotAttemptedReason,
        /// The target that was not written.
        window: WindowInstance,
        /// The plan.
        plan_sequence: u64,
    },
    /// The token no longer names a live window; nothing was sent.
    Stale {
        /// Why.
        cause: StaleCause,
        /// The dead target.
        window: WindowInstance,
        /// The plan.
        plan_sequence: u64,
    },
}

impl OperationOutcome {
    /// Whether a native send may have run. `false` for admission and every
    /// proven-no-send outcome.
    #[must_use]
    pub const fn was_issued(&self) -> bool {
        matches!(
            self,
            Self::Applied { .. }
                | Self::Partial { .. }
                | Self::Uncertain { .. }
                | Self::Failed { .. }
        )
    }

    /// Whether the outcome proves nothing was sent.
    #[must_use]
    pub const fn proves_no_send(&self) -> bool {
        matches!(self, Self::NotAttempted { .. } | Self::Stale { .. })
    }

    /// The evidence of the (single) target, when a send was issued.
    #[must_use]
    pub fn evidence(&self) -> Option<&TargetEvidence> {
        match self {
            Self::Applied { targets }
            | Self::Partial { targets }
            | Self::Uncertain { targets }
            | Self::Failed { targets, .. } => targets.first(),
            _ => None,
        }
    }

    /// The actual frame recorded by the readback, when there is one.
    #[must_use]
    pub fn actual(&self) -> Option<Rect> { self.evidence().and_then(|e| e.actual) }
}

const fn not_attempted(reason: NotIssuedReason) -> NotAttemptedReason {
    match reason {
        NotIssuedReason::Cancelled => NotAttemptedReason::Cancelled,
        NotIssuedReason::AlreadyUsed => NotAttemptedReason::AlreadyUsed,
        NotIssuedReason::GenerationInvalid => NotAttemptedReason::GenerationInvalid,
        NotIssuedReason::RevisionChanged => NotAttemptedReason::RevisionChanged,
        NotIssuedReason::Superseded => NotAttemptedReason::Superseded,
        NotIssuedReason::Sealed => NotAttemptedReason::Sealed,
        NotIssuedReason::OwnerUnavailable => NotAttemptedReason::OwnerUnavailable,
        NotIssuedReason::EmptyRequest => NotAttemptedReason::EmptyRequest,
        NotIssuedReason::PermissionDenied => NotAttemptedReason::PermissionDenied,
        NotIssuedReason::TimeoutSetFailed(error) => NotAttemptedReason::TimeoutSetFailed(error),
        NotIssuedReason::MembershipUnverified(error) => {
            NotAttemptedReason::MembershipUnverified(error)
        }
        NotIssuedReason::Dispatch(error) => NotAttemptedReason::Dispatch(error),
        NotIssuedReason::AbandonedBeforeSend => NotAttemptedReason::AbandonedBeforeSend,
    }
}

fn near(a: f64, b: f64) -> bool { (a - b).abs() <= INFERENCE_TOLERANCE }

const fn read_position(read: &FrameRead) -> Option<(f64, f64)> {
    match read.position {
        AxReading::Value(point) => Some((point.x, point.y)),
        _ => None,
    }
}

const fn read_size(read: &FrameRead) -> Option<(f64, f64)> {
    match read.size {
        AxReading::Value(size) => Some((size.width, size.height)),
        _ => None,
    }
}

fn component(
    requested: bool,
    status: WriteStatus,
    read: Option<(f64, f64)>,
    expected: (f64, f64),
) -> ComponentEvidence {
    if !requested {
        return ComponentEvidence::NotRequested;
    }
    match status {
        WriteStatus::NotRequested => ComponentEvidence::NotRequested,
        // `CannotComplete` can follow a write that took effect: never "refused".
        WriteStatus::Failed(AxError::CannotComplete) => ComponentEvidence::Unobserved,
        WriteStatus::Failed(error) => ComponentEvidence::Refused(error),
        WriteStatus::Written => match read {
            Some((a, b)) if near(a, expected.0) && near(b, expected.1) => {
                ComponentEvidence::Confirmed
            }
            Some(_) => ComponentEvidence::Mismatch,
            None => ComponentEvidence::Unobserved,
        },
    }
}

/// Maps a native frame-write outcome onto an [`OperationOutcome`].
///
/// `expected` is the frame the attempt wanted and `wanted` the components it
/// asked for. An `Applied` result needs a readback that confirms every wanted
/// component; native acceptance alone is never enough.
#[must_use]
pub fn classify_frame(
    window: WindowInstance,
    plan_sequence: u64,
    expected: Rect,
    wanted: FrameComponents,
    outcome: &WindowActionOutcome,
) -> OperationOutcome {
    match *outcome {
        WindowActionOutcome::NotIssued(reason) => OperationOutcome::NotAttempted {
            reason: not_attempted(reason),
            window,
            plan_sequence,
        },
        WindowActionOutcome::ExpiredBeforeSend => OperationOutcome::NotAttempted {
            reason: NotAttemptedReason::Expired,
            window,
            plan_sequence,
        },
        WindowActionOutcome::Stale(reason) => OperationOutcome::Stale {
            cause: StaleCause::Window(reason),
            window,
            plan_sequence,
        },
        WindowActionOutcome::Issued { kind, native, observed } => {
            issued(window, plan_sequence, expected, wanted, kind, native, observed)
        }
    }
}

fn issued(
    window: WindowInstance,
    plan_sequence: u64,
    expected: Rect,
    wanted: FrameComponents,
    kind: IssuedKind,
    native: NativeStatus,
    observed: Option<FrameRead>,
) -> OperationOutcome {
    let (position_status, size_status) = match native {
        NativeStatus::Frame { position, size } => (position, size),
        // The frame result was never observed: every wanted component is unknown.
        NativeStatus::Unobserved | NativeStatus::Action(_) => (
            if wanted.position {
                WriteStatus::Failed(AxError::CannotComplete)
            } else {
                WriteStatus::NotRequested
            },
            if wanted.size {
                WriteStatus::Failed(AxError::CannotComplete)
            } else {
                WriteStatus::NotRequested
            },
        ),
    };
    let uncertain_kind = matches!(kind, IssuedKind::Uncertain);
    let position = component(
        wanted.position,
        position_status,
        observed.as_ref().and_then(read_position),
        (expected.x, expected.y),
    );
    let size = component(
        wanted.size,
        size_status,
        observed.as_ref().and_then(read_size),
        (expected.width, expected.height),
    );
    let actual = observed.as_ref().and_then(FrameRead::complete).map(|frame| {
        Rect::new(
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    });
    let evidence = TargetEvidence {
        window,
        plan_sequence,
        expected,
        position,
        size,
        actual,
    };
    let targets = vec![evidence];
    let parts = [position, size];
    let requested: Vec<ComponentEvidence> = parts.into_iter().filter(|c| c.requested()).collect();
    let confirmed = requested.iter().filter(|c| **c == ComponentEvidence::Confirmed).count();
    let unobserved = requested.contains(&ComponentEvidence::Unobserved);
    let refused = requested.iter().filter(|c| matches!(c, ComponentEvidence::Refused(_))).count();

    if requested.is_empty() || unobserved || uncertain_kind {
        return OperationOutcome::Uncertain { targets };
    }
    if confirmed == requested.len() {
        return OperationOutcome::Applied { targets };
    }
    if refused == requested.len() {
        return OperationOutcome::Failed {
            reason: FailureReason::Refused,
            targets,
        };
    }
    OperationOutcome::Partial { targets }
}

#[cfg(test)]
mod tests {
    use stache_macos::ax::{AxPoint, AxSize};
    use stache_macos::windows::{AdapterId, RuntimeEpoch};

    use super::*;

    fn token() -> WindowInstance {
        static ADAPTER: std::sync::OnceLock<AdapterId> = std::sync::OnceLock::new();
        WindowInstance::for_testing(
            *ADAPTER.get_or_init(AdapterId::for_testing),
            RuntimeEpoch::for_testing(1),
            stache_macos::ApplicationId::for_testing(1),
            1,
        )
    }

    const TARGET: Rect = Rect::new(100.0, 100.0, 800.0, 600.0);
    const BOTH: FrameComponents = FrameComponents { position: true, size: true };

    const fn read(x: f64, y: f64, w: f64, h: f64) -> FrameRead {
        FrameRead {
            position: AxReading::Value(AxPoint { x, y }),
            size: AxReading::Value(AxSize { width: w, height: h }),
        }
    }

    fn issued_frame(
        kind: IssuedKind,
        position: WriteStatus,
        size: WriteStatus,
        observed: Option<FrameRead>,
    ) -> WindowActionOutcome {
        WindowActionOutcome::Issued {
            kind,
            native: NativeStatus::Frame { position, size },
            observed,
        }
    }

    fn classify(outcome: &WindowActionOutcome) -> OperationOutcome {
        classify_frame(token(), 7, TARGET, BOTH, outcome)
    }

    #[test]
    fn applied_needs_a_matching_readback_of_every_component() {
        let outcome = classify(&issued_frame(
            IssuedKind::Applied,
            WriteStatus::Written,
            WriteStatus::Written,
            Some(read(100.0, 100.0, 800.0, 600.0)),
        ));
        let OperationOutcome::Applied { targets } = outcome else {
            panic!("expected applied, got {outcome:?}");
        };
        assert_eq!(targets[0].position, ComponentEvidence::Confirmed);
        assert_eq!(targets[0].size, ComponentEvidence::Confirmed);
        assert_eq!(targets[0].plan_sequence, 7);
    }

    #[test]
    fn native_acceptance_without_readback_is_uncertain_not_applied() {
        let outcome = classify(&issued_frame(
            IssuedKind::Applied,
            WriteStatus::Written,
            WriteStatus::Written,
            None,
        ));
        assert!(
            matches!(outcome, OperationOutcome::Uncertain { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_mismatching_readback_is_partial_and_records_the_actual_frame() {
        let outcome = classify(&issued_frame(
            IssuedKind::Applied,
            WriteStatus::Written,
            WriteStatus::Written,
            Some(read(100.0, 100.0, 1000.0, 700.0)),
        ));
        let OperationOutcome::Partial { targets } = &outcome else {
            panic!("expected partial, got {outcome:?}");
        };
        assert_eq!(targets[0].position, ComponentEvidence::Confirmed);
        assert_eq!(targets[0].size, ComponentEvidence::Mismatch);
        assert_eq!(outcome.actual(), Some(Rect::new(100.0, 100.0, 1000.0, 700.0)));
    }

    #[test]
    fn position_applied_and_size_refused_is_partial() {
        let outcome = classify(&issued_frame(
            IssuedKind::Partial,
            WriteStatus::Written,
            WriteStatus::Failed(AxError::ActionUnsupported),
            Some(read(100.0, 100.0, 640.0, 480.0)),
        ));
        let OperationOutcome::Partial { targets } = &outcome else {
            panic!("expected partial, got {outcome:?}");
        };
        assert_eq!(targets[0].position, ComponentEvidence::Confirmed);
        assert_eq!(
            targets[0].size,
            ComponentEvidence::Refused(AxError::ActionUnsupported)
        );
    }

    #[test]
    fn every_component_refused_is_failed_with_the_actual_frame() {
        let outcome = classify(&issued_frame(
            IssuedKind::Rejected,
            WriteStatus::Failed(AxError::AttributeUnsupported),
            WriteStatus::Failed(AxError::AttributeUnsupported),
            Some(read(10.0, 10.0, 500.0, 400.0)),
        ));
        assert!(matches!(outcome, OperationOutcome::Failed { .. }), "{outcome:?}");
        assert_eq!(outcome.actual(), Some(Rect::new(10.0, 10.0, 500.0, 400.0)));
    }

    #[test]
    fn cannot_complete_and_unobserved_are_uncertain() {
        let cannot = classify(&issued_frame(
            IssuedKind::Uncertain,
            WriteStatus::Failed(AxError::CannotComplete),
            WriteStatus::NotRequested,
            None,
        ));
        assert!(matches!(cannot, OperationOutcome::Uncertain { .. }));
        let lost = classify(&WindowActionOutcome::Issued {
            kind: IssuedKind::Uncertain,
            native: NativeStatus::Unobserved,
            observed: None,
        });
        assert!(matches!(lost, OperationOutcome::Uncertain { .. }));
    }

    #[test]
    fn proven_no_send_is_a_typed_reason_never_uncertain() {
        let cases = [
            (
                WindowActionOutcome::NotIssued(NotIssuedReason::Sealed),
                NotAttemptedReason::Sealed,
            ),
            (
                WindowActionOutcome::NotIssued(NotIssuedReason::Dispatch(DispatchError::Full)),
                NotAttemptedReason::Dispatch(DispatchError::Full),
            ),
            (
                WindowActionOutcome::ExpiredBeforeSend,
                NotAttemptedReason::Expired,
            ),
        ];
        for (native, expected) in cases {
            let outcome = classify(&native);
            assert!(native.proves_no_send());
            assert!(outcome.proves_no_send() && !outcome.was_issued(), "{outcome:?}");
            assert_eq!(outcome, OperationOutcome::NotAttempted {
                reason: expected,
                window: token(),
                plan_sequence: 7
            });
        }
        let stale = classify(&WindowActionOutcome::Stale(StaleReason::Destroyed));
        assert_eq!(stale, OperationOutcome::Stale {
            cause: StaleCause::Window(StaleReason::Destroyed),
            window: token(),
            plan_sequence: 7
        });
    }

    #[test]
    fn enqueued_is_admission_only() {
        assert!(!OperationOutcome::Enqueued.was_issued());
        assert!(!OperationOutcome::Enqueued.proves_no_send());
        assert_eq!(OperationOutcome::Enqueued.actual(), None);
    }

    #[test]
    fn only_requested_components_are_judged() {
        let position_only = FrameComponents { position: true, size: false };
        let outcome = classify_frame(
            token(),
            1,
            TARGET,
            position_only,
            &issued_frame(
                IssuedKind::Applied,
                WriteStatus::Written,
                WriteStatus::NotRequested,
                Some(read(100.0, 100.0, 333.0, 222.0)),
            ),
        );
        assert!(
            matches!(outcome, OperationOutcome::Applied { .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_partial_readback_confirms_only_the_half_that_was_read() {
        let half = Some(FrameRead {
            position: AxReading::Value(AxPoint { x: 100.0, y: 100.0 }),
            size: AxReading::Unsupported,
        });
        let outcome = classify(&issued_frame(
            IssuedKind::Applied,
            WriteStatus::Written,
            WriteStatus::Written,
            half,
        ));
        // The size half has no readback, so the operation cannot be Applied.
        assert!(
            matches!(outcome, OperationOutcome::Uncertain { .. }),
            "{outcome:?}"
        );
        assert_eq!(outcome.actual(), None);
    }
}
