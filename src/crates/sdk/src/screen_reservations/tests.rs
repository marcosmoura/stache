//! Fake-backed contract tests: no native work, barriers instead of sleeps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use crate::ModuleContext;
use crate::error::{ConfigError, ErrorCategory, ModuleError, ModuleResult, Operation};
use crate::events::EventBus;
use crate::logging::DiscardLogger;
use crate::module::{Module, ModuleConfig, ModuleDescriptor, ServiceId};
use crate::permissions::{Permission, PermissionError, PermissionProvider, PermissionStatus};
use crate::registry::{ModuleHandle, Registry};
use crate::threading::MainThreadExecutor;
use crate::windows::UnavailableWindows;

const BAR: ModuleId = ModuleId::new("bar");
const TILING: ModuleId = ModuleId::new("tiling");
const PLAIN: ModuleId = ModuleId::new("plain");

const D1: DisplayId = DisplayId::new(1);
const D2: DisplayId = DisplayId::new(2);
const D3: DisplayId = DisplayId::new(3);
const LONG: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq)]
struct Cfg(bool);

impl ModuleConfig for Cfg {
    fn enabled(&self) -> bool { self.0 }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

type Step = Arc<dyn Fn(&ModuleContext) -> ModuleResult<()> + Send + Sync>;

#[derive(Default)]
struct Script {
    start: Mutex<Option<Step>>,
    stop: Mutex<Option<Step>>,
}

impl Script {
    fn on_start(&self, step: impl Fn(&ModuleContext) -> ModuleResult<()> + Send + Sync + 'static) {
        *lock(&self.start) = Some(Arc::new(step));
    }

    fn on_stop(&self, step: impl Fn(&ModuleContext) -> ModuleResult<()> + Send + Sync + 'static) {
        *lock(&self.stop) = Some(Arc::new(step));
    }
}

struct Fake {
    id: ModuleId,
    reserves: bool,
    provides: &'static [ServiceId],
    requires: &'static [ServiceId],
    permissions: &'static [Permission],
    script: Arc<Script>,
}

impl Module for Fake {
    type Config = Cfg;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: self.id,
            provides: self.provides,
            requires: self.requires,
            permissions: self.permissions,
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: self.reserves,
        }
    }

    fn configure(&mut self, _: &Cfg, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let step = lock(&self.script.start).clone();
        step.map_or(Ok(()), |step| step(ctx))
    }

    fn reconfigure(&mut self, _: &Cfg, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let step = lock(&self.script.stop).clone();
        step.map_or(Ok(()), |step| step(ctx))
    }
}

struct Grant;

impl PermissionProvider for Grant {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(self.status(permission))
    }
}

struct Inline;

impl MainThreadExecutor for Inline {
    fn is_main_thread(&self) -> bool { true }

    fn execute(
        &self,
        job: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), crate::threading::ThreadError> {
        job();
        Ok(())
    }
}

fn registry() -> Registry {
    Registry::new(
        Arc::new(DiscardLogger),
        Arc::new(EventBus::new()),
        Arc::new(Inline),
        Arc::new(Grant),
        Arc::new(UnavailableWindows),
    )
}

type Slot = Arc<Mutex<Option<ScreenReservations>>>;

/// Registers a reserving module whose `start` captures its capability.
fn reserver(registry: &Registry, id: ModuleId) -> (ModuleHandle<Cfg>, Arc<Script>, Slot) {
    let script = Arc::new(Script::default());
    let slot: Slot = Arc::new(Mutex::new(None));
    {
        let slot = Arc::clone(&slot);
        script.on_start(move |ctx| {
            *lock(&slot) = Some(ctx.screen_reservations().expect("declared"));
            Ok(())
        });
    }
    let handle = registry
        .register(
            Fake {
                id,
                reserves: true,
                provides: &[],
                requires: &[],
                permissions: &[],
                script: Arc::clone(&script),
            },
            Cfg(true),
        )
        .expect("register");
    (handle, script, slot)
}

