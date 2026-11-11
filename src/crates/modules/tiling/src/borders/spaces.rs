//! The pure Spaces and Mission Control gate (part 05).
//!
//! Approach only, never code (decision 5): the reference border tool's `src/events.c`
//! (`space_handler`: delayed redraw after a Space change), `src/border.c` and
//! `src/misc/window.h` (per-Space placement, no draw on an invisible Space) and
//! yabai `src/mission_control.c` (Dock Accessibility notifications) were
//! studied for which signals exist. The state machine here is written from the
//! spec.
//!
//! # Reasons
//!
//! [`SpaceGate`] holds a set of [`SuspendReason`]s.
//!
//! * **Global** reasons (`MissionControl`, `SpaceTransition { display }`,
//!   `Unknown { cause }`) suspend every border. Entering one that was not held
//!   yields one [`GateEffect::HideAll`] and bumps the gate epoch. While any is
//!   held the engine sends no `Show` and no `Order`. Overlapping reasons resume
//!   only after the last clears.
//! * `FullscreenTransition { window }` is **window-scoped**: the spec's single
//!   "every reason sends `HideAll`" is refined here, because a fullscreen
//!   transition of one window must not blank the borders of unrelated windows.
//!   Entering it yields [`GateEffect::HideWindow`] (an ordinary `Hide` of that
//!   one overlay) and does **not** bump the epoch: a per-handle `Hide` is
//!   ordered after any earlier `Show` of the same handle by the control's merge,
//!   so no epoch is needed to keep it hidden. Until the reason clears, that
//!   window is not shown ([`SpaceGate::window_blocked`]); leaving it re-confirms
//!   that window like a reveal ([`Resume::Window`]).
//!
//! # Resuming
//!
//! Leaving the last global reason yields [`Resume::All`]: the engine
//! re-confirms every bordered window on the active Space of its display, calls
//! [`SpaceGate::confirmed`] when the confirmation succeeded, and the show rule
//! of part 04 plus one display frame then shows what is due. Model updates
//! continue while suspended, so a resume shows the current state.
//!
//! # Fail closed
//!
//! The gate starts in `Unknown { SourcesNotStarted }`. Exactly one diagnostic
//! ([`SpaceGate::diagnostic`]) is published while any `Unknown` holds. It
//! clears only after the cause cleared **and** a full re-confirmation
//! succeeded: a cleared cause stays listed until [`SpaceGate::confirmed`].
//!
//! Facts may be duplicated or out of order: entering a held reason and leaving
//! one that is not held are no-ops. The gate reads no clock; timing belongs to
//! the engine's show rule.

use std::collections::BTreeSet;

use stache_macos::DisplayId;
use stache_macos::border_overlay::{HideReason, SpaceFact, UnknownCause};
use stache_macos::windows::WindowInstance;

/// Why borders are suspended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuspendReason {
    /// Mission Control, App Exposé or Show Desktop.
    MissionControl,
    /// A Space transition on one display.
    SpaceTransition {
        /// The display.
        display: DisplayId,
    },
    /// A fullscreen transition of one window (window-scoped).
    FullscreenTransition {
        /// The window.
        window: WindowInstance,
    },
    /// The state cannot be determined.
    Unknown {
        /// Why.
        cause: UnknownCause,
    },
}

impl SuspendReason {
    /// The reason sent with `HideAll`.
    #[must_use]
    pub const fn hide_reason(self) -> HideReason {
        match self {
            Self::MissionControl => HideReason::MissionControl,
            Self::SpaceTransition { .. } => HideReason::SpaceChange,
            Self::FullscreenTransition { .. } => HideReason::Fullscreen,
            Self::Unknown { .. } => HideReason::Unknown,
        }
    }

    /// Whether the reason concerns one window only.
    #[must_use]
    pub const fn window_scoped(self) -> bool { matches!(self, Self::FullscreenTransition { .. }) }
}

/// What the engine must send because a reason was entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateEffect {
    /// Hide every overlay, under the new gate epoch.
    HideAll {
        /// Why.
        reason: HideReason,
        /// The epoch after the bump.
        epoch: u64,
    },
    /// Hide the overlay of one window.
    HideWindow {
        /// The window.
        window: WindowInstance,
    },
}

/// What the engine must re-confirm because a reason was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// The last global reason cleared: re-confirm every bordered window.
    All,
    /// A window-scoped reason cleared with no global reason held: re-confirm
    /// that window.
    Window(WindowInstance),
}

