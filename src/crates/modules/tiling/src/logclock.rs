//! A monotonic millisecond stamp for latency-oriented debug lines.
//!
//! One [`LogOrigin`] is captured when the tracking runtime starts and shared by
//! the actor, the visibility control and the borders runner, so lines written by
//! different threads can be ordered and subtracted: `[t+12345.6ms]`.

use std::time::Instant;

/// The instant the runtime started; stamps are measured from it.
#[derive(Clone, Copy, Debug)]
pub struct LogOrigin(Instant);

impl Default for LogOrigin {
    fn default() -> Self { Self(Instant::now()) }
}

impl LogOrigin {
    /// An origin at `start`.
    #[must_use]
    pub const fn new(start: Instant) -> Self { Self(start) }

    /// `[t+<ms>ms]` for the current moment, one decimal.
    #[must_use]
    pub fn stamp(&self) -> String {
        format!("[t+{:.1}ms]", self.0.elapsed().as_secs_f64() * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_prefixed_and_monotonic() {
        let origin = LogOrigin::default();
        let first = origin.stamp();
        assert!(first.starts_with("[t+") && first.ends_with("ms]"), "{first}");
        let value = |s: &str| s[3..s.len() - 3].parse::<f64>().expect("number");
        assert!(value(&origin.stamp()) >= value(&first));
    }
}
