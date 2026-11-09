//! Host screen reservations as Tiling consumes them.
//!
//! Tiling only **reads** the host's [`ScreenReservationSnapshot`]. It never
//! declares `reserves_screen`, never calls `Registry::apply_screen_topology`
//! and never reads another module's configuration. Until Bar part 03 applies a
//! host topology the snapshot lists no display, which means "no host inset on
//! any display"; Tiling's own native display state still gates every write.
//!
//! The snapshot is converted once into the pure [`HostReservations`] value and
//! handed to the actor as [`crate::Input::Reservations`]. The watcher thread
//! only enqueues that value: it holds no state lock and does no native work.
//!
//! # Display identity
//!
//! The SDK identity is a numeric `CGDirectDisplayID` projection tagged with a
//! topology generation. The app crate owns the one native-to-SDK projection
//! (`stache_app::display::sdk_display_id`); this crate cannot depend on the app
//! crate, so the factory injects it as a [`DisplayProjection`] and Tiling never
//! reimplements it.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::{fmt, io};

use stache_macos::DisplayId;
use stache_sdk::{
    CancellationToken, DisplayId as SdkDisplayId, EdgeInsets, ScreenReservationObserver,
    ScreenReservationSnapshot, ScreenReservationWait,
};

use crate::events::Ingress;
use crate::geometry::Insets;
use crate::state::Input;

/// The longest the watcher sleeps before it re-checks cancellation.
pub const WATCH_QUANTUM: Duration = Duration::from_millis(250);
/// The most reservation entries one hint carries (the SDK caps owners at 64).
pub const MAX_HOST_DISPLAYS: usize = 256;

/// The injected native to SDK display identity projection.
#[derive(Clone)]
pub struct DisplayProjection(Arc<dyn Fn(DisplayId) -> SdkDisplayId + Send + Sync>);

impl DisplayProjection {
    /// Wraps the host's projection function.
    #[must_use]
    pub fn new(project: impl Fn(DisplayId) -> SdkDisplayId + Send + Sync + 'static) -> Self {
        Self(Arc::new(project))
    }

    /// The SDK identity of a native display.
    #[must_use]
    pub fn project(&self, native: DisplayId) -> SdkDisplayId { (self.0)(native) }
}

impl fmt::Debug for DisplayProjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("DisplayProjection") }
}

impl PartialEq for DisplayProjection {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}

/// One display's aggregated host reservation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostDisplay {
    /// The host topology generation in which this presence of the id began.
    pub since: u64,
    /// The per-edge maximum over every owner, measured inward from the full
    /// display bounds.
    pub insets: Insets,
}

/// What the host says about one native display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostLookup {
    /// No host topology has been applied (the snapshot lists no display), so
    /// every display has zero host inset.
    NoHostTopology,
    /// The display is present in the host topology.
    Present(HostDisplay),
    /// A host topology exists and does not list the display.
    Absent,
}

/// A pure, immutable copy of the host reservation snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct HostReservations {
    /// The hub's monotonic revision.
    pub revision: u64,
    /// The host topology generation the displays belong to.
    pub topology: u64,
    displays: BTreeMap<u32, HostDisplay>,
    projection: Option<DisplayProjection>,
}

impl Default for HostReservations {
    fn default() -> Self { Self::empty() }
}

/// Converts validated SDK insets to the pure geometry type.
#[must_use]
pub const fn insets_from_sdk(insets: &EdgeInsets) -> Insets {
    Insets::new(insets.top(), insets.right(), insets.bottom(), insets.left())
}

