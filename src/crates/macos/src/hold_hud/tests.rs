//! Fake-backed tests for the HUD adapter. None creates a native panel.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_sdk::{CancellationToken, MainThreadExecutor, ThreadError};

use super::testing::{FakeHudBackend, FakeHudCall};
use super::*;
use crate::applications::testing::ManualExecutor;

const SHORT: Duration = Duration::from_millis(30);

fn rect(x: f64, y: f64, width: f64, height: f64) -> HudRect { HudRect::new(x, y, width, height) }

fn screen(id: u32, x: f64, width: f64, height: f64) -> HudScreen {
    HudScreen {
        id: DisplayId::new(id),
        frame: rect(x, 0.0, width, height),
        visible: rect(x, 0.0, width, height - 25.0),
    }
}

fn two_screens() -> HudScreens {
    HudScreens {
        screens: vec![
            screen(1, 0.0, 1440.0, 900.0),
            screen(2, 1440.0, 1920.0, 1080.0),
        ],
        mouse: Some(HudPoint { x: 2000.0, y: 500.0 }),
        main: Some(DisplayId::new(1)),
    }
}

struct Rig {
    control: HudControl,
    events: HudEvents,
    backend: FakeHudBackend,
    executor: Arc<ManualExecutor>,
}

fn rig(on_main: bool, budget: Duration) -> Rig {
    let backend = FakeHudBackend::with_screens(two_screens());
    let executor = Arc::new(ManualExecutor::new(on_main));
    let (control, events) = HudControl::with_acknowledgement_budget(
        executor.clone(),
        Arc::new(backend.clone()),
        budget,
    );
    Rig {
        control,
        events,
        backend,
        executor,
    }
}

fn ready() -> (Rig, HudHandle) {
    let rig = rig(true, Duration::from_secs(2));
    let handle = rig.control.install(&CancellationToken::new()).expect("installs");
    (rig, handle)
}

fn request(handle: HudHandle, press: u64) -> HudRequest {
    HudRequest::new(handle, PressId::new(press), 1_000).with_label("Safari")
}

fn press(raw: u64) -> PressId { PressId::new(raw) }

fn outcomes(events: &HudEvents) -> Vec<(u64, HudOutcome)> {
    events.drain().into_iter().map(|e| (e.press.raw(), e.outcome)).collect()
}

/// An executor that claims to be the main thread and never runs a job.
struct StuckMain {
    queued: Mutex<usize>,
}

impl MainThreadExecutor for StuckMain {
    fn is_main_thread(&self) -> bool { true }

    fn execute(&self, _job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        *self.queued.lock().expect("lock") += 1;
        Ok(())
    }
}

// ---- pure text, layout, placement, progress ----

#[test]
fn text_names_the_app_and_falls_back() {
    assert_eq!(hud_text(Some("Safari")), "Hold ⌘Q to quit Safari");
    for missing in [None, Some(""), Some("   "), Some("\n\t")] {
        assert_eq!(hud_text(missing), "Hold ⌘Q to quit the app");
    }
    assert_eq!(hud_text(Some("A\nB")), "Hold ⌘Q to quit A B");
}

fn assert_inside(inner: HudRect, outer: HudRect) {
    let eps = 1e-9;
    assert!(inner.x >= outer.x - eps, "{inner:?} left of {outer:?}");
    assert!(inner.y >= outer.y - eps, "{inner:?} below {outer:?}");
    assert!(
        inner.right() <= outer.right() + eps,
        "{inner:?} right of {outer:?}"
    );
    assert!(inner.top() <= outer.top() + eps, "{inner:?} above {outer:?}");
}

fn assert_layout_valid(layout: &HudLayout, visible: HudRect) {
    assert_inside(layout.panel, visible);
    let local = rect(0.0, 0.0, layout.panel.width, layout.panel.height);
    assert_inside(layout.track_frame, local);
    assert_inside(layout.text_frame, local);
}

#[test]
fn layout_centers_near_the_lower_edge() {
    let visible = rect(100.0, 50.0, 1440.0, 800.0);
    let layout = compute_layout(visible, Some("Safari"), HudAnchor::Bottom);
    assert_layout_valid(&layout, visible);
    let centered = layout.panel.x + layout.panel.width / 2.0;
    assert!((centered - (visible.x + visible.width / 2.0)).abs() < 1e-9);
    assert!((layout.panel.y - (visible.y + 48.0)).abs() < 1e-9);
    assert!(!layout.truncated);
    assert_eq!(layout.text, "Hold ⌘Q to quit Safari");
}

#[test]
fn layout_stays_inside_small_and_degenerate_frames() {
    for visible in [
        rect(0.0, 0.0, 200.0, 80.0),
        rect(-500.0, -300.0, 100.0, 40.0),
        rect(0.0, 0.0, 10.0, 10.0),
        rect(5.0, 5.0, 0.0, 0.0),
        rect(0.0, 0.0, f64::NAN, f64::INFINITY),
        rect(0.0, 0.0, -50.0, -50.0),
    ] {
        let layout = compute_layout(visible, Some("Some Application"), HudAnchor::Bottom);
        assert_layout_valid(&layout, visible.sanitized());
    }
}

#[test]
fn long_label_truncates_and_keeps_the_track_on_screen() {
    let visible = rect(0.0, 0.0, 1440.0, 875.0);
    let name = "A".repeat(500);
    let layout = compute_layout(visible, Some(&name), HudAnchor::Bottom);
    assert!(layout.truncated);
    assert!(layout.text.ends_with('…'));
    assert!(layout.text.starts_with("Hold ⌘Q to quit"));
    assert!(layout.text.chars().count() < 200);
    assert_layout_valid(&layout, visible);
    assert!(layout.track_frame.width > 0.0);

    let narrow = rect(0.0, 0.0, 200.0, 300.0);
    let layout = compute_layout(narrow, Some(&name), HudAnchor::Bottom);
    assert!(layout.truncated);
    assert_layout_valid(&layout, narrow);
}

