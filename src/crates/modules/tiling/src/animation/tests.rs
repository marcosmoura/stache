//! Pure curves and durations, plus the helpers the coordinator and pipeline
//! tests share. Nothing here moves a window or starts a timer.

use std::time::Duration;

use stache_macos::ApplicationId;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{
    AdapterId, IssuedKind, NativeStatus, RuntimeEpoch, WindowActionOutcome, WindowInstance,
};

use super::easing::{ease_in, ease_in_out, ease_linear, ease_out, ease_out_expo, lerp};
use super::*;
use crate::config::{AnimationConfig, EasingType};
use crate::geometry::Rect;
use crate::outcome::FrameComponents;
use crate::state::Stamp;

fn near(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

pub(super) fn app(n: u64) -> ApplicationId { ApplicationId::for_testing(n) }

pub(super) fn adapter() -> AdapterId {
    static ADAPTER: std::sync::OnceLock<AdapterId> = std::sync::OnceLock::new();
    *ADAPTER.get_or_init(AdapterId::for_testing)
}

pub(super) fn window_in(application: u64, serial: u64) -> WindowInstance {
    WindowInstance::for_testing(adapter(), RuntimeEpoch::for_testing(1), app(application), serial)
}

pub(super) fn window(serial: u64) -> WindowInstance { window_in(1, serial) }

pub(super) const BOTH: FrameComponents = FrameComponents { position: true, size: true };
pub(super) const FROM: Rect = Rect::new(0.0, 0.0, 100.0, 100.0);
pub(super) const TO: Rect = Rect::new(1000.0, 0.0, 100.0, 100.0);
pub(super) const PERIOD: Duration = Duration::from_millis(16);

pub(super) fn settings(easing: EasingType) -> AnimationSettings {
    AnimationSettings::from_config(&AnimationConfig {
        enabled: true,
        duration: 200,
        easing,
    })
}

pub(super) fn job(plan: u64, from: Rect, target: Rect, easing: EasingType) -> AnimationJob {
    AnimationJob {
        stamp: Stamp {
            epoch: RuntimeEpoch::for_testing(1),
            registry: 1,
        },
        revision: 1,
        plan_sequence: plan,
        window: window(1),
        from,
        target,
        components: BOTH,
        period: PERIOD,
        display: None,
        cross_display: false,
        settings: settings(easing),
    }
}

pub(super) fn read(frame: Rect) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x: frame.x, y: frame.y }),
        size: AxReading::Value(AxSize {
            width: frame.width,
            height: frame.height,
        }),
    }
}

/// A write the shard accepted, with no readback (the intermediate and final
/// frames of an animation).
pub(super) fn written() -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame {
            position: WriteStatus::Written,
            size: WriteStatus::Written,
        },
        observed: None,
    }
}

// ------------------------------------------------------------------- curves
// ------------------------------------------------------------------- curves

#[test]
fn easing_matches_the_legacy_formulas() {
    assert!(near(ease_linear(0.3), 0.3));
    assert!(near(ease_in(0.5), 0.125));
    assert!(near(ease_out(0.25), 0.578_125));
    assert!(near(ease_out(0.5), 0.875));
    assert!(near(ease_in_out(0.25), 0.0625));
    assert!(near(ease_in_out(0.5), 0.5));
    assert!(near(ease_in_out(0.75), 0.9375));
    assert!(near(ease_out_expo(0.5), 0.968_75));
    assert!(near(ease_out_expo(1.0), 1.0));
    assert!(near(lerp(50.0, 150.0, 0.25), 75.0));
    for easing in [
        EasingType::Linear,
        EasingType::EaseIn,
        EasingType::EaseOut,
        EasingType::EaseInOut,
        EasingType::EaseOutExpo,
        EasingType::Spring,
    ] {
        assert!(near(apply_easing(0.0, easing), 0.0) || easing == EasingType::EaseOutExpo);
        assert!(near(apply_easing(1.0, easing), 1.0), "{easing:?}");
    }
    assert!(
        near(apply_easing(0.5, EasingType::Spring), 0.5),
        "spring is simulated, not eased"
    );
}

