//! Latest-per-exact-token geometry coalescing, flushed per display.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use stache_macos::DisplayId;
use stache_macos::windows::WindowInstance;

/// Slowest flush rate.
pub const MIN_REFRESH_HZ: f64 = 30.0;
/// Fastest flush rate.
pub const MAX_REFRESH_HZ: f64 = 360.0;
/// Rate used when a display reports none.
pub const DEFAULT_REFRESH_HZ: f64 = 60.0;
/// Differences below this are display-mode rounding noise, not a retiming.
pub const RETIME_EPSILON: f64 = 0.001;
/// Pending geometry tokens across every display.
pub const GEOMETRY_CAPACITY: usize = 4096;

/// The explicit clamp rule: an unknown, `NaN`, infinite, zero or negative rate
/// is [`DEFAULT_REFRESH_HZ`]; any other rate is clamped into
/// [`MIN_REFRESH_HZ`]..=[`MAX_REFRESH_HZ`].
#[must_use]
pub fn clamp_refresh_hz(raw: Option<f64>) -> f64 {
    match raw {
        Some(hz) if hz.is_finite() && hz > 0.0 => hz.clamp(MIN_REFRESH_HZ, MAX_REFRESH_HZ),
        _ => DEFAULT_REFRESH_HZ,
    }
}

/// The flush interval for a display's reported rate.
#[must_use]
pub fn flush_period(raw: Option<f64>) -> Duration {
    Duration::from_secs_f64(1.0 / clamp_refresh_hz(raw))
}

/// What recording a sample did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// A token with no pending sample.
    New,
    /// Superseded the token's pending sample.
    Coalesced,
    /// The cardinality bound refused a new token.
    Dropped,
}

struct Bucket {
    deadline: Instant,
    samples: BTreeSet<WindowInstance>,
}

/// Pending geometry samples, at most one per exact token, grouped by display.
pub struct GeometryCoalescer {
    capacity: usize,
    periods: BTreeMap<DisplayId, Duration>,
    buckets: BTreeMap<Option<DisplayId>, Bucket>,
    pending: BTreeMap<WindowInstance, Option<DisplayId>>,
}

impl Default for GeometryCoalescer {
    fn default() -> Self { Self::with_capacity(GEOMETRY_CAPACITY) }
}

impl GeometryCoalescer {
    /// A coalescer holding at most `capacity` tokens.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            periods: BTreeMap::new(),
            buckets: BTreeMap::new(),
            pending: BTreeMap::new(),
        }
    }

    /// Pending token count.
    #[must_use]
    pub fn len(&self) -> usize { self.pending.len() }

    /// Whether nothing is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.pending.is_empty() }

    /// Whether a token has a pending sample.
    #[must_use]
    pub fn contains(&self, token: WindowInstance) -> bool { self.pending.contains_key(&token) }

    fn period(&self, route: Option<DisplayId>) -> Duration {
        route
            .and_then(|display| self.periods.get(&display).copied())
            .unwrap_or_else(|| flush_period(None))
    }

    fn place(&mut self, token: WindowInstance, route: Option<DisplayId>, deadline_cap: Instant) {
        let period_deadline = deadline_cap;
        let bucket = self.buckets.entry(route).or_insert_with(|| Bucket {
            deadline: period_deadline,
            samples: BTreeSet::new(),
        });
        bucket.deadline = bucket.deadline.min(period_deadline);
        bucket.samples.insert(token);
        self.pending.insert(token, route);
    }

    /// Records a sample, superseding any pending one for the exact token.
    pub fn record(
        &mut self,
        token: WindowInstance,
        route: Option<DisplayId>,
        now: Instant,
    ) -> Record {
        if let Some(held) = self.pending.get(&token).copied() {
            if held != route {
                self.remove(token);
                let deadline = now + self.period(route);
                self.place(token, route, deadline);
            }
            return Record::Coalesced;
        }
        if self.pending.len() >= self.capacity {
            return Record::Dropped;
        }
        // An existing bucket keeps its (earlier) deadline: a continuous stream
        // still flushes once per period instead of being postponed forever.
        let deadline = now + self.period(route);
        self.place(token, route, deadline);
        Record::New
    }

    /// Removes a token's pending sample; whether one existed.
    pub fn remove(&mut self, token: WindowInstance) -> bool {
        let Some(route) = self.pending.remove(&token) else {
            return false;
        };
        if let Some(bucket) = self.buckets.get_mut(&route) {
            bucket.samples.remove(&token);
            if bucket.samples.is_empty() {
                self.buckets.remove(&route);
            }
        }
        true
    }

    /// Takes every bucket whose deadline passed, in display order.
    pub fn due(&mut self, now: Instant) -> Vec<Vec<WindowInstance>> {
        let (ready, keep): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut self.buckets)
            .into_iter()
            .partition(|(_, bucket)| bucket.deadline <= now);
        self.buckets = keep;
        ready
            .into_values()
            .map(|bucket| {
                for token in &bucket.samples {
                    self.pending.remove(token);
                }
                bucket.samples.into_iter().collect()
            })
            .collect()
    }

    /// The earliest flush deadline.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.buckets.values().map(|bucket| bucket.deadline).min()
    }

    /// Replaces every display period (cancelling the old timers) and remaps
    /// pending samples by exact token through `route`. A remapped sample never
    /// waits longer than before nor longer than one new period.
    pub fn retime(
        &mut self,
        periods: BTreeMap<DisplayId, Duration>,
        route: impl Fn(WindowInstance) -> Option<DisplayId>,
        now: Instant,
    ) {
        let held: Vec<(WindowInstance, Option<DisplayId>, Instant)> = self
            .pending
            .iter()
            .map(|(token, old)| {
                let deadline = self.buckets.get(old).map_or(now, |bucket| bucket.deadline);
                (*token, *old, deadline)
            })
            .collect();
        self.periods = periods;
        self.buckets.clear();
        self.pending.clear();
        for (token, old, deadline) in held {
            let target = route(token)
                .filter(|display| self.periods.contains_key(display))
                .or_else(|| old.filter(|display| self.periods.contains_key(display)));
            let capped = deadline.min(now + self.period(target));
            self.place(token, target, capped);
        }
    }

    /// The configured period for a display.
    #[must_use]
    pub fn period_for(&self, display: DisplayId) -> Duration { self.period(Some(display)) }
}
