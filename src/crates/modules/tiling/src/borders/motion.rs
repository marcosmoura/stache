//! Pure border motion for the native overlay:
//! the easing mapping, which borders carry which motion, and the reference
//! evaluator the overlay's animations must match.
//!
//! The overlay installs render-server animations; Stache never evaluates them
//! per frame. [`sample`] is the same timeline written as a pure function of
//! time, so tests can check the animation descriptions against it on a fake
//! clock. [`state_motion`] and [`transition_for`] map the configuration.
//!
//! Behavioral reference only (decision 8): Hyprland's `border` and
//! `borderangle` animations. Approach studied, never code (decision 5):
//! the reference border tool's `src/animation.c`, whose display-link proxy animation is not
//! reproduced.

use std::time::Duration;

use stache_macos::border_overlay::{
    BorderColor, BorderMotion, MotionRepeat, OverlayEasing, Transition,
};

use crate::animation::apply_easing;
use crate::config::{BorderAnimationStyle, BorderStateConfig, BordersConfig, EasingType};

/// Maps a Tiling easing onto the overlay's set. Total: `spring` is linear, as
/// in `apply_easing`.
#[must_use]
pub const fn overlay_easing(easing: EasingType) -> OverlayEasing {
    match easing {
        EasingType::Linear | EasingType::Spring => OverlayEasing::Linear,
        EasingType::EaseIn => OverlayEasing::EaseIn,
        EasingType::EaseOut => OverlayEasing::EaseOut,
        EasingType::EaseInOut => OverlayEasing::EaseInOut,
        EasingType::EaseOutExpo => OverlayEasing::EaseOutExpo,
    }
}

/// The Tiling curve an overlay easing stands for (the inverse of
/// [`overlay_easing`] on its image).
const fn tiling_easing(easing: OverlayEasing) -> EasingType {
    match easing {
        OverlayEasing::Linear => EasingType::Linear,
        OverlayEasing::EaseIn => EasingType::EaseIn,
        OverlayEasing::EaseOut => EasingType::EaseOut,
        OverlayEasing::EaseInOut => EasingType::EaseInOut,
        OverlayEasing::EaseOutExpo => EasingType::EaseOutExpo,
    }
}

fn ease(fraction: f64, easing: OverlayEasing) -> f64 {
    apply_easing(fraction.clamp(0.0, 1.0), tiling_easing(easing))
}

/// The motion a state's `animation` asks for: `ping_pong` (or no `style`)
/// swaps the colors, `duration` per leg; `once` and `loop` turn the gradient
/// angle, `duration` per turn. Only gradient states animate; an unknown style
/// (rejected by validation) is no motion.
#[must_use]
pub fn state_motion(state: &BorderStateConfig) -> Option<BorderMotion> {
    let BorderStateConfig::GradientColor { animation: Some(animation), .. } = state else {
        return None;
    };
    let period = Duration::from_millis(u64::from(animation.duration));
    let easing = overlay_easing(animation.easing);
    let rotate = |repeat| BorderMotion::Rotate { turn: period, easing, repeat };
    match animation.effective_style() {
        BorderAnimationStyle::PingPong => Some(BorderMotion::PingPong { leg: period, easing }),
        BorderAnimationStyle::Once => Some(rotate(MotionRepeat::Once)),
        BorderAnimationStyle::Loop => Some(rotate(MotionRepeat::Loop)),
        BorderAnimationStyle::Unknown(_) => None,
    }
}

/// The engine-wide focus cross-fade `transition` asks for; `None` (instant)
/// when absent. A 0 ms duration is kept and means instant
/// ([`Transition::effective`]).
#[must_use]
pub fn transition_for(config: &BordersConfig) -> Option<Transition> {
    config.transition.as_ref().map(|transition| Transition {
        duration: Duration::from_millis(u64::from(transition.duration)),
        easing: overlay_easing(transition.easing),
    })
}