impl HostReservations {
    /// No snapshot seen yet: zero host inset everywhere.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            revision: 0,
            topology: 0,
            displays: BTreeMap::new(),
            projection: None,
        }
    }

    /// Converts one snapshot. Entries beyond [`MAX_HOST_DISPLAYS`] are ignored
    /// (the SDK never produces them).
    #[must_use]
    pub fn from_snapshot(
        snapshot: &ScreenReservationSnapshot,
        projection: &DisplayProjection,
    ) -> Self {
        let displays = snapshot
            .displays
            .iter()
            .take(MAX_HOST_DISPLAYS)
            .map(|entry| {
                (entry.display.raw(), HostDisplay {
                    since: entry.since.get(),
                    insets: insets_from_sdk(&entry.insets),
                })
            })
            .collect();
        Self {
            revision: snapshot.revision,
            topology: snapshot.topology.get(),
            displays,
            projection: Some(projection.clone()),
        }
    }

    /// Whether the snapshot lists any display.
    #[must_use]
    pub fn has_topology(&self) -> bool { !self.displays.is_empty() }

    /// What the host says about `native`.
    #[must_use]
    pub fn lookup(&self, native: DisplayId) -> HostLookup {
        if self.displays.is_empty() {
            return HostLookup::NoHostTopology;
        }
        let Some(projection) = &self.projection else {
            return HostLookup::Absent;
        };
        self.displays
            .get(&projection.project(native).raw())
            .map_or(HostLookup::Absent, |display| HostLookup::Present(*display))
    }
}

/// Where the watcher reads snapshots from. The SDK observer implements it;
/// tests use a scripted fake.
pub trait ReservationSource: Send + Sync + 'static {
    /// The latest coherent snapshot.
    fn read(&self) -> Arc<ScreenReservationSnapshot>;

    /// Blocks until the revision differs from `seen`, the timeout elapses, the
    /// token is cancelled or the host closes.
    fn wait_for_change(
        &self,
        seen: u64,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> ScreenReservationWait;
}

impl ReservationSource for ScreenReservationObserver {
    fn read(&self) -> Arc<ScreenReservationSnapshot> { Self::read(self) }

    fn wait_for_change(
        &self,
        seen: u64,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> ScreenReservationWait {
        Self::wait_for_change(self, seen, timeout, cancel)
    }
}

/// A cancellation-wakeable thread that turns snapshots into actor hints.
///
/// It reads first and then waits with the revision it read (subscribe before
/// fetch), so a publication between the two is never lost. Each change is
/// forwarded as one pure [`Input::Reservations`]; the watcher does nothing else.
pub struct ReservationWatcher {
    cancel: CancellationToken,
    thread: Mutex<Option<JoinHandle<()>>>,
    primed: Arc<Primed>,
}

/// Set once the first snapshot reached the actor (or the thread ended).
#[derive(Default)]
struct Primed {
    /// `None` while pending; `Some(true)` once delivered, `Some(false)` when
    /// the thread ended without delivering.
    done: Mutex<Option<bool>>,
    changed: Condvar,
}

impl Primed {
    fn set(&self, delivered: bool) {
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        if done.is_none() {
            *done = Some(delivered);
        }
        drop(done);
        self.changed.notify_all();
    }
}

/// Marks the latch when the thread leaves, however it leaves.
struct PrimeOnExit(Arc<Primed>);

impl Drop for PrimeOnExit {
    fn drop(&mut self) { self.0.set(false); }
}

impl ReservationWatcher {
    /// The watcher thread name.
    pub const THREAD_NAME: &'static str = "stache-tiling-reservations";

    /// Starts the watcher.
    ///
    /// # Errors
    ///
    /// The thread could not be spawned.
    pub fn spawn(
        source: Arc<dyn ReservationSource>,
        projection: DisplayProjection,
        ingress: Arc<Ingress>,
    ) -> io::Result<Self> {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let primed = Arc::new(Primed::default());
        let thread_primed = Arc::clone(&primed);
        let thread =
            std::thread::Builder::new().name(Self::THREAD_NAME.to_owned()).spawn(move || {
                let _exit = PrimeOnExit(Arc::clone(&thread_primed));
                let mut seen = {
                    let snapshot = source.read();
                    let revision = snapshot.revision;
                    if ingress
                        .submit(Input::Reservations(HostReservations::from_snapshot(
                            &snapshot,
                            &projection,
                        )))
                        .is_err()
                    {
                        return;
                    }
                    thread_primed.set(true);
                    revision
                };
                loop {
                    match source.wait_for_change(seen, WATCH_QUANTUM, &token) {
                        ScreenReservationWait::Changed(snapshot) => {
                            seen = snapshot.revision;
                            let hint = HostReservations::from_snapshot(&snapshot, &projection);
                            if ingress.submit(Input::Reservations(hint)).is_err() {
                                break;
                            }
                        }
                        ScreenReservationWait::TimedOut => {
                            if token.is_cancelled() {
                                break;
                            }
                        }
                        ScreenReservationWait::Cancelled | ScreenReservationWait::Closed => break,
                    }
                }
            })?;
        Ok(Self {
            cancel,
            thread: Mutex::new(Some(thread)),
            primed,
        })
    }

