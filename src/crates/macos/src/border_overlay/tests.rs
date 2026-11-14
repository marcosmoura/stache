//! Overlay engine tests: no real window, panel, layer or `dlsym` is used.

use std::ffi::CStr;
use std::sync::Arc;
use std::time::Duration;

use objc2_app_kit::NSWindowCollectionBehavior;

use super::testing::{FakeEvent, FakeOverlayBackend};
use super::*;
use crate::ManualExecutor;

const SHORT: Duration = Duration::from_millis(20);

pub(super) struct Rig {
    pub(super) executor: Arc<ManualExecutor>,
    pub(super) backend: Arc<FakeOverlayBackend>,
    pub(super) control: OverlayControl,
}

impl Rig {
    /// An off-main caller: jobs wait in the queue until drained.
    pub(super) fn queued() -> Self {
        let executor = Arc::new(ManualExecutor::new(false));
        let backend = Arc::new(FakeOverlayBackend::single_display());
        let control = OverlayControl::new(
            Arc::clone(&executor) as Arc<dyn stache_sdk::MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn OverlayBackend>,
            OverlayCapabilities::default(),
        );
        Self { executor, backend, control }
    }

    pub(super) fn inline() -> Self {
        let rig = Self::queued();
        rig.executor.set_on_main(true);
        rig
    }

    pub(super) fn drain(&self) -> usize { self.executor.drain() }
}

pub(super) fn h(raw: u64) -> OverlayHandle { OverlayHandle::from_raw(raw) }

pub(super) fn rect(x: f64, y: f64, width: f64, height: f64) -> OverlayRect {
    OverlayRect::new(x, y, width, height)
}

pub(super) const RED: BorderColor = BorderColor { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
pub(super) const BLUE: BorderColor = BorderColor { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };

pub(super) fn appearance(width: f64) -> BorderAppearance {
    BorderAppearance {
        width,
        hidpi: true,
        paint: BorderPaint::Solid(RED),
        corner_radius: None,
        motion: None,
        placement: BorderPlacement::Below,
    }
}

pub(super) fn show(target: u32, frame: OverlayRect) -> OverlayKind {
    OverlayKind::Show {
        target_cg_id: target,
        frame,
        appearance: appearance(4.0),
        change: AppearanceChange::Reload,
    }
}

fn configures(backend: &FakeOverlayBackend) -> Vec<(u64, OverlayPlan)> {
    backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::Configure(panel, plan) => Some((panel, plan)),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------ TB-01-01

#[test]
fn apply_off_main_runs_no_native_code_until_the_main_job_runs() {
    let rig = Rig::queued();
    let receipt = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 100.0, 80.0))));
    assert!(receipt.refused.is_empty());
    assert_eq!(
        rig.backend.event_count(),
        0,
        "apply never runs native code off main"
    );
    assert_eq!(rig.executor.queued(), 1);
    assert_eq!(rig.drain(), 1);
    assert!(rig.backend.event_count() > 0);
    let current = std::thread::current().id();
    assert!(rig.backend.apply_threads().iter().all(|id| *id == current));
}

#[test]
fn apply_enqueues_one_job_for_many_batches_and_merges_them() {
    let rig = Rig::queued();
    for index in 0..5 {
        let frame = rect(f64::from(index), 0.0, 100.0, 80.0);
        let kind = if index == 0 {
            show(10, frame)
        } else {
            OverlayKind::Move { frame }
        };
        let _ = rig.control.apply(OverlayBatch::new().with(h(1), kind));
    }
    assert_eq!(rig.executor.queued(), 1, "one pending batch, one job");
    rig.drain();
    let plans = configures(&rig.backend);
    assert_eq!(plans.len(), 1, "latest wins: a single Show with the last frame");
    // x of the panel is the last frame x minus the visible width and padding.
    let visible = 4.0 * VISIBLE_FRACTION;
    assert!((plans[0].1.panel_frame.x - (4.0 - visible - OVERLAY_PADDING)).abs() < 1e-9);
    assert!(rig.control.counters().superseded >= 4);
    assert_eq!(rig.control.counters().batches, 1);
}

#[test]
fn a_waiter_gets_a_typed_ack_after_the_main_job_and_times_out_before() {
    let rig = Rig::queued();
    let receipt = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(
        receipt.ticket.wait(SHORT),
        Err(AckError::TimedOut),
        "bounded wait"
    );
    rig.drain();
    let ack = receipt.ticket.wait(SHORT).expect("ack");
    assert_eq!(ack.summary.shown, 1);
    assert_eq!(ack.summary.created, 1);
}

#[test]
fn main_never_waits_for_an_unfinished_job() {
    let rig = Rig::queued();
    let receipt = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    rig.executor.set_on_main(true);
    assert_eq!(
        receipt.ticket.wait(Duration::from_secs(60)),
        Err(AckError::WouldBlockMain)
    );
    assert_eq!(receipt.ticket.try_get(), None);
}

#[test]
fn an_inline_main_apply_has_its_ack_ready_without_waiting() {
    let rig = Rig::inline();
    let receipt = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(
        receipt.ticket.wait(Duration::ZERO).expect("ack").summary.shown,
        1
    );
}

#[test]
fn a_rejected_dispatch_is_a_typed_failure_and_the_work_is_kept() {
    let rig = Rig::queued();
    rig.executor.reject(true);
    let receipt = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(receipt.ticket.wait(SHORT), Err(AckError::DispatchFailed));
    rig.executor.reject(false);
    let retry = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(1.0, 1.0, 50.0, 50.0),
    }));
    rig.drain();
    assert_eq!(retry.ticket.wait(SHORT).expect("ack").summary.shown, 1);
}

#[test]
fn hide_after_show_in_one_batch_keeps_the_hide() {
    let rig = Rig::queued();
    let batch = OverlayBatch::new()
        .with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0)))
        .with(h(1), OverlayKind::Hide);
    let _ = rig.control.apply(batch);
    rig.drain();
    assert_eq!(rig.backend.ordered_in(), 0);
    assert!(
        !rig.backend
            .events()
            .iter()
            .any(|event| matches!(event, FakeEvent::OrderRelative { .. }))
    );
}

#[test]
fn hide_absorbs_later_moves_and_orders_until_the_next_show() {
    let rig = Rig::queued();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    rig.drain();
    assert_eq!(rig.backend.ordered_in(), 1);
    let batch = OverlayBatch::new()
        .with(h(1), OverlayKind::Hide)
        .with(h(1), OverlayKind::Move {
            frame: rect(5.0, 5.0, 50.0, 50.0),
        })
        .with(h(1), OverlayKind::Order { target_cg_id: 11 });
    let _ = rig.control.apply(batch);
    rig.drain();
    assert_eq!(rig.backend.ordered_in(), 0, "the Hide was not dropped");
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(12, rect(5.0, 5.0, 50.0, 50.0))));
    rig.drain();
    assert_eq!(rig.backend.ordered_in(), 1);
}

#[test]
fn release_during_a_pending_show_wins_and_is_never_dropped() {
    let rig = Rig::queued();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    let late = rig.control.apply(
        OverlayBatch::new()
            .with(h(1), OverlayKind::Release)
            .with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0)))
            .with(h(1), OverlayKind::Move {
                frame: rect(9.0, 9.0, 50.0, 50.0),
            }),
    );
    assert_eq!(late.refused, vec![
        Refusal {
            handle: h(1),
            reason: RefusalReason::ReleasedHandle
        },
        Refusal {
            handle: h(1),
            reason: RefusalReason::ReleasedHandle
        },
    ]);
    rig.drain();
    assert_eq!(rig.backend.live_overlays(), 0);
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.backend.pool_len(), 0, "never created, so nothing to pool");
}

#[test]
fn release_after_an_applied_show_returns_the_panel_to_the_pool() {
    let rig = Rig::queued();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    rig.drain();
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Release));
    rig.drain();
    assert_eq!(rig.backend.live_overlays(), 0);
    assert_eq!(rig.backend.pool_len(), 1);
    assert_eq!(rig.backend.ordered_in(), 0);
}

#[test]
fn unknown_handles_are_refused_with_a_typed_reason() {
    let rig = Rig::queued();
    let receipt = rig.control.apply(OverlayBatch::new().with(h(7), OverlayKind::Hide));
    assert_eq!(receipt.refused, vec![Refusal {
        handle: h(7),
        reason: RefusalReason::UnknownHandle
    }]);
    assert_eq!(rig.executor.queued(), 0, "nothing to apply, nothing enqueued");
    assert_eq!(
        receipt.ticket.wait(Duration::ZERO).expect("empty ack").summary,
        ApplySummary::default()
    );
}

#[test]
fn hide_all_runs_before_every_other_command_and_survives_later_merges() {
    let rig = Rig::queued();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    rig.drain();
    let before = rig.backend.event_count();

    let mut first = OverlayBatch::hide_all(HideReason::MissionControl);
    first.commands.push(OverlayCommand {
        handle: h(2),
        kind: show(20, rect(0.0, 0.0, 40.0, 40.0)),
        epoch: 0,
    });
    let _ = rig.control.apply(first);
    // A later batch without HideAll must not erase it.
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(3.0, 3.0, 50.0, 50.0),
    }));
    rig.drain();

    let events = rig.backend.events();
    let applied = &events[before..];
    let order_out = applied.iter().position(|event| matches!(event, FakeEvent::OrderOut(_)));
    let first_configure =
        applied.iter().position(|event| matches!(event, FakeEvent::Configure(..)));
    assert!(order_out.expect("hide all ran") < first_configure.expect("commands ran"));
    assert_eq!(rig.control.counters().hide_alls, 1);
}