#[test]
fn placement_prefers_requested_then_mouse_then_main_then_first() {
    let mut screens = two_screens();
    let id = |s: (HudScreen, PlacementSource)| (s.0.id.raw(), s.1);

    let requested = select_screen(&screens, Some(DisplayId::new(1))).map(id);
    assert_eq!(requested, Ok((1, PlacementSource::RequestedDisplay)));

    let detached = select_screen(&screens, Some(DisplayId::new(99))).map(id);
    assert_eq!(detached, Ok((2, PlacementSource::MouseScreen)));

    screens.mouse = Some(HudPoint { x: -9000.0, y: -9000.0 });
    assert_eq!(
        select_screen(&screens, None).map(id),
        Ok((1, PlacementSource::MainScreen))
    );

    screens.main = Some(DisplayId::new(77));
    assert_eq!(
        select_screen(&screens, None).map(id),
        Ok((1, PlacementSource::FirstScreen))
    );

    screens.main = None;
    screens.mouse = None;
    assert_eq!(
        select_screen(&screens, None).map(id),
        Ok((1, PlacementSource::FirstScreen))
    );
}

#[test]
fn placement_without_a_screen_is_a_typed_failure() {
    let empty = HudScreens::default();
    assert_eq!(
        select_screen(&empty, Some(DisplayId::new(1))).map(|_| ()),
        Err(NoScreen)
    );
}

#[test]
fn progress_math_is_total_over_u64() {
    assert!((progress_fraction(0, 0) - 1.0).abs() < f64::EPSILON);
    assert!((progress_fraction(5, 10) - 0.5).abs() < f64::EPSILON);
    assert!((progress_fraction(10, 10) - 1.0).abs() < f64::EPSILON);
    assert!((progress_fraction(11, 10) - 1.0).abs() < f64::EPSILON);
    assert!((progress_fraction(u64::MAX, u64::MAX) - 1.0).abs() < f64::EPSILON);
    assert!(progress_fraction(0, u64::MAX) < 1e-9);
    let near = progress_fraction(u64::MAX - 1, u64::MAX);
    assert!((0.0..=1.0).contains(&near));
}

#[test]
fn progress_fraction_at_is_linear_from_the_start_and_clamped() {
    let near = |a: f64, b: f64| (a - b).abs() < 1e-12;
    assert!(near(progress_fraction_at(0.0, 2.0, 0.0), 0.0));
    assert!(near(progress_fraction_at(0.0, 2.0, 0.5), 0.25));
    assert!(near(progress_fraction_at(0.0, 2.0, 2.0), 1.0));
    assert!(
        near(progress_fraction_at(0.0, 2.0, 99.0), 1.0),
        "clamped to full"
    );
    assert!(
        near(progress_fraction_at(0.4, 1.0, 0.5), 0.7),
        "resumes from `from`"
    );
    assert!(
        near(progress_fraction_at(0.4, 1.0, -3.0), 0.4),
        "before the start"
    );
    assert!(near(progress_fraction_at(0.4, 1.0, f64::NAN), 0.4));
    assert!(
        near(progress_fraction_at(0.4, 0.0, 0.0), 1.0),
        "an empty run is complete"
    );
    assert!(near(progress_fraction_at(0.4, -1.0, 0.0), 1.0));
    assert!(near(progress_fraction_at(2.0, 1.0, 0.0), 1.0), "from is clamped");
    assert!(near(progress_fraction_at(f64::NAN, 1.0, 0.5), 0.5));
}

#[test]
fn progress_plan_is_full_animated_or_a_safe_static_fraction() {
    assert_eq!(progress_plan(0, 0), ProgressPlan::Full);
    assert_eq!(progress_plan(1_000, 1_000), ProgressPlan::Full);
    assert_eq!(progress_plan(u64::MAX, 5), ProgressPlan::Full);
    assert_eq!(progress_plan(5_000, 10_000), ProgressPlan::Animate {
        from: 0.5,
        seconds: 5.0
    });
    assert!(matches!(
        progress_plan(0, MAX_ANIMATED_MS),
        ProgressPlan::Animate { seconds, .. } if (seconds - 600.0).abs() < 1e-9
    ));
    // A huge duration is never mistaken for completion and never animates.
    assert!(matches!(progress_plan(0, MAX_ANIMATED_MS + 1), ProgressPlan::Static(f) if f < 1e-3));
    assert!(matches!(progress_plan(0, u64::MAX), ProgressPlan::Static(f) if f < 1e-9));
    assert!(matches!(progress_plan(1, u64::MAX), ProgressPlan::Static(_)));
    // Almost elapsed on a huge duration still animates its short remainder.
    assert!(matches!(
        progress_plan(u64::MAX - 1_000, u64::MAX),
        ProgressPlan::Animate { .. }
    ));
}

#[test]
fn style_strengthens_for_accessibility_and_ignores_motion() {
    let base = style_for(HudAppearance::default());
    let contrast = style_for(HudAppearance {
        increase_contrast: true,
        ..HudAppearance::default()
    });
    assert!(contrast.track_alpha > base.track_alpha);
    assert!(contrast.fill_uses_label_color && !base.fill_uses_label_color);
    let transparent = style_for(HudAppearance {
        reduce_transparency: true,
        ..HudAppearance::default()
    });
    assert!(transparent.opaque_surface && !base.opaque_surface);
    let motion = style_for(HudAppearance {
        reduce_motion: true,
        ..HudAppearance::default()
    });
    assert_eq!(motion, base);
}

// ---- Part 06: anchors, motion frames, hide tokens ----