/// Like [`reserver`], with declared services and permissions.
fn reserver_with(
    registry: &Registry,
    id: ModuleId,
    provides: &'static [ServiceId],
    requires: &'static [ServiceId],
    permissions: &'static [Permission],
) -> (ModuleHandle<Cfg>, Slot) {
    let script = Arc::new(Script::default());
    let slot: Slot = Arc::new(Mutex::new(None));
    {
        let slot = Arc::clone(&slot);
        script.on_start(move |ctx| {
            *lock(&slot) = Some(ctx.screen_reservations().expect("declared"));
            Ok(())
        });
    }
    let handle = registry
        .register(
            Fake {
                id,
                reserves: true,
                provides,
                requires,
                permissions,
                script,
            },
            Cfg(true),
        )
        .expect("register");
    (handle, slot)
}

fn captured(slot: &Slot) -> ScreenReservations {
    lock(slot).clone().expect("captured a capability")
}

fn top(points: f64) -> EdgeInsets { EdgeInsets::new(points, 0.0, 0.0, 0.0).expect("valid") }

fn insets(top: f64, left: f64, bottom: f64, right: f64) -> EdgeInsets {
    EdgeInsets::new(top, left, bottom, right).expect("valid")
}

const T1: TopologyGeneration = TopologyGeneration::new(1);

/// Applies topology 1 with `displays` on a registry.
fn topology(registry: &Registry, displays: &[DisplayId]) {
    registry.apply_screen_topology(T1, displays).expect("topology");
}

fn aggregate(registry: &Registry, display: DisplayId) -> EdgeInsets {
    registry.observe_screen_reservations().read().insets(display)
}

fn owners(registry: &Registry, display: DisplayId) -> Vec<(ModuleId, u64)> {
    registry
        .observe_screen_reservations()
        .read()
        .display(display)
        .map(|entry| entry.contributions.iter().map(|c| (c.owner, c.generation)).collect())
        .unwrap_or_default()
}

// --- identity, validation, gating --------------------------------------------

#[test]
fn bar_02_03_insets_reject_invalid_values_deterministically() {
    for bad in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            EdgeInsets::new(0.0, 0.0, bad, 0.0),
            Err(ScreenReservationError::InvalidInset { edge: Edge::Bottom, .. })
        ));
    }
    assert!(matches!(
        EdgeInsets::new(0.0, 0.0, 0.0, f64::MAX),
        Err(ScreenReservationError::InsetTooLarge { edge: Edge::Right, .. })
    ));
    assert!(matches!(
        EdgeInsets::new(MAX_RESERVATION_INSET + 1.0, f64::NAN, 0.0, 0.0),
        Err(ScreenReservationError::InsetTooLarge { edge: Edge::Top, .. })
    ));
    let edge = EdgeInsets::new(MAX_RESERVATION_INSET, -0.0, 0.0, 0.0).expect("bound is inclusive");
    assert!(edge.left().is_sign_positive(), "negative zero is normalized");
    assert_eq!(edge.get(Edge::Top), MAX_RESERVATION_INSET);
}

#[test]
fn bar_02_03_a_module_must_declare_screen_reservations() {
    let registry = registry();
    let script = Arc::new(Script::default());
    let outcome: Arc<Mutex<Option<ScreenReservationError>>> = Arc::new(Mutex::new(None));
    let readable = Arc::new(Mutex::new(false));
    {
        let outcome = Arc::clone(&outcome);
        let readable = Arc::clone(&readable);
        script.on_start(move |ctx| {
            *lock(&outcome) = ctx.screen_reservations().err();
            // Reading needs no declaration: any module may consume the snapshot.
            *lock(&readable) = ctx.observe_screen_reservations().read().displays.is_empty();
            Ok(())
        });
    }
    registry
        .register(
            Fake {
                id: PLAIN,
                reserves: false,
                provides: &[],
                requires: &[],
                permissions: &[],
                script,
            },
            Cfg(true),
        )
        .expect("register");
    registry.start_all().expect("start");
    assert_eq!(
        lock(&outcome).clone(),
        Some(ScreenReservationError::NotDeclared { module: PLAIN })
    );
    assert!(*lock(&readable), "an undeclared module still reads the snapshot");
}