#[test]
fn hide_all_hides_every_live_overlay_without_releasing_it() {
    let rig = Rig::inline();
    for raw in 1..=3 {
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    }
    assert_eq!(rig.backend.ordered_in(), 3);
    let receipt = rig.control.apply(OverlayBatch::hide_all(HideReason::SpaceChange));
    assert!(receipt.ticket.wait(Duration::ZERO).expect("ack").summary.hid_all);
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.backend.live_overlays(), 3);
}

#[test]
fn shows_beyond_the_cap_are_refused_and_counted() {
    let rig = Rig::queued();
    let mut batch = OverlayBatch::new();
    for raw in 1..=(MAX_OVERLAYS as u64 + 6) {
        batch = batch.with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0)));
    }
    let receipt = rig.control.apply(batch);
    assert_eq!(receipt.refused.len(), 6);
    assert!(
        receipt
            .refused
            .iter()
            .all(|refusal| refusal.reason == RefusalReason::CapReached)
    );
    assert_eq!(rig.control.counters().refused_shows, 6);
    rig.drain();
    assert_eq!(rig.backend.live_overlays(), MAX_OVERLAYS);
    assert_eq!(rig.backend.open_panels(), MAX_OVERLAYS);
}

#[test]
fn releasing_frees_capacity_for_a_new_handle() {
    let rig = Rig::inline();
    for raw in 1..=MAX_OVERLAYS as u64 {
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    }
    let refused = rig
        .control
        .apply(OverlayBatch::new().with(h(1000), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(refused.refused.len(), 1);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Release));
    let accepted = rig
        .control
        .apply(OverlayBatch::new().with(h(1001), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert!(accepted.refused.is_empty());
    assert_eq!(rig.backend.live_overlays(), MAX_OVERLAYS);
    assert_eq!(
        rig.backend.open_panels(),
        MAX_OVERLAYS,
        "the released panel was reused"
    );
}

// ------------------------------------------------------------------ TB-01-02

fn two_displays() -> Vec<OverlayScreen> {
    vec![
        OverlayScreen {
            frame: rect(100.0, 50.0, 1440.0, 900.0),
            scale: 2.0,
        },
        OverlayScreen {
            frame: rect(1540.0, -130.0, 1920.0, 1080.0),
            scale: 1.0,
        },
    ]
}

#[test]
fn top_left_points_flip_against_the_primary_screen_with_a_non_zero_origin() {
    let primary = rect(100.0, 50.0, 1440.0, 900.0);
    let flipped = appkit_frame_from_top_left(rect(10.0, 20.0, 300.0, 200.0), primary);
    assert_eq!(flipped, rect(110.0, 730.0, 300.0, 200.0));
    // A window flush with the top of the primary screen ends at its top edge.
    let top = appkit_frame_from_top_left(rect(0.0, 0.0, 10.0, 100.0), primary);
    assert!((top.y + top.height - (primary.y + primary.height)).abs() < 1e-9);
    // A window above the primary (negative y) lies above its top edge.
    let above = appkit_frame_from_top_left(rect(0.0, -300.0, 10.0, 100.0), primary);
    assert!(above.y > primary.y + primary.height);
}

#[test]
fn outset_covers_zero_normal_and_huge_widths() {
    let target = rect(100.0, 100.0, 200.0, 100.0);
    assert_eq!(outset_frame(target, 0.0, 0.0), target);
    assert_eq!(outset_frame(target, 4.0, 2.0), rect(94.0, 94.0, 212.0, 112.0));
    let huge = outset_frame(target, 1.0e9, 2.0);
    assert!((huge.width - (200.0 + 2.0 * (MAX_BORDER_WIDTH + 2.0))).abs() < 1e-9);
    assert_eq!(outset_frame(target, f64::NAN, -5.0), target);
    assert_eq!(outset_frame(target, -3.0, 0.0), target);
}

#[test]
fn corner_radius_selection_covers_symbol_present_absent_and_zero() {
    assert!((select_corner_radius(Some(22.0), 16.0) - 22.0).abs() < f64::EPSILON);
    assert!((select_corner_radius(None, 16.0) - 16.0).abs() < f64::EPSILON);
    assert!(select_corner_radius(Some(0.0), 16.0).abs() < f64::EPSILON);
    assert!((select_corner_radius(Some(-1.0), 16.0) - 16.0).abs() < f64::EPSILON);
    assert!((select_corner_radius(Some(f64::NAN), 16.0) - 16.0).abs() < f64::EPSILON);
}

#[test]
fn the_fallback_radius_is_a_per_os_constant() {
    assert!(fallback_corner_radius(26) > fallback_corner_radius(15));
    assert!((fallback_corner_radius(27) - fallback_corner_radius(26)).abs() < f64::EPSILON);
}

#[test]
fn scale_selection_follows_hidpi_and_the_display() {
    assert!((scale_for_overlay(true, 2.0) - 2.0).abs() < f64::EPSILON);
    assert!((scale_for_overlay(false, 2.0) - 1.0).abs() < f64::EPSILON);
    assert!((scale_for_overlay(true, 0.0) - 1.0).abs() < f64::EPSILON);
    assert!((scale_for_overlay(true, f64::NAN) - 1.0).abs() < f64::EPSILON);

    let screens = two_displays();
    let on_primary = rect(200.0, 200.0, 300.0, 300.0);
    let on_secondary = rect(1600.0, 100.0, 300.0, 300.0);
    let straddling = rect(1440.0, 200.0, 300.0, 300.0);
    assert_eq!(display_scale_for_frame(on_primary, &screens), Some(2.0));
    assert_eq!(display_scale_for_frame(on_secondary, &screens), Some(1.0));
    // 100 points on the primary, 200 on the secondary: the secondary wins.
    assert_eq!(display_scale_for_frame(straddling, &screens), Some(1.0));
    assert_eq!(
        display_scale_for_frame(rect(-5000.0, 0.0, 10.0, 10.0), &screens),
        Some(2.0)
    );
    assert_eq!(display_scale_for_frame(on_primary, &[]), None);
}

#[test]
fn a_plan_follows_the_display_scale_and_the_corner_radius() {
    let screens = two_displays();
    // Primary spans top-left y 0..900; the secondary lies to its right.
    // Width 6 (default `Below`): 5 visible outside, 1 hidden under the window.
    let on_primary = plan_overlay(
        rect(300.0, 100.0, 400.0, 300.0),
        &appearance(6.0),
        Some(20.0),
        16.0,
        &screens,
    );
    assert!((on_primary.scale - 2.0).abs() < f64::EPSILON);
    assert!(
        (on_primary.inner_radius - 20.0).abs() < f64::EPSILON,
        "radius + 1 - the hidden 1"
    );
    assert!(
        (on_primary.outer_radius - 26.0).abs() < f64::EPSILON,
        "radius + 1 + the visible 5, concentric with the target edge"
    );
    assert!((on_primary.target_radius - 21.0).abs() < f64::EPSILON);
    assert_eq!(on_primary.target, rect(7.0, 7.0, 400.0, 300.0));
    assert_eq!(on_primary.inner, rect(8.0, 8.0, 398.0, 298.0));
    assert_eq!(on_primary.outer, rect(2.0, 2.0, 410.0, 310.0));
    assert_eq!(
        on_primary.panel_frame,
        rect(100.0 + 293.0, 50.0 + 900.0 - (93.0 + 314.0), 414.0, 314.0)
    );

    let on_secondary = plan_overlay(
        rect(1500.0, 100.0, 400.0, 300.0),
        &BorderAppearance {
            placement: BorderPlacement::Above,
            ..appearance(6.0)
        },
        None,
        16.0,
        &screens,
    );
    assert!((on_secondary.scale - 1.0).abs() < f64::EPSILON);
    assert!(
        (on_secondary.inner_radius - 18.0).abs() < f64::EPSILON,
        "fallback + 2 above"
    );

    let mut off = appearance(4.0);
    off.hidpi = false;
    let unscaled = plan_overlay(rect(300.0, 100.0, 400.0, 300.0), &off, None, 16.0, &screens);
    assert!((unscaled.scale - 1.0).abs() < f64::EPSILON);
}

#[test]
fn a_plan_is_safe_for_square_tiny_and_zero_width_targets() {
    let screens = two_displays();
    let plan = plan_overlay(
        rect(0.0, 0.0, 100.0, 100.0),
        &appearance(3.0),
        Some(0.0),
        16.0,
        &screens,
    );
    assert!(plan.inner_radius.abs() < f64::EPSILON && plan.outer_radius.abs() < f64::EPSILON);

    let tiny = plan_overlay(
        rect(0.0, 0.0, 10.0, 6.0),
        &BorderAppearance {
            placement: BorderPlacement::Above,
            ..appearance(2.0)
        },
        Some(40.0),
        16.0,
        &screens,
    );
    assert!(
        (tiny.inner_radius - 3.0).abs() < f64::EPSILON,
        "clamped to half the short side"
    );

    let none = plan_overlay(
        rect(0.0, 0.0, 100.0, 100.0),
        &appearance(0.0),
        None,
        16.0,
        &screens,
    );
    assert!(!none.visible);
    assert_eq!(none.outer.width, none.inner.width);
}

#[test]
fn the_collection_behavior_is_transient_and_ignores_cycle_only() {
    use super::native::COLLECTION_BEHAVIOR;
    assert_eq!(
        COLLECTION_BEHAVIOR,
        NSWindowCollectionBehavior::Transient | NSWindowCollectionBehavior::IgnoresCycle
    );
    for forbidden in [
        NSWindowCollectionBehavior::CanJoinAllSpaces,
        NSWindowCollectionBehavior::MoveToActiveSpace,
        NSWindowCollectionBehavior::Stationary,
        NSWindowCollectionBehavior::Managed,
        NSWindowCollectionBehavior::FullScreenAuxiliary,
    ] {
        assert!(!COLLECTION_BEHAVIOR.intersects(forbidden), "{forbidden:?}");
    }
}

#[test]
fn the_radius_resolver_needs_every_symbol_and_resolves_without_calling() {
    let missing = unsafe { SystemCornerRadiusResolver::from_lookup(|_| std::ptr::null_mut()) };
    assert!(!missing.symbol_present());
    assert_eq!(missing.capability(), RadiusCapability::FallbackOnly);
    assert_eq!(missing.missing_symbol(), Some(c"SLSMainConnectionID"));
    assert_eq!(missing.corner_radius(1), None, "missing symbol: fallback");

    // Only the corner-radii getter is missing: the whole capability is.
    // SAFETY: the dummy addresses are never called (the lookup fails).
    let partial = unsafe {
        SystemCornerRadiusResolver::from_lookup(|name: &CStr| {
            if name == c"SLSWindowIteratorGetCornerRadii" {
                std::ptr::null_mut()
            } else {
                std::ptr::NonNull::<u8>::dangling().as_ptr().cast()
            }
        })
    };
    assert_eq!(partial.capability(), RadiusCapability::FallbackOnly);
    assert_eq!(
        partial.missing_symbol(),
        Some(c"SLSWindowIteratorGetCornerRadii")
    );
    assert_eq!(partial.corner_radius(1), None);

    let seen = std::sync::Mutex::new(Vec::new());
    // SAFETY: the resolver is never queried, so the dummy addresses are never called.
    let present = unsafe {
        SystemCornerRadiusResolver::from_lookup(|name: &CStr| {
            seen.lock().unwrap().push(name.to_owned());
            std::ptr::NonNull::<u8>::dangling().as_ptr().cast()
        })
    };
    assert!(present.symbol_present());
    assert_eq!(present.capability(), RadiusCapability::Available);
    let seen = seen.into_inner().unwrap();
    assert_eq!(seen.len(), RADIUS_SYMBOLS.len());
    assert!(seen.iter().any(|name| name.as_c_str() == c"SLSWindowIteratorGetCornerRadii"));
    assert!(seen.iter().any(|name| name.as_c_str() == c"SLSWindowQueryWindows"));
}

#[test]
fn the_radius_resolver_rejects_insane_values() {
    for raw in [f64::NAN, f64::INFINITY, -1.0, MAX_SANE_RADIUS + 0.5, 1.0e9] {
        let resolver = SystemCornerRadiusResolver::with_query(move |_| Some(raw));
        assert_eq!(resolver.corner_radius(7), None, "{raw}");
        assert_eq!(resolver.cached(), 0, "an insane value is not cached");
    }
    let none = SystemCornerRadiusResolver::with_query(|_| None);
    assert_eq!(none.corner_radius(7), None);
    for raw in [0.0, 12.0, MAX_SANE_RADIUS] {
        let resolver = SystemCornerRadiusResolver::with_query(move |_| Some(raw));
        assert_eq!(resolver.corner_radius(7), Some(raw));
    }
}

#[test]
fn the_radius_resolver_caches_per_window_until_forgotten() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let queries = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&queries);
    let resolver = SystemCornerRadiusResolver::with_query(move |id| {
        counter.fetch_add(1, Ordering::SeqCst);
        Some(f64::from(id))
    });
    assert_eq!(resolver.corner_radius(10), Some(10.0));
    assert_eq!(resolver.corner_radius(10), Some(10.0));
    assert_eq!(resolver.corner_radius(20), Some(20.0));
    assert_eq!(queries.load(Ordering::SeqCst), 2, "cached per window id");
    resolver.forget(10);
    assert_eq!(resolver.corner_radius(10), Some(10.0));
    assert_eq!(queries.load(Ordering::SeqCst), 3, "forget re-queries");

    // A failed query is not cached: it is retried.
    let flaky = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::clone(&flaky);
    let retried = SystemCornerRadiusResolver::with_query(move |_| {
        (attempts.fetch_add(1, Ordering::SeqCst) > 0).then_some(14.0)
    });
    assert_eq!(retried.corner_radius(3), None);
    assert_eq!(retried.corner_radius(3), Some(14.0));

    // Bounded.
    let bounded = SystemCornerRadiusResolver::with_query(|_| Some(8.0));
    for id in 0..u32::try_from(RADIUS_CACHE_LIMIT * 2).unwrap() {
        let _ = bounded.corner_radius(id);
    }
    assert!(bounded.cached() <= RADIUS_CACHE_LIMIT);
}

