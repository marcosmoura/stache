//! Damped-spring progress, ported from the legacy analytical solution of
//! `x'' + 2 zeta w0 x' + w0^2 x = w0^2`.

use std::time::Duration;

/// Convergence threshold (distance from the target progress).
const SPRING_POSITION_THRESHOLD: f64 = 0.01;
/// Damping ratio: slightly underdamped, snappy with imperceptible overshoot.
const SPRING_DAMPING_RATIO: f64 = 0.85;
/// Settling time multiplier for the target duration.
const SPRING_SETTLE_FACTOR: f64 = 5.5;

/// Spring parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    /// Natural frequency.
    pub omega_0: f64,
    /// Damping ratio.
    pub damping_ratio: f64,
}

impl SpringParams {
    /// Parameters that settle in about `duration`.
    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        let target = duration.as_secs_f64().max(0.01);
        Self {
            omega_0: SPRING_SETTLE_FACTOR / target,
            damping_ratio: SPRING_DAMPING_RATIO,
        }
    }
}

/// One spring animation.
#[derive(Clone, Debug)]
pub struct SpringState {
    /// Seconds simulated so far.
    pub elapsed: f64,
    params: SpringParams,
}

impl SpringState {
    /// A spring that settles in about `target_duration`.
    #[must_use]
    pub fn new(target_duration: Duration) -> Self {
        Self {
            elapsed: 0.0,
            params: SpringParams::from_duration(target_duration),
        }
    }

    /// Advances by `dt` seconds. Returns the progress (0 to 1.5) and whether the
    /// spring settled.
    pub fn update(&mut self, dt: f64) -> (f64, bool) {
        self.elapsed += dt;
        let position = self.calculate_position(self.elapsed);
        let settled = (position - 1.0).abs() < SPRING_POSITION_THRESHOLD && self.elapsed > 0.02;
        let position = if settled { 1.0 } else { position };
        (position.clamp(0.0, 1.5), settled)
    }

    /// The progress at time `t` seconds.
    #[must_use]
    pub fn calculate_position(&self, t: f64) -> f64 {
        let (omega_0, zeta) = (self.params.omega_0, self.params.damping_ratio);
        if zeta < 1.0 {
            Self::underdamped_position(t, omega_0, zeta)
        } else if (zeta - 1.0).abs() < 0.001 {
            Self::critically_damped_position(t, omega_0)
        } else {
            Self::overdamped_position(t, omega_0, zeta)
        }
    }

    /// Underdamped (`zeta < 1`).
    #[must_use]
    pub fn underdamped_position(t: f64, omega_0: f64, zeta: f64) -> f64 {
        let complement = zeta.mul_add(-zeta, 1.0);
        let omega_d = omega_0 * complement.sqrt();
        let decay = (-zeta * omega_0 * t).exp();
        let cos_term = (omega_d * t).cos();
        let sin_term = (zeta / complement.sqrt()) * (omega_d * t).sin();
        decay.mul_add(-(cos_term + sin_term), 1.0)
    }

    /// Critically damped (`zeta = 1`).
    #[must_use]
    pub fn critically_damped_position(t: f64, omega_0: f64) -> f64 {
        let decay = (-omega_0 * t).exp();
        decay.mul_add(-omega_0.mul_add(t, 1.0), 1.0)
    }

    /// Overdamped (`zeta > 1`).
    #[must_use]
    pub fn overdamped_position(t: f64, omega_0: f64, zeta: f64) -> f64 {
        let minus_one = zeta.mul_add(zeta, -1.0);
        let gamma = omega_0 * minus_one.sqrt();
        let decay = (-zeta * omega_0 * t).exp();
        let cosh_term = (gamma * t).cosh();
        let sinh_term = (zeta / minus_one.sqrt()) * (gamma * t).sinh();
        decay.mul_add(-(cosh_term + sinh_term), 1.0)
    }
}