    /// Waits until the first snapshot was handed to the actor, so reservations
    /// are consumed before the first frame effect. `false` on timeout or when
    /// the thread ended before delivering (the actor mailbox closed).
    #[must_use]
    pub fn wait_primed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let mut done = self.primed.done.lock().unwrap_or_else(PoisonError::into_inner);
        while done.is_none() {
            let remaining =
                deadline.map_or(WATCH_QUANTUM, |d| d.saturating_duration_since(Instant::now()));
            if remaining.is_zero() {
                return false;
            }
            done = self
                .primed
                .changed
                .wait_timeout(done, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *done == Some(true)
    }

    /// Cancels the wait, wakes the thread and joins it for at most `hop`.
    /// `false` when the thread has not ended: its handle is kept (never
    /// detached) and stopping again retries exactly that join. Idempotent.
    #[must_use]
    pub fn stop(&self, hop: Duration) -> bool {
        self.cancel.cancel();
        crate::actor::join_within(&self.thread, hop)
    }

    /// Whether the thread has been joined.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.thread.lock().unwrap_or_else(PoisonError::into_inner).is_none()
    }
}

impl Drop for ReservationWatcher {
    fn drop(&mut self) { let _ = self.stop(Duration::from_secs(2)); }
}

#[cfg(test)]
mod tests {
    use std::sync::Condvar;
    use std::time::Instant;

    use stache_sdk::{DisplayReservations, ReservationContribution, TopologyGeneration};

    use super::*;
    use crate::events::Pipeline;

    pub fn projection() -> DisplayProjection {
        DisplayProjection::new(|native| SdkDisplayId::new(native.raw()))
    }

    pub fn edge(top: f64, left: f64, bottom: f64, right: f64) -> EdgeInsets {
        EdgeInsets::new(top, left, bottom, right).expect("valid insets")
    }