#[test]
fn bar_02_03_topology_must_advance_and_never_lists_the_null_display() {
    let registry = registry();
    assert_eq!(
        registry.observe_screen_reservations().read().topology,
        TopologyGeneration::INITIAL
    );
    assert_eq!(
        registry.apply_screen_topology(T1, &[DisplayId::new(0)]),
        Err(ScreenReservationError::NullDisplay)
    );
    topology(&registry, &[D1]);
    assert_eq!(
        registry.apply_screen_topology(T1, &[D1, D2]),
        Err(ScreenReservationError::NonMonotonicTopology { current: T1, requested: T1 })
    );
    assert_eq!(registry.observe_screen_reservations().read().displays.len(), 1);
}

// --- aggregation --------------------------------------------------------------

#[test]
fn bar_02_03_overlapping_reservations_aggregate_by_maximum_never_sum() {
    let registry = registry();
    topology(&registry, &[D2, D1]);
    let (_a, _, bar) = reserver(&registry, BAR);
    let (_b, _, tiling) = reserver(&registry, TILING);
    registry.start_all().expect("start");

    captured(&bar).replace(T1, &[(D1, insets(40.0, 0.0, 5.0, 0.0))]).expect("bar");
    assert_eq!(aggregate(&registry, D1), insets(40.0, 0.0, 5.0, 0.0));

    // Same edge, smaller and larger values; a different edge; a different display.
    captured(&tiling)
        .replace(T1, &[(D1, insets(24.0, 8.0, 9.0, 0.0)), (D2, top(3.0))])
        .expect("tiling");
    assert_eq!(
        aggregate(&registry, D1),
        insets(40.0, 8.0, 9.0, 0.0),
        "max per edge, not 64"
    );
    assert_eq!(aggregate(&registry, D2), top(3.0));
    assert_eq!(
        aggregate(&registry, D3),
        EdgeInsets::ZERO,
        "unknown display reads zero"
    );

    let snapshot = registry.observe_screen_reservations().read();
    let order: Vec<DisplayId> = snapshot.displays.iter().map(|d| d.display).collect();
    assert_eq!(order, [D1, D2], "deterministic display order");
    let contributors: Vec<ModuleId> = snapshot
        .display(D1)
        .expect("d1")
        .contributions
        .iter()
        .map(|c| c.owner)
        .collect();
    assert_eq!(contributors, [BAR, TILING], "contributors ordered by owner id");
    assert_eq!(snapshot.display(D2).expect("d2").contributions.len(), 1);
}

#[test]
fn bar_02_03_replacement_is_atomic_and_removal_is_idempotent() {
    let registry = registry();
    topology(&registry, &[D1, D2]);
    let (_h, _, slot) = reserver(&registry, BAR);
    registry.start_all().expect("start");
    let cap = captured(&slot);
    let observer = registry.observe_screen_reservations();

    let first = cap.replace(T1, &[(D1, top(10.0)), (D2, top(20.0))]).expect("stage");
    assert_eq!(observer.read().revision, first);
    // Replace the whole set: D2 disappears, D1 changes, in one revision step.
    let second = cap.replace(T1, &[(D1, top(12.0))]).expect("replace");
    assert_eq!(second, first + 1);
    let snapshot = observer.read();
    assert_eq!(snapshot.insets(D1), top(12.0));
    assert_eq!(snapshot.insets(D2), EdgeInsets::ZERO);

    // An identical replacement is not a change.
    assert_eq!(cap.replace(T1, &[(D1, top(12.0))]).expect("same"), second);

    cap.remove();
    let removed = observer.read().revision;
    assert_eq!(removed, second + 1);
    cap.remove();
    assert_eq!(
        observer.read().revision,
        removed,
        "second remove changes nothing"
    );
    assert_eq!(observer.read().insets(D1), EdgeInsets::ZERO);
}

