//! Per-application animation metrics with bounded memory.
//!
//! The coordinator records what each finished animation did (frames scheduled,
//! written and dropped, effective rate, write latency, end lag) into a
//! [`MetricsBook`] at job end only. A job collects write latencies in a fixed
//! [`LatencyRing`] while it runs, so no frame allocates; the book keeps the last
//! [`WINDOW`] animations of at most [`MAX_APPLICATIONS`] applications, evicting
//! the least recently used. Status reads the book through a short lock; no AX
//! call is made for any of it.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use stache_macos::ApplicationId;

use super::fallback::MotionKind;
use super::worker::FrameCounters;

/// Animations kept per application.
pub const WINDOW: usize = 8;
/// Applications kept; the least recently recorded is evicted past this.
pub const MAX_APPLICATIONS: usize = 64;
/// Write latencies a job remembers (a ring: the newest replace the oldest).
pub const LATENCY_SAMPLES: usize = 64;
/// How many of the latest animations the fallback policy looks at.
pub const POLICY_LOOKBACK: usize = 3;
/// How many of them must be bad to trigger the fallback.
pub const POLICY_BAD: usize = 2;
/// Below this effective rate (written frames per second) an animation is bad.
/// A last-resort threshold for clearly broken applications, absolute and not
/// relative to the refresh: pacing still targets the display's native rate, and
/// a healthy application that writes 20-30 frames per second per window
/// (dropping the rest, latest wins) must keep animating.
pub const MIN_RATE_HZ: f64 = 15.0;
/// An animation whose final acknowledgement lands later than this after its
/// scheduled end is bad. A last resort like [`MIN_RATE_HZ`]: a healthy
/// multi-window application with about 100 ms of end lag must keep animating.
pub const MAX_END_LAG: Duration = Duration::from_millis(250);

/// Write latencies of one running job in a fixed buffer.
#[derive(Clone, Copy, Debug)]
pub struct LatencyRing {
    micros: [u32; LATENCY_SAMPLES],
    len: usize,
    next: usize,
}

impl Default for LatencyRing {
    fn default() -> Self { Self::new() }
}

impl LatencyRing {
    /// An empty ring.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            micros: [0; LATENCY_SAMPLES],
            len: 0,
            next: 0,
        }
    }

    /// Remembers one latency, replacing the oldest when full.
    pub fn push(&mut self, latency: Duration) {
        self.micros[self.next] = u32::try_from(latency.as_micros()).unwrap_or(u32::MAX);
        self.next = (self.next + 1) % LATENCY_SAMPLES;
        self.len = (self.len + 1).min(LATENCY_SAMPLES);
    }

    /// The `(p50, p95)` latencies by nearest rank; zero when empty. Sorts a
    /// copy on the stack.
    #[must_use]
    pub fn percentiles(&self) -> (Duration, Duration) {
        if self.len == 0 {
            return (Duration::ZERO, Duration::ZERO);
        }
        let mut sorted = self.micros;
        let held = &mut sorted[..self.len];
        held.sort_unstable();
        let at = |p: usize| Duration::from_micros(u64::from(held[rank(self.len, p)]));
        (at(50), at(95))
    }
}

/// The zero-based nearest-rank index of percentile `p` among `n` samples.
const fn rank(n: usize, p: usize) -> usize { (n * p).div_ceil(100).saturating_sub(1) }

/// The nearest-rank percentile of `values` (sorted in place); `None` if empty.
fn percentile<T: PartialOrd + Copy>(values: &mut [T], p: usize) -> Option<T> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(values[rank(values.len(), p)])
}

/// What one application's animation of one layout change did. The windows of
/// one plan are folded into one record: the slowest rate, the largest latency
/// and end lag, the summed frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimationRecord {
    /// The layout plan the animation belonged to.
    pub plan_sequence: u64,
    /// Whether the windows moved only or changed size; the fallback policy
    /// looks at each kind separately.
    pub kind: MotionKind,
    /// Frames scheduled, written and dropped.
    pub frames: FrameCounters,
    /// Written frames per second from the first tick to the final
    /// acknowledgement (infinite when no time passed).
    pub rate_hz: f64,
    /// Median write latency (submission to acknowledgement).
    pub write_p50: Duration,
    /// 95th percentile write latency.
    pub write_p95: Duration,
    /// Final acknowledgement minus the scheduled end (zero if early).
    pub end_lag: Duration,
    /// The display frame period the animation was paced at.
    pub period: Duration,
}

impl AnimationRecord {
    /// Whether the effective rate was below [`MIN_RATE_HZ`].
    #[must_use]
    pub fn rate_bad(&self) -> bool { self.rate_hz < MIN_RATE_HZ }

    /// Whether the animation ended more than [`MAX_END_LAG`] late.
    #[must_use]
    pub fn lag_bad(&self) -> bool { self.end_lag > MAX_END_LAG }

    /// Folds another window of the same plan into this record.
    fn fold(&mut self, other: &Self) {
        self.frames += other.frames;
        self.rate_hz = self.rate_hz.min(other.rate_hz);
        self.write_p50 = self.write_p50.max(other.write_p50);
        self.write_p95 = self.write_p95.max(other.write_p95);
        self.end_lag = self.end_lag.max(other.end_lag);
    }
}

/// Which criterion an application failed in 2 of its last 3 animations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The effective rate was below [`MIN_RATE_HZ`].
    Rate,
    /// The animation ended more than [`MAX_END_LAG`] late.
    Lag,
}