/// The result of feeding one fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GateStep {
    /// Something to hide, now.
    pub effect: Option<GateEffect>,
    /// Something to re-confirm.
    pub resume: Option<Resume>,
}

/// The gate state machine; see the module documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceGate {
    reasons: BTreeSet<SuspendReason>,
    epoch: u64,
    /// Causes that cleared but have not been followed by a full confirmation.
    awaiting: BTreeSet<UnknownCause>,
}

impl Default for SpaceGate {
    fn default() -> Self { Self::new() }
}

impl SpaceGate {
    /// The startup gate: `Unknown { SourcesNotStarted }` until a source reports
    /// that it is registered and a confirmation succeeded. Nothing is shown yet,
    /// so no `HideAll` is owed and the epoch is 0.
    #[must_use]
    pub fn new() -> Self {
        let mut reasons = BTreeSet::new();
        reasons.insert(SuspendReason::Unknown {
            cause: UnknownCause::SourcesNotStarted,
        });
        Self {
            reasons,
            epoch: 0,
            awaiting: BTreeSet::new(),
        }
    }

    /// A gate with no reason held (pure tests).
    #[must_use]
    pub const fn open() -> Self {
        Self {
            reasons: BTreeSet::new(),
            epoch: 0,
            awaiting: BTreeSet::new(),
        }
    }

    /// The gate epoch: bumped each time a global reason is entered.
    #[must_use]
    pub const fn epoch(&self) -> u64 { self.epoch }

    /// Whether a global reason is held (no border may be shown or re-ordered).
    #[must_use]
    pub fn suspended(&self) -> bool { self.reasons.iter().any(|reason| !reason.window_scoped()) }

    /// Whether a window-scoped reason keeps `window`'s border hidden.
    #[must_use]
    pub fn window_blocked(&self, window: WindowInstance) -> bool {
        self.reasons.contains(&SuspendReason::FullscreenTransition { window })
    }

    /// Whether `reason` is held.
    #[must_use]
    pub fn holds(&self, reason: SuspendReason) -> bool { self.reasons.contains(&reason) }

    /// The held reasons.
    pub fn reasons(&self) -> impl Iterator<Item = SuspendReason> + '_ {
        self.reasons.iter().copied()
    }

    /// Enters a reason. `None` for a duplicate.
    pub fn enter(&mut self, reason: SuspendReason) -> Option<GateEffect> {
        if !self.reasons.insert(reason) {
            return None;
        }
        if let SuspendReason::FullscreenTransition { window } = reason {
            return Some(GateEffect::HideWindow { window });
        }
        self.epoch += 1;
        Some(GateEffect::HideAll {
            reason: reason.hide_reason(),
            epoch: self.epoch,
        })
    }

    /// Leaves a reason. `None` when it was not held or other global reasons
    /// remain.
    pub fn leave(&mut self, reason: SuspendReason) -> Option<Resume> {
        if !self.reasons.remove(&reason) {
            return None;
        }
        if let SuspendReason::Unknown { cause } = reason {
            self.awaiting.insert(cause);
        }
        if self.suspended() {
            return None;
        }
        match reason {
            SuspendReason::FullscreenTransition { window } => Some(Resume::Window(window)),
            _ => Some(Resume::All),
        }
    }

    /// Feeds one fact from a source.
    pub fn apply(&mut self, fact: SpaceFact) -> GateStep {
        let mut step = GateStep::default();
        let (reason, entering) = match fact {
            // Logged by the engine; never a reason.
            SpaceFact::Diagnostic(_) => return step,
            SpaceFact::MissionControlEntered => (SuspendReason::MissionControl, true),
            SpaceFact::MissionControlExited => (SuspendReason::MissionControl, false),
            SpaceFact::SpaceChangeBegan { display } => {
                (SuspendReason::SpaceTransition { display }, true)
            }
            SpaceFact::SpaceChangeCompleted { display } => {
                (SuspendReason::SpaceTransition { display }, false)
            }
            SpaceFact::FullscreenChanged { window, entering } => {
                (SuspendReason::FullscreenTransition { window }, entering)
            }
            SpaceFact::SourceLost(cause) => (SuspendReason::Unknown { cause }, true),
            SpaceFact::SourceRestored(cause) => (SuspendReason::Unknown { cause }, false),
            SpaceFact::DisplayLayoutChanging => (
                SuspendReason::Unknown {
                    cause: UnknownCause::DisplayChange,
                },
                true,
            ),
            SpaceFact::DisplayLayoutChanged => (
                SuspendReason::Unknown {
                    cause: UnknownCause::DisplayChange,
                },
                false,
            ),
        };
        if entering {
            step.effect = self.enter(reason);
            return step;
        }
        step.resume = self.leave(reason);
        // A Space query is retried only when the world changed: any settling
        // fact clears a held `QueryFailed`, which re-confirms (never on a timer).
        let settling = !matches!(fact, SpaceFact::SourceRestored(UnknownCause::QueryFailed));
        if settling {
            let retry = self.leave(SuspendReason::Unknown {
                cause: UnknownCause::QueryFailed,
            });
            step.resume = merge_resume(step.resume, retry);
        }
        step
    }

    /// A fact was lost (mailbox overflow): every reason learned from facts is
    /// forgotten except the startup one, `Unknown { MailboxOverflow }` is
    /// entered and the source must report its state in full. The effect is a
    /// `HideAll`.
    pub fn overflow(&mut self) -> Option<GateEffect> {
        let startup = SuspendReason::Unknown {
            cause: UnknownCause::SourcesNotStarted,
        };
        let keep_startup = self.reasons.contains(&startup);
        self.reasons.clear();
        if keep_startup {
            self.reasons.insert(startup);
        }
        self.enter(SuspendReason::Unknown {
            cause: UnknownCause::MailboxOverflow,
        })
    }

    /// A full re-confirmation succeeded: causes that cleared are no longer
    /// reported.
    pub fn confirmed(&mut self) { self.awaiting.clear(); }

    /// The one diagnostic while any `Unknown` holds or a cleared one awaits its
    /// confirmation.
    #[must_use]
    pub fn diagnostic(&self) -> Option<String> {
        let mut causes: BTreeSet<UnknownCause> = self.awaiting.clone();
        causes.extend(self.reasons.iter().filter_map(|reason| match reason {
            SuspendReason::Unknown { cause } => Some(*cause),
            _ => None,
        }));
        if causes.is_empty() {
            return None;
        }
        let list: Vec<String> = causes.iter().map(ToString::to_string).collect();
        Some(format!(
            "borders: Spaces or Mission Control state cannot be determined ({}); borders hidden",
            list.join(", ")
        ))
    }
}