#[test]
fn the_lazy_radius_resolver_looks_nothing_up_until_first_use() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let lookups = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&lookups);
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let lazy = super::LazyCornerRadiusResolver::with(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        SystemCornerRadiusResolver::unavailable(c"SLSWindowIteratorGetCornerRadii")
    })
    .observed(move |capability| sink.lock().unwrap().push(capability));
    lazy.forget(1);
    assert!(!lazy.is_resolved(), "forget resolves nothing");
    assert_eq!(
        lookups.load(Ordering::SeqCst),
        0,
        "construction looks nothing up"
    );
    assert!(observed.lock().unwrap().is_empty());
    assert_eq!(lazy.corner_radius(1), None);
    assert_eq!(lazy.corner_radius(2), None);
    assert!(lazy.is_resolved());
    assert_eq!(lookups.load(Ordering::SeqCst), 1, "looked up once, on first use");
    assert_eq!(*observed.lock().unwrap(), vec![RadiusCapability::FallbackOnly]);

    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&observed);
    let available = super::LazyCornerRadiusResolver::with(|| {
        SystemCornerRadiusResolver::with_query(|_| Some(12.0))
    })
    .observed(move |capability| sink.lock().unwrap().push(capability));
    assert_eq!(available.corner_radius(4), Some(12.0));
    assert_eq!(*observed.lock().unwrap(), vec![RadiusCapability::Available]);
}

#[test]
fn the_radius_capability_reaches_the_control_once_resolved() {
    let rig = Rig::inline();
    assert_eq!(rig.control.capabilities().radius, RadiusCapability::FallbackOnly);
    assert_eq!(
        rig.control.diagnostics().corner_radius,
        Some(CORNER_RADIUS_DIAGNOSTIC)
    );
    rig.control.set_radius_capability(RadiusCapability::Available);
    assert_eq!(rig.control.diagnostics().corner_radius, None);
}

/// A per-window fake resolver that counts queries and forgets.
struct PerWindow {
    radii: std::collections::HashMap<u32, f64>,
    queries: std::sync::Mutex<Vec<u32>>,
    forgets: std::sync::Mutex<Vec<u32>>,
}

impl CornerRadiusResolver for PerWindow {
    fn corner_radius(&self, target_cg_id: u32) -> Option<f64> {
        self.queries.lock().unwrap().push(target_cg_id);
        self.radii.get(&target_cg_id).copied()
    }

    fn forget(&self, target_cg_id: u32) { self.forgets.lock().unwrap().push(target_cg_id); }
}

fn per_window_rig(radii: &[(u32, f64)]) -> (Rig, Arc<PerWindow>) {
    let resolver = Arc::new(PerWindow {
        radii: radii.iter().copied().collect(),
        queries: std::sync::Mutex::new(Vec::new()),
        forgets: std::sync::Mutex::new(Vec::new()),
    });
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeOverlayBackend::with_resolver(
        vec![OverlayScreen {
            frame: OverlayRect::new(0.0, 0.0, 1000.0, 800.0),
            scale: 2.0,
        }],
        Arc::clone(&resolver) as Arc<dyn CornerRadiusResolver>,
    ));
    let control = OverlayControl::new(
        Arc::clone(&executor) as Arc<dyn stache_sdk::MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn OverlayBackend>,
        OverlayCapabilities::default(),
    );
    (Rig { executor, backend, control }, resolver)
}