#[test]
fn bar_02_03_invalid_replacements_are_rejected_without_changing_state() {
    let registry = registry();
    topology(&registry, &[D1]);
    let (_h, _, slot) = reserver(&registry, BAR);
    registry.start_all().expect("start");
    let cap = captured(&slot);
    cap.replace(T1, &[(D1, top(10.0))]).expect("stage");
    let observer = registry.observe_screen_reservations();
    let before = observer.read();

    assert_eq!(
        cap.replace(T1, &[(D1, top(1.0)), (D1, top(2.0))]),
        Err(ScreenReservationError::DuplicateDisplay { display: D1 })
    );
    assert_eq!(
        cap.replace(T1, &[(D1, top(1.0)), (D3, top(2.0))]),
        Err(ScreenReservationError::UnknownDisplay { display: D3 })
    );
    assert_eq!(
        cap.replace(TopologyGeneration::new(7), &[(D1, top(1.0))]),
        Err(ScreenReservationError::StaleTopology {
            staged: TopologyGeneration::new(7),
            current: T1
        })
    );
    let many: Vec<(DisplayId, EdgeInsets)> = (0..=u32::try_from(MAX_RESERVED_DISPLAYS)
        .expect("fits"))
        .map(|n| (DisplayId::new(n + 1), top(1.0)))
        .collect();
    assert_eq!(
        cap.replace(T1, &many),
        Err(ScreenReservationError::TooManyDisplays {
            count: MAX_RESERVED_DISPLAYS + 1
        })
    );
    assert_eq!(
        observer.read(),
        before,
        "every rejection left the snapshot untouched"
    );
}

// --- display removal and ID reuse ---------------------------------------------

#[test]
fn bar_02_03_display_removal_drops_orphans_and_a_reused_id_is_not_resurrected() {
    let registry = registry();
    topology(&registry, &[D1, D2]);
    let (_h, _, slot) = reserver(&registry, BAR);
    registry.start_all().expect("start");
    let cap = captured(&slot);
    cap.replace(T1, &[(D1, top(10.0)), (D2, top(20.0))]).expect("stage");
    let observer = registry.observe_screen_reservations();
    let seen = observer.read();
    assert_eq!(seen.display(D2).expect("d2").since, T1);

    let t2 = TopologyGeneration::new(2);
    registry.apply_screen_topology(t2, &[D1]).expect("remove d2");
    let after = observer.read();
    assert!(after.revision > seen.revision);
    assert_eq!(after.topology, t2);
    assert!(after.display(D2).is_none(), "orphan entry dropped");
    assert_eq!(after.insets(D1), top(10.0), "a surviving display keeps its set");

    // The caller still holding topology 1 cannot land on anything.
    assert!(matches!(
        cap.replace(T1, &[(D2, top(99.0))]),
        Err(ScreenReservationError::StaleTopology { .. })
    ));
    assert!(matches!(
        cap.replace(t2, &[(D2, top(99.0))]),
        Err(ScreenReservationError::UnknownDisplay { .. })
    ));

    // The numeric ID returns as a different presence; nothing is resurrected.
    let t3 = TopologyGeneration::new(3);
    registry.apply_screen_topology(t3, &[D1, D2]).expect("reuse d2");
    let reused = observer.read();
    let entry = reused.display(D2).expect("d2 again");
    assert_eq!(entry.since, t3, "a reused ID is a new presence");
    assert!(entry.contributions.is_empty());
    assert_eq!(
        reused.display(D1).expect("d1").since,
        T1,
        "a persistent display keeps its origin"
    );
}

// --- lifecycle ----------------------------------------------------------------