const ANCHORS: [HudAnchor; 5] = [
    HudAnchor::Bottom,
    HudAnchor::Top,
    HudAnchor::Left,
    HudAnchor::Right,
    HudAnchor::Center,
];

#[test]
fn bottom_layout_matches_the_default_placement() {
    for visible in [
        rect(100.0, 50.0, 1440.0, 800.0),
        rect(0.0, 0.0, 200.0, 80.0),
    ] {
        let bottom = compute_layout(visible, Some("Safari"), HudAnchor::Bottom);
        assert_eq!(HudAnchor::default(), HudAnchor::Bottom);
        let again = compute_layout(visible, Some("Safari"), HudAnchor::default());
        assert_eq!(bottom, again);
    }
    let visible = rect(100.0, 50.0, 1440.0, 800.0);
    let layout = compute_layout(visible, Some("Safari"), HudAnchor::Bottom);
    assert!((layout.panel.y - (visible.y + 48.0)).abs() < 1e-9);
}

#[test]
fn layout_places_each_anchor_and_stays_inside_the_visible_frame() {
    let visible = rect(1440.0, 25.0, 1920.0, 1000.0);
    let panel = |anchor| compute_layout(visible, Some("Safari"), anchor).panel;
    let mid_x = visible.x + visible.width / 2.0;
    let mid_y = visible.y + visible.height / 2.0;
    let center_x = |r: HudRect| r.x + r.width / 2.0;
    let center_y = |r: HudRect| r.y + r.height / 2.0;

    let top = panel(HudAnchor::Top);
    assert!((center_x(top) - mid_x).abs() < 1e-9);
    assert!((visible.top() - top.top() - 48.0).abs() < 1e-9);
    let left = panel(HudAnchor::Left);
    assert!((center_y(left) - mid_y).abs() < 1e-9);
    assert!((left.x - visible.x - 48.0).abs() < 1e-9);
    let right = panel(HudAnchor::Right);
    assert!((center_y(right) - mid_y).abs() < 1e-9);
    assert!((visible.right() - right.right() - 48.0).abs() < 1e-9);
    let center = panel(HudAnchor::Center);
    assert!((center_x(center) - mid_x).abs() < 1e-9);
    assert!((center_y(center) - mid_y).abs() < 1e-9);

    for visible in [
        rect(0.0, 0.0, 1440.0, 875.0),
        rect(1440.0, -200.0, 1920.0, 1000.0),
        rect(0.0, 0.0, 200.0, 80.0),
        rect(-500.0, -300.0, 100.0, 40.0),
        rect(0.0, 0.0, 10.0, 10.0),
        rect(5.0, 5.0, 0.0, 0.0),
        rect(0.0, 0.0, f64::NAN, f64::INFINITY),
        rect(0.0, 0.0, -50.0, -50.0),
    ] {
        for anchor in ANCHORS {
            let layout = compute_layout(visible, Some("Some Application"), anchor);
            assert_layout_valid(&layout, visible.sanitized());
        }
    }
}

#[test]
fn fade_motion_has_equal_start_and_end_frames() {
    let panel = rect(500.0, 100.0, 300.0, 64.0);
    for anchor in ANCHORS {
        assert_eq!(
            motion_frames(panel, anchor, HudMotion::Fade, false),
            (panel, panel)
        );
        assert_eq!(
            motion_frames(panel, anchor, HudMotion::Fade, true),
            (panel, panel)
        );
    }
}

#[test]
fn slide_offsets_follow_the_anchor() {
    let panel = rect(500.0, 100.0, 300.0, 64.0);
    let start = |anchor| {
        let (start, end) = motion_frames(panel, anchor, HudMotion::Slide, false);
        assert_eq!(end, panel);
        assert_eq!((start.width, start.height), (panel.width, panel.height));
        start
    };
    let bottom = start(HudAnchor::Bottom);
    assert_eq!((bottom.x, bottom.y), (500.0, 100.0 - (64.0 + 48.0)));
    let top = start(HudAnchor::Top);
    assert_eq!((top.x, top.y), (500.0, 100.0 + 64.0 + 48.0));
    let left = start(HudAnchor::Left);
    assert_eq!((left.x, left.y), (500.0 - (300.0 + 48.0), 100.0));
    let right = start(HudAnchor::Right);
    assert_eq!((right.x, right.y), (500.0 + 300.0 + 48.0, 100.0));
    let center = start(HudAnchor::Center);
    assert_eq!((center.x, center.y), (500.0, 100.0 + 64.0));
}

#[test]
fn reduce_motion_turns_a_slide_into_a_fade() {
    let panel = rect(500.0, 100.0, 300.0, 64.0);
    assert_eq!(effective_motion(HudMotion::Slide, true), HudMotion::Fade);
    assert_eq!(effective_motion(HudMotion::Slide, false), HudMotion::Slide);
    assert_eq!(effective_motion(HudMotion::Fade, true), HudMotion::Fade);
    for anchor in ANCHORS {
        assert_eq!(
            motion_frames(panel, anchor, HudMotion::Slide, true),
            (panel, panel)
        );
    }
}

#[test]
fn durations_are_the_specified_constants() {
    assert_eq!(SHOW_DURATION, Duration::from_millis(200));
    assert_eq!(HIDE_DURATION, Duration::from_millis(150));
}

#[test]
fn a_stale_hide_token_is_not_current() {
    let generation = MotionGeneration::default();
    let hide = generation.advance();
    assert!(generation.is_current(hide));
    let show = generation.advance();
    assert!(!generation.is_current(hide), "a show supersedes the hide");
    assert!(generation.is_current(show));
    let second_hide = generation.advance();
    assert!(!generation.is_current(show));
    assert!(!generation.is_current(hide));
    assert!(generation.is_current(second_hide));
}