#[test]
fn borders_use_each_windows_real_radius_plus_one_below_and_two_above() {
    let (rig, resolver) = per_window_rig(&[(10, 26.0), (11, 12.0)]);
    let frame = rect(100.0, 100.0, 300.0, 200.0);
    let screens = vec![OverlayScreen {
        frame: OverlayRect::new(0.0, 0.0, 1000.0, 800.0),
        scale: 2.0,
    }];
    let mut above = appearance(4.0);
    above.placement = BorderPlacement::Above;
    let shows = [
        (1, 10, appearance(4.0)),
        (2, 11, appearance(4.0)),
        (3, 12, appearance(4.0)),
        (4, 10, above),
    ];
    for (handle, target, look) in shows {
        let _ = rig.control.apply(OverlayBatch::new().with(h(handle), OverlayKind::Show {
            target_cg_id: target,
            frame,
            appearance: look,
            change: AppearanceChange::Reload,
        }));
    }
    let expect = |target: Option<f64>, look: &BorderAppearance| {
        plan_overlay(frame, look, target, 16.0, &screens)
    };
    for (panel, radius, look) in [
        (1, Some(26.0), appearance(4.0)),
        (2, Some(12.0), appearance(4.0)),
        (3, None, appearance(4.0)),
        (4, Some(26.0), above),
    ] {
        let tree = rig.backend.layer_tree(panel).expect("configured");
        let plan = expect(radius, &look);
        assert!(
            (tree.inner_radius - plan.inner_radius).abs() < 1e-9,
            "panel {panel}"
        );
        assert!(
            (tree.outer_radius - plan.outer_radius).abs() < 1e-9,
            "panel {panel}"
        );
    }
    let below_real = expect(Some(26.0), &appearance(4.0));
    assert!((below_real.target_radius - (26.0 + INNER_RADIUS_ADJUSTMENT)).abs() < 1e-9);
    assert!((INNER_RADIUS_ADJUSTMENT - 1.0).abs() < f64::EPSILON);
    let above_real = expect(Some(26.0), &above);
    assert!((above_real.inner_radius - (26.0 + ABOVE_INNER_RADIUS_ADJUSTMENT)).abs() < 1e-9);
    assert!((ABOVE_INNER_RADIUS_ADJUSTMENT - 2.0).abs() < f64::EPSILON);
    let fallback = expect(None, &appearance(4.0));
    assert!((fallback.target_radius - (16.0 + INNER_RADIUS_ADJUSTMENT)).abs() < 1e-9);
    assert!(
        rig.backend.layer_tree(1).unwrap().inner_radius
            > rig.backend.layer_tree(2).unwrap().inner_radius,
        "per-window radii differ"
    );
    // New handles forget their target first; each Show queried.
    assert_eq!(*resolver.forgets.lock().unwrap(), vec![10, 11, 12, 10]);
    assert_eq!(*resolver.queries.lock().unwrap(), vec![10, 11, 12, 10]);
}

#[test]
fn order_requeries_the_radius_and_release_forgets_it() {
    let (rig, resolver) = per_window_rig(&[(10, 20.0), (11, 30.0)]);
    shown(&rig, show_with(appearance(4.0)));
    resolver.forgets.lock().unwrap().clear();
    resolver.queries.lock().unwrap().clear();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 11 }));
    assert_eq!(*resolver.forgets.lock().unwrap(), vec![11]);
    assert_eq!(*resolver.queries.lock().unwrap(), vec![11]);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Release));
    assert_eq!(*resolver.forgets.lock().unwrap(), vec![11, 11]);
}

#[test]
fn constructing_a_control_creates_nothing() {
    let rig = Rig::queued();
    assert_eq!(rig.backend.event_count(), 0);
    assert_eq!(rig.backend.open_panels(), 0);
    assert_eq!(rig.executor.queued(), 0);
}

#[test]
fn a_released_panel_is_reused_instead_of_created() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Release));
    let receipt = rig
        .control
        .apply(OverlayBatch::new().with(h(2), show(11, rect(0.0, 0.0, 50.0, 50.0))));
    let summary = receipt.ticket.wait(Duration::ZERO).expect("ack").summary;
    assert_eq!((summary.created, summary.reused), (0, 1));
    assert_eq!(rig.backend.open_panels(), 1);
    assert_eq!(rig.backend.pool_len(), 0);
}

#[test]
fn the_pool_is_bounded_and_trims_the_excess() {
    let rig = Rig::inline();
    let total = MAX_POOL as u64 + 3;
    for raw in 1..=total {
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    }
    for raw in 1..=total {
        let _ = rig.control.apply(OverlayBatch::new().with(h(raw), OverlayKind::Release));
    }
    assert_eq!(rig.backend.pool_len(), MAX_POOL);
    assert_eq!(rig.backend.open_panels(), MAX_POOL, "the excess was closed");
    assert_eq!(rig.backend.ordered_in(), 0);
}

#[test]
fn release_all_closes_live_and_pooled_overlays_and_acknowledges_the_count() {
    let rig = Rig::inline();
    for raw in 1..=5 {
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    }
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Release));
    let _ = rig.control.apply(OverlayBatch::new().with(h(2), OverlayKind::Release));
    assert_eq!((rig.backend.live_overlays(), rig.backend.pool_len()), (3, 2));
    let ack = rig.control.release_all().wait(Duration::ZERO).expect("ack");
    assert_eq!(
        ack,
        ReleaseAck { closed: 5, handles: 3 },
        "closed counts the pool; handles are the live ones a ledger is compared with"
    );
    assert_eq!(rig.backend.live_overlays(), 0);
    assert_eq!(rig.backend.pool_len(), 0);
    assert_eq!(rig.backend.open_panels(), 0);
    assert_eq!(rig.backend.ordered_in(), 0);
    // Idempotent.
    assert_eq!(
        rig.control.release_all().wait(Duration::ZERO),
        Ok(ReleaseAck { closed: 0, handles: 0 })
    );
}

#[test]
fn release_all_does_not_count_a_handle_whose_release_is_already_queued() {
    let rig = Rig::queued();
    for raw in 1..=3 {
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    }
    rig.drain();
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Release));
    // Handle 1 is being released by a queued batch: a ledger no longer lists it.
    let ticket = rig.control.release_all();
    rig.drain();
    assert_eq!(ticket.wait(Duration::ZERO).map(|ack| ack.handles), Ok(2));
}

#[test]
fn release_all_discards_pending_work_and_frees_the_cap() {
    let rig = Rig::queued();
    let pending = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    let ticket = rig.control.release_all();
    assert_eq!(pending.ticket.try_get(), Some(Err(AckError::Superseded)));
    // Work submitted after release_all is applied after it, not destroyed by it.
    let after = rig
        .control
        .apply(OverlayBatch::new().with(h(2), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    rig.drain();
    assert_eq!(
        ticket.wait(SHORT),
        Ok(ReleaseAck { closed: 0, handles: 1 }),
        "a discarded pending show was still a reserved handle"
    );
    assert_eq!(after.ticket.wait(SHORT).expect("ack").summary.shown, 1);
    assert_eq!(rig.backend.live_overlays(), 1);
    assert_eq!(rig.backend.open_panels(), 1);
    assert_eq!(rig.control.release_all().try_get(), None, "queued, not yet run");
    rig.drain();
    assert_eq!(rig.backend.open_panels(), 0);
}

#[test]
fn a_refused_show_and_a_failed_creation_leave_no_live_overlay() {
    let rig = Rig::inline();
    rig.backend.fail_creation(true);
    let receipt = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(
        receipt.ticket.wait(Duration::ZERO).expect("ack").summary.failed,
        1
    );
    assert_eq!(rig.backend.live_overlays(), 0);
    assert_eq!(rig.backend.open_panels(), 0);

    rig.backend.fail_creation(false);
    for raw in 100..(100 + MAX_OVERLAYS as u64 + 4) {
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    }
    assert_eq!(rig.backend.live_overlays(), MAX_OVERLAYS);
    let _ = rig.control.release_all().wait(Duration::ZERO);
    assert_eq!(rig.backend.live_overlays(), 0);
    assert_eq!(rig.backend.open_panels(), 0);
}

#[test]
fn the_ordering_mode_reaches_the_panel_calls() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(77, rect(0.0, 0.0, 50.0, 50.0))));
    rig.control.set_ordering_mode(OrderingMode::RaiseFallback);
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 78 }));
    let modes: Vec<_> = rig
        .backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::OrderRelative { target, mode, level, .. } => Some((target, mode, level)),
            _ => None,
        })
        .collect();
    assert_eq!(modes, vec![
        (77, OrderingMode::RelativeToTarget, 0),
        (78, OrderingMode::RaiseFallback, 0),
    ]);
}

#[test]
fn each_capability_gap_has_exactly_one_diagnostic() {
    let rig = Rig::queued();
    let diagnostics = rig.control.diagnostics();
    assert_eq!(diagnostics.ordering, None);
    assert_eq!(diagnostics.space, None);
    assert_eq!(diagnostics.corner_radius, Some(CORNER_RADIUS_DIAGNOSTIC));
    assert_eq!(diagnostics.messages().len(), 1);

    rig.control.set_ordering_mode(OrderingMode::RaiseFallback);
    rig.control.set_space_capability(SpaceCapability::Undetermined);
    let diagnostics = rig.control.diagnostics();
    assert_eq!(
        diagnostics.ordering,
        Some("borders: overlays cannot be ordered relative to other applications' windows")
    );
    assert_eq!(diagnostics.space, Some(SPACE_DIAGNOSTIC));
    assert_eq!(diagnostics.messages().len(), 3);

    let healthy = OverlayDiagnostics::from_capabilities(&OverlayCapabilities {
        ordering: OrderingMode::RelativeToTarget,
        radius: RadiusCapability::Available,
        space: SpaceCapability::Structural,
    });
    assert!(healthy.messages().is_empty());
}

// ------------------------------------------------------------------ TB-02-02

/// Gradient parameter of a unit-space point: 0 at `start`'s isoline, 1 at `end`'s.
fn gradient_t(start: UnitPoint, end: UnitPoint, q: (f64, f64)) -> f64 {
    let (ux, uy) = (end.x - start.x, end.y - start.y);
    ((q.0 - start.x) * ux + (q.1 - start.y) * uy) / (ux * ux + uy * uy)
}

const UNIT_CORNERS: [(f64, f64); 4] = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];