#[test]
fn bar_02_03_stop_disable_and_pause_clear_exact_generation_before_stop_cleanup() {
    for how in ["disable", "pause", "stop_all"] {
        let registry = registry();
        topology(&registry, &[D1]);
        let (handle, script, slot) = reserver(&registry, BAR);
        let (_other, _, other_slot) = reserver(&registry, TILING);
        let seen_in_stop: Arc<Mutex<Vec<(bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen_in_stop);
            let observer = registry.observe_screen_reservations();
            let slot = Arc::clone(&slot);
            script.on_stop(move |_| {
                // Inside `Module::stop`, before workers join.
                let snapshot = observer.read();
                let late = captured(&slot).replace(T1, &[(D1, top(77.0))]).is_err();
                let still_listed = snapshot
                    .display(D1)
                    .is_some_and(|d| d.contributions.iter().any(|c| c.owner == BAR));
                lock(&seen).push((still_listed, late));
                Ok(())
            });
        }
        registry.start_all().expect("start");
        captured(&slot).replace(T1, &[(D1, top(10.0))]).expect("bar");
        captured(&other_slot).replace(T1, &[(D1, top(4.0))]).expect("tiling");
        assert_eq!(aggregate(&registry, D1), top(10.0));

        match how {
            "disable" => registry.set_enabled(handle, false).expect("disable"),
            "pause" => registry.pause(handle).expect("pause"),
            _ => registry.shutdown().expect("shutdown"),
        }
        let seen = lock(&seen_in_stop).clone();
        assert_eq!(seen.len(), 1, "{how}: stop ran once");
        assert!(
            !seen[0].0,
            "{how}: the stopping owner is already gone inside stop"
        );
        assert!(
            seen[0].1,
            "{how}: a late write from the ending generation is rejected"
        );
        if how != "stop_all" {
            assert_eq!(
                aggregate(&registry, D1),
                top(4.0),
                "{how}: the other owner is untouched"
            );
            assert_eq!(owners(&registry, D1).iter().map(|o| o.0).collect::<Vec<_>>(), [
                TILING
            ]);
        }
    }
}

static NEEDS_ACCESSIBILITY: [Permission; 1] = [Permission::Accessibility];
static SERVICE: ServiceId = ServiceId::new("test.service");
static PROVIDES: [ServiceId; 1] = [SERVICE];
static REQUIRES: [ServiceId; 1] = [SERVICE];

#[test]
fn bar_02_03_permission_revocation_clears_the_owner_exactly() {
    let registry = registry();
    topology(&registry, &[D1]);
    let (_handle, slot) = reserver_with(&registry, BAR, &[], &[], &NEEDS_ACCESSIBILITY);
    let (_other, other_slot) = reserver_with(&registry, TILING, &[], &[], &[]);
    registry.start_all().expect("start");
    captured(&slot).replace(T1, &[(D1, top(10.0))]).expect("bar");
    captured(&other_slot).replace(T1, &[(D1, top(4.0))]).expect("tiling");
    assert_eq!(aggregate(&registry, D1), top(10.0));

    registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    assert_eq!(aggregate(&registry, D1), top(4.0));
    assert_eq!(owners(&registry, D1).iter().map(|o| o.0).collect::<Vec<_>>(), [
        TILING
    ]);
    assert!(
        captured(&slot).replace(T1, &[(D1, top(1.0))]).is_err(),
        "the revoked generation is dead"
    );
}

#[test]
fn bar_02_03_dependency_loss_clears_the_owner_exactly() {
    let registry = registry();
    topology(&registry, &[D1]);
    let (_provider, _provider_slot) = reserver_with(&registry, PLAIN, &PROVIDES, &[], &[]);
    let (_handle, slot) = reserver_with(&registry, BAR, &[], &REQUIRES, &[]);
    let (_other, other_slot) = reserver_with(&registry, TILING, &[], &[], &[]);
    registry.start_all().expect("start");
    captured(&slot).replace(T1, &[(D1, top(10.0))]).expect("bar");
    captured(&other_slot).replace(T1, &[(D1, top(4.0))]).expect("tiling");
    assert_eq!(aggregate(&registry, D1), top(10.0));

    registry.service_failed(SERVICE).expect("service lost");
    assert_eq!(aggregate(&registry, D1), top(4.0));
    assert_eq!(owners(&registry, D1).iter().map(|o| o.0).collect::<Vec<_>>(), [
        TILING
    ]);
    assert!(captured(&slot).replace(T1, &[(D1, top(1.0))]).is_err());
}