#[derive(Debug, Default)]
struct AppMetrics {
    records: VecDeque<AnimationRecord>,
    used: u64,
}

/// The bounded rolling window of animation records per application.
#[derive(Debug, Default)]
pub struct MetricsBook {
    apps: BTreeMap<ApplicationId, AppMetrics>,
    clock: u64,
}

/// A summary of an application's window for one status line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MetricsSummary {
    /// Animations in the window.
    pub animations: usize,
    /// Median effective rate (Hz).
    pub rate_p50_hz: f64,
    /// Frames dropped across the window.
    pub dropped: usize,
    /// 95th percentile of the animations' write latency p95.
    pub write_p95: Duration,
    /// 95th percentile of the animations' end lag.
    pub end_lag_p95: Duration,
}

impl MetricsBook {
    /// Records a finished animation and returns the criterion the application
    /// now fails in 2 of its last 3 of the record's kind, if any.
    pub fn record(
        &mut self,
        application: ApplicationId,
        record: AnimationRecord,
    ) -> Option<Trigger> {
        self.clock += 1;
        if !self.apps.contains_key(&application) && self.apps.len() >= MAX_APPLICATIONS {
            let oldest = self.apps.iter().min_by_key(|(_, m)| m.used).map(|(app, _)| *app);
            if let Some(oldest) = oldest {
                self.apps.remove(&oldest);
            }
        }
        let entry = self.apps.entry(application).or_insert_with(|| AppMetrics {
            records: VecDeque::with_capacity(WINDOW),
            used: 0,
        });
        entry.used = self.clock;
        match entry.records.iter_mut().rev().find(|last| last.kind == record.kind) {
            Some(last) if last.plan_sequence == record.plan_sequence => last.fold(&record),
            _ => {
                if entry.records.len() == WINDOW {
                    entry.records.pop_front();
                }
                entry.records.push_back(record);
            }
        }
        let last = entry
            .records
            .iter()
            .rev()
            .filter(|r| r.kind == record.kind)
            .take(POLICY_LOOKBACK);
        let (mut rate, mut lag, mut any) = (0, 0, 0);
        for record in last {
            let (r, l) = (record.rate_bad(), record.lag_bad());
            rate += usize::from(r);
            lag += usize::from(l);
            any += usize::from(r || l);
        }
        if any < POLICY_BAD {
            None
        } else if rate >= POLICY_BAD || (lag < POLICY_BAD && rate >= lag) {
            Some(Trigger::Rate)
        } else {
            Some(Trigger::Lag)
        }
    }

    /// Drops an application's records of one kind (it left the fallback for
    /// that kind, so it needs two new bad records of three to fall back again).
    pub fn forget_kind(&mut self, application: ApplicationId, kind: MotionKind) {
        if let Some(entry) = self.apps.get_mut(&application) {
            entry.records.retain(|record| record.kind != kind);
        }
    }

    /// Drops an application's window (it terminated).
    pub fn forget(&mut self, application: ApplicationId) { self.apps.remove(&application); }

    /// Drops everything (animations were turned off).
    pub fn clear(&mut self) { self.apps.clear(); }

    /// Applications held.
    #[must_use]
    pub fn len(&self) -> usize { self.apps.len() }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.apps.is_empty() }

    /// The records of one application, oldest first.
    #[must_use]
    pub fn records(&self, application: ApplicationId) -> Vec<AnimationRecord> {
        self.apps
            .get(&application)
            .map_or_else(Vec::new, |m| m.records.iter().copied().collect())
    }

    /// One summary per application with data, in application order.
    #[must_use]
    pub fn summaries(&self) -> Vec<(ApplicationId, MetricsSummary)> {
        self.apps
            .iter()
            .filter(|(_, m)| !m.records.is_empty())
            .map(|(app, m)| (*app, summarize(&m.records)))
            .collect()
    }
}

fn summarize(records: &VecDeque<AnimationRecord>) -> MetricsSummary {
    let mut rates = [0.0_f64; WINDOW];
    let mut writes = [Duration::ZERO; WINDOW];
    let mut lags = [Duration::ZERO; WINDOW];
    let n = records.len().min(WINDOW);
    for (i, record) in records.iter().take(WINDOW).enumerate() {
        rates[i] = record.rate_hz;
        writes[i] = record.write_p95;
        lags[i] = record.end_lag;
    }
    MetricsSummary {
        animations: n,
        rate_p50_hz: percentile(&mut rates[..n], 50).unwrap_or(0.0),
        dropped: records.iter().map(|r| r.frames.dropped).sum(),
        write_p95: percentile(&mut writes[..n], 95).unwrap_or_default(),
        end_lag_p95: percentile(&mut lags[..n], 95).unwrap_or_default(),
    }
}

/// `animation metrics: <who>: <n> animations, rate p50 <hz> Hz, dropped <d>,
/// write p95 <ms> ms, end lag p95 <ms> ms`.
#[must_use]
pub fn describe(who: &str, summary: &MetricsSummary) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    format!(
        "animation metrics: {who}: {} animation{}, rate p50 {:.1} Hz, dropped {}, write p95 \
         {:.1} ms, end lag p95 {:.1} ms",
        summary.animations,
        if summary.animations == 1 { "" } else { "s" },
        summary.rate_p50_hz.min(9999.9),
        summary.dropped,
        ms(summary.write_p95),
        ms(summary.end_lag_p95),
    )
}