/// The direction in points, in degrees `[0, 360)`: `M^-1 (end - start)` with `M = diag(w, h)`.
fn point_direction(size: (f64, f64), start: UnitPoint, end: UnitPoint) -> f64 {
    ((end.y - start.y) / size.1)
        .atan2((end.x - start.x) / size.0)
        .to_degrees()
        .rem_euclid(360.0)
}

fn angle_gap(a: f64, b: f64) -> f64 {
    let gap = (a - b).rem_euclid(360.0);
    gap.min(360.0 - gap)
}

#[test]
fn gradient_points_draw_the_exact_angle_with_from_on_the_documented_corner() {
    // (angle, corners where `from` lands: the corner or the edge it lies on).
    let table: [(f64, &[(f64, f64)]); 12] = [
        (0.0, &[(0.0, 0.0), (0.0, 1.0)]),
        (45.0, &[(0.0, 0.0)]),
        (90.0, &[(0.0, 0.0), (1.0, 0.0)]),
        (135.0, &[(1.0, 0.0)]),
        (180.0, &[(1.0, 0.0), (1.0, 1.0)]),
        (225.0, &[(1.0, 1.0)]),
        (270.0, &[(0.0, 1.0), (1.0, 1.0)]),
        (315.0, &[(0.0, 1.0)]),
        (359.99, &[(0.0, 1.0)]),
        (-90.0, &[(0.0, 1.0), (1.0, 1.0)]),
        (360.0, &[(0.0, 0.0), (0.0, 1.0)]),
        (720.0, &[(0.0, 0.0), (0.0, 1.0)]),
    ];
    for size in [(200.0, 200.0), (300.0, 100.0), (100.0, 300.0)] {
        for (angle, from_corners) in table {
            let (start, end) = gradient_points(size, angle);
            let label = format!("{angle} on {size:?}");
            let expected = angle.rem_euclid(360.0);
            assert!(
                angle_gap(point_direction(size, start, end), expected) < 0.01,
                "direction {label}"
            );
            for corner in UNIT_CORNERS {
                let on_from = from_corners.contains(&corner);
                // The opposite corner (or edge) carries `to`.
                let opposite = (1.0 - corner.0, 1.0 - corner.1);
                let t = gradient_t(start, end, corner);
                if on_from {
                    assert!(t.abs() < 1e-6, "from at {corner:?} {label}: t={t}");
                    let t_opposite = gradient_t(start, end, opposite);
                    assert!((t_opposite - 1.0).abs() < 1e-6, "to at {opposite:?} {label}");
                } else if !from_corners.contains(&opposite) {
                    assert!(
                        t > 1e-6 && t < 1.0 - 1e-6,
                        "{corner:?} is interior {label}: t={t}"
                    );
                }
            }
        }
    }
}

#[test]
fn gradient_points_match_the_old_bucket_corners_at_225_and_315() {
    for size in [(200.0, 200.0), (300.0, 100.0), (100.0, 300.0)] {
        // 225: top-right -> bottom-left, as the old buckets drew [180, 270).
        let (start, end) = gradient_points(size, 225.0);
        assert!(gradient_t(start, end, (1.0, 1.0)).abs() < 1e-9, "{size:?}");
        assert!(
            (gradient_t(start, end, (0.0, 0.0)) - 1.0).abs() < 1e-9,
            "{size:?}"
        );
        // 315: top-left -> bottom-right, as the old buckets drew [270, 360).
        let (start, end) = gradient_points(size, 315.0);
        assert!(gradient_t(start, end, (0.0, 1.0)).abs() < 1e-9, "{size:?}");
        assert!(
            (gradient_t(start, end, (1.0, 0.0)) - 1.0).abs() < 1e-9,
            "{size:?}"
        );
    }
    // Square diagonals put the stops exactly on the corners.
    let (start, end) = gradient_points((50.0, 50.0), 45.0);
    assert!(start.x.abs() < 1e-9 && start.y.abs() < 1e-9);
    assert!((end.x - 1.0).abs() < 1e-9 && (end.y - 1.0).abs() < 1e-9);
}

#[test]
fn gradient_points_are_safe_for_degenerate_input() {
    for (size, angle) in [
        ((0.0, 0.0), 45.0),
        ((10.0, 0.0), 90.0),
        ((10.0, 10.0), f64::NAN),
    ] {
        let (start, end) = gradient_points(size, angle);
        for value in [start.x, start.y, end.x, end.y] {
            assert!(value.is_finite(), "{size:?} {angle}");
        }
    }
}

// ------------------------------------------------------------------ TB-02-03

pub(super) fn shown(rig: &Rig, kind: OverlayKind) {
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), kind));
}

pub(super) fn show_with(appearance: BorderAppearance) -> OverlayKind {
    OverlayKind::Show {
        target_cg_id: 10,
        frame: rect(100.0, 100.0, 300.0, 200.0),
        appearance,
        change: AppearanceChange::Reload,
    }
}

pub(super) fn paint(paint: BorderPaint, width: f64) -> BorderAppearance {
    BorderAppearance { paint, ..appearance(width) }
}

#[test]
fn recolor_angle_and_width_changes_mutate_the_same_panel_in_place() {
    let rig = Rig::inline();
    shown(&rig, show_with(paint(BorderPaint::Solid(RED), 4.0)));
    let first = rig.backend.layer_tree(1).expect("tree");
    assert_eq!(first.fill, FillSpec::Solid(RED));

    // Recolor.
    shown(&rig, show_with(paint(BorderPaint::Solid(BLUE), 4.0)));
    assert_eq!(
        rig.backend.layer_tree(1).expect("tree").fill,
        FillSpec::Solid(BLUE)
    );

    // Solid -> gradient at 0, then a new angle, on the same panel.
    let gradient = |angle| BorderPaint::Gradient { from: RED, to: BLUE, angle };
    shown(&rig, show_with(paint(gradient(0.0), 4.0)));
    let FillSpec::Gradient { start: s0, end: e0, .. } = rig.backend.layer_tree(1).unwrap().fill
    else {
        panic!("gradient fill");
    };
    shown(&rig, show_with(paint(gradient(90.0), 4.0)));
    let FillSpec::Gradient { start: s1, end: e1, .. } = rig.backend.layer_tree(1).unwrap().fill
    else {
        panic!("gradient fill");
    };
    assert_ne!((s0, e0), (s1, e1), "the angle changed the gradient points");
    assert!(
        (s1.x - 0.5).abs() < 1e-9 && s1.y.abs() < 1e-9,
        "90 starts at the bottom edge"
    );

    // Width change moves the edges of the same layers.
    shown(&rig, show_with(paint(gradient(90.0), 8.0)));
    let wide = rig.backend.layer_tree(1).unwrap();
    assert!((wide.inner.x - 8.0).abs() < f64::EPSILON);

    // Glow: no crisp ring, only the rendered soft band and a clip that excludes the target.
    assert_eq!(rig.backend.shown_layers(1), vec![AnimLayer::Gradient]);
    shown(&rig, show_with(paint(BorderPaint::Glow(RED), 8.0)));
    assert_eq!(
        rig.backend.shown_layers(1),
        vec![AnimLayer::Glow],
        "the ring is hidden"
    );
    let glow = rig.backend.layer_tree(1).unwrap();
    assert_eq!(glow.fill, FillSpec::Glow {
        color: RED,
        blur: 12.0,
        core: 4.0
    });
    let render = glow.glow_render().expect("soft band");
    assert!(close(render.blur, 12.0));
    assert!(
        close(render.core.width, render.clip.hole.width + 8.0),
        "hole outset by 4"
    );
    assert!(close(render.core_radius, glow.clip.unwrap().radius + 4.0));
    assert!(
        glow.bounds.width >= 300.0 + 2.0 * glow_radius(8.0),
        "the panel margin holds the whole band"
    );
    let clip = glow.clip.expect("glow is clipped to the outside of the target");
    assert!((clip.hole.width - 300.0).abs() < f64::EPSILON);
    assert!(
        rig.backend.layer_tree(1).unwrap().bounds.width > 300.0 + 16.0,
        "room for the shadow"
    );

    // Back to solid drops the shadow and the clip.
    shown(&rig, show_with(paint(BorderPaint::Solid(BLUE), 4.0)));
    assert_eq!(rig.backend.layer_tree(1).unwrap().clip, None);

    assert_eq!(rig.backend.panels_created(), 1, "no recreation");
    assert_eq!(rig.backend.open_panels(), 1);
    assert_eq!(
        rig.backend.implicit_animations(),
        0,
        "implicit actions stay disabled"
    );
    assert_eq!(rig.backend.ordered_in(), 1);
}

#[test]
fn a_resize_recomputes_the_gradient_points() {
    let rig = Rig::inline();
    let gradient = BorderPaint::Gradient {
        from: RED,
        to: BLUE,
        angle: 45.0,
    };
    shown(&rig, show_with(paint(gradient, 4.0)));
    let before = rig.backend.layer_tree(1).unwrap();
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(100.0, 100.0, 600.0, 100.0),
    }));
    let after = rig.backend.layer_tree(1).unwrap();
    assert_ne!(before.fill, after.fill);
    let FillSpec::Gradient { start, end, .. } = after.fill else {
        panic!("gradient fill")
    };
    let size = (after.ring_frame.width, after.ring_frame.height);
    assert!(angle_gap(point_direction(size, start, end), 45.0) < 0.01);
}