#[test]
fn bar_02_03_repeated_capability_requests_register_one_end_listener() {
    let registry = registry();
    topology(&registry, &[D1]);
    let script = Arc::new(Script::default());
    let slot: Slot = Arc::new(Mutex::new(None));
    {
        let slot = Arc::clone(&slot);
        script.on_start(move |ctx| {
            for _ in 0..50 {
                *lock(&slot) = Some(ctx.screen_reservations().expect("declared"));
            }
            Ok(())
        });
    }
    let handle = registry
        .register(
            Fake {
                id: BAR,
                reserves: true,
                provides: &[],
                requires: &[],
                permissions: &[],
                script,
            },
            Cfg(true),
        )
        .expect("register");
    registry.start_all().expect("start");
    let generation = captured(&slot).scope.id();
    assert_eq!(
        lock(&registry.reservation_hub().inner)
            .listening
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![(BAR, generation)],
        "fifty requests, one listener registration"
    );
    captured(&slot).replace(T1, &[(D1, top(10.0))]).expect("stage");

    registry.set_enabled(handle, false).expect("disable");
    assert_eq!(
        aggregate(&registry, D1),
        EdgeInsets::ZERO,
        "exact removal on end"
    );
    assert!(lock(&registry.reservation_hub().inner).listening.is_empty());
}

#[test]
fn bar_02_03_a_failed_start_leaves_nothing_behind() {
    let registry = registry();
    topology(&registry, &[D1]);
    let script = Arc::new(Script::default());
    let slot: Slot = Arc::new(Mutex::new(None));
    {
        let slot = Arc::clone(&slot);
        script.on_start(move |ctx| {
            let cap = ctx.screen_reservations().expect("declared");
            cap.replace(T1, &[(D1, top(10.0))]).expect("partial stage");
            *lock(&slot) = Some(cap);
            Err(ModuleError::new(
                BAR,
                Operation::Start,
                ErrorCategory::Platform,
                "panel failed",
            ))
        });
    }
    registry
        .register(
            Fake {
                id: BAR,
                reserves: true,
                provides: &[],
                requires: &[],
                permissions: &[],
                script,
            },
            Cfg(true),
        )
        .expect("register");
    let _ = registry.start_all();
    assert_eq!(
        aggregate(&registry, D1),
        EdgeInsets::ZERO,
        "partial staging rolled back"
    );
    assert!(owners(&registry, D1).is_empty());
    assert!(
        captured(&slot).replace(T1, &[(D1, top(5.0))]).is_err(),
        "the failed generation is dead"
    );
}

#[test]
fn bar_02_03_an_old_handle_cannot_replace_or_remove_the_successor() {
    let registry = registry();
    topology(&registry, &[D1]);
    let (handle, _script, slot) = reserver(&registry, BAR);
    registry.start_all().expect("start");
    let old = captured(&slot);
    let old_generation = old.scope.id();
    old.replace(T1, &[(D1, top(10.0))]).expect("old stage");
    registry.set_enabled(handle, false).expect("disable");
    assert_eq!(aggregate(&registry, D1), EdgeInsets::ZERO);
    registry.set_enabled(handle, true).expect("enable");
    let successor = captured(&slot);
    successor.replace(T1, &[(D1, top(30.0))]).expect("successor stage");
    let observer = registry.observe_screen_reservations();
    let revision = observer.read().revision;

    assert!(matches!(
        old.replace(T1, &[(D1, top(1.0))]),
        Err(ScreenReservationError::StaleGeneration { owner, .. }) if owner == BAR
    ));
    old.remove();
    // A late end for the old generation, arriving after the successor staged.
    registry.reservation_hub().end_generation(BAR, old_generation);
    let after = observer.read();
    assert_eq!(
        after.revision, revision,
        "no write and no wake from the old generation"
    );
    assert_eq!(after.insets(D1), top(30.0));
    let (_, generation) = owners(&registry, D1)[0];
    assert!(
        generation > old_generation,
        "the surviving entry belongs to the successor"
    );

    successor.remove();
    assert_eq!(
        aggregate(&registry, D1),
        EdgeInsets::ZERO,
        "the successor's own handle works"
    );
}

