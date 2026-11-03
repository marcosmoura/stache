//! Spring physics for physically-based animations.
//!
//! Uses an analytical solution to the damped harmonic oscillator equation
//! for smooth, physically accurate spring animations.
//!
//! The spring uses the damped harmonic oscillator equation:
//! ```text
//! x''(t) + 2ζω₀x'(t) + ω₀²x(t) = ω₀²
//! ```
//!
//! Where:
//! - ζ (zeta) = damping ratio (controls bounciness)
//! - ω₀ = natural frequency = √(k/m)

use std::time::Duration;

// ============================================================================
// Constants
// ============================================================================

/// Spring convergence threshold (position delta).
const SPRING_POSITION_THRESHOLD: f64 = 0.01;

/// Spring damping ratio (ζ).
///
/// - 1.0 = critically damped (no overshoot, but can feel sluggish)
/// - 0.85 = slightly underdamped (snappier feel, minimal overshoot ~2%)
///
/// We use 0.85 for a more responsive feel while keeping overshoot imperceptible.
const SPRING_DAMPING_RATIO: f64 = 0.85;

/// Spring mass (fixed at 1.0).
#[allow(dead_code)] // Kept for documentation/reference
const SPRING_MASS: f64 = 1.0;

/// Settling time multiplier.
///
/// For critically damped: ~6.6 (99% settled)
/// For underdamped (ζ=0.85): ~5.5 is sufficient due to faster initial response
const SPRING_SETTLE_FACTOR: f64 = 5.5;

// ============================================================================
// Spring Physics
// ============================================================================

/// Spring physics parameters.
#[derive(Debug, Clone, Copy)]
pub struct SpringParams {
    pub omega_0: f64,
    pub damping_ratio: f64,
}

impl SpringParams {
    /// Creates spring parameters from a target duration.
    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        let target_secs = duration.as_secs_f64().max(0.01);
        let omega_0 = SPRING_SETTLE_FACTOR / target_secs;

        Self {
            omega_0,
            damping_ratio: SPRING_DAMPING_RATIO,
        }
    }
}

/// State for a spring animation.
#[derive(Debug, Clone)]
pub struct SpringState {
    pub elapsed: f64,
    params: SpringParams,
}

impl SpringState {
    /// Creates a new spring state with the given target duration.
    #[must_use]
    pub fn new(target_duration: Duration) -> Self {
        Self {
            elapsed: 0.0,
            params: SpringParams::from_duration(target_duration),
        }
    }

    /// Updates the spring state with a time delta.
    ///
    /// Returns the current position (0.0 to ~1.0) and whether the spring has settled.
    pub fn update(&mut self, dt: f64) -> (f64, bool) {
        self.elapsed += dt;
        let position = self.calculate_position(self.elapsed);

        let is_settled = (position - 1.0).abs() < SPRING_POSITION_THRESHOLD && self.elapsed > 0.02;
        let final_position = if is_settled { 1.0 } else { position };

        (final_position.clamp(0.0, 1.5), is_settled)
    }

    /// Calculates the spring position at time t.
    #[must_use]
    pub fn calculate_position(&self, t: f64) -> f64 {
        let omega_0 = self.params.omega_0;
        let zeta = self.params.damping_ratio;

        if zeta < 1.0 {
            Self::underdamped_position(t, omega_0, zeta)
        } else if (zeta - 1.0).abs() < 0.001 {
            Self::critically_damped_position(t, omega_0)
        } else {
            Self::overdamped_position(t, omega_0, zeta)
        }
    }

    /// Calculates position for an underdamped spring (ζ < 1).
    #[inline]
    #[must_use]
    pub fn underdamped_position(t: f64, omega_0: f64, zeta: f64) -> f64 {
        let zeta_sq_complement = zeta.mul_add(-zeta, 1.0);
        let omega_d = omega_0 * zeta_sq_complement.sqrt();
        let decay = (-zeta * omega_0 * t).exp();
        let cos_term = (omega_d * t).cos();
        let sin_term = (zeta / zeta_sq_complement.sqrt()) * (omega_d * t).sin();

        decay.mul_add(-(cos_term + sin_term), 1.0)
    }

    /// Calculates position for a critically damped spring (ζ = 1).
    #[inline]
    #[must_use]
    pub fn critically_damped_position(t: f64, omega_0: f64) -> f64 {
        let decay = (-omega_0 * t).exp();
        decay.mul_add(-omega_0.mul_add(t, 1.0), 1.0)
    }

    /// Calculates position for an overdamped spring (ζ > 1).
    #[inline]
    #[must_use]
    pub fn overdamped_position(t: f64, omega_0: f64, zeta: f64) -> f64 {
        let zeta_sq_minus_one = zeta.mul_add(zeta, -1.0);
        let gamma = omega_0 * zeta_sq_minus_one.sqrt();
        let decay = (-zeta * omega_0 * t).exp();
        let cosh_term = (gamma * t).cosh();
        let sinh_term = (zeta / zeta_sq_minus_one.sqrt()) * (gamma * t).sinh();

        decay.mul_add(-(cosh_term + sinh_term), 1.0)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spring_params_from_duration() {
        let params = SpringParams::from_duration(Duration::from_millis(200));
        assert!(params.omega_0 > 0.0);
        // omega_0 determines spring stiffness (stiffness = omega_0^2 * mass)
        // Using slightly underdamped (0.85) for snappier feel
        assert!((params.damping_ratio - SPRING_DAMPING_RATIO).abs() < f64::EPSILON);
    }

    #[test]
    fn test_spring_state_progression() {
        let mut state = SpringState::new(Duration::from_millis(200));

        let (pos0, _) = state.update(0.0);
        assert!(pos0 >= 0.0);

        let (pos1, _) = state.update(0.1);
        assert!(pos1 > pos0);

        let (pos2, _) = state.update(0.1);
        assert!(pos2 > pos1);
    }

    #[test]
    fn test_spring_state_settles() {
        let mut state = SpringState::new(Duration::from_millis(100));

        for _ in 0..100 {
            let (_, settled) = state.update(0.01);
            if settled {
                return;
            }
        }

        // Should have settled by now
        panic!("Spring did not settle within 100 iterations");
    }

    #[test]
    fn test_critically_damped_position() {
        let position_0 = SpringState::critically_damped_position(0.0, 33.0);
        assert!((position_0 - 0.0).abs() < 0.01);

        let position_inf = SpringState::critically_damped_position(1.0, 33.0);
        assert!((position_inf - 1.0).abs() < 0.01);
    }
}