const fn merge_resume(first: Option<Resume>, second: Option<Resume>) -> Option<Resume> {
    match (first, second) {
        (Some(Resume::All), _) | (_, Some(Resume::All)) => Some(Resume::All),
        (None, other) | (other, _) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Feed;

    fn display(n: u32) -> DisplayId { DisplayId::new(n) }

    fn unknown(cause: UnknownCause) -> SuspendReason { SuspendReason::Unknown { cause } }

    fn gate() -> SpaceGate { SpaceGate::open() }

    #[test]
    fn a_diagnostic_fact_never_suspends_resumes_or_bumps() {
        use stache_macos::border_overlay::SpaceDiagnostic;
        let mut gate = gate();
        let fact = SpaceFact::Diagnostic(SpaceDiagnostic::PrivateEvent { event: 1401 });
        assert_eq!(gate.apply(fact), GateStep::default());
        let _ = gate.enter(SuspendReason::MissionControl);
        assert_eq!(gate.apply(fact), GateStep::default());
        assert!(gate.suspended(), "a diagnostic does not settle anything");
        assert_eq!(gate.epoch(), 1);
    }

    // ---- TB-05-01: each reason enters suspension with one HideAll and a bump

    #[test]
    fn each_global_reason_enters_with_one_hide_all_and_an_epoch_bump() {
        let reasons = [
            (SuspendReason::MissionControl, HideReason::MissionControl),
            (
                SuspendReason::SpaceTransition { display: display(1) },
                HideReason::SpaceChange,
            ),
            (unknown(UnknownCause::QueryFailed), HideReason::Unknown),
        ];
        for (reason, hide) in reasons {
            let mut gate = gate();
            assert!(!gate.suspended());
            assert_eq!(
                gate.enter(reason),
                Some(GateEffect::HideAll { reason: hide, epoch: 1 })
            );
            assert_eq!(gate.epoch(), 1);
            assert!(gate.suspended());
            assert_eq!(gate.enter(reason), None, "a duplicate sends nothing");
            assert_eq!(gate.epoch(), 1, "and does not bump");
        }
    }

    #[test]
    fn a_fullscreen_transition_hides_one_window_without_suspending_or_bumping() {
        let feed = Feed::new(1);
        let window = |n: u64| feed.token(1, n);
        let mut gate = gate();
        let reason = SuspendReason::FullscreenTransition { window: window(1) };
        assert_eq!(
            gate.enter(reason),
            Some(GateEffect::HideWindow { window: window(1) })
        );
        assert_eq!(gate.epoch(), 0);
        assert!(!gate.suspended(), "other borders keep working");
        assert!(gate.window_blocked(window(1)));
        assert!(!gate.window_blocked(window(2)));
        assert_eq!(gate.leave(reason), Some(Resume::Window(window(1))));
        assert!(!gate.window_blocked(window(1)));
    }

    #[test]
    fn overlapping_reasons_resume_only_after_the_last_clears() {
        let mut gate = gate();
        let space = SuspendReason::SpaceTransition { display: display(1) };
        assert!(gate.enter(SuspendReason::MissionControl).is_some());
        assert!(gate.enter(space).is_some());
        assert_eq!(gate.epoch(), 2, "each entered reason bumps");
        assert_eq!(gate.leave(SuspendReason::MissionControl), None);
        assert!(gate.suspended());
        assert_eq!(gate.leave(space), Some(Resume::All));
        assert!(!gate.suspended());
        assert_eq!(gate.leave(space), None, "leaving a reason not held is a no-op");
    }

    #[test]
    fn a_window_reason_clearing_inside_a_suspension_waits_for_the_full_resume() {
        let feed = Feed::new(1);
        let window = |n: u64| feed.token(1, n);
        let mut gate = gate();
        let fullscreen = SuspendReason::FullscreenTransition { window: window(1) };
        let _ = gate.enter(fullscreen);
        let _ = gate.enter(SuspendReason::MissionControl);
        assert_eq!(gate.leave(fullscreen), None);
        assert_eq!(gate.leave(SuspendReason::MissionControl), Some(Resume::All));
    }

    #[test]
    fn facts_map_to_reasons_and_tolerate_duplicates_and_disorder() {
        let feed = Feed::new(1);
        let window = |n: u64| feed.token(1, n);
        let mut gate = gate();
        // Exit and completion without a start are no-ops.
        assert_eq!(gate.apply(SpaceFact::MissionControlExited), GateStep::default());
        assert_eq!(
            gate.apply(SpaceFact::SpaceChangeCompleted { display: display(1) }),
            GateStep::default()
        );
        let entered = gate.apply(SpaceFact::MissionControlEntered);
        assert!(matches!(
            entered.effect,
            Some(GateEffect::HideAll { epoch: 1, .. })
        ));
        assert_eq!(gate.apply(SpaceFact::MissionControlEntered), GateStep::default());
        assert_eq!(gate.epoch(), 1);
        let exited = gate.apply(SpaceFact::MissionControlExited);
        assert_eq!(exited.resume, Some(Resume::All));
        assert_eq!(gate.apply(SpaceFact::MissionControlExited), GateStep::default());

        let began = gate.apply(SpaceFact::SpaceChangeBegan { display: display(2) });
        assert!(matches!(
            began.effect,
            Some(GateEffect::HideAll {
                reason: HideReason::SpaceChange,
                ..
            })
        ));
        // Another display's completion does not clear this display's reason.
        assert_eq!(
            gate.apply(SpaceFact::SpaceChangeCompleted { display: display(1) }).resume,
            None
        );
        assert_eq!(
            gate.apply(SpaceFact::SpaceChangeCompleted { display: display(2) }).resume,
            Some(Resume::All)
        );

        let layout = gate.apply(SpaceFact::DisplayLayoutChanging);
        assert!(layout.effect.is_some());
        assert_eq!(
            gate.apply(SpaceFact::DisplayLayoutChanged).resume,
            Some(Resume::All)
        );

        let enter = gate.apply(SpaceFact::FullscreenChanged {
            window: window(3),
            entering: true,
        });
        assert_eq!(enter.effect, Some(GateEffect::HideWindow { window: window(3) }));
        let leave = gate.apply(SpaceFact::FullscreenChanged {
            window: window(3),
            entering: false,
        });
        assert_eq!(leave.resume, Some(Resume::Window(window(3))));
    }

    // ---- TB-05-03 (pure half): the fail-closed diagnostic

    #[test]
    fn the_gate_starts_closed_with_one_diagnostic_and_opens_after_registration_and_confirmation() {
        let mut gate = SpaceGate::new();
        assert!(gate.suspended());
        assert_eq!(gate.epoch(), 0);
        assert_eq!(
            gate.diagnostic().as_deref(),
            Some(
                "borders: Spaces or Mission Control state cannot be determined (sources not started); borders hidden"
            )
        );
        let step = gate.apply(SpaceFact::SourceRestored(UnknownCause::SourcesNotStarted));
        assert_eq!(step.resume, Some(Resume::All));
        assert!(!gate.suspended());
        assert!(
            gate.diagnostic().is_some(),
            "still reported until the confirmation succeeds"
        );
        gate.confirmed();
        assert_eq!(gate.diagnostic(), None);
    }

    #[test]
    fn every_fail_closed_cause_holds_one_diagnostic_until_cleared_and_confirmed() {
        let causes = [
            UnknownCause::DockUnregistered,
            UnknownCause::MailboxOverflow,
            UnknownCause::MissingSymbol("SLSExample"),
            UnknownCause::RegistrationFailed,
            UnknownCause::QueryFailed,
            UnknownCause::DisplayChange,
            UnknownCause::OwnerStopped,
        ];
        for cause in causes {
            let mut gate = gate();
            let lost = gate.apply(SpaceFact::SourceLost(cause));
            assert!(
                matches!(lost.effect, Some(GateEffect::HideAll { .. })),
                "{cause}"
            );
            assert!(gate.suspended(), "{cause}");
            let message = gate.diagnostic().expect("diagnostic");
            assert!(message.contains(&cause.to_string()), "{message}");
            assert_eq!(message.matches("borders:").count(), 1, "one diagnostic");

            let restored = gate.apply(SpaceFact::SourceRestored(cause));
            assert_eq!(restored.resume, Some(Resume::All), "{cause}");
            assert!(
                gate.diagnostic().is_some(),
                "{cause}: cleared only after confirmation"
            );
            gate.confirmed();
            assert_eq!(gate.diagnostic(), None, "{cause}");
        }
    }

    #[test]
    fn several_causes_share_one_diagnostic() {
        let mut gate = gate();
        let _ = gate.apply(SpaceFact::SourceLost(UnknownCause::DockUnregistered));
        let _ = gate.apply(SpaceFact::SourceLost(UnknownCause::OwnerStopped));
        let message = gate.diagnostic().expect("diagnostic");
        assert_eq!(message.matches("borders:").count(), 1);
        assert!(
            message.contains("Dock not registered") && message.contains("owner thread stopped")
        );
    }

    #[test]
    fn an_overflow_forgets_fact_reasons_and_waits_for_the_source_to_resync() {
        let mut gate = gate();
        let _ = gate.apply(SpaceFact::MissionControlEntered);
        let _ = gate.apply(SpaceFact::SourceLost(UnknownCause::DockUnregistered));
        let before = gate.epoch();
        let effect = gate.overflow();
        assert!(matches!(
            effect,
            Some(GateEffect::HideAll {
                reason: HideReason::Unknown,
                ..
            })
        ));
        assert_eq!(gate.epoch(), before + 1);
        assert!(gate.suspended());
        assert!(!gate.holds(SuspendReason::MissionControl));
        let step = gate.apply(SpaceFact::SourceRestored(UnknownCause::MailboxOverflow));
        assert_eq!(step.resume, Some(Resume::All));
    }

    #[test]
    fn an_overflow_keeps_the_startup_reason() {
        let mut gate = SpaceGate::new();
        let _ = gate.overflow();
        assert!(gate.holds(unknown(UnknownCause::SourcesNotStarted)));
        let step = gate.apply(SpaceFact::SourceRestored(UnknownCause::MailboxOverflow));
        assert_eq!(step.resume, None, "still not started");
    }

    #[test]
    fn a_failed_query_is_retried_only_when_the_world_changes() {
        let mut gate = gate();
        let _ = gate.enter(unknown(UnknownCause::QueryFailed));
        // No timer: nothing clears it by itself.
        assert!(gate.suspended());
        let step = gate.apply(SpaceFact::SpaceChangeCompleted { display: display(1) });
        assert_eq!(step.resume, Some(Resume::All), "a settling fact retries");
        // The restoration fact of the same cause is not itself a retry trigger.
        let _ = gate.enter(unknown(UnknownCause::QueryFailed));
        let step = gate.apply(SpaceFact::SourceRestored(UnknownCause::QueryFailed));
        assert_eq!(step.resume, Some(Resume::All));
    }
}