    pub fn snapshot(
        revision: u64,
        topology: u64,
        displays: &[(u32, u64, EdgeInsets)],
    ) -> ScreenReservationSnapshot {
        ScreenReservationSnapshot {
            revision,
            topology: TopologyGeneration::new(topology),
            displays: displays
                .iter()
                .map(|(id, since, insets)| DisplayReservations {
                    display: SdkDisplayId::new(*id),
                    since: TopologyGeneration::new(*since),
                    insets: *insets,
                    contributions: Vec::<ReservationContribution>::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn edge_insets_convert_edge_for_edge() {
        let converted = insets_from_sdk(&edge(1.0, 2.0, 3.0, 4.0));
        // SDK order is top, left, bottom, right; geometry order top, right, bottom, left.
        assert_eq!(converted, Insets::new(1.0, 4.0, 3.0, 2.0));
        for (index, value) in [7.0, 8.0, 9.0, 10.0].into_iter().enumerate() {
            let mut sides = [0.0; 4];
            sides[index] = value;
            let converted = insets_from_sdk(&edge(sides[0], sides[1], sides[2], sides[3]));
            let expected = Insets::new(sides[0], sides[3], sides[2], sides[1]);
            assert_eq!(converted, expected, "edge {index}");
        }
    }

    #[test]
    fn an_empty_snapshot_means_no_host_topology() {
        let host = HostReservations::from_snapshot(&snapshot(1, 0, &[]), &projection());
        assert!(!host.has_topology());
        assert_eq!(host.lookup(DisplayId::new(5)), HostLookup::NoHostTopology);
        assert_eq!(
            HostReservations::empty().lookup(DisplayId::new(5)),
            HostLookup::NoHostTopology
        );
    }

    #[test]
    fn a_present_topology_distinguishes_present_and_absent_displays() {
        let host = HostReservations::from_snapshot(
            &snapshot(3, 2, &[(1, 1, edge(40.0, 0.0, 0.0, 0.0))]),
            &projection(),
        );
        assert_eq!(
            host.lookup(DisplayId::new(1)),
            HostLookup::Present(HostDisplay {
                since: 1,
                insets: Insets::new(40.0, 0.0, 0.0, 0.0)
            })
        );
        assert_eq!(host.lookup(DisplayId::new(2)), HostLookup::Absent);
    }

    #[test]
    fn the_injected_projection_decides_identity() {
        let offset = DisplayProjection::new(|native| SdkDisplayId::new(native.raw() + 100));
        let host = HostReservations::from_snapshot(
            &snapshot(1, 1, &[(101, 1, edge(1.0, 0.0, 0.0, 0.0))]),
            &offset,
        );
        assert!(matches!(host.lookup(DisplayId::new(1)), HostLookup::Present(_)));
        assert_eq!(host.lookup(DisplayId::new(101)), HostLookup::Absent);
    }

    /// A scripted source: `read` pops snapshots, waits follow a script.
    struct Script {
        state: Mutex<ScriptState>,
        changed: Condvar,
    }

    struct ScriptState {
        current: Arc<ScreenReservationSnapshot>,
        reads: usize,
        closed: bool,
    }

    impl Script {
        fn new(first: ScreenReservationSnapshot) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(ScriptState {
                    current: Arc::new(first),
                    reads: 0,
                    closed: false,
                }),
                changed: Condvar::new(),
            })
        }

        fn publish(&self, next: ScreenReservationSnapshot) {
            self.state.lock().expect("script").current = Arc::new(next);
            self.changed.notify_all();
        }

        fn close(&self) {
            self.state.lock().expect("script").closed = true;
            self.changed.notify_all();
        }
    }

    impl ReservationSource for Script {
        fn read(&self) -> Arc<ScreenReservationSnapshot> {
            let mut state = self.state.lock().expect("script");
            state.reads += 1;
            Arc::clone(&state.current)
        }

        fn wait_for_change(
            &self,
            seen: u64,
            timeout: Duration,
            cancel: &CancellationToken,
        ) -> ScreenReservationWait {
            let deadline = Instant::now() + timeout;
            let mut state = self.state.lock().expect("script");
            loop {
                if cancel.is_cancelled() {
                    return ScreenReservationWait::Cancelled;
                }
                if state.closed {
                    return ScreenReservationWait::Closed;
                }
                if state.current.revision != seen {
                    return ScreenReservationWait::Changed(Arc::clone(&state.current));
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return ScreenReservationWait::TimedOut;
                }
                // A short slice keeps the fake cancellation-observant without
                // sharing the production hub's wake wiring.
                let slice = left.min(Duration::from_millis(5));
                state = self
                    .changed
                    .wait_timeout(state, slice)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }

    fn new_ingress() -> Arc<Ingress> { Arc::new(Ingress::new(Pipeline::new())) }

    fn drain(ingress: &Ingress) -> Vec<HostReservations> {
        ingress
            .with(|pipeline| pipeline.take(64))
            .into_iter()
            .filter_map(|input| match input {
                Input::Reservations(host) => Some(host),
                _ => None,
            })
            .collect()
    }

    fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = probe() {
                return value;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn the_watcher_reads_first_then_forwards_each_change_as_a_pure_hint() {
        let source = Script::new(snapshot(1, 0, &[]));
        let ingress = new_ingress();
        let watcher = ReservationWatcher::spawn(source.clone(), projection(), Arc::clone(&ingress))
            .expect("spawn");
        let mut seen = Vec::new();
        seen.extend(wait_for(|| {
            let got = drain(&ingress);
            (!got.is_empty()).then_some(got)
        }));
        assert_eq!(seen[0].revision, 1);
        source.publish(snapshot(2, 1, &[(1, 1, edge(40.0, 0.0, 0.0, 0.0))]));
        seen.extend(wait_for(|| {
            let got = drain(&ingress);
            (!got.is_empty()).then_some(got)
        }));
        assert_eq!(seen[1].revision, 2);
        assert!(seen[1].has_topology());
        assert!(watcher.stop(Duration::from_secs(2)));
        assert!(watcher.is_stopped());
    }

    /// A source whose wait ignores cancellation until released.
    struct Blocked {
        release: Mutex<bool>,
        changed: Condvar,
    }

    impl ReservationSource for Blocked {
        fn read(&self) -> Arc<ScreenReservationSnapshot> { Arc::new(snapshot(1, 0, &[])) }

        fn wait_for_change(
            &self,
            _seen: u64,
            _timeout: Duration,
            _cancel: &CancellationToken,
        ) -> ScreenReservationWait {
            let mut released = self.release.lock().expect("release");
            while !*released {
                released = self.changed.wait(released).unwrap_or_else(PoisonError::into_inner);
            }
            drop(released);
            ScreenReservationWait::Cancelled
        }
    }

    #[test]
    fn a_stuck_watcher_is_not_joined_without_bound_and_stays_owed_until_it_ends() {
        let source = Arc::new(Blocked {
            release: Mutex::new(false),
            changed: Condvar::new(),
        });
        let ingress = new_ingress();
        let watcher = ReservationWatcher::spawn(source.clone(), projection(), Arc::clone(&ingress))
            .expect("spawn");
        assert!(watcher.wait_primed(Duration::from_secs(5)));
        let started = Instant::now();
        assert!(!watcher.stop(Duration::from_millis(30)), "still running: owed");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the join was bounded"
        );
        assert!(!watcher.is_stopped(), "the handle is kept, not detached");
        *source.release.lock().expect("release") = true;
        source.changed.notify_all();
        assert!(
            watcher.stop(Duration::from_secs(5)),
            "a retry joins exactly that thread"
        );
        assert!(watcher.is_stopped());
    }

    #[test]
    fn a_publication_between_read_and_wait_is_not_lost() {
        // The change lands before the thread ever waits: the wait compares the
        // revision it read and returns the newer snapshot immediately.
        let source = Script::new(snapshot(1, 0, &[]));
        let ingress = new_ingress();
        let first = source.read();
        source.publish(snapshot(2, 1, &[(9, 1, edge(5.0, 0.0, 0.0, 0.0))]));
        let wait = source.wait_for_change(
            first.revision,
            Duration::from_secs(1),
            &CancellationToken::new(),
        );
        assert!(matches!(wait, ScreenReservationWait::Changed(s) if s.revision == 2));
        drop(ingress);
    }

    #[test]
    fn stop_wakes_a_waiting_watcher_and_joins() {
        let source = Script::new(snapshot(1, 0, &[]));
        let ingress = new_ingress();
        let watcher =
            ReservationWatcher::spawn(source, projection(), Arc::clone(&ingress)).expect("spawn");
        wait_for(|| {
            let got = drain(&ingress);
            (!got.is_empty()).then_some(())
        });
        let started = Instant::now();
        assert!(watcher.stop(Duration::from_secs(2)));
        assert!(watcher.is_stopped());
        assert!(started.elapsed() < Duration::from_secs(2));
        // Idempotent.
        assert!(watcher.stop(Duration::from_secs(2)));
    }

    #[test]
    fn a_closed_host_or_closed_mailbox_ends_the_watcher() {
        let source = Script::new(snapshot(1, 0, &[]));
        let ingress = new_ingress();
        let watcher = ReservationWatcher::spawn(source.clone(), projection(), Arc::clone(&ingress))
            .expect("spawn");
        source.close();
        wait_for(|| {
            let thread = watcher.thread.lock().expect("thread");
            thread.as_ref().is_some_and(JoinHandle::is_finished).then_some(())
        });
        assert!(watcher.stop(Duration::from_secs(2)));

        let source = Script::new(snapshot(1, 0, &[]));
        let closed = new_ingress();
        closed.close();
        let watcher = ReservationWatcher::spawn(source, projection(), closed).expect("spawn");
        wait_for(|| {
            let thread = watcher.thread.lock().expect("thread");
            thread.as_ref().is_some_and(JoinHandle::is_finished).then_some(())
        });
        assert!(watcher.stop(Duration::from_secs(2)));
    }
}