#[test]
fn requests_default_to_bottom_fade_and_carry_overrides() {
    let (rig, handle) = ready();
    assert_eq!(request(handle, 1).anchor, HudAnchor::Bottom);
    assert_eq!(request(handle, 1).motion, HudMotion::Fade);
    rig.control.show(request(handle, 1));
    rig.control
        .show(request(handle, 2).with_anchor(HudAnchor::Right).with_motion(HudMotion::Slide));
    let shows = rig.backend.shows();
    assert_eq!(shows.len(), 2);
    assert_eq!(
        (shows[0].anchor, shows[0].effective),
        (HudAnchor::Bottom, HudMotion::Fade)
    );
    assert_eq!(
        (shows[1].anchor, shows[1].motion),
        (HudAnchor::Right, HudMotion::Slide)
    );
    assert_eq!(shows[1].effective, HudMotion::Slide);
    assert_ne!(shows[1].start, shows[1].end);
}

#[test]
fn the_fake_resolves_reduce_motion_to_a_fade() {
    let (rig, handle) = ready();
    rig.backend.set_appearance(HudAppearance {
        reduce_motion: true,
        ..HudAppearance::default()
    });
    rig.control
        .show(request(handle, 1).with_anchor(HudAnchor::Top).with_motion(HudMotion::Slide));
    let shows = rig.backend.shows();
    assert_eq!(shows[0].motion, HudMotion::Slide);
    assert_eq!(shows[0].effective, HudMotion::Fade);
    assert_eq!(shows[0].start, shows[0].end);
}

#[test]
fn a_hide_followed_by_a_successor_show_never_hides_the_successor() {
    let (rig, handle) = ready();
    rig.control.show(request(handle, 1));
    rig.control.hide(handle, press(1));
    rig.control.show(request(handle, 2));
    rig.control.hide(handle, press(1));
    assert_eq!(rig.backend.displayed(handle), Some(press(2)));
}

// ---- install ----

#[test]
fn install_creates_one_hidden_panel() {
    let (rig, handle) = ready();
    assert_eq!(rig.control.phase(), HudPhase::Ready);
    assert_eq!(rig.backend.panels_created(), 1);
    assert_eq!(rig.backend.order_count(), 0);
    assert_eq!(rig.backend.displayed(handle), None);
    assert_eq!(rig.control.install(&CancellationToken::new()), Ok(handle));
    assert_eq!(rig.backend.panels_created(), 1);
}

#[test]
fn install_reports_nil_refusal_and_missing_screen_without_readiness() {
    let rig = rig(true, Duration::from_secs(2));
    rig.backend.fail_next_install(HudInstallError::NilConstruction("NSPanel"));
    assert_eq!(
        rig.control.install(&CancellationToken::new()),
        Err(HudInstallError::NilConstruction("NSPanel"))
    );
    assert_eq!(rig.control.phase(), HudPhase::Idle);
    rig.control.show(request(rig.control.handle(), 1));
    assert_eq!(outcomes(&rig.events), vec![(
        1,
        HudOutcome::Unavailable(HudUnavailable::NotReady)
    )]);

    rig.backend.set_screens(HudScreens::default());
    assert_eq!(
        rig.control.install(&CancellationToken::new()),
        Err(HudInstallError::NoScreen)
    );
    assert_eq!(rig.backend.panels_created(), 0);

    rig.backend.set_screens(two_screens());
    assert!(rig.control.install(&CancellationToken::new()).is_ok());
    assert_eq!(rig.backend.panels_created(), 1);
}

#[test]
fn install_dispatch_rejection_is_typed_and_retryable() {
    let rig = rig(true, Duration::from_secs(2));
    rig.executor.reject(true);
    assert_eq!(
        rig.control.install(&CancellationToken::new()),
        Err(HudInstallError::DispatchRejected)
    );
    assert_eq!(rig.control.phase(), HudPhase::Idle);
    rig.executor.reject(false);
    assert!(rig.control.install(&CancellationToken::new()).is_ok());
    assert_eq!(rig.backend.panels_created(), 1);
}

#[test]
fn install_timeout_reuses_the_reservation_and_late_drain_does_not_duplicate() {
    let rig = rig(false, SHORT);
    let token = CancellationToken::new();
    assert_eq!(rig.control.install(&token), Err(HudInstallError::TimedOut));
    assert_eq!(rig.control.phase(), HudPhase::Installing);
    assert_eq!(rig.control.install(&token), Err(HudInstallError::TimedOut));
    assert_eq!(rig.executor.queued(), 1, "retry must not queue a second panel");
    assert_eq!(rig.backend.panels_created(), 0);

    assert_eq!(rig.executor.drain(), 1);
    assert_eq!(rig.backend.panels_created(), 1);
    assert!(rig.control.install(&token).is_ok());
    assert_eq!(rig.backend.panels_created(), 1);
}