// --- snapshot watch -----------------------------------------------------------

#[test]
fn bar_02_03_wait_returns_changes_and_a_publication_during_check_and_sleep_is_not_lost() {
    let registry = Arc::new(registry());
    topology(&registry, &[D1]);
    let (_h, _, slot) = reserver(&registry, BAR);
    registry.start_all().expect("start");
    let cap = captured(&slot);
    let observer = registry.observe_screen_reservations();
    let seen = observer.read().revision;
    let cancel = CancellationToken::new();

    // Already changed: returns immediately without sleeping.
    cap.replace(T1, &[(D1, top(1.0))]).expect("stage");
    assert!(matches!(
        observer.wait_for_change(seen, LONG, &cancel),
        ScreenReservationWait::Changed(snapshot) if snapshot.insets(D1) == top(1.0)
    ));

    // Publication lands between the check and the sleep.
    let current = observer.read().revision;
    let fired = Arc::new(AtomicBool::new(false));
    {
        let fired = Arc::clone(&fired);
        let (tx, rx) = mpsc::channel();
        let publisher = thread::spawn(move || {
            rx.recv().expect("signal");
            cap.replace(T1, &[(D1, top(2.0))]).expect("stage during wait");
        });
        registry.reservation_hub().set_before_wait(Some(Arc::new(move || {
            if !fired.swap(true, Ordering::SeqCst) {
                tx.send(()).expect("signal publisher");
            }
        })));
        let outcome = observer.wait_for_change(current, LONG, &cancel);
        publisher.join().expect("publisher");
        assert!(matches!(
            outcome,
            ScreenReservationWait::Changed(snapshot) if snapshot.insets(D1) == top(2.0)
        ));
    }
}

#[test]
fn bar_02_03_wait_times_out() {
    let registry = registry();
    let observer = registry.observe_screen_reservations();
    let seen = observer.read().revision;
    assert_eq!(
        observer.wait_for_change(seen, Duration::ZERO, &CancellationToken::new()),
        ScreenReservationWait::TimedOut
    );
}

#[test]
fn bar_02_03_cancel_during_wait_and_shutdown_close_waiters() {
    let registry = Arc::new(registry());
    let observer = registry.observe_screen_reservations();
    let seen = observer.read().revision;

    // Cancel while the waiter is about to sleep.
    let cancel = CancellationToken::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    registry.reservation_hub().set_before_wait(Some(Arc::new(move || {
        let _ = entered_tx.send(());
    })));
    let waiter = {
        let observer = observer.clone();
        let cancel = cancel.clone();
        thread::spawn(move || observer.wait_for_change(seen, Duration::MAX, &cancel))
    };
    entered_rx.recv().expect("waiter reached the check/sleep point");
    cancel.cancel();
    assert_eq!(waiter.join().expect("waiter"), ScreenReservationWait::Cancelled);

    // Shutdown closes a blocked waiter.
    let (entered_tx, entered_rx) = mpsc::channel();
    registry.reservation_hub().set_before_wait(Some(Arc::new(move || {
        let _ = entered_tx.send(());
    })));
    let waiter = {
        let observer = observer.clone();
        thread::spawn(move || {
            observer.wait_for_change(seen, Duration::MAX, &CancellationToken::new())
        })
    };
    entered_rx.recv().expect("waiter reached the check/sleep point");
    registry.invalidate();
    assert_eq!(waiter.join().expect("waiter"), ScreenReservationWait::Closed);
    // Reads keep the last snapshot after close.
    assert_eq!(observer.read().revision, seen);
}

#[test]
fn bar_02_03_cancellation_wins_over_a_pending_change() {
    let registry = registry();
    topology(&registry, &[D1]);
    let observer = registry.observe_screen_reservations();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        observer.wait_for_change(0, LONG, &cancel),
        ScreenReservationWait::Cancelled,
        "cancel > change"
    );
}