/// The motion to install on a border: a ping-pong only on the focused border
/// (a focus change removes it from the old one), a rotation on every border
/// whose active state carries it.
#[must_use]
pub const fn motion_to_install(state: Option<BorderMotion>, focused: bool) -> Option<BorderMotion> {
    match state {
        Some(BorderMotion::PingPong { .. }) if !focused => None,
        other => other,
    }
}

/// What a gradient border shows at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Displayed {
    /// Gradient direction in degrees.
    pub angle: f64,
    /// Color at the start of the gradient.
    pub from: BorderColor,
    /// Color at the end of the gradient.
    pub to: BorderColor,
}

fn lerp(a: f64, b: f64, t: f64) -> f64 { (b - a).mul_add(t, a) }

fn mix(a: BorderColor, b: BorderColor, t: f64) -> BorderColor {
    BorderColor {
        r: lerp(a.r, b.r, t),
        g: lerp(a.g, b.g, t),
        b: lerp(a.b, b.b, t),
        a: lerp(a.a, b.a, t),
    }
}

fn clamped(duration: Duration) -> Duration { duration.max(Duration::from_millis(16)) }

/// Splits `t` into whole `period`s and the remainder, in exact integer time.
fn split(t: Duration, period: Duration) -> (u128, Duration) {
    let whole = t.as_nanos() / period.as_nanos();
    let rest = t.as_nanos() % period.as_nanos();
    let rest = Duration::from_nanos(u64::try_from(rest).unwrap_or(u64::MAX));
    (whole, rest)
}