#[test]
fn install_wait_honours_cancellation_and_keeps_the_job_queued() {
    let rig = rig(false, Duration::from_secs(2));
    let token = CancellationToken::new();
    token.cancel();
    let started = Instant::now();
    assert_eq!(rig.control.install(&token), Err(HudInstallError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(rig.executor.queued(), 1);
}

#[test]
fn install_panic_is_contained_and_retry_succeeds() {
    let rig = rig(true, Duration::from_secs(2));
    rig.backend.panic_next_install();
    let result = rig.control.install(&CancellationToken::new());
    assert!(
        matches!(result, Err(HudInstallError::Native(message)) if message.contains("panicked"))
    );
    assert_eq!(rig.control.phase(), HudPhase::Idle);
    assert!(rig.control.install(&CancellationToken::new()).is_ok());
}

#[test]
fn a_main_thread_caller_never_waits_for_its_own_queue() {
    let backend = FakeHudBackend::with_screens(two_screens());
    let executor = Arc::new(StuckMain { queued: Mutex::new(0) });
    let (control, _events) = HudControl::new(executor.clone(), Arc::new(backend));
    let started = Instant::now();
    assert_eq!(
        control.install(&CancellationToken::new()),
        Err(HudInstallError::TimedOut)
    );
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(*executor.queued.lock().expect("lock"), 1);
}

// ---- show / hide ----

#[test]
fn show_then_hide_reports_shown_and_clears_the_animation() {
    let (rig, handle) = ready();
    rig.control.show(request(handle, 1).with_elapsed_ms(250));
    let events = rig.events.drain();
    assert_eq!(events.len(), 1);
    let HudOutcome::Shown(shown) = &events[0].outcome else {
        panic!("expected Shown")
    };
    assert_eq!(events[0].handle, handle);
    assert_eq!(events[0].press, press(1));
    assert_eq!(shown.text, "Hold ⌘Q to quit Safari");
    assert_eq!(shown.placement, HudPlacement {
        display: DisplayId::new(2),
        source: PlacementSource::MouseScreen
    });
    assert!(matches!(shown.plan, ProgressPlan::Animate { from, .. } if (from - 0.25).abs() < 1e-9));
    assert_eq!(rig.backend.displayed(handle), Some(press(1)));
    assert!(rig.backend.animation_active(handle));

    rig.control.hide(handle, press(1));
    assert_eq!(rig.backend.displayed(handle), None);
    assert!(
        !rig.backend.animation_active(handle),
        "no animation while hidden"
    );
    assert!(outcomes(&rig.events).is_empty());
}

#[test]
fn freeze_keeps_the_panel_stops_the_animation_and_emits_nothing() {
    let (rig, handle) = ready();
    rig.control.show(request(handle, 1).with_elapsed_ms(250));
    let _ = rig.events.drain();
    assert!(rig.backend.animation_active(handle));

    rig.control.freeze(handle, press(1));
    assert!(rig.backend.calls().contains(&FakeHudCall::Freeze { handle, press: press(1) }));
    assert_eq!(rig.backend.displayed(handle), Some(press(1)), "still on screen");
    assert!(!rig.backend.animation_active(handle));
    assert!(outcomes(&rig.events).is_empty(), "freeze emits no event");

    // The logical state is unchanged: a later hide still hides the press.
    rig.control.hide(handle, press(1));
    assert_eq!(rig.backend.displayed(handle), None);
}

#[test]
fn freeze_is_a_no_op_for_a_successor_another_handle_and_a_released_control() {
    let (rig, handle) = ready();
    rig.control.show(request(handle, 1));
    let _ = rig.events.drain();
    rig.executor.set_on_main(false);
    rig.control.show(request(handle, 2));
    rig.control.freeze(handle, press(1));
    rig.executor.drain();
    let freezes = |rig: &Rig| {
        rig.backend
            .calls()
            .iter()
            .filter(|call| matches!(call, FakeHudCall::Freeze { .. }))
            .count()
    };
    assert_eq!(freezes(&rig), 0, "a successor press is not frozen");
    assert!(rig.backend.animation_active(handle));
    rig.executor.set_on_main(true);

    let backend = FakeHudBackend::with_screens(two_screens());
    let one = rig_with_backend(&backend, Duration::from_secs(2));
    let two = rig_with_backend(&backend, Duration::from_secs(2));
    let first = one.control.install(&CancellationToken::new()).expect("installs");
    let second = two.control.install(&CancellationToken::new()).expect("installs");
    one.control.show(request(first, 1));
    let _ = one.events.drain();
    one.control.freeze(second, press(1));
    assert_eq!(freezes(&one), 0, "another handle is ignored");
    assert!(backend.animation_active(first));

    let released = one.control.release(&CancellationToken::new());
    assert!(matches!(released, HudReleaseOutcome::Released));
    one.control.freeze(first, press(1));
    assert_eq!(freezes(&one), 0, "a released control is ignored");
    assert!(outcomes(&one.events).is_empty());
}

#[test]
fn show_honours_the_display_hint_and_reports_the_fallback() {
    let (rig, handle) = ready();
    rig.control.show(request(handle, 1).with_display(DisplayId::new(1)));
    rig.control.show(request(handle, 2).with_display(DisplayId::new(42)));
    let shown: Vec<_> = rig
        .events
        .drain()
        .into_iter()
        .filter_map(|e| match e.outcome {
            HudOutcome::Shown(shown) => Some(shown.placement.source),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![
        PlacementSource::RequestedDisplay,
        PlacementSource::MouseScreen
    ]);
}

#[test]
fn zero_and_huge_durations_do_not_animate() {
    let (rig, handle) = ready();
    rig.control.show(HudRequest::new(handle, press(1), 0));
    assert_eq!(rig.backend.last_shown().expect("shown").plan, ProgressPlan::Full);
    assert!(!rig.backend.animation_active(handle));
    rig.control.show(HudRequest::new(handle, press(2), u64::MAX));
    assert!(matches!(
        rig.backend.last_shown().expect("shown").plan,
        ProgressPlan::Static(_)
    ));
    assert!(!rig.backend.animation_active(handle));
    assert_eq!(rig.events.drain().len(), 2);
}

#[test]
fn unauthorized_show_is_stale_and_never_ordered() {
    let (rig, handle) = ready();
    let allowed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&allowed);
    rig.control.show(
        request(handle, 1)
            .with_authorization(HudAuthorization::new(move || flag.load(Ordering::SeqCst))),
    );
    assert_eq!(outcomes(&rig.events), vec![(1, HudOutcome::Stale)]);
    assert_eq!(rig.backend.order_count(), 0);

    // Revoked between commit and ordering: the backend gate refuses it.
    let revoke = Arc::clone(&allowed);
    allowed.store(true, Ordering::SeqCst);
    rig.backend
        .set_pre_order(Some(Arc::new(move || revoke.store(false, Ordering::SeqCst))));
    let flag = Arc::clone(&allowed);
    rig.control.show(
        request(handle, 2)
            .with_authorization(HudAuthorization::new(move || flag.load(Ordering::SeqCst))),
    );
    assert_eq!(outcomes(&rig.events), vec![(2, HudOutcome::Stale)]);
    assert_eq!(rig.backend.order_count(), 0);
}

#[test]
fn hide_before_the_show_commits_makes_the_show_inert() {
    let (rig, handle) = ready();
    rig.executor.set_on_main(false);
    rig.control.show(request(handle, 1));
    rig.control.hide(handle, press(1));
    assert_eq!(outcomes(&rig.events), vec![(1, HudOutcome::Stale)]);
    rig.executor.drain();
    assert_eq!(rig.backend.order_count(), 0);
    assert_eq!(rig.backend.displayed(handle), None);
    assert!(outcomes(&rig.events).is_empty());
}

#[test]
fn release_before_the_show_commits_makes_the_show_inert() {
    let rig = rig_with_backend(&FakeHudBackend::with_screens(two_screens()), SHORT);
    let handle = rig.control.install(&CancellationToken::new()).expect("installs");
    rig.executor.set_on_main(false);
    rig.control.show(request(handle, 1));
    let token = CancellationToken::new();
    assert!(matches!(
        rig.control.release(&token),
        HudReleaseOutcome::Retained(HudRetained {
            cleanup: HudCleanup::Queued,
            ..
        })
    ));
    assert_eq!(outcomes(&rig.events), vec![(1, HudOutcome::Stale)]);
    rig.executor.drain();
    assert_eq!(rig.backend.order_count(), 0);
    assert_eq!(rig.backend.panels_live(), 0);
    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
}

#[test]
fn hide_after_native_show_before_delivery_leaves_nothing_visible() {
    let (rig, handle) = ready();
    let control = rig.control.clone();
    rig.backend
        .set_post_order(Some(Arc::new(move || control.hide(handle, press(1)))));
    rig.control.show(request(handle, 1));
    assert_eq!(outcomes(&rig.events), vec![(1, HudOutcome::Stale)]);
    assert_eq!(rig.backend.order_count(), 1);
    assert_eq!(rig.backend.displayed(handle), None);
    assert!(!rig.backend.animation_active(handle));
}

#[test]
fn hide_during_ordering_gate_is_stale_without_ordering() {
    let (rig, handle) = ready();
    let control = rig.control.clone();
    rig.backend
        .set_pre_order(Some(Arc::new(move || control.hide(handle, press(1)))));
    rig.control.show(request(handle, 1));
    assert_eq!(outcomes(&rig.events), vec![(1, HudOutcome::Stale)]);
    assert_eq!(rig.backend.order_count(), 0);
}

#[test]
fn a_newer_press_supersedes_an_older_queued_one() {
    let (rig, handle) = ready();
    rig.executor.set_on_main(false);
    rig.control.show(request(handle, 1));
    rig.control.show(request(handle, 2));
    assert_eq!(outcomes(&rig.events), vec![(1, HudOutcome::Stale)]);
    rig.executor.drain();
    let events = outcomes(&rig.events);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, 2);
    assert!(matches!(events[0].1, HudOutcome::Shown(_)));
    assert_eq!(rig.backend.displayed(handle), Some(press(2)));
    assert_eq!(rig.backend.order_count(), 1);
}

#[test]
fn an_older_hide_cannot_hide_a_successor_press() {
    let (rig, handle) = ready();
    rig.control.show(request(handle, 1));
    let _ = rig.events.drain();
    rig.executor.set_on_main(false);
    rig.control.show(request(handle, 2));
    rig.control.hide(handle, press(1));
    rig.executor.drain();
    assert_eq!(rig.backend.displayed(handle), Some(press(2)));
}

#[test]
fn an_unacknowledged_show_times_out_and_a_late_drain_is_inert() {
    let backend = FakeHudBackend::with_screens(two_screens());
    let short = rig_with_backend(&backend, SHORT);
    let handle_short = short.control.install(&CancellationToken::new()).expect("installs");
    short.executor.set_on_main(false);
    short.control.show(request(handle_short, 7));
    let event = short.events.recv_timeout(Duration::from_secs(2)).expect("timeout event");
    assert_eq!(event.press, press(7));
    assert_eq!(event.outcome, HudOutcome::Unavailable(HudUnavailable::TimedOut));

    short.executor.drain();
    assert_eq!(backend.displayed(handle_short), None);
    assert_eq!(backend.order_count(), 0, "a late job cannot resurrect the press");
    assert!(short.events.drain().is_empty());
}

fn rig_with_backend(backend: &FakeHudBackend, budget: Duration) -> Rig {
    let executor = Arc::new(ManualExecutor::new(true));
    let (control, events) = HudControl::with_acknowledgement_budget(
        executor.clone(),
        Arc::new(backend.clone()),
        budget,
    );
    Rig {
        control,
        events,
        backend: backend.clone(),
        executor,
    }
}

#[test]
fn a_late_success_after_timeout_is_hidden_and_never_reported() {
    let backend = FakeHudBackend::with_screens(two_screens());
    let rig = rig_with_backend(&backend, SHORT);
    let handle = rig.control.install(&CancellationToken::new()).expect("installs");
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let rig_events = Arc::new(Mutex::new(rig.events));
    let observer = Arc::clone(&rig_events);
    backend.set_post_order(Some(Arc::new(move || {
        std::thread::sleep(SHORT * 3);
        let drained = observer.lock().expect("lock").drain();
        sink.lock().expect("lock").extend(drained);
    })));
    rig.control.show(request(handle, 1));
    let seen = events.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].outcome,
        HudOutcome::Unavailable(HudUnavailable::TimedOut)
    );
    assert_eq!(backend.displayed(handle), None, "late success is ordered out");
    assert!(rig_events.lock().expect("lock").drain().is_empty());
}