#[test]
fn width_zero_hides_the_overlay_and_a_later_width_shows_it_again() {
    let rig = Rig::inline();
    shown(&rig, show_with(paint(BorderPaint::Solid(RED), 4.0)));
    assert_eq!(rig.backend.ordered_in(), 1);
    shown(&rig, show_with(paint(BorderPaint::Solid(RED), 0.0)));
    assert_eq!(rig.backend.ordered_in(), 0, "ordered out, not drawn transparent");
    assert_eq!(rig.backend.layer_tree(1).unwrap().fill, FillSpec::None);
    shown(&rig, show_with(paint(BorderPaint::Solid(RED), 3.0)));
    assert_eq!(rig.backend.ordered_in(), 1);
    assert_eq!(rig.backend.panels_created(), 1);

    // A first Show with width 0 never orders the panel in.
    let fresh = Rig::inline();
    shown(&fresh, show_with(paint(BorderPaint::Solid(RED), 0.0)));
    assert_eq!(fresh.backend.ordered_in(), 0);
}

#[test]
fn a_hide_orders_the_overlay_out_and_a_corner_radius_override_wins() {
    let rig = Rig::inline();
    shown(&rig, show_with(paint(BorderPaint::Solid(RED), 4.0)));
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Hide));
    assert_eq!(rig.backend.ordered_in(), 0);

    let screens = two_displays();
    let mut explicit = appearance(4.0);
    explicit.corner_radius = Some(5.0);
    explicit.placement = BorderPlacement::Above;
    let plan = plan_overlay(
        rect(0.0, 0.0, 100.0, 100.0),
        &explicit,
        Some(20.0),
        16.0,
        &screens,
    );
    assert!(
        (plan.inner_radius - 7.0).abs() < f64::EPSILON,
        "override + 2 above"
    );
    explicit.corner_radius = None;
    let plan = plan_overlay(
        rect(0.0, 0.0, 100.0, 100.0),
        &explicit,
        Some(20.0),
        16.0,
        &screens,
    );
    assert!(
        (plan.inner_radius - 22.0).abs() < f64::EPSILON,
        "None keeps the resolved radius (+ 2 above)"
    );
}

// ------------------------------------------------------------------ TB-05-02

fn ordered_in(rig: &Rig) -> usize {
    rig.backend
        .events()
        .iter()
        .filter(|event| matches!(event, FakeEvent::OrderRelative { .. }))
        .count()
}

#[test]
fn a_show_computed_before_a_hide_all_and_applied_after_it_is_dropped() {
    let rig = Rig::queued();
    // Epoch 0: shown and live.
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    rig.drain();
    assert_eq!(rig.backend.ordered_in(), 1);

    // A re-show computed under epoch 0 waits in the queue; the gate then hides
    // everything under epoch 1. The re-show must not survive.
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(5.0, 5.0, 50.0, 50.0))));
    let _ = rig
        .control
        .apply(OverlayBatch::hide_all(HideReason::MissionControl).at_epoch(1));
    rig.drain();

    assert_eq!(rig.backend.ordered_in(), 0, "hidden, not re-shown");
    assert_eq!(ordered_in(&rig), 1, "only the first show ever ordered in");
    assert_eq!(rig.control.counters().stale_dropped, 1);
}

#[test]
fn a_show_applied_in_a_later_batch_than_the_hide_all_is_dropped_but_a_newer_one_is_not() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    let _ = rig.control.apply(OverlayBatch::hide_all(HideReason::SpaceChange).at_epoch(3));
    assert_eq!(rig.backend.ordered_in(), 0);

    // A late stale Show and a stale Order are dropped; the reservation stays.
    let _ = rig.control.apply(
        OverlayBatch::new()
            .at_epoch(2)
            .with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0)))
            .with(h(1), OverlayKind::Order { target_cg_id: 11 }),
    );
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(
        rig.control.counters().stale_dropped,
        1,
        "show and order merged into one entry"
    );

    // Equal or newer epochs are applied: the gate reopened at epoch 3.
    let _ = rig
        .control
        .apply(OverlayBatch::new().at_epoch(3).with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(rig.backend.ordered_in(), 1);
}

#[test]
fn a_stale_order_alone_is_dropped_and_a_move_never_shows_a_hidden_overlay() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    let _ = rig
        .control
        .apply(OverlayBatch::hide_all(HideReason::MissionControl).at_epoch(1));
    let before = ordered_in(&rig);
    let _ = rig.control.apply(
        OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 12 }).with(
            h(1),
            OverlayKind::Move {
                frame: rect(9.0, 9.0, 50.0, 50.0),
            },
        ),
    );
    assert_eq!(ordered_in(&rig), before);
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.control.counters().stale_dropped, 1);
}

#[test]
fn hide_all_overtakes_pending_batches_and_is_never_merged_away_with_epochs() {
    let rig = Rig::queued();
    for raw in 1..=3 {
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(raw), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    }
    rig.drain();
    assert_eq!(rig.backend.ordered_in(), 3);

    // Pending: a Show (epoch 0), then HideAll (epoch 1), then another batch
    // under epoch 1 that must be applied after the HideAll.
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(1.0, 1.0, 50.0, 50.0))));
    let _ = rig.control.apply(OverlayBatch::hide_all(HideReason::Unknown).at_epoch(1));
    let _ = rig
        .control
        .apply(OverlayBatch::new().at_epoch(1).with(h(2), show(10, rect(2.0, 2.0, 50.0, 50.0))));
    let before = rig.backend.event_count();
    rig.drain();
    let events = rig.backend.events();
    let applied = &events[before..];
    let first_out = applied.iter().position(|e| matches!(e, FakeEvent::OrderOut(_)));
    let first_in = applied.iter().position(|e| matches!(e, FakeEvent::OrderRelative { .. }));
    assert!(first_out.expect("hid") < first_in.expect("the newer show ran"));
    assert_eq!(rig.backend.ordered_in(), 1, "only the epoch-1 show");
    assert_eq!(rig.control.counters().hide_alls, 1);
}

#[test]
fn a_new_control_after_release_all_starts_again_at_epoch_zero() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    let _ = rig
        .control
        .apply(OverlayBatch::hide_all(HideReason::MissionControl).at_epoch(9));
    let _ = rig.control.release_all();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(2), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(
        rig.backend.ordered_in(),
        1,
        "epoch 0 shows again after a restart"
    );
}

// ------------------------------------------------------------------ mailbox

mod mailbox {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::super::spaces::{
        MAX_SPACE_FACTS, NoSpaceEvents, PushOutcome, SpaceEvents, SpaceFact, SpaceMailbox,
        UnknownCause,
    };
    use super::*;

    #[test]
    fn facts_are_bounded_ordered_and_wake_the_actor() {
        let mailbox = SpaceMailbox::new();
        let wakes = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&wakes);
        mailbox.set_waker(Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })));
        let sink = mailbox.sink();
        assert_eq!(sink.push(SpaceFact::MissionControlEntered), PushOutcome::Queued);
        assert_eq!(sink.push(SpaceFact::MissionControlExited), PushOutcome::Queued);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        let drained = mailbox.drain();
        assert_eq!(drained.facts, [
            SpaceFact::MissionControlEntered,
            SpaceFact::MissionControlExited
        ]);
        assert!(!drained.overflowed);
        assert!(!mailbox.has_pending());
    }

    #[test]
    fn overflow_drops_the_fact_and_reports_it_once() {
        let mailbox = SpaceMailbox::new();
        let sink = mailbox.sink();
        for _ in 0..MAX_SPACE_FACTS {
            assert_eq!(sink.push(SpaceFact::DisplayLayoutChanged), PushOutcome::Queued);
        }
        assert_eq!(
            sink.push(SpaceFact::DisplayLayoutChanging),
            PushOutcome::Overflowed
        );
        let drained = mailbox.drain();
        assert_eq!(drained.facts.len(), MAX_SPACE_FACTS);
        assert!(drained.overflowed);
        assert!(!mailbox.drain().overflowed, "reported once");
    }

    #[test]
    fn a_closed_mailbox_accepts_nothing_until_reopened() {
        let mailbox = SpaceMailbox::new();
        let sink = mailbox.sink();
        let _ = sink.push(SpaceFact::MissionControlEntered);
        mailbox.close();
        assert_eq!(sink.push(SpaceFact::MissionControlEntered), PushOutcome::Closed);
        assert!(mailbox.drain().facts.is_empty(), "close discards the queue");
        mailbox.reopen();
        assert_eq!(
            sink.push(SpaceFact::SourceLost(UnknownCause::OwnerStopped)),
            PushOutcome::Queued
        );
    }

    #[test]
    fn the_no_source_never_reports() {
        let mailbox = SpaceMailbox::new();
        let mut none = NoSpaceEvents;
        none.start(mailbox.sink());
        none.resync();
        none.stop();
        assert!(!mailbox.has_pending());
    }

    #[test]
    fn causes_read_as_text() {
        assert_eq!(
            UnknownCause::MissingSymbol("SLSx").to_string(),
            "missing symbol SLSx"
        );
        assert_eq!(UnknownCause::DockUnregistered.to_string(), "Dock not registered");
    }
}

// ------------------------------------------------------------ placement

fn placed(width: f64, placement: BorderPlacement) -> BorderAppearance {
    BorderAppearance { placement, ..appearance(width) }
}

fn plan_for(look: &BorderAppearance, resolved: Option<f64>) -> OverlayPlan {
    plan_overlay(
        rect(300.0, 100.0, 400.0, 300.0),
        look,
        resolved,
        16.0,
        &two_displays(),
    )
}