/// What is displayed `t` after a state became the window's active appearance.
///
/// `target` is the state's own look; `motion` its motion; `fade` the engine's
/// cross-fade with the look it starts from (`None` when instant or the border
/// was not shown). Rules, all written from the spec:
///
/// - `Rotate`: the angle turns from `a` to `a + 360` over the (16 ms clamped)
///   turn, eased per turn; `Once` is exactly `a` from the end of the turn on,
///   `Loop` is periodic in the turn.
/// - `PingPong`: the colors swap over one leg and swap back, forever; the same
///   values as the legacy `PingPong` cursor at the same instants.
/// - `fade`: colors blend from the previous look to the target over the
///   duration (a 0 ms transition is instant, others clamp to 16 ms); the angle
///   is the target's from the start. A ping-pong owns the colors, so no fade
///   runs against one.
#[must_use]
pub fn sample(
    target: Displayed,
    motion: Option<BorderMotion>,
    fade: Option<(Transition, Displayed)>,
    t: Duration,
) -> Displayed {
    let mut shown = target;
    match motion {
        Some(BorderMotion::Rotate { turn, easing, repeat }) => {
            let turn = clamped(turn);
            shown.angle = match repeat {
                MotionRepeat::Once if t >= turn => target.angle,
                MotionRepeat::Once => 360.0_f64
                    .mul_add(ease(t.as_secs_f64() / turn.as_secs_f64(), easing), target.angle),
                MotionRepeat::Loop => {
                    let (_, phase) = split(t, turn);
                    360.0_f64.mul_add(
                        ease(phase.as_secs_f64() / turn.as_secs_f64(), easing),
                        target.angle,
                    )
                }
            };
        }
        Some(BorderMotion::PingPong { leg, easing }) => {
            let leg = clamped(leg);
            let (index, rest) = split(t, leg);
            let eased = ease(rest.as_secs_f64() / leg.as_secs_f64(), easing);
            let forward = index % 2 == 0;
            let progress = if forward { eased } else { 1.0 - eased };
            shown.from = mix(target.from, target.to, progress);
            shown.to = mix(target.to, target.from, progress);
        }
        None => {}
    }
    if let (Some((transition, previous)), false) =
        (fade, matches!(motion, Some(BorderMotion::PingPong { .. })))
        && let Some(duration) = transition.effective()
        && t < duration
    {
        let progress = ease(t.as_secs_f64() / duration.as_secs_f64(), transition.easing);
        shown.from = mix(previous.from, shown.from, progress);
        shown.to = mix(previous.to, shown.to, progress);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BorderAnimationConfig, BorderStateConfig, BordersConfig, GradientConfig};

    const RED: BorderColor = BorderColor { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
    const BLUE: BorderColor = BorderColor { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };
    const GREEN: BorderColor = BorderColor { r: 0.0, g: 1.0, b: 0.0, a: 1.0 };
    const ALL: [EasingType; 6] = [
        EasingType::Linear,
        EasingType::EaseIn,
        EasingType::EaseOut,
        EasingType::EaseInOut,
        EasingType::EaseOutExpo,
        EasingType::Spring,
    ];

    fn ms(value: u64) -> Duration { Duration::from_millis(value) }

    fn look(angle: f64) -> Displayed { Displayed { angle, from: RED, to: BLUE } }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "call sites pass the optional motion field"
    )]
    fn rotate(turn: u64, easing: EasingType, repeat: MotionRepeat) -> Option<BorderMotion> {
        Some(BorderMotion::Rotate {
            turn: ms(turn),
            easing: overlay_easing(easing),
            repeat,
        })
    }

    #[test]
    fn the_easing_mapping_is_total_and_spring_is_linear() {
        for easing in ALL {
            let mapped = overlay_easing(easing);
            if easing == EasingType::Spring {
                assert_eq!(mapped, OverlayEasing::Linear);
            }
            // The control points track the Tiling curve the evaluator uses.
            for step in 0..=20 {
                let t = f64::from(step) / 20.0;
                let curve = apply_easing(t, easing);
                let bezier = mapped.progress(t);
                assert!(
                    (curve - bezier).abs() < 0.08,
                    "{easing:?} at {t}: curve {curve} vs control points {bezier}"
                );
            }
        }
        assert_eq!(
            overlay_easing(EasingType::EaseOutExpo),
            OverlayEasing::EaseOutExpo
        );
        assert_eq!(OverlayEasing::EaseInOut.control_points(), [0.65, 0.0, 0.35, 1.0]);
    }

    #[test]
    fn once_ends_exactly_at_the_configured_angle_for_every_easing() {
        for easing in ALL {
            let motion = rotate(1000, easing, MotionRepeat::Once);
            let start = sample(look(30.0), motion, None, Duration::ZERO);
            assert!((start.angle - 30.0).abs() < 1e-9, "{easing:?} starts at a");
            let mid = sample(look(30.0), motion, None, ms(500));
            let want = 360.0_f64.mul_add(apply_easing(0.5, easing), 30.0);
            assert!((mid.angle - want).abs() < 1e-9, "{easing:?} midway");
            for at in [1000, 1001, 5000] {
                let end = sample(look(30.0), motion, None, ms(at));
                assert_eq!(
                    end.angle.to_bits(),
                    30.0_f64.to_bits(),
                    "{easing:?} ends exactly at a ({at} ms)"
                );
            }
            // Just before the end the angle is almost a full turn on.
            let near = sample(look(30.0), motion, None, ms(999));
            assert!(
                near.angle > 30.0 + 300.0,
                "{easing:?} nearly a turn: {}",
                near.angle
            );
        }
    }

    #[test]
    fn loop_is_periodic_across_three_turns_for_every_easing() {
        for easing in ALL {
            let motion = rotate(800, easing, MotionRepeat::Loop);
            for offset in [0, 100, 250, 400, 799] {
                let first = sample(look(10.0), motion, None, ms(offset));
                for turns in 1..=3 {
                    let again = sample(look(10.0), motion, None, ms(offset + 800 * turns));
                    assert!(
                        (first.angle - again.angle).abs() < 1e-6,
                        "{easing:?} at {offset} + {turns} turns"
                    );
                }
            }
            let boundary = sample(look(10.0), motion, None, ms(2400));
            assert!((boundary.angle - 10.0).abs() < 1e-9, "{easing:?} restarts at a");
            // The colors are untouched by a rotation.
            assert_eq!((boundary.from, boundary.to), (RED, BLUE));
        }
    }

    #[test]
    fn durations_below_sixteen_milliseconds_clamp_to_sixteen() {
        for motion in [
            rotate(1, EasingType::Linear, MotionRepeat::Loop),
            rotate(0, EasingType::Linear, MotionRepeat::Loop),
        ] {
            let half = sample(look(0.0), motion, None, ms(8));
            assert!((half.angle - 180.0).abs() < 1e-6, "8 ms is half of a 16 ms turn");
        }
        let once = rotate(5, EasingType::Linear, MotionRepeat::Once);
        assert!(sample(look(0.0), once, None, ms(15)).angle > 300.0);
        assert!(sample(look(0.0), once, None, ms(16)).angle.abs() < 1e-9);
        let swap = Some(BorderMotion::PingPong {
            leg: ms(2),
            easing: OverlayEasing::Linear,
        });
        let half = sample(look(0.0), swap, None, ms(8));
        assert!((half.from.r - 0.5).abs() < 1e-9, "a 2 ms leg is a 16 ms leg");
        let instant = Transition {
            duration: ms(0),
            easing: OverlayEasing::Linear,
        };
        let faded = sample(
            look(0.0),
            None,
            Some((instant, Displayed {
                from: GREEN,
                to: GREEN,
                angle: 0.0,
            })),
            Duration::ZERO,
        );
        assert_eq!(faded, look(0.0), "0 ms is instant");
        let short = Transition {
            duration: ms(4),
            easing: OverlayEasing::Linear,
        };
        let half = sample(
            look(0.0),
            None,
            Some((short, Displayed {
                from: GREEN,
                to: GREEN,
                angle: 0.0,
            })),
            ms(8),
        );
        assert!((half.from.g - 0.5).abs() < 1e-9, "a 4 ms fade runs 16 ms");
    }

    fn animated_config(duration: u32, easing: EasingType) -> BordersConfig {
        BordersConfig {
            focused: BorderStateConfig::GradientColor {
                width: 4,
                gradient: GradientConfig {
                    from: "#FF0000".to_owned(),
                    to: "#0000FF".to_owned(),
                    angle: 45.0,
                },
                animation: Some(BorderAnimationConfig { style: None, duration, easing }),
            },
            ..BordersConfig::default()
        }
    }

    #[test]
    fn a_fade_blends_from_the_previous_look_and_keeps_the_new_angle() {
        let transition = Transition {
            duration: ms(200),
            easing: OverlayEasing::Linear,
        };
        let previous = Displayed {
            angle: 270.0,
            from: GREEN,
            to: GREEN,
        };
        let target = Displayed {
            angle: 90.0,
            from: RED,
            to: BLUE,
        };
        let start = sample(target, None, Some((transition, previous)), Duration::ZERO);
        assert_eq!((start.from, start.to), (GREEN, GREEN));
        assert!(
            (start.angle - 90.0).abs() < f64::EPSILON,
            "the angle switches at the start"
        );
        let half = sample(target, None, Some((transition, previous)), ms(100));
        assert!((half.from.r - 0.5).abs() < 1e-9 && (half.from.g - 0.5).abs() < 1e-9);
        assert!((half.to.b - 0.5).abs() < 1e-9);
        assert_eq!(
            sample(target, None, Some((transition, previous)), ms(200)),
            target
        );
        // Eased fades use the transition's easing.
        let eased = Transition {
            duration: ms(200),
            easing: OverlayEasing::EaseIn,
        };
        let quarter = sample(target, None, Some((eased, previous)), ms(100));
        assert!((quarter.from.r - apply_easing(0.5, EasingType::EaseIn)).abs() < 1e-9);
        // A rotation keeps turning through a fade; a ping-pong is not faded against.
        let spinning = rotate(1000, EasingType::Linear, MotionRepeat::Loop);
        let both = sample(target, spinning, Some((transition, previous)), ms(100));
        assert!((both.angle - (90.0 + 36.0)).abs() < 1e-9);
        assert!((both.from.g - 0.5).abs() < 1e-9);
        let swap = Some(BorderMotion::PingPong {
            leg: ms(1000),
            easing: OverlayEasing::Linear,
        });
        let owned = sample(target, swap, Some((transition, previous)), ms(100));
        assert_eq!(owned.from.g.to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn ping_pong_only_animates_the_focused_border_and_rotations_always() {
        let swap = Some(BorderMotion::PingPong {
            leg: ms(500),
            easing: OverlayEasing::Linear,
        });
        let spin = rotate(2000, EasingType::EaseInOut, MotionRepeat::Loop);
        assert_eq!(motion_to_install(swap, true), swap);
        assert_eq!(motion_to_install(swap, false), None);
        assert_eq!(motion_to_install(spin, true), spin);
        assert_eq!(motion_to_install(spin, false), spin);
        assert_eq!(motion_to_install(None, true), None);
    }

    fn borders(value: serde_json::Value) -> BordersConfig {
        serde_json::from_value(value).expect("borders config")
    }

    fn animated(animation: &serde_json::Value) -> BorderStateConfig {
        serde_json::from_value(serde_json::json!({
            "width": 4,
            "gradient": {"from": "#ff0000", "to": "#0000ff", "angle": 45},
            "animation": animation.clone(),
        }))
        .expect("state")
    }

    /// TB-08-04: a legacy `animation` (only `duration` and `easing`) is the same
    /// ping-pong as before `style` existed.
    #[test]
    fn a_legacy_animation_maps_to_the_same_ping_pong() {
        let legacy = animated(&serde_json::json!({"duration": 800, "easing": "ease_in_out"}));
        let explicit = animated(
            &serde_json::json!({"style": "ping_pong", "duration": 800, "easing": "ease_in_out"}),
        );
        let expected = Some(BorderMotion::PingPong {
            leg: ms(800),
            easing: OverlayEasing::EaseInOut,
        });
        assert_eq!(state_motion(&legacy), expected);
        assert_eq!(state_motion(&explicit), expected);
        // Easing omitted stays linear.
        let bare = animated(&serde_json::json!({"duration": 300}));
        assert_eq!(
            state_motion(&bare),
            Some(BorderMotion::PingPong {
                leg: ms(300),
                easing: OverlayEasing::Linear
            })
        );
    }

    #[test]
    fn once_and_loop_map_to_rotations_per_turn() {
        let once = animated(&serde_json::json!({"style": "once", "duration": 4000}));
        assert_eq!(
            state_motion(&once),
            rotate(4000, EasingType::Linear, MotionRepeat::Once)
        );
        let looping =
            animated(&serde_json::json!({"style": "loop", "duration": 4000, "easing": "ease_out"}));
        assert_eq!(
            state_motion(&looping),
            rotate(4000, EasingType::EaseOut, MotionRepeat::Loop)
        );
        // Durations below 16 ms clamp in the motion model.
        let fast = state_motion(&animated(&serde_json::json!({"style": "loop", "duration": 1})))
            .expect("motion");
        assert_eq!(fast.period(), ms(16));
    }

    #[test]
    fn only_animated_gradients_have_a_motion() {
        let still = serde_json::from_value::<BorderStateConfig>(serde_json::json!({
            "width": 4, "gradient": {"from": "#ff0000", "to": "#0000ff"}
        }))
        .expect("state");
        assert_eq!(state_motion(&still), None);
        assert_eq!(state_motion(&BorderStateConfig::Disabled(false)), None);
        let solid = serde_json::from_value::<BorderStateConfig>(
            serde_json::json!({"width": 4, "color": "#fff"}),
        )
        .expect("state");
        assert_eq!(state_motion(&solid), None);
    }

    #[test]
    fn transition_maps_to_the_overlay_cross_fade() {
        assert_eq!(transition_for(&BordersConfig::default()), None);
        let fade = transition_for(&borders(serde_json::json!({
            "transition": {"duration": 200, "easing": "ease_out"}
        })))
        .expect("transition");
        assert_eq!(fade, Transition {
            duration: ms(200),
            easing: OverlayEasing::EaseOut
        });
        assert_eq!(fade.effective(), Some(ms(200)));
        // Easing defaults to ease_out; 0 ms is instant.
        let instant = transition_for(&borders(serde_json::json!({"transition": {"duration": 0}})))
            .expect("transition");
        assert_eq!(instant.easing, OverlayEasing::EaseOut);
        assert_eq!(instant.effective(), None);
    }
}