#[test]
fn owners_are_independent_and_foreign_handles_are_stale() {
    let backend = FakeHudBackend::with_screens(two_screens());
    let one = rig_with_backend(&backend, Duration::from_secs(2));
    let two = rig_with_backend(&backend, Duration::from_secs(2));
    let first = one.control.install(&CancellationToken::new()).expect("installs");
    let second = two.control.install(&CancellationToken::new()).expect("installs");
    assert_ne!(first.id(), second.id());
    assert_eq!(backend.panels_created(), 2);

    one.control.show(request(first, 1));
    two.control.show(request(second, 1));
    one.control.hide(first, press(1));
    assert_eq!(backend.displayed(first), None);
    assert_eq!(backend.displayed(second), Some(press(1)));

    let _ = one.events.drain();
    one.control.show(request(second, 5));
    assert_eq!(outcomes(&one.events), vec![(5, HudOutcome::Stale)]);
    assert_eq!(backend.displayed(first), None);
}

#[test]
fn show_panics_and_native_failures_are_contained() {
    let (rig, handle) = ready();
    rig.backend.panic_next_show();
    rig.control.show(request(handle, 1));
    let events = outcomes(&rig.events);
    assert!(matches!(&events[0].1, HudOutcome::Failed(message) if message.contains("panicked")));

    rig.backend
        .fail_next_show(HudShowFailure::Native("ordering refused".to_owned()));
    rig.control.show(request(handle, 2));
    assert_eq!(outcomes(&rig.events), vec![(
        2,
        HudOutcome::Failed("ordering refused".to_owned())
    )]);

    rig.control.show(request(handle, 3));
    assert!(matches!(outcomes(&rig.events)[0].1, HudOutcome::Shown(_)));
}