fn close(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

#[test]
fn the_default_placement_is_below() {
    assert_eq!(BorderPlacement::default(), BorderPlacement::Below);
    assert_eq!(appearance(4.0).placement, BorderPlacement::Below);
}

#[test]
fn five_sixths_of_the_width_is_visible_outside_the_target_for_both_placements() {
    for placement in [BorderPlacement::Below, BorderPlacement::Above] {
        let plan = plan_for(&placed(12.0, placement), Some(10.0));
        let visible = 12.0 * VISIBLE_FRACTION;
        assert!(close(visible, 10.0));
        assert!(close(plan.target.x - plan.outer.x, visible), "{placement:?}");
        assert!(
            close(plan.outer.width - plan.target.width, 2.0 * visible),
            "{placement:?}"
        );
        assert!(
            close(plan.outer.height - plan.target.height, 2.0 * visible),
            "{placement:?}"
        );
        assert!(close(
            plan.panel_frame.width,
            400.0 + 2.0 * (visible + OVERLAY_PADDING)
        ));
    }
}

#[test]
fn below_places_the_inner_edge_inside_the_target_by_a_sixth_of_the_width() {
    let plan = plan_for(&placed(12.0, BorderPlacement::Below), Some(10.0));
    assert!(close(plan.inner.x - plan.target.x, 2.0));
    assert!(close(plan.inner.y - plan.target.y, 2.0));
    assert!(close(plan.inner.width, 400.0 - 4.0));
    assert!(close(plan.inner.height, 300.0 - 4.0));
    // Radius 10 + 1, less the 2 hidden points; the outer edge adds the 10 visible.
    assert!(close(plan.inner_radius, 9.0));
    assert!(close(plan.outer_radius, 21.0));
    assert!(close(plan.target_radius, 11.0));
}

#[test]
fn below_clamps_the_inner_radius_at_zero_and_the_inward_extension_to_the_target() {
    let plan = plan_for(&placed(30.0, BorderPlacement::Below), Some(4.0));
    assert!(close(plan.inner_radius, 0.0), "5 - 5 clamps to zero");
    let tiny = plan_overlay(
        rect(0.0, 0.0, 6.0, 4.0),
        &placed(30.0, BorderPlacement::Below),
        Some(2.0),
        16.0,
        &two_displays(),
    );
    assert!(tiny.inner.width >= 0.0 && tiny.inner.height >= 0.0);
    assert!(tiny.inner_radius >= 0.0);
}

#[test]
fn above_places_the_inner_edge_on_the_target_frame() {
    let plan = plan_for(&placed(12.0, BorderPlacement::Above), Some(10.0));
    assert_eq!(plan.inner, plan.target);
    assert!(close(plan.inner_radius, 12.0));
    assert!(close(plan.outer_radius, 22.0));
}

#[test]
fn the_inner_radius_gains_two_points_above_and_none_for_square_windows() {
    let above = |resolved| plan_for(&placed(6.0, BorderPlacement::Above), resolved);
    assert!(close(above(Some(20.0)).inner_radius, 22.0));
    assert!(close(above(None).inner_radius, 18.0), "fallback 16 + 2");
    let flat = above(Some(0.0));
    assert!(
        close(flat.inner_radius, 0.0) && close(flat.outer_radius, 0.0),
        "a real square window gets no adjustment"
    );
    assert!(close(flat.target_radius, 0.0));
}

#[test]
fn above_rounds_the_inner_edge_one_point_more_than_below() {
    assert!(close(INNER_RADIUS_ADJUSTMENT, 1.0));
    assert!(close(ABOVE_INNER_RADIUS_ADJUSTMENT, 2.0));
    let above = plan_for(&placed(6.0, BorderPlacement::Above), Some(20.0));
    let below = plan_for(&placed(6.0, BorderPlacement::Below), Some(20.0));
    assert!(close(above.target_radius, 22.0), "above: radius + 2");
    assert!(close(below.target_radius, 21.0), "below: radius + 1");
    assert!(
        close(above.outer_radius, 22.0 + 5.0),
        "outer follows + visible width"
    );
    assert!(close(below.outer_radius, 21.0 + 5.0));
}

#[test]
fn the_glow_clip_and_blur_follow_the_target_for_both_placements() {
    for placement in [BorderPlacement::Below, BorderPlacement::Above] {
        let look = BorderAppearance {
            placement,
            paint: BorderPaint::Glow(RED),
            ..appearance(9.0)
        };
        let spec = plan_for(&look, Some(10.0)).layer_spec();
        let clip = spec.clip.expect("glow is clipped");
        let plan = plan_for(&look, Some(10.0));
        assert_eq!(clip.hole, plan.target, "{placement:?}");
        let edge = if placement == BorderPlacement::Above {
            12.0
        } else {
            11.0
        };
        assert!(close(clip.radius, edge), "{placement:?}");
        assert_eq!(
            spec.fill,
            FillSpec::Glow {
                color: RED,
                blur: 13.5,
                core: 4.5
            },
            "full width blur and core"
        );
    }
}

fn order_events(rig: &Rig) -> Vec<(u32, BorderPlacement)> {
    rig.backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::OrderRelative { target, placement, .. } => Some((target, placement)),
            _ => None,
        })
        .collect()
}

#[test]
fn ordering_records_the_placement_relative_to_the_target() {
    for placement in [BorderPlacement::Below, BorderPlacement::Above] {
        let rig = Rig::inline();
        shown(&rig, show_with(placed(4.0, placement)));
        assert_eq!(order_events(&rig), vec![(10, placement)]);
    }
}

#[test]
fn ordering_on_focus_keeps_the_chosen_placement() {
    for placement in [BorderPlacement::Below, BorderPlacement::Above] {
        let rig = Rig::inline();
        shown(&rig, show_with(placed(4.0, placement)));
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 33 }));
        assert_eq!(order_events(&rig), vec![(10, placement), (33, placement)]);
    }
}

fn order_levels(rig: &Rig) -> Vec<(BorderPlacement, isize)> {
    rig.backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::OrderRelative { placement, level, .. } => Some((placement, level)),
            _ => None,
        })
        .collect()
}

/// `Top` is ordered above its target one window level higher, so the target's
/// own raise cannot cover the ring, and it is drawn exactly like `Above`.
#[test]
fn a_top_border_is_ordered_one_level_above_its_target_and_drawn_like_above() {
    for (placement, raised) in [
        (BorderPlacement::Below, 0),
        (BorderPlacement::Above, 0),
        (BorderPlacement::Top, 1),
    ] {
        let rig = Rig::inline();
        shown(&rig, show_with(placed(4.0, placement)));
        let _ = rig
            .control
            .apply(OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 10 }));
        assert_eq!(
            order_levels(&rig),
            vec![(placement, raised), (placement, raised)],
            "the show and a later re-order use the same level"
        );
        assert_eq!(
            placement.panel_level(3),
            3 + raised,
            "relative to the target's level"
        );
    }
    for resolved in [Some(20.0), Some(0.0), None] {
        assert_eq!(
            plan_for(&placed(6.0, BorderPlacement::Top), resolved),
            plan_for(&placed(6.0, BorderPlacement::Above), resolved)
        );
    }
}

/// A visible border that gains or loses the top placement (its window took or
/// lost the focus) is re-ordered at its new level, never ordered out and in.
#[test]
fn a_visible_border_changing_to_or_from_top_is_re_ordered_at_its_new_level() {
    let rig = Rig::inline();
    shown(&rig, show_with(placed(4.0, BorderPlacement::Above)));
    shown(&rig, show_with(placed(4.0, BorderPlacement::Top)));
    shown(&rig, show_with(placed(4.0, BorderPlacement::Top)));
    shown(&rig, show_with(placed(4.0, BorderPlacement::Above)));
    assert_eq!(
        order_levels(&rig),
        vec![
            (BorderPlacement::Above, 0),
            (BorderPlacement::Top, 1),
            (BorderPlacement::Above, 0)
        ],
        "one order per change, none for the unchanged show"
    );
    assert!(
        !rig.backend.events().iter().any(|event| matches!(event, FakeEvent::OrderOut(_))),
        "never ordered out"
    );
}

// ------------------------------------------------- translation-only moves

fn translations(backend: &FakeOverlayBackend) -> Vec<(u64, OverlayPlan)> {
    backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::Translate(panel, plan) => Some((panel, plan)),
            _ => None,
        })
        .collect()
}

/// Native run 2 follow-up: a drag moves the panel on every sample. When only the
/// position changed, the main thread sets the panel's frame origin and does not
/// re-plan or reconfigure the layers.
#[test]
fn a_move_that_only_translates_sets_the_panel_origin_and_leaves_the_layers_alone() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(100.0, 100.0, 300.0, 200.0))));
    assert_eq!(configures(&rig.backend).len(), 1);
    assert!(translations(&rig.backend).is_empty());
    for step in 1..=5 {
        let x = 100.0 + f64::from(step) * 7.0;
        let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
            frame: rect(x, 100.0 + f64::from(step), 300.0, 200.0),
        }));
    }
    assert_eq!(
        configures(&rig.backend).len(),
        1,
        "no layer reconfigure on a pure translation"
    );
    let moved = translations(&rig.backend);
    assert_eq!(moved.len(), 5);
    let first = configures(&rig.backend)[0].1;
    let last = moved[4].1;
    assert!((last.panel_frame.x - first.panel_frame.x - 35.0).abs() < 1e-9);
    assert!((last.panel_frame.width - first.panel_frame.width).abs() < 1e-9);
    assert_eq!(
        last.layer_spec(),
        first.layer_spec(),
        "the layers are what they were"
    );
}

