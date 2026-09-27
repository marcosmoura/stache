//! Stache-hidden application ownership and shutdown restoration.
//!
//! Phase 6 model: the actor is the sole runtime writer of ownership via a
//! sealed `VisibilityRegistry` (see below); shutdown restoration seals and
//! drains the registry and restores each owned identity with exact-instance
//! validation.

use std::collections::BTreeSet;

use parking_lot::Mutex;

use super::effects::window_ops::{HideAppOutcome, UnhideAppOutcome};
use crate::modules::tiling::identity::AppIdentity;

/// Summary of a best-effort restoration attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreSummary {
    /// Number of tracked applications for which restoration was attempted.
    pub attempted: usize,
    /// Number of applications successfully unhidden.
    pub restored: usize,
}

use objc::runtime::{BOOL, YES};
use objc::{msg_send, sel, sel_impl};

/// Narrow decision seam. One resolved app value flows through identity lookup,
/// hidden-state lookup, and unhide, so tests run the same ordering logic.
#[must_use]
fn restore_one_exact_with<T>(
    owned: AppIdentity,
    resolve: impl FnOnce(i32) -> Option<T>,
    identity_of: impl FnOnce(&T) -> Option<AppIdentity>,
    hidden_of: impl FnOnce(&T) -> Option<bool>,
    unhide: impl FnOnce(&T) -> bool,
) -> bool {
    let Some(app) = resolve(owned.pid) else {
        return false;
    };
    if identity_of(&app) != Some(owned) {
        return false;
    }
    if hidden_of(&app) != Some(true) {
        return false;
    }
    unhide(&app)
}

/// Production wrapper. The raw pointer exists only inside this autorelease
/// pool and synchronous call; it is never stored or sent across threads.
#[must_use]
fn restore_one_exact(owned: AppIdentity) -> bool {
    let owned_pid = owned.pid;
    objc::rc::autoreleasepool(|| {
        restore_one_exact_with(
            owned,
            |pid| unsafe {
                let class = objc::runtime::Class::get("NSRunningApplication")?;
                let app: *mut objc::runtime::Object = msg_send![
                    class,
                    runningApplicationWithProcessIdentifier: pid
                ];
                (!app.is_null()).then_some(app)
            },
            // `owned_pid` is authoritative: apps that report NSNotFound for
            // `processIdentifier` still resolve their exact identity.
            |app| unsafe { AppIdentity::from_ns_running_app_with_pid(*app, owned_pid) },
            |app| unsafe {
                let hidden: BOOL = msg_send![*app, isHidden];
                Some(hidden == YES)
            },
            |app| unsafe {
                let result: BOOL = msg_send![*app, unhide];
                result == YES
            },
        )
    })
}

/// Explicit loop so `FnMut(AppIdentity)` is called with an owned identity.
/// Every identity is attempted even when an earlier restore fails.
#[must_use]
pub fn restore_from_with(
    identities: Vec<AppIdentity>,
    restore: impl FnMut(AppIdentity) -> bool,
) -> RestoreSummary {
    let attempted = identities.len();
    let mut restore = restore;
    let mut restored = 0;
    for identity in identities {
        let did_restore = restore(identity);
        tracing::debug!(
            pid = identity.pid,
            launch_date_bits = identity.launch_date.bits(),
            restored = did_restore,
            "shutdown restore result for Stache-owned application"
        );
        if did_restore {
            restored += 1;
        }
    }
    RestoreSummary { attempted, restored }
}

/// Seals, drains, then restores each owned identity with exact-instance
/// validation.
///
/// The handle must be alive (restoration runs before tiling shutdown closes
/// the actor channel). No registry lock is held during restoration.
#[must_use]
pub fn restore_stache_hidden_apps() -> RestoreSummary {
    crate::modules::tiling::init::get_handle().map_or(
        RestoreSummary { attempted: 0, restored: 0 },
        |handle| {
            let identities = handle.seal_and_drain_visibility();
            restore_from_with(identities, restore_one_exact)
        },
    )
}

/// Passive registry shared between `StateActor` and `StateActorHandle`.
///
/// The actor is the sole runtime writer. The handle's `seal_and_drain_visibility`
/// is the only external mutation API. Raw `RegistryState` fields are private —
/// actor code uses the closure methods `hide_if_open`/`unhide_if_open` and the
/// seal-aware `relinquish_if_open`.
pub struct VisibilityRegistry {
    inner: Mutex<RegistryState>,
}