#[test]
fn the_spring_matches_the_damped_oscillator_and_settles() {
    let spring = SpringState::new(Duration::from_millis(200));
    let (omega, zeta) = (5.5_f64 / 0.2, 0.85_f64);
    let omega_d = omega * (1.0 - zeta * zeta).sqrt();
    for t in [0.0_f64, 0.05, 0.1, 0.2, 0.5] {
        let expected = 1.0
            - (-zeta * omega * t).exp()
                * ((omega_d * t).cos() + zeta / (1.0 - zeta * zeta).sqrt() * (omega_d * t).sin());
        assert!(near(spring.calculate_position(t), expected), "t={t}");
    }
    assert!(near(spring.calculate_position(0.0), 0.0));
    let mut spring = SpringState::new(Duration::from_millis(100));
    let settled = (0..100).any(|_| spring.update(0.01).1);
    assert!(settled, "the spring converges");
    // Critically damped and overdamped branches stay finite and end at 1.
    assert!(near(SpringState::critically_damped_position(0.0, 33.0), 0.0));
    assert!((SpringState::critically_damped_position(5.0, 33.0) - 1.0).abs() < 1e-6);
    assert!((SpringState::overdamped_position(5.0, 33.0, 1.5) - 1.0).abs() < 1e-6);
    let params = SpringParams::from_duration(Duration::ZERO);
    assert!(params.omega_0.is_finite(), "a zero duration is floored");
}

#[test]
fn a_transition_measures_and_interpolates_like_legacy() {
    let t = Transition::new(FROM, Rect::new(500.0, 10.0, 110.0, 120.0));
    assert!(near(t.max_distance(), 500.0));
    assert!(t.involves_resize());
    assert!(!Transition::new(FROM, Rect::new(50.0, 50.0, 100.0, 100.0)).involves_resize());
    let half = Transition::new(FROM, Rect::new(100.0, 200.0, 200.0, 300.0)).interpolate(0.5);
    assert_eq!(half, Rect::new(50.0, 100.0, 150.0, 200.0));
    // The progress is clamped, so a spring overshoot never passes the target.
    assert_eq!(t.interpolate(1.4), t.to);
    assert_eq!(t.interpolate(-1.0), t.from);
}

#[test]
fn the_default_duration_is_200_ms_and_scales_below_500_px() {
    let defaults = AnimationConfig::default();
    assert!(!defaults.enabled, "animations stay off by default");
    assert_eq!(defaults.duration, 200);
    let settings = AnimationSettings::default();
    assert_eq!(settings.duration, Duration::from_millis(200));
    assert_eq!(settings.calculate_duration(500.0), Duration::from_millis(200));
    assert_eq!(settings.calculate_duration(5000.0), Duration::from_millis(200));
    assert_eq!(settings.calculate_duration(20.0), Duration::from_millis(50));
    assert_eq!(settings.calculate_duration(0.0), Duration::from_millis(50));
    // 50 + 150 * sqrt(100 / 500) = 117.08
    assert_eq!(settings.calculate_duration(100.0), Duration::from_millis(117));
    let mid = settings.calculate_duration(250.0);
    assert!(mid > Duration::from_millis(117) && mid < Duration::from_millis(200));
    let clamp = |duration| {
        AnimationSettings::from_config(&AnimationConfig {
            enabled: true,
            duration,
            easing: EasingType::EaseOut,
        })
        .duration
    };
    assert_eq!(clamp(10), Duration::from_millis(100));
    assert_eq!(clamp(60_000), Duration::from_millis(1000));
}

#[test]
fn only_an_enabled_meaningful_move_is_animated() {
    let on = AnimationConfig {
        enabled: true,
        ..AnimationConfig::default()
    };
    let off = AnimationConfig::default();
    assert!(!wants_animation(&off, &FROM, &TO), "off by default");
    assert!(wants_animation(&on, &FROM, &TO));
    assert!(!wants_animation(&on, &FROM, &Rect::new(4.0, 0.0, 100.0, 100.0)));
    assert!(wants_animation(&on, &FROM, &Rect::new(5.0, 0.0, 100.0, 100.0)));
    assert!(
        wants_animation(&on, &FROM, &Rect::new(0.0, 0.0, 100.0, 140.0)),
        "a resize counts"
    );
}