#[test]
fn the_glow_bitmap_renders_once_and_again_only_when_its_inputs_change() {
    let rig = Rig::inline();
    let glow_at = |frame: OverlayRect, color: BorderColor, width: f64| OverlayKind::Show {
        target_cg_id: 10,
        frame,
        appearance: paint(BorderPaint::Glow(color), width),
        change: AppearanceChange::Reload,
    };
    shown(&rig, glow_at(rect(100.0, 100.0, 300.0, 200.0), RED, 6.0));
    let renders = rig.backend.glow_renders(1);
    assert_eq!(renders.len(), 1, "the first show renders the halo");
    let first = renders[0];
    assert_eq!(first.color, RED);
    assert!(close(first.blur, glow_blur(6.0)));
    assert!(close(first.core.x, first.clip.hole.x - glow_core(6.0)));
    let tree = rig.backend.layer_tree(1).expect("tree");
    assert!(
        first.bounds.width < tree.bounds.width && first.bounds.height < tree.bounds.height,
        "a tile smaller than the panel"
    );
    assert!(close(
        first.scale,
        rig.backend.layer_tree(1).expect("tree").scale
    ));
    assert!(
        close(first.clip.hole.x, tree.clip.expect("clip").hole.x),
        "panel margin"
    );

    // Pure translations: the fast path, and a full show at a moved frame.
    for step in 1..=3 {
        let x = 100.0 + f64::from(step) * 9.0;
        let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
            frame: rect(x, 100.0, 300.0, 200.0),
        }));
    }
    shown(&rig, glow_at(rect(150.0, 120.0, 300.0, 200.0), RED, 6.0));
    assert_eq!(
        rig.backend.glow_renders(1).len(),
        1,
        "a translation never re-renders"
    );

    // A resize (a show or a move) never re-renders the tile; width, corner
    // radius and color each re-render once.
    shown(&rig, glow_at(rect(150.0, 120.0, 320.0, 200.0), RED, 6.0));
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(150.0, 120.0, 900.0, 600.0),
    }));
    assert_eq!(rig.backend.glow_renders(1).len(), 1, "resize");
    shown(&rig, glow_at(rect(150.0, 120.0, 320.0, 200.0), RED, 12.0));
    let renders = rig.backend.glow_renders(1);
    assert_eq!(renders.len(), 2, "width");
    assert!(close(renders[1].blur, glow_blur(12.0)));
    shown(&rig, glow_at(rect(150.0, 120.0, 320.0, 200.0), BLUE, 12.0));
    assert_eq!(rig.backend.glow_renders(1).len(), 3, "color");
    shown(&rig, OverlayKind::Show {
        target_cg_id: 10,
        frame: rect(150.0, 120.0, 320.0, 200.0),
        appearance: BorderAppearance {
            corner_radius: Some(4.0),
            ..paint(BorderPaint::Glow(BLUE), 12.0)
        },
        change: AppearanceChange::Reload,
    });
    assert_eq!(rig.backend.glow_renders(1).len(), 4, "corner radius");
    shown(&rig, glow_at(rect(150.0, 120.0, 320.0, 200.0), BLUE, 12.0));
    assert_eq!(rig.backend.glow_renders(1).len(), 5, "radius back");
    shown(&rig, glow_at(rect(150.0, 120.0, 320.0, 200.0), BLUE, 12.0));
    assert_eq!(rig.backend.glow_renders(1).len(), 5, "unchanged");

    // Solid and back: the cached bitmap still matches, no re-render.
    shown(&rig, show_with(paint(BorderPaint::Solid(BLUE), 12.0)));
    assert!(rig.backend.layer_tree(1).expect("tree").glow_render().is_none());
    shown(&rig, glow_at(rect(150.0, 120.0, 320.0, 200.0), BLUE, 12.0));
    assert_eq!(rig.backend.glow_renders(1).len(), 5, "cached contents reused");
}

#[test]
fn a_move_that_resizes_or_changes_the_display_scale_replans_the_layers() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(100.0, 100.0, 300.0, 200.0))));
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(100.0, 100.0, 320.0, 200.0),
    }));
    assert_eq!(configures(&rig.backend).len(), 2, "a resize reconfigures");
    assert!(translations(&rig.backend).is_empty());

    // Crossing to a display with another scale changes the layers' scale.
    let backend = Arc::new(FakeOverlayBackend::new(two_displays(), None));
    let control = OverlayControl::new(
        Arc::new(ManualExecutor::new(true)) as Arc<dyn stache_sdk::MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn OverlayBackend>,
        OverlayCapabilities::default(),
    );
    let _ =
        control.apply(OverlayBatch::new().with(h(1), show(10, rect(300.0, 100.0, 400.0, 300.0))));
    let _ = control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(1500.0, 100.0, 400.0, 300.0),
    }));
    assert_eq!(configures(&backend).len(), 2, "a new display scale reconfigures");
    assert!(translations(&backend).is_empty());
}

/// The glow tile is independent of the window size, its `contentsCenter` is a
/// centered strip whose corners hold the whole corner band, the interior is
/// clear, and a display scale change re-renders it. A target too small to
/// slice renders at the panel size with the full unit center.
#[test]
fn the_glow_tile_is_independent_of_the_window_size() {
    let look = paint(BorderPaint::Glow(RED), 6.0);
    let screens = |scale| {
        vec![OverlayScreen {
            frame: OverlayRect::new(0.0, 0.0, 3000.0, 2000.0),
            scale,
        }]
    };
    let render = |w, h, scale| {
        plan_overlay(rect(100.0, 100.0, w, h), &look, None, 16.0, &screens(scale))
            .layer_spec()
            .glow_render()
            .expect("glow")
    };
    let small = render(300.0, 200.0, 2.0);
    let large = render(1440.0, 900.0, 2.0);
    assert_eq!(small, large, "same tile at any size");
    let side = small.bounds.width;
    assert!(
        close(side, small.bounds.height) && close(side, side.round()),
        "{small:?}"
    );
    assert!(
        side * 2.0 * side * 2.0 * 4.0 < 300_000.0,
        "bounded bitmap: {side}"
    );
    // The center strip lies past each corner's reach and inside the hole.
    let reach = small.clip.radius + glow_core(6.0) + 2.0 * glow_blur(6.0);
    let c = small.center;
    assert!(close(c.x, c.y) && close(c.width, c.height));
    assert!(close(c.x * side, small.clip.hole.x + reach));
    assert!(c.width * side >= 2.0 - 1e-9);
    assert!(close(c.x.mul_add(2.0, c.width), 1.0), "symmetric");
    assert!(
        c.x * side >= small.clip.hole.x
            && (c.x + c.width) * side <= small.clip.hole.x + small.clip.hole.width
    );
    assert_ne!(render(300.0, 200.0, 1.0), small, "scale re-renders");

    let tiny = render(30.0, 20.0, 2.0);
    let panel =
        plan_overlay(rect(100.0, 100.0, 30.0, 20.0), &look, None, 16.0, &screens(2.0)).layer_spec();
    assert_eq!(tiny.bounds, panel.bounds, "fallback: the panel bitmap");
    assert_eq!(tiny.center, OverlayRect::new(0.0, 0.0, 1.0, 1.0));
    assert_eq!(tiny.clip, panel.clip.expect("clip"));
}

// ------------------------------------------------------------ ordering storms

fn ordering_events(backend: &FakeOverlayBackend) -> (usize, usize) {
    let events = backend.events();
    let relative = events.iter().filter(|e| matches!(e, FakeEvent::OrderRelative { .. })).count();
    let out = events.iter().filter(|e| matches!(e, FakeEvent::OrderOut(_))).count();
    (relative, out)
}

#[test]
fn a_recolor_or_grow_of_a_visible_panel_never_reorders_it() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(ordering_events(&rig.backend), (1, 0));
    for change in [
        AppearanceChange::StateChange,
        AppearanceChange::Grow,
        AppearanceChange::Reload,
    ] {
        let kind = OverlayKind::Show {
            target_cg_id: 10,
            frame: rect(0.0, 0.0, 50.0, 50.0),
            appearance: BorderAppearance {
                paint: BorderPaint::Solid(BLUE),
                ..appearance(4.0)
            },
            change,
        };
        let _ = rig.control.apply(OverlayBatch::new().with(h(1), kind));
    }
    assert_eq!(
        ordering_events(&rig.backend),
        (1, 0),
        "no re-order, no order-out"
    );
}

#[test]
fn an_order_of_a_visible_panel_orders_it_once_without_ordering_out_or_reconfiguring() {
    let rig = Rig::inline();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    let configured = configures(&rig.backend).len();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 10 }));
    assert_eq!(ordering_events(&rig.backend), (2, 0));
    assert_eq!(
        configures(&rig.backend).len(),
        configured,
        "an unchanged radius rewrites no layer"
    );
}

#[test]
fn the_control_records_each_panel_window_number_after_main_applied_its_show() {
    let rig = Rig::queued();
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), show(10, rect(0.0, 0.0, 50.0, 50.0))));
    assert_eq!(rig.control.window_number(h(1)), None, "not before the main job");
    rig.drain();
    let number = rig.control.window_number(h(1)).expect("recorded on main");
    assert_eq!(
        u64::from(number),
        crate::border_overlay::testing::FAKE_WINDOW_BASE + 1
    );
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Release));
    rig.drain();
    assert_eq!(rig.control.window_number(h(1)), None, "forgotten on release");
}