#[derive(Debug)]
struct RegistryState {
    sealed: bool,
    owned: BTreeSet<AppIdentity>,
}

impl Default for VisibilityRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(RegistryState {
                sealed: false,
                owned: BTreeSet::new(),
            }),
        }
    }
}

#[allow(dead_code)] // actor/restore consumers land in the 19D cutover
impl VisibilityRegistry {
    /// Atomically: sealed check → OS op → `BTreeSet` mutation.
    /// If sealed, returns `HideAppOutcome::Failed` without calling `op`.
    pub(crate) fn hide_if_open(
        &self,
        identity: AppIdentity,
        op: impl FnOnce(AppIdentity) -> HideAppOutcome,
    ) -> HideAppOutcome {
        let mut state = self.inner.lock();
        if state.sealed {
            return HideAppOutcome::Failed;
        }
        let outcome = op(identity);
        if outcome == HideAppOutcome::HiddenByStache {
            state.owned.insert(identity);
        }
        outcome
    }

    /// Atomically: sealed check → OS op → `BTreeSet` mutation.
    /// If sealed, returns `UnhideAppOutcome::Failed` without calling `op`.
    pub(crate) fn unhide_if_open(
        &self,
        identity: AppIdentity,
        op: impl FnOnce(AppIdentity) -> UnhideAppOutcome,
    ) -> UnhideAppOutcome {
        let mut state = self.inner.lock();
        if state.sealed {
            return UnhideAppOutcome::Failed;
        }
        let outcome = op(identity);
        if matches!(
            outcome,
            UnhideAppOutcome::UnhiddenByStache | UnhideAppOutcome::AlreadyShown
        ) {
            state.owned.remove(&identity);
        }
        outcome
    }

    /// Actor-owned, seal-aware relinquishment. Returns false (no mutation) if
    /// the registry is sealed. Used by the actor's revalidation and exact
    /// termination paths; `EventProcessor` never mutates ownership.
    pub(crate) fn relinquish_if_open(&self, identity: &AppIdentity) -> bool {
        let mut state = self.inner.lock();
        if state.sealed {
            return false;
        }
        state.owned.remove(identity)
    }

    /// Inserts an identity after a seal check. Test/revalidation-only; not the
    /// workspace hide/unhide hot path.
    pub(crate) fn insert(&self, identity: AppIdentity) -> bool {
        let mut state = self.inner.lock();
        if state.sealed {
            return false;
        }
        state.owned.insert(identity)
    }

