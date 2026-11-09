//! Easing curves, ported from the legacy formulas.

use crate::config::EasingType;

/// Linear interpolation between two values.
#[inline]
#[must_use]
pub fn lerp(start: f64, end: f64, t: f64) -> f64 { (end - start).mul_add(t, start) }

/// Linear easing (no acceleration).
#[inline]
#[must_use]
pub const fn ease_linear(t: f64) -> f64 { t }

/// Ease-in (slow start, accelerates).
#[inline]
#[must_use]
pub fn ease_in(t: f64) -> f64 { t * t * t }

/// Ease-out (fast start, decelerates).
#[inline]
#[must_use]
pub fn ease_out(t: f64) -> f64 {
    let t1 = t - 1.0;
    (t1 * t1).mul_add(t1, 1.0)
}

/// Ease-in-out (slow start and end).
#[inline]
#[must_use]
pub fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let t1 = 2.0f64.mul_add(t, -2.0);
        (0.5 * t1 * t1).mul_add(t1, 1.0)
    }
}

/// Exponential ease-out (very fast start, slow end): `1 - 2^(-10t)`.
#[inline]
#[must_use]
pub fn ease_out_expo(t: f64) -> f64 {
    if t >= 1.0 {
        1.0
    } else {
        1.0 - (-10.0 * t).exp2()
    }
}

/// Applies the easing curve of `easing`. Spring is a physics simulation, so
/// its curve is the identity here (see [`super::SpringState`]).
#[inline]
#[must_use]
pub fn apply_easing(t: f64, easing: EasingType) -> f64 {
    match easing {
        EasingType::Linear | EasingType::Spring => ease_linear(t),
        EasingType::EaseIn => ease_in(t),
        EasingType::EaseOut => ease_out(t),
        EasingType::EaseInOut => ease_in_out(t),
        EasingType::EaseOutExpo => ease_out_expo(t),
    }
}
