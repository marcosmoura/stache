//! Easing functions for time-based animations.
//!
//! Provides standard easing curves: linear, ease-in, ease-out, and ease-in-out.

use crate::config::EasingType;

// ============================================================================
// Easing Functions
// ============================================================================

/// Linear interpolation between two values.
#[inline]
#[must_use]
pub fn lerp(start: f64, end: f64, t: f64) -> f64 { (end - start).mul_add(t, start) }

/// Linear easing (no acceleration).
#[inline]
pub const fn ease_linear(t: f64) -> f64 { t }

/// Ease-in (slow start, accelerates).
#[inline]
pub fn ease_in(t: f64) -> f64 { t * t * t }

/// Ease-out (fast start, decelerates).
#[inline]
pub fn ease_out(t: f64) -> f64 {
    let t1 = t - 1.0;
    (t1 * t1).mul_add(t1, 1.0)
}

/// Ease-in-out (slow start and end).
#[inline]
pub fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let t1 = 2.0f64.mul_add(t, -2.0);
        (0.5 * t1 * t1).mul_add(t1, 1.0)
    }
}

/// Exponential ease-out (very fast start, slow end).
///
/// This provides the snappiest feel among standard easing curves.
/// Uses the formula: 1 - 2^(-10t) for t > 0, 0 for t = 0.
#[inline]
pub fn ease_out_expo(t: f64) -> f64 {
    if t >= 1.0 {
        1.0
    } else {
        1.0 - (-10.0 * t).exp2()
    }
}

/// Applies an easing function based on the easing type.
#[inline]
#[must_use]
pub fn apply_easing(t: f64, easing: EasingType) -> f64 {
    match easing {
        EasingType::Linear => ease_linear(t),
        EasingType::EaseIn => ease_in(t),
        EasingType::EaseOut => ease_out(t),
        EasingType::EaseInOut => ease_in_out(t),
        EasingType::EaseOutExpo => ease_out_expo(t),
        EasingType::Spring => t, // Spring uses physics simulation
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lerp() {
        assert!((lerp(0.0, 100.0, 0.0) - 0.0).abs() < f64::EPSILON);
        assert!((lerp(0.0, 100.0, 0.5) - 50.0).abs() < f64::EPSILON);
        assert!((lerp(0.0, 100.0, 1.0) - 100.0).abs() < f64::EPSILON);
        assert!((lerp(50.0, 150.0, 0.25) - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_linear() {
        assert!((ease_linear(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((ease_linear(0.5) - 0.5).abs() < f64::EPSILON);
        assert!((ease_linear(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_in() {
        assert!((ease_in(0.0) - 0.0).abs() < f64::EPSILON);
        assert!(ease_in(0.5) < 0.5);
        assert!((ease_in(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_out() {
        assert!((ease_out(0.0) - 0.0).abs() < f64::EPSILON);
        assert!(ease_out(0.5) > 0.5);
        assert!((ease_out(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_in_out() {
        assert!((ease_in_out(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((ease_in_out(0.5) - 0.5).abs() < f64::EPSILON);
        assert!((ease_in_out(1.0) - 1.0).abs() < f64::EPSILON);
        assert!(ease_in_out(0.25) < 0.25);
        assert!(ease_in_out(0.75) > 0.75);
    }

    #[test]
    fn test_ease_out_expo() {
        // At t=0, should be 0
        assert!((ease_out_expo(0.0) - 0.0).abs() < 0.01);
        // At t=0.5, should be past midpoint (fast start)
        assert!(ease_out_expo(0.5) > 0.9);
        // At t=1, should be 1
        assert!((ease_out_expo(1.0) - 1.0).abs() < f64::EPSILON);
        // Should be faster than cubic ease_out at t=0.2
        assert!(ease_out_expo(0.2) > ease_out(0.2));
    }

    #[test]
    fn test_apply_easing() {
        assert!((apply_easing(0.5, EasingType::Linear) - ease_linear(0.5)).abs() < f64::EPSILON);
        assert!((apply_easing(0.5, EasingType::EaseIn) - ease_in(0.5)).abs() < f64::EPSILON);
        assert!((apply_easing(0.5, EasingType::EaseOut) - ease_out(0.5)).abs() < f64::EPSILON);
        assert!((apply_easing(0.5, EasingType::EaseInOut) - ease_in_out(0.5)).abs() < f64::EPSILON);
        assert!(
            (apply_easing(0.5, EasingType::EaseOutExpo) - ease_out_expo(0.5)).abs() < f64::EPSILON
        );
        assert!((apply_easing(0.5, EasingType::Spring) - 0.5).abs() < f64::EPSILON);
    }
}