    pub(crate) fn contains(&self, identity: &AppIdentity) -> bool {
        self.inner.lock().owned.contains(identity)
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize { self.inner.lock().owned.len() }

    pub(crate) fn is_empty(&self) -> bool { self.inner.lock().owned.is_empty() }

    pub(crate) fn sealed(&self) -> bool { self.inner.lock().sealed }

    /// Atomically seals and drains in a single lock acquisition.
    /// This is the only mutation API exposed outside the actor/controller.
    pub(crate) fn seal_and_drain(&self) -> Vec<AppIdentity> {
        let mut state = self.inner.lock();
        state.sealed = true;
        let mut result: Vec<_> = state.owned.iter().copied().collect();
        result.sort_unstable();
        state.owned.clear();
        result
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use crate::modules::tiling::identity::LaunchDateBits;

    #[allow(clippy::cast_precision_loss)] // test-only identity construction
    fn bits(v: u64) -> LaunchDateBits {
        LaunchDateBits::from_time_interval_since_reference_date(v as f64).unwrap()
    }
    fn id10_1() -> AppIdentity {
        AppIdentity {
            pid: 10_i32,
            launch_date: bits(1),
        }
    }
    fn id20_2() -> AppIdentity {
        AppIdentity {
            pid: 20_i32,
            launch_date: bits(2),
        }
    }
    fn id30_3() -> AppIdentity {
        AppIdentity {
            pid: 30_i32,
            launch_date: bits(3),
        }
    }

    #[test]
    fn registry_accepts_and_drains_identities() {
        let reg = VisibilityRegistry::default();
        let id1 = id10_1();
        let id2 = id20_2();
        assert!(!reg.sealed());
        assert!(reg.insert(id1));
        assert!(reg.insert(id2));
        assert!(reg.contains(&id1));
        assert_eq!(reg.len(), 2);
        let drained = reg.seal_and_drain();
        assert_eq!(drained, vec![id1, id2]); // sorted by Ord
        assert!(reg.is_empty());
        reg.insert(id30_3());
        assert!(reg.is_empty(), "late insert after seal is a no-op");
    }

    #[allow(clippy::cast_sign_loss)] // test uses small positive pids as launch dates
    #[test]
    fn registry_thread_safety() {
        use std::sync::Arc;
        use std::thread;
        let reg = Arc::new(VisibilityRegistry::default());
        let ids: Vec<_> = (1..=100_i32)
            .map(|i| AppIdentity {
                pid: i,
                launch_date: bits(i as u64),
            })
            .collect();
        let mut handles = Vec::new();
        for chunk in ids.chunks(25) {
            let r = Arc::clone(&reg);
            let c = chunk.to_vec();
            handles.push(thread::spawn(move || {
                for id in c {
                    r.insert(id);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(reg.len(), 100);
        assert_eq!(reg.seal_and_drain().len(), 100);
    }

    #[test]
    fn registry_seal_rejects_actor_ops() {
        let reg = VisibilityRegistry::default();
        reg.seal_and_drain();
        assert_eq!(
            reg.hide_if_open(id10_1(), |id| { panic!("no OS op after seal: {id:?}") }),
            HideAppOutcome::Failed
        );
        assert_eq!(
            reg.unhide_if_open(id10_1(), |id| { panic!("no OS op after seal: {id:?}") }),
            UnhideAppOutcome::Failed
        );
        assert!(
            !reg.relinquish_if_open(&id10_1()),
            "relinquishment after seal is a no-op"
        );
    }

    #[test]
    fn registry_relinquishment_is_seal_aware() {
        let reg = VisibilityRegistry::default();
        reg.insert(id10_1());
        assert!(reg.relinquish_if_open(&id10_1()));
        assert!(!reg.contains(&id10_1()));
        assert!(
            !reg.relinquish_if_open(&id10_1()),
            "absent identity relinquishes nothing"
        );
    }
}

#[cfg(test)]
mod restoration_tests {
    use super::*;
    use crate::modules::tiling::identity::LaunchDateBits;

    #[derive(Clone, Copy)]
    struct FakeApp {
        identity: AppIdentity,
        hidden: bool,
    }

    #[allow(clippy::cast_precision_loss)] // test-only identity construction
    fn bits(v: u64) -> LaunchDateBits {
        LaunchDateBits::from_time_interval_since_reference_date(v as f64).unwrap()
    }

    #[test]
    fn restore_proves_identity_equality_before_action() {
        let identity_a = AppIdentity {
            pid: 42,
            launch_date: bits(100),
        };
        let identity_a_reused = AppIdentity {
            pid: 42,
            launch_date: bits(200),
        };
        let mut actions = Vec::new();

        let restored = restore_one_exact_with(
            identity_a,
            |_| {
                Some(FakeApp {
                    identity: identity_a_reused,
                    hidden: true,
                })
            },
            |app| Some(app.identity),
            |app| Some(app.hidden),
            |app| {
                actions.push(app.identity);
                true
            },
        );

        assert!(!restored);
        assert!(actions.is_empty(), "no action for mismatched identity");
    }

    #[test]
    fn restore_skips_same_pid_replacement_for_stale_owned_identity() {
        let identity_a = AppIdentity {
            pid: 42,
            launch_date: bits(100),
        };
        let identity_b = AppIdentity {
            pid: 42,
            launch_date: bits(200),
        };
        let registry = VisibilityRegistry::default();
        registry.insert(identity_a); // Simulates a missed A termination event.

        let mut unhide_calls = 0;
        let summary = restore_from_with(registry.seal_and_drain(), |owned| {
            restore_one_exact_with(
                owned,
                |_| {
                    Some(FakeApp {
                        identity: identity_b,
                        hidden: true,
                    })
                },
                |app| Some(app.identity),
                |app| Some(app.hidden),
                |_| {
                    unhide_calls += 1;
                    true
                },
            )
        });

        assert_eq!(summary, RestoreSummary { attempted: 1, restored: 0 });
        assert_eq!(unhide_calls, 0, "replacement process must remain untouched");
    }

    #[test]
    fn restore_proves_correct_identity_is_unhidden() {
        let identity_a = AppIdentity { pid: 10, launch_date: bits(1) };
        let identity_b = AppIdentity { pid: 20, launch_date: bits(2) };

        let mut actions = Vec::new();
        let summary = restore_from_with(vec![identity_a, identity_b], |identity| {
            restore_one_exact_with(
                identity,
                |_| Some(FakeApp { identity, hidden: true }),
                |app| Some(app.identity),
                |app| Some(app.hidden),
                |app| {
                    actions.push(app.identity);
                    true
                },
            )
        });

        assert_eq!(summary.attempted, 2);
        assert_eq!(summary.restored, 2);
        assert_eq!(actions, vec![identity_a, identity_b]);
    }

    #[test]
    fn restore_skips_not_hidden_without_unhide() {
        let identity = AppIdentity { pid: 10, launch_date: bits(1) };
        let mut actions = Vec::new();
        assert!(!restore_one_exact_with(
            identity,
            |_| Some(FakeApp { identity, hidden: false }),
            |app| Some(app.identity),
            |app| Some(app.hidden),
            |app| {
                actions.push(app.identity);
                true
            },
        ));
        assert!(actions.is_empty());
    }

    #[test]
    fn restore_attempts_later_identities_after_failure() {
        let first = AppIdentity { pid: 10, launch_date: bits(1) };
        let second = AppIdentity { pid: 20, launch_date: bits(2) };
        let mut calls = Vec::new();
        let summary = restore_from_with(vec![first, second], |identity| {
            restore_one_exact_with(
                identity,
                |_| Some(FakeApp { identity, hidden: true }),
                |app| Some(app.identity),
                |app| Some(app.hidden),
                |app| {
                    calls.push(app.identity);
                    app.identity == second
                },
            )
        });
        assert_eq!(summary, RestoreSummary { attempted: 2, restored: 1 });
        assert_eq!(calls, vec![first, second]);
    }

    #[test]
    fn empty_identities_list_is_no_op() {
        let summary = restore_from_with(vec![], |_| unreachable!());
        assert_eq!(summary, RestoreSummary { attempted: 0, restored: 0 });
    }

    #[test]
    fn restore_works_through_registry_seal_and_drain() {
        let registry = VisibilityRegistry::default();
        registry.insert(AppIdentity { pid: 10, launch_date: bits(1) });
        registry.insert(AppIdentity { pid: 20, launch_date: bits(2) });
        let mut restored = Vec::new();
        let summary = restore_from_with(registry.seal_and_drain(), |identity| {
            restore_one_exact_with(
                identity,
                |_| Some(FakeApp { identity, hidden: true }),
                |app| Some(app.identity),
                |app| Some(app.hidden),
                |app| {
                    restored.push(app.identity);
                    true
                },
            )
        });
        assert_eq!(summary.attempted, 2);
        assert_eq!(summary.restored, 2);
        assert_eq!(restored.len(), 2);
        assert!(registry.is_empty());
    }

    #[test]
    fn relinquishment_is_seal_aware_after_drain() {
        let registry = VisibilityRegistry::default();
        registry.insert(AppIdentity { pid: 10, launch_date: bits(1) });
        registry.seal_and_drain();
        assert!(!registry.relinquish_if_open(&AppIdentity { pid: 10, launch_date: bits(1) }));
    }

    #[test]
    fn fresh_registry_generation_is_unsealed_and_distinct() {
        // A resume must install a new unsealed registry generation that does not
        // inherit the drained contents of the previous one.
        let first = VisibilityRegistry::default();
        first.insert(AppIdentity { pid: 10, launch_date: bits(1) });
        let drained = first.seal_and_drain();
        assert_eq!(drained.len(), 1);

        let second = VisibilityRegistry::default();
        assert!(!second.sealed());
        assert!(second.insert(AppIdentity { pid: 20, launch_date: bits(2) }));
        assert!(!second.contains(&AppIdentity { pid: 10, launch_date: bits(1) }));
    }
}