#[test]
fn a_lost_screen_is_a_typed_unavailable_not_a_show() {
    let (rig, handle) = ready();
    rig.backend.set_screens(HudScreens::default());
    rig.control.show(request(handle, 1));
    assert_eq!(outcomes(&rig.events), vec![(
        1,
        HudOutcome::Unavailable(HudUnavailable::NoScreen)
    )]);
    assert_eq!(rig.backend.order_count(), 0);
}

#[test]
fn show_before_install_and_after_release_is_unavailable() {
    let rig = rig(true, Duration::from_secs(2));
    rig.control.show(request(rig.control.handle(), 1));
    assert_eq!(outcomes(&rig.events), vec![(
        1,
        HudOutcome::Unavailable(HudUnavailable::NotReady)
    )]);
    let handle = rig.control.install(&CancellationToken::new()).expect("installs");
    assert_eq!(
        rig.control.release(&CancellationToken::new()),
        HudReleaseOutcome::Released
    );
    rig.control.show(request(handle, 2));
    assert_eq!(outcomes(&rig.events), vec![(
        2,
        HudOutcome::Unavailable(HudUnavailable::Retired)
    )]);
}

#[test]
fn a_rejected_show_dispatch_is_unavailable() {
    let (rig, handle) = ready();
    rig.executor.reject(true);
    rig.control.show(request(handle, 1));
    assert_eq!(outcomes(&rig.events), vec![(
        1,
        HudOutcome::Unavailable(HudUnavailable::DispatchRejected)
    )]);
}

// ---- release ----

#[test]
fn release_drops_the_panel_and_is_idempotent() {
    let (rig, handle) = ready();
    rig.control.show(request(handle, 1));
    let token = CancellationToken::new();
    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
    assert_eq!(rig.backend.panels_live(), 0);
    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
    assert_eq!(rig.backend.release_count(), 1);
    assert_eq!(rig.control.phase(), HudPhase::Released);
    assert_eq!(rig.control.install(&token), Err(HudInstallError::Retired));
    assert_eq!(rig.backend.panels_created(), 1);
}

#[test]
fn release_with_nothing_installed_touches_no_native_object() {
    let rig = rig(true, Duration::from_secs(2));
    assert_eq!(
        rig.control.release(&CancellationToken::new()),
        HudReleaseOutcome::Released
    );
    assert_eq!(rig.backend.release_count(), 0);
}

#[test]
fn release_timeout_retains_exact_ownership_then_cleans_exactly_once() {
    let (rig_short, handle) = {
        let backend = FakeHudBackend::with_screens(two_screens());
        let rig = rig_with_backend(&backend, SHORT);
        let handle = rig.control.install(&CancellationToken::new()).expect("installs");
        (rig, handle)
    };
    let rig = rig_short;
    rig.executor.set_on_main(false);
    let token = CancellationToken::new();
    let outcome = rig.control.release(&token);
    assert_eq!(
        outcome,
        HudReleaseOutcome::Retained(HudRetained {
            handle,
            reason: HudRetainReason::TimedOut,
            cleanup: HudCleanup::Queued,
        })
    );
    assert_eq!(rig.control.phase(), HudPhase::Releasing);
    assert_eq!(rig.backend.panels_live(), 1);
    // A second release reuses the queued job and a fresh install cannot duplicate.
    assert!(matches!(
        rig.control.release(&token),
        HudReleaseOutcome::Retained(_)
    ));
    assert_eq!(rig.executor.queued(), 1);
    assert_eq!(rig.control.install(&token), Err(HudInstallError::Retired));
    assert_eq!(rig.backend.panels_created(), 1);

    rig.executor.drain();
    assert_eq!(rig.backend.panels_live(), 0);
    assert_eq!(rig.backend.release_count(), 1);
    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
    assert_eq!(rig.backend.release_count(), 1);
}

#[test]
fn release_failure_retains_the_panel_and_retry_is_idempotent() {
    let (rig, handle) = ready();
    let token = CancellationToken::new();
    rig.backend.fail_next_release("boom");
    assert_eq!(
        rig.control.release(&token),
        HudReleaseOutcome::Retained(HudRetained {
            handle,
            reason: HudRetainReason::RemovalFailed("boom".to_owned()),
            cleanup: HudCleanup::Failed,
        })
    );
    assert_eq!(rig.backend.panels_live(), 1);
    assert!(matches!(
        rig.control.install(&token),
        Err(HudInstallError::Retained(HudRetainReason::RemovalFailed(_)))
    ));
    assert_eq!(rig.backend.panels_created(), 1, "no fresh duplicate panel");

    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
    assert_eq!(rig.backend.panels_live(), 0);
    assert_eq!(rig.backend.release_count(), 2);
    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
    assert_eq!(rig.backend.release_count(), 2);
}

#[test]
fn an_already_cancelled_lifecycle_still_attempts_physical_cleanup() {
    let (rig, _handle) = ready();
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
    assert_eq!(rig.backend.panels_live(), 0);
}

#[test]
fn a_cancelled_release_wait_reports_retained_and_the_queued_job_still_cleans() {
    let (rig, handle) = ready();
    rig.executor.set_on_main(false);
    let token = CancellationToken::new();
    token.cancel();
    let started = Instant::now();
    assert_eq!(
        rig.control.release(&token),
        HudReleaseOutcome::Retained(HudRetained {
            handle,
            reason: HudRetainReason::Cancelled,
            cleanup: HudCleanup::Queued,
        })
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(rig.executor.queued(), 1);
    rig.executor.drain();
    assert_eq!(rig.backend.panels_live(), 0);
}

#[test]
fn a_rejected_release_dispatch_is_retained_and_retryable() {
    let (rig, handle) = ready();
    rig.executor.reject(true);
    let token = CancellationToken::new();
    assert_eq!(
        rig.control.release(&token),
        HudReleaseOutcome::Retained(HudRetained {
            handle,
            reason: HudRetainReason::DispatchRejected,
            cleanup: HudCleanup::Failed,
        })
    );
    assert_eq!(rig.backend.panels_live(), 1);
    rig.executor.reject(false);
    assert_eq!(rig.control.release(&token), HudReleaseOutcome::Released);
    assert_eq!(rig.backend.panels_live(), 0);
}

#[test]
fn a_release_panic_is_contained_and_retained() {
    let (rig, _handle) = ready();
    rig.backend.panic_next_release();
    let outcome = rig.control.release(&CancellationToken::new());
    assert!(matches!(
        outcome,
        HudReleaseOutcome::Retained(HudRetained {
            reason: HudRetainReason::RemovalFailed(ref message),
            cleanup: HudCleanup::Failed,
            ..
        }) if message.contains("panicked")
    ));
    assert_eq!(
        rig.control.release(&CancellationToken::new()),
        HudReleaseOutcome::Released
    );
}

#[test]
fn a_main_thread_release_never_waits_on_a_blocked_main_queue() {
    let backend = FakeHudBackend::with_screens(two_screens());
    let executor = Arc::new(StuckMain { queued: Mutex::new(0) });
    let (control, _events) = HudControl::new(executor, Arc::new(backend));
    // The queue never drains, so the control stays Installing and release must
    // report the queued cleanup honestly instead of waiting on its own thread.
    assert_eq!(
        control.install(&CancellationToken::new()),
        Err(HudInstallError::TimedOut)
    );
    let started = Instant::now();
    let outcome = control.release(&CancellationToken::new());
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(matches!(
        outcome,
        HudReleaseOutcome::Retained(HudRetained {
            reason: HudRetainReason::MainThreadCaller,
            cleanup: HudCleanup::Queued,
            ..
        })
    ));
}

#[test]
fn dropping_the_last_control_enqueues_cleanup_instead_of_forgetting_the_panel() {
    let (rig, _handle) = ready();
    let Rig {
        control,
        events,
        backend,
        executor,
    } = rig;
    executor.set_on_main(false);
    drop(control);
    assert_eq!(backend.panels_live(), 1);
    assert_eq!(executor.queued(), 1);
    executor.drain();
    assert_eq!(backend.panels_live(), 0);
    assert!(events.drain().is_empty());
    assert_eq!(
        backend.calls().iter().filter(|c| matches!(c, FakeHudCall::Release(_))).count(),
        1
    );
}

#[test]
fn a_never_draining_executor_bounds_every_wait() {
    let backend = FakeHudBackend::with_screens(two_screens());
    let rig = rig_with_backend(&backend, SHORT);
    let handle = rig.control.install(&CancellationToken::new()).expect("installs");
    rig.executor.set_on_main(false);
    rig.control.show(request(handle, 1));
    let started = Instant::now();
    let event = rig.events.recv_timeout(Duration::from_secs(2)).expect("timeout");
    assert_eq!(event.outcome, HudOutcome::Unavailable(HudUnavailable::TimedOut));
    assert!(matches!(
        rig.control.release(&CancellationToken::new()),
        HudReleaseOutcome::Retained(HudRetained {
            reason: HudRetainReason::TimedOut,
            ..
        })
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(backend.order_count(), 0);
}

// ---- compile proofs ----

#[test]
fn thread_safety_is_a_compile_time_fact() {
    const fn needs_send_sync<T: Send + Sync>() {}
    const fn needs_send<T: Send>() {}
    needs_send_sync::<HudControl>();
    needs_send_sync::<HudHandle>();
    needs_send_sync::<HudRequest>();
    needs_send_sync::<FakeHudBackend>();
    needs_send::<HudEvents>();
}
