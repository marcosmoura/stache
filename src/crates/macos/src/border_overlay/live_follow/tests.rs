//! Fakes-only tests of live follow: slots, symbols, the follower core and the
//! owner thread (TB-06-01, TB-06-02). Nothing here calls `SkyLight`, creates a
//! display link or registers a callback.

use std::ffi::{CStr, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::skylight::{
    EVENT_WINDOW_MOVE, SkyLightWindowNotify, WINDOW_EVENTS, WindowSymbols, kind_of_event,
    window_id_from_payload,
};
use super::{
    EventSlots, FAST_REORDER_CAP, FakeLiveFollow, FakeSessionControl, FakeSink, FastOrderResult,
    FastReorder, FollowCore, FollowMove, FollowSink, FollowTarget, FollowTiming, LiveFollow,
    LiveStatus, OWNER_THREAD_NAME, SinkCall, SlotEvent, StopObserver, StopOutcome,
    SystemLiveFollow, WindowEventKind, forgotten_follow_owners, unavailable_message,
};
use crate::border_overlay::{MAX_OVERLAYS, OverlayRect};
use crate::display_link::testing::FakeFrameClockFactory;
use crate::display_link::{FrameClockFactory, WakeFn};
use crate::windows::{AdapterId, RuntimeEpoch, WindowInstance};
use crate::{ApplicationId, DisplayId};

fn window(n: u64) -> WindowInstance {
    static ADAPTER: std::sync::OnceLock<AdapterId> = std::sync::OnceLock::new();
    let adapter = *ADAPTER.get_or_init(AdapterId::for_testing);
    WindowInstance::for_testing(
        adapter,
        RuntimeEpoch::for_testing(1),
        ApplicationId::for_testing(1),
        n,
    )
}

const D1: DisplayId = DisplayId::new(1);
const D2: DisplayId = DisplayId::new(2);

fn target(n: u64, display: Option<DisplayId>) -> FollowTarget {
    FollowTarget {
        window: window(n),
        cg_window_id: 100 + u32::try_from(n).unwrap(),
        display,
        owner_pid: Some(9_000),
        overlay_window: None,
        above: false,
        fast_reorder: false,
    }
}

fn rect(x: f64) -> OverlayRect { OverlayRect::new(x, 10.0, 400.0, 300.0) }

fn event(cg: u32, kinds: &[WindowEventKind]) -> SlotEvent {
    SlotEvent {
        cg_window_id: cg,
        kinds: kinds.iter().fold(0, |bits, kind| bits | kind.bit()),
    }
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ----------------------------------------------------------------- slots

#[test]
fn only_watched_windows_are_recorded_and_the_rest_are_dropped() {
    let slots = EventSlots::new(500);
    // Own window (pid 500), a foreign one and one that is not bordered.
    let watched = slots.watch(&[(11, Some(900)), (12, Some(500)), (13, None)]);
    assert_eq!(watched, [11, 13], "Stache's own window is never watched");
    slots.record(WindowEventKind::Move, 11);
    slots.record(WindowEventKind::Move, 12); // own
    slots.record(WindowEventKind::Move, 77); // foreign, not bordered
    slots.record(WindowEventKind::Move, 0); // unknown id
    let counters = slots.counters();
    assert_eq!(counters.recorded, 1);
    assert_eq!(counters.unwatched, 3);
    assert_eq!(counters.own_dropped, 1);
    assert_eq!(slots.drain(), [event(11, &[WindowEventKind::Move])]);
    assert!(slots.drain().is_empty(), "a drain takes the events");
}

#[test]
fn events_are_latest_per_window_and_hide_unhide_cancel() {
    let slots = EventSlots::new(1);
    let _ = slots.watch(&[(21, None), (22, None)]);
    for _ in 0..1000 {
        slots.record(WindowEventKind::Move, 21);
        slots.record(WindowEventKind::Resize, 21);
    }
    slots.record(WindowEventKind::Hide, 22);
    slots.record(WindowEventKind::Unhide, 22);
    slots.record(WindowEventKind::Reorder, 22);
    let drained = slots.drain();
    assert_eq!(drained.len(), 2, "one slot per window, never a queue");
    assert_eq!(
        drained[0],
        event(21, &[WindowEventKind::Move, WindowEventKind::Resize])
    );
    assert_eq!(
        drained[1],
        event(22, &[WindowEventKind::Unhide, WindowEventKind::Reorder])
    );
    slots.record(WindowEventKind::Unhide, 22);
    slots.record(WindowEventKind::Hide, 22);
    assert_eq!(slots.drain(), [event(22, &[WindowEventKind::Hide])]);
}

#[test]
fn the_watched_set_never_exceeds_the_overlay_cap() {
    let slots = EventSlots::new(1);
    let many: Vec<(u32, Option<i32>)> = (1..=100).map(|id| (id, None)).collect();
    let watched = slots.watch(&many);
    assert_eq!(watched.len(), MAX_OVERLAYS);
    assert_eq!(slots.counters().capped, 100 - MAX_OVERLAYS as u64);
    slots.record(WindowEventKind::Move, 100);
    assert_eq!(
        slots.counters().unwatched,
        1,
        "an id beyond the cap is not recorded"
    );
    // Replacing the set frees slots; a removed window's pending events are gone.
    slots.record(WindowEventKind::Move, 5);
    let _ = slots.watch(&[(200, None)]);
    assert!(slots.drain().is_empty());
    slots.record(WindowEventKind::Move, 5);
    assert_eq!(slots.counters().unwatched, 2);
}

#[test]
fn close_waits_for_a_callback_in_progress_and_then_refuses() {
    let slots = Arc::new(EventSlots::new(1));
    let _ = slots.watch(&[(31, None)]);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    {
        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
        // The waker runs inside `record`, after the slot was written and with no
        // slot lock held: it is where a callback is held in flight.
        slots.set_waker(Some(Arc::new(move || {
            entered.store(true, Ordering::SeqCst);
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
        })));
    }
    let callback = {
        let slots = Arc::clone(&slots);
        std::thread::spawn(move || slots.record(WindowEventKind::Move, 31))
    };
    wait_until("the held callback", || entered.load(Ordering::SeqCst));
    assert_eq!(slots.in_flight(), 1);
    assert!(
        !slots.close(Duration::from_millis(30)),
        "a callback is still inside"
    );
    release.store(true, Ordering::SeqCst);
    callback.join().unwrap();
    assert!(slots.close(Duration::from_millis(500)));
    slots.record(WindowEventKind::Move, 31);
    assert_eq!(slots.counters().refused, 1, "no record after close");
    assert_eq!(slots.counters().recorded, 1);
}

#[test]
fn the_waker_may_take_locks_of_its_own_while_slots_hold_none() {
    // `record` holds no lock of the slots while it wakes: the waker can call
    // back into the slots (`watch`, `drain`) without deadlocking.
    let slots = Arc::new(EventSlots::new(1));
    let _ = slots.watch(&[(41, None)]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    {
        let (slots_inner, seen) = (Arc::clone(&slots), Arc::clone(&seen));
        slots.set_waker(Some(Arc::new(move || {
            seen.lock().unwrap().extend(slots_inner.drain());
            let _ = slots_inner.watch(&[(41, None)]);
        })));
    }
    slots.record(WindowEventKind::Move, 41);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

// ---------------------------------------------------------------- symbols

#[test]
fn event_ids_map_to_kinds_and_unknown_ids_to_nothing() {
    assert_eq!(kind_of_event(EVENT_WINDOW_MOVE), Some(WindowEventKind::Move));
    for (id, kind) in WINDOW_EVENTS {
        assert_eq!(kind_of_event(id), Some(kind));
    }
    assert_eq!(kind_of_event(1), None);
    let ids: std::collections::BTreeSet<u32> = WINDOW_EVENTS.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), WINDOW_EVENTS.len(), "candidate ids are distinct");
    // Seen natively (2026-11-14): 808 re-stacks, 815 is the order-in and 816
    // the order-out. The last two were first assumed the other way round.
    assert_eq!(kind_of_event(808), Some(WindowEventKind::Reorder));
    assert_eq!(kind_of_event(815), Some(WindowEventKind::Unhide));
    assert_eq!(kind_of_event(816), Some(WindowEventKind::Hide));
}

#[test]
fn the_payload_window_id_needs_four_readable_bytes() {
    let payload: [u32; 2] = [0xABCD, 7];
    let data = payload.as_ptr().cast::<c_void>();
    // SAFETY: `payload` has eight readable bytes.
    unsafe {
        assert_eq!(window_id_from_payload(data, 8), Some(0xABCD));
        assert_eq!(window_id_from_payload(data, 4), Some(0xABCD));
        assert_eq!(window_id_from_payload(data, 3), None, "short payload");
        assert_eq!(window_id_from_payload(std::ptr::null(), 8), None, "null payload");
    }
}

unsafe extern "C" fn dummy_main() -> c_int { 1 }
unsafe extern "C" fn dummy_notify(
    _: c_int,
    _: unsafe extern "C" fn(u32, *mut c_void, usize, *mut c_void, c_int),
    _: u32,
    _: *mut c_void,
) -> c_int {
    0
}

fn lookup_except(missing: &'static str) -> impl Fn(&CStr) -> *mut c_void {
    move |name: &CStr| {
        let text = name.to_str().unwrap();
        if text == missing {
            std::ptr::null_mut()
        } else if text == "SLSMainConnectionID" {
            dummy_main as *const () as *mut c_void
        } else {
            dummy_notify as *const () as *mut c_void
        }
    }
}

#[test]
fn every_missing_symbol_is_named_and_registration_fails_without_registering() {
    assert_eq!(
        WindowSymbols::missing().missing_symbol(),
        Some("SLSMainConnectionID")
    );
    for name in [
        "SLSMainConnectionID",
        "SLSRegisterConnectionNotifyProc",
        "SLSRemoveConnectionNotifyProc",
        "SLSRequestNotificationsForWindows",
        "SLSGetWindowBounds",
    ] {
        // SAFETY: the dummies are never called; the lookup returns null or the
        // address of a function with a matching pointer width.
        let symbols = unsafe { WindowSymbols::from_lookup(lookup_except(name)) };
        assert_eq!(symbols.missing_symbol(), Some(name), "{name}");
        let mut notify = SkyLightWindowNotify::with_symbols(symbols);
        let error = notify.register(Arc::new(EventSlots::new(1))).unwrap_err();
        assert_eq!(error.reason, format!("missing symbol {name}"));
        assert!(
            notify.bounds(1).is_none(),
            "nothing is read without a registration"
        );
        assert!(notify.subscribe(&[1]).is_err());
    }
    // SAFETY: as above.
    let all = unsafe { WindowSymbols::from_lookup(lookup_except("none")) };
    assert_eq!(all.missing_symbol(), None);
}

#[test]
fn the_diagnostic_names_the_reason_and_the_fallback() {
    assert_eq!(
        unavailable_message("missing symbol SLSGetWindowBounds"),
        "borders: live follow unavailable (missing symbol SLSGetWindowBounds); borders hide while a window moves"
    );
    assert_eq!(LiveStatus::Active.diagnostic(), None);
    assert_eq!(LiveStatus::Starting.diagnostic(), None);
    assert!(LiveStatus::Unavailable("x".into()).diagnostic().is_some());
}

// ----------------------------------------------------------- follower core

struct Rig {
    core: FollowCore,
    clocks: FakeFrameClockFactory,
    sink: FakeSink,
    wakes: Arc<std::sync::atomic::AtomicUsize>,
    base: Instant,
    /// Every bounds read: the window id and the millisecond it ran at.
    reads: Vec<(u32, u64)>,
}

impl Rig {
    fn new() -> Self { Self::with_timing(|_| {}) }

    fn with_timing(adjust: impl FnOnce(&mut FollowTiming)) -> Self {
        let clocks = FakeFrameClockFactory::new();
        let sink = FakeSink::new();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let wake: WakeFn = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let mut timing = FollowTiming {
            clock_stop: Duration::from_millis(20),
            ..FollowTiming::default()
        };
        adjust(&mut timing);
        let core = FollowCore::new(
            timing,
            Arc::new(clocks.clone()) as Arc<dyn FrameClockFactory>,
            Arc::new(sink.clone()),
            wake,
        );
        Self {
            core,
            clocks,
            sink,
            wakes,
            base: Instant::now(),
            reads: Vec::new(),
        }
    }

    fn at(&self, millis: u64) -> Instant { self.base + Duration::from_millis(millis) }

    /// An event for `cg` at `millis`; an immediate read answers from `bounds`.
    fn event(
        &mut self,
        millis: u64,
        cg: u32,
        kinds: &[WindowEventKind],
        bounds: &[(u32, OverlayRect)],
    ) {
        let table = bounds.to_vec();
        let mut ran = Vec::new();
        self.core.on_event(self.at(millis), event(cg, kinds), &mut |id| {
            ran.push(id);
            table.iter().find(|(held, _)| *held == id).map(|(_, rect)| *rect)
        });
        self.reads.extend(ran.into_iter().map(|id| (id, millis)));
    }

    /// An event with no bounds known (nothing reads).
    fn quiet_event(&mut self, millis: u64, cg: u32, kinds: &[WindowEventKind]) {
        self.event(millis, cg, kinds, &[]);
    }

    /// A tick of `display` at `millis`, then one follower pass reading `bounds`.
    fn tick(&mut self, display: DisplayId, millis: u64, bounds: &[(u32, OverlayRect)]) {
        self.clocks.fire(display, self.at(millis));
        self.pass(millis, bounds);
    }

    fn pass(&mut self, millis: u64, bounds: &[(u32, OverlayRect)]) {
        let table = bounds.to_vec();
        let mut ran = Vec::new();
        self.core.tick(self.at(millis), &mut |id| {
            ran.push(id);
            table.iter().find(|(held, _)| *held == id).map(|(_, rect)| *rect)
        });
        self.reads.extend(ran.into_iter().map(|id| (id, millis)));
    }

    fn reads_of(&self, cg: u32) -> Vec<u64> {
        self.reads.iter().filter(|(id, _)| *id == cg).map(|(_, at)| *at).collect()
    }
}

#[test]
fn the_first_event_reads_at_once_and_reports_without_waiting_for_a_tick() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, Some(D1))]);
    assert_eq!(rig.clocks.live(), 0, "idle owns no clock");
    rig.event(0, 101, &[WindowEventKind::Move], &[(101, rect(10.0))]);
    assert_eq!(
        rig.clocks.created(),
        vec![D1],
        "the first event starts the clock"
    );
    let batches = rig.sink.batches();
    assert_eq!(batches.len(), 1, "a Move went out before any tick");
    assert_eq!(batches[0][0].bounds, rect(10.0));
    assert_eq!(rig.reads_of(101), vec![0]);
    assert_eq!(rig.core.counters().immediate, 1);
    assert_eq!(rig.wakes.load(Ordering::SeqCst), 0);
}

#[test]
fn further_events_in_the_same_period_coalesce_to_the_next_tick() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, Some(D1))]);
    rig.event(0, 101, &[WindowEventKind::Move], &[(101, rect(10.0))]);
    // Many events inside the period cost no read at all.
    for millis in 1..16 {
        rig.event(millis, 101, &[WindowEventKind::Resize], &[(101, rect(99.0))]);
    }
    assert_eq!(rig.reads_of(101), vec![0]);
    assert_eq!(rig.sink.batches().len(), 1);
    // A tick inside the period of the immediate read waits for that period's end
    // (16.667 ms), it does not read twice.
    rig.tick(D1, 16, &[(101, rect(40.0))]);
    assert_eq!(rig.reads_of(101), vec![0]);
    let due = rig.core.next_deadline(rig.at(16)).expect("held until the period ends");
    assert_eq!(due, rig.at(0) + Duration::from_micros(16_667));
    rig.pass(17, &[(101, rect(40.0))]);
    assert_eq!(rig.reads_of(101), vec![0, 17]);
    let batches = rig.sink.batches();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[1][0].bounds, rect(40.0), "the latest bounds");
    // A pass without a tick or a held window reads nothing.
    rig.pass(20, &[(101, rect(99.0))]);
    assert_eq!(rig.sink.batches().len(), 2);
}

#[test]
fn a_window_is_never_read_twice_within_one_display_period() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, Some(D1)), target(2, Some(D1))]);
    let table = [(101, rect(1.0)), (102, rect(2.0))];
    // Events every millisecond on both windows, display ticks every 17 ms.
    for millis in 0..=90 {
        rig.event(millis, 101, &[WindowEventKind::Move], &table);
        rig.event(millis, 102, &[WindowEventKind::Resize], &table);
        if millis > 0 && millis % 17 == 0 {
            rig.clocks.fire(D1, rig.at(millis));
        }
        rig.pass(millis, &table);
    }
    for cg in [101, 102] {
        let reads = rig.reads_of(cg);
        assert!(reads.len() >= 5, "{cg} kept following: {reads:?}");
        for pair in reads.windows(2) {
            assert!(
                Duration::from_millis(pair[1] - pair[0]) >= Duration::from_micros(16_667),
                "{cg} read twice inside one period: {reads:?}"
            );
        }
    }
}

#[test]
fn two_moving_windows_share_one_batch_per_tick() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[
        target(1, Some(D1)),
        target(2, Some(D1)),
        target(3, Some(D2)),
    ]);
    let table = [(101, rect(1.0)), (102, rect(2.0))];
    rig.event(0, 101, &[WindowEventKind::Resize], &table);
    rig.event(0, 102, &[WindowEventKind::Move], &table);
    assert_eq!(rig.clocks.live(), 1, "one clock per display");
    // Each window's first event was read at once, so the tick after the period
    // reads both together in ONE coalesced batch.
    let before = rig.sink.batches().len();
    rig.tick(D1, 17, &table);
    let batches = rig.sink.batches();
    assert_eq!(batches.len(), before + 1, "ONE coalesced batch");
    assert_eq!(batches.last().unwrap().len(), 2);
    // A window on another display starts its own clock.
    rig.quiet_event(20, 103, &[WindowEventKind::Move]);
    assert_eq!(rig.clocks.live(), 2);
}

#[test]
fn the_clock_stops_after_the_quiet_period_with_a_final_read() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, Some(D1))]);
    rig.event(0, 101, &[WindowEventKind::Move], &[(101, rect(4.0))]);
    rig.tick(D1, 17, &[(101, rect(5.0))]);
    assert_eq!(rig.core.next_deadline(rig.at(17)), Some(rig.at(100)));
    // Still inside the quiet period: the clock keeps running.
    rig.pass(99, &[(101, rect(6.0))]);
    assert_eq!(rig.clocks.live(), 1);
    // Quiet elapsed: one last read lands the border, then the clock stops.
    rig.pass(100, &[(101, rect(7.0))]);
    let batches = rig.sink.batches();
    assert_eq!(batches.last().unwrap()[0].bounds, rect(7.0));
    assert_eq!(rig.clocks.live(), 0, "idle owns no clock");
    assert_eq!(rig.clocks.stopped(), vec![D1]);
    assert_eq!(rig.core.next_deadline(rig.at(101)), None);
    assert!(rig.sink.calls().contains(&SinkCall::Following(window(1), false)));
    // Another drag starts a fresh clock and reads at once again.
    rig.event(500, 101, &[WindowEventKind::Move], &[(101, rect(8.0))]);
    assert_eq!(rig.clocks.created(), vec![D1, D1]);
    assert_eq!(rig.reads_of(101).last(), Some(&500));
}

#[test]
fn a_window_owned_by_tiling_is_not_read() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, Some(D1))]);
    rig.sink.set_busy(window(1), true);
    rig.event(0, 101, &[WindowEventKind::Move], &[(101, rect(5.0))]);
    rig.tick(D1, 17, &[(101, rect(5.0))]);
    assert!(rig.sink.batches().is_empty());
    assert_eq!(rig.core.counters().reads, 0);
    assert_eq!(rig.core.counters().deferred, 2);
    rig.sink.set_busy(window(1), false);
    rig.tick(D1, 34, &[(101, rect(8.0))]);
    assert_eq!(rig.sink.batches().len(), 1);
}

/// Native run 2: the candidate hide event fired after nearly every show and hid
/// the border. Hide, unhide and close are not trusted; move, resize and (for
/// `above` borders, decided by Tiling) reorder act.
#[test]
fn private_hide_unhide_and_close_are_ignored_by_default_and_reorder_is_forwarded() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, Some(D1)), target(2, Some(D1))]);
    rig.quiet_event(0, 101, &[WindowEventKind::Hide]);
    rig.quiet_event(0, 101, &[WindowEventKind::Unhide]);
    rig.quiet_event(0, 101, &[WindowEventKind::Reorder]);
    rig.quiet_event(1, 102, &[WindowEventKind::Close]);
    assert_eq!(
        rig.sink.calls(),
        vec![SinkCall::Reorder(window(1))],
        "only the reorder reaches the sink"
    );
    assert_eq!(rig.clocks.live(), 0, "none of these starts a clock");
    assert_eq!(rig.core.counters().ignored, 3);
    // A close does not forget the window: its moves are still followed, and
    // Tiling's destroy path releases the border.
    rig.event(2, 102, &[WindowEventKind::Move], &[(102, rect(3.0))]);
    assert_eq!(rig.core.counters().unknown, 0);
    assert_eq!(rig.sink.batches().len(), 1);
    // A hide combined with a move still follows the move.
    rig.event(3, 101, &[WindowEventKind::Hide, WindowEventKind::Move], &[(
        101,
        rect(4.0),
    )]);
    assert!(!rig.sink.calls().contains(&SinkCall::Hide(window(1))));
    assert_eq!(rig.sink.batches().len(), 2);
}

/// Each unverified event can be enabled alone by a later verified run.
#[test]
fn each_unverified_event_acts_only_behind_its_own_flag() {
    let mut rig = Rig::with_timing(|timing| {
        timing.act_on_hide = true;
        timing.act_on_reorder = false;
    });
    rig.core.set_targets(&[target(1, Some(D1))]);
    rig.quiet_event(0, 101, &[WindowEventKind::Hide]);
    rig.quiet_event(0, 101, &[WindowEventKind::Unhide]);
    rig.quiet_event(0, 101, &[WindowEventKind::Reorder]);
    assert_eq!(rig.sink.calls(), vec![SinkCall::Hide(window(1))]);

    let mut rig = Rig::with_timing(|timing| {
        timing.act_on_unhide = true;
        timing.act_on_reorder = true;
        timing.act_on_close = true;
    });
    rig.core.set_targets(&[target(1, Some(D1)), target(2, Some(D1))]);
    rig.quiet_event(0, 101, &[WindowEventKind::Hide]);
    rig.quiet_event(0, 101, &[WindowEventKind::Unhide]);
    rig.quiet_event(0, 101, &[WindowEventKind::Reorder]);
    rig.quiet_event(1, 102, &[WindowEventKind::Move]);
    rig.quiet_event(2, 102, &[WindowEventKind::Close]);
    assert!(rig.sink.calls().contains(&SinkCall::Unhide(window(1))));
    assert!(rig.sink.calls().contains(&SinkCall::Reorder(window(1))));
    assert!(rig.sink.calls().contains(&SinkCall::Close(window(2))));
    assert!(!rig.sink.calls().contains(&SinkCall::Hide(window(1))));
    assert_eq!(rig.clocks.live(), 0, "a closed window stops its clock");
    rig.quiet_event(3, 102, &[WindowEventKind::Move]);
    assert_eq!(rig.core.counters().unknown, 1);
}

#[test]
fn a_window_without_a_display_is_left_to_the_fallback() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, None)]);
    rig.quiet_event(0, 101, &[WindowEventKind::Move]);
    assert_eq!(rig.clocks.attempts(), vec![], "no clock for an unknown display");
    assert_eq!(rig.core.counters().no_display, 1);
}

#[test]
fn a_display_whose_clock_cannot_be_created_is_paced_by_the_timer() {
    let mut rig = Rig::new();
    rig.clocks.fail_creation(D1, "no link");
    rig.core.set_targets(&[target(1, Some(D1))]);
    rig.event(0, 101, &[WindowEventKind::Move], &[(101, rect(1.0))]);
    assert_eq!(rig.core.counters().clock_failures, 1);
    assert_eq!(rig.clocks.live(), 0);
    assert!(rig.core.next_deadline(rig.at(0)).is_some());
    assert_eq!(rig.sink.batches().len(), 1, "the first event read at once");
    rig.pass(5, &[(101, rect(2.0))]); // before the next pace: no read
    rig.pass(17, &[(101, rect(3.0))]);
    let batches = rig.sink.batches();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[1][0].bounds, rect(3.0));
}

#[test]
fn a_clock_with_a_held_callback_is_retained_not_dropped_and_stops_on_retry() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[target(1, Some(D1))]);
    rig.quiet_event(0, 101, &[WindowEventKind::Move]);
    let held = rig.clocks.fire_held(D1, Instant::now()).unwrap();
    held.wait_entered();
    assert!(
        !rig.core.shutdown(),
        "a callback is inside the clock: it cannot stop"
    );
    assert_eq!(rig.core.stuck_clocks(), 1, "retained and reported");
    assert_eq!(rig.clocks.live(), 1, "never released while a callback runs");
    assert!(rig.clocks.stopped().is_empty());
    held.release();
    rig.pass(10, &[]);
    assert_eq!(rig.core.stuck_clocks(), 0, "the retry stops it");
    assert_eq!(rig.clocks.live(), 0);
    assert_eq!(rig.clocks.stopped(), vec![D1]);
}

// ------------------------------------------------------------- owner thread

struct Live {
    follow: SystemLiveFollow,
    session: FakeSessionControl,
    clocks: FakeFrameClockFactory,
    sink: FakeSink,
}

fn live(quiet_millis: u64) -> Live {
    let session = FakeSessionControl::new();
    let clocks = FakeFrameClockFactory::new();
    let sink = FakeSink::new();
    let follow = SystemLiveFollow::new(
        session.maker(),
        Arc::new(clocks.clone()) as Arc<dyn FrameClockFactory>,
        None,
    )
    .with_timing(FollowTiming {
        quiet: Duration::from_millis(quiet_millis),
        clock_stop: Duration::from_millis(30),
        ..FollowTiming::default()
    })
    .with_budget(Duration::from_secs(2));
    Live { follow, session, clocks, sink }
}

fn start(live: &mut Live) {
    live.follow.start(Arc::new(live.sink.clone()));
    wait_until("registration", || live.follow.status() != LiveStatus::Starting);
}

#[test]
fn the_owner_thread_registers_pumps_and_stops_cleanly() {
    let mut live = live(60_000);
    assert_eq!(live.session.created(), 0, "nothing exists before start");
    start(&mut live);
    assert_eq!(live.follow.status(), LiveStatus::Active);
    assert!(live.session.registered());
    live.follow.set_targets(&[target(1, Some(D1))]);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert_eq!(live.session.subscriptions()[0], [101]);
    wait_until("a pump", || !live.session.pump_threads().is_empty());
    assert!(
        live.session.pump_threads().iter().all(|name| name == OWNER_THREAD_NAME),
        "the run loop is only ever pumped on the owner thread"
    );
    assert_eq!(
        live.follow.stop_bounded(Duration::from_secs(2)),
        StopOutcome::Stopped
    );
    assert!(!live.session.registered(), "registrations removed");
    assert!(
        !live.session.inject(WindowEventKind::Move, 101),
        "no callback after stop"
    );
    assert_eq!(
        live.follow.stop_bounded(Duration::from_secs(1)),
        StopOutcome::NotRunning
    );
}

#[test]
fn own_foreign_and_unknown_events_never_reach_the_follower() {
    let mut live = live(60_000);
    start(&mut live);
    let own_pid = i32::try_from(std::process::id()).unwrap();
    let own = FollowTarget {
        owner_pid: Some(own_pid),
        ..target(2, Some(D1))
    };
    live.follow.set_targets(&[target(1, Some(D1)), own]);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert_eq!(
        live.session.subscriptions()[0],
        [101],
        "Stache's own window is not subscribed"
    );
    assert!(live.session.inject(WindowEventKind::Move, 102), "own window");
    assert!(live.session.inject(WindowEventKind::Move, 555), "foreign window");
    assert!(live.session.inject(WindowEventKind::Move, 0), "unknown id");
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(live.clocks.created(), vec![], "none of them starts a clock");
    assert_eq!(live.follow.slot_counters().unwatched, 3);
    assert_eq!(live.follow.slot_counters().own_dropped, 1);
    live.follow.stop();
}

#[test]
fn the_subscription_never_exceeds_the_overlay_cap() {
    let mut live = live(60_000);
    start(&mut live);
    let many: Vec<FollowTarget> = (1..=100).map(|n| target(n, Some(D1))).collect();
    live.follow.set_targets(&many);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert!(live.session.subscriptions().iter().all(|set| set.len() <= MAX_OVERLAYS));
    live.follow.stop();
}

#[test]
fn a_drag_ticks_the_clock_reads_once_per_tick_and_goes_idle() {
    let mut live = live(40);
    start(&mut live);
    live.session.set_bounds(101, rect(11.0));
    live.follow.set_targets(&[target(1, Some(D1))]);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert!(live.session.inject(WindowEventKind::Move, 101));
    wait_until("the clock", || live.clocks.is_live(D1));
    live.clocks.fire(D1, Instant::now());
    wait_until("a batch", || !live.sink.batches().is_empty());
    assert_eq!(live.sink.batches()[0][0].bounds, rect(11.0));
    assert_eq!(
        live.session.bounds_reads().iter().filter(|id| **id == 101).count(),
        1
    );
    // No more events: the quiet period passes and the owner stops the clock.
    wait_until("the clock to stop", || live.clocks.live() == 0);
    assert_eq!(live.clocks.stopped(), vec![D1]);
    live.follow.stop();
}

#[test]
fn a_missing_symbol_or_refused_registration_is_unavailable_with_the_reason() {
    for reason in [
        "missing symbol SLSGetWindowBounds",
        "registering window event 806 failed",
    ] {
        let mut live = live(60_000);
        live.session.fail_register(reason);
        start(&mut live);
        assert_eq!(live.follow.status(), LiveStatus::Unavailable(reason.to_owned()));
        assert_eq!(
            live.follow.status().diagnostic().unwrap(),
            unavailable_message(reason)
        );
        live.follow.set_targets(&[target(1, Some(D1))]);
        std::thread::sleep(Duration::from_millis(20));
        assert!(live.session.subscriptions().is_empty(), "nothing is subscribed");
        assert!(
            !live.session.inject(WindowEventKind::Move, 101),
            "nothing is registered"
        );
        assert_eq!(live.clocks.created(), vec![]);
        assert_eq!(
            live.follow.stop_bounded(Duration::from_secs(2)),
            StopOutcome::Stopped
        );
    }
}

#[test]
fn a_refused_subscription_makes_live_follow_unavailable_and_stops_following() {
    let mut live = live(60_000);
    start(&mut live);
    live.session.fail_subscribe("refused (code 1)");
    live.follow.set_targets(&[target(1, Some(D1))]);
    wait_until("unavailable", || {
        matches!(live.follow.status(), LiveStatus::Unavailable(_))
    });
    assert!(
        !live.session.inject(WindowEventKind::Move, 101),
        "registrations removed"
    );
    live.follow.stop();
}

#[test]
fn a_stop_with_a_held_clock_callback_retains_the_stuck_clock_and_reports_it() {
    let mut live = live(60_000);
    start(&mut live);
    live.follow.set_targets(&[target(1, Some(D1))]);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert!(live.session.inject(WindowEventKind::Move, 101));
    wait_until("the clock", || live.clocks.is_live(D1));
    let held = live.clocks.fire_held(D1, Instant::now()).unwrap();
    held.wait_entered();
    let outcome = live.follow.stop_bounded(Duration::from_secs(2));
    assert_eq!(
        outcome,
        StopOutcome::TimedOut {
            owner_joined: true,
            callbacks_drained: false
        },
        "the owner exited but a clock callback may still run"
    );
    assert_eq!(live.follow.stuck_clocks(), 1, "retained, never dropped");
    assert_eq!(live.clocks.live(), 1, "never released while a callback runs");
    held.release();
    assert_eq!(
        live.follow.stop_bounded(Duration::from_secs(2)),
        StopOutcome::Stopped
    );
    assert_eq!(live.follow.stuck_clocks(), 0);
    assert_eq!(live.clocks.live(), 0);
}

#[test]
fn the_fake_live_follow_records_what_the_engine_pushes() {
    let fake = FakeLiveFollow::active();
    let mut boxed: Box<dyn LiveFollow> = Box::new(fake.clone());
    boxed.start(Arc::new(FakeSink::new()));
    boxed.set_targets(&[target(1, Some(D1))]);
    boxed.stop();
    assert_eq!((fake.started(), fake.stopped()), (1, 1));
    assert_eq!(fake.target_sets().len(), 1);
    assert!(fake.sink().is_none());
}

/// A sink whose `authority` blocks on the owner thread while `hold` is set.
#[derive(Clone, Default)]
struct HoldSink {
    hold: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
}

impl FollowSink for HoldSink {
    fn authority(&self, _window: WindowInstance) -> Option<u64> {
        self.entered.store(true, Ordering::SeqCst);
        while self.hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    fn moves(&self, _moves: &[FollowMove]) {}

    fn following(&self, _window: WindowInstance, _following: bool) {}

    fn hide(&self, _window: WindowInstance) {}

    fn unhide(&self, _window: WindowInstance) {}

    fn reorder(&self, _window: WindowInstance) {}

    fn close(&self, _window: WindowInstance) {}
}

/// Dropping the follower while its owner thread is held inside a callback: the
/// stop times out, the observer is told, and the owner is forgotten explicitly
/// (counted), never freed under the running callback. Released, it exits.
#[test]
fn a_drop_whose_stop_times_out_forgets_the_owner_explicitly_and_reports_it() {
    let session = FakeSessionControl::new();
    let clocks = FakeFrameClockFactory::new();
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let observer: StopObserver = {
        let outcomes = Arc::clone(&outcomes);
        Arc::new(move |outcome| outcomes.lock().unwrap().push(outcome))
    };
    let mut follow = SystemLiveFollow::new(
        session.maker(),
        Arc::new(clocks) as Arc<dyn FrameClockFactory>,
        Some(observer),
    )
    .with_budget(Duration::from_millis(50));
    let sink = HoldSink::default();
    sink.hold.store(true, Ordering::SeqCst);
    follow.start(Arc::new(sink.clone()));
    wait_until("registration", || follow.status() != LiveStatus::Starting);
    follow.set_targets(&[target(1, Some(D1))]);
    wait_until("subscription", || !session.subscriptions().is_empty());
    assert!(session.inject(WindowEventKind::Move, 101));
    wait_until("held in the callback", || sink.entered.load(Ordering::SeqCst));
    let before = forgotten_follow_owners();
    drop(follow);
    assert!(
        matches!(
            outcomes.lock().unwrap().last(),
            Some(StopOutcome::TimedOut { owner_joined: false, .. })
        ),
        "{outcomes:?}"
    );
    assert_eq!(forgotten_follow_owners(), before + 1);
    sink.hold.store(false, Ordering::SeqCst);
    wait_until("the owner leaves", || !session.registered());
}

// ------------------------------------------------------- raw event logging

#[test]
fn raw_events_of_watched_windows_are_logged_in_order_including_unknown_ids() {
    let slots = EventSlots::new(500);
    let _ = slots.watch(&[(11, Some(900)), (12, Some(500))]);
    slots.on_window_event(808, 11);
    slots.on_window_event(823, 11); // logging-only candidate
    slots.on_window_event(806, 11);
    slots.on_window_event(808, 12); // own panel: never watched
    slots.on_window_event(808, 77); // not bordered
    assert_eq!(slots.drain_raw(), [
        super::RawEvent { event: 808, cg_window_id: 11 },
        super::RawEvent { event: 823, cg_window_id: 11 },
        super::RawEvent { event: 806, cg_window_id: 11 },
    ]);
    assert!(slots.drain_raw().is_empty(), "a drain takes the log");
    // Known ids still set their kind bit; the unknown one sets none.
    assert_eq!(slots.drain(), [event(11, &[
        WindowEventKind::Reorder,
        WindowEventKind::Move
    ])]);
}

#[test]
fn the_raw_log_is_bounded_and_counts_what_it_drops() {
    let slots = EventSlots::new(1);
    let _ = slots.watch(&[(11, None)]);
    for _ in 0..super::RAW_CAPACITY + 5 {
        slots.on_window_event(825, 11);
    }
    assert_eq!(slots.drain_raw().len(), super::RAW_CAPACITY);
    assert_eq!(slots.counters().raw_dropped, 5);
}

#[test]
fn logging_only_ids_cover_800_to_830_except_the_known_ones() {
    let extra = super::skylight::logging_only_events();
    assert_eq!(extra.len(), 31 - WINDOW_EVENTS.len());
    for (id, _) in WINDOW_EVENTS {
        assert!(!extra.contains(&id));
    }
    assert!(extra.iter().all(|id| (800..=830).contains(id)));
}

#[test]
fn the_owner_hands_raw_events_to_the_sink_in_order_on_its_thread() {
    let mut live = live(60_000);
    start(&mut live);
    live.follow.set_targets(&[target(1, Some(D1))]);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert!(live.session.inject_raw(808, 101));
    assert!(live.session.inject_raw(820, 101));
    assert!(live.session.inject_raw(808, 555), "foreign window");
    wait_until("raw events", || {
        live.sink
            .calls()
            .iter()
            .filter_map(|call| match call {
                SinkCall::Raw(raw) => Some(raw.clone()),
                _ => None,
            })
            .flatten()
            .count()
            >= 2
    });
    let raw: Vec<(u32, u32)> = live
        .sink
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            SinkCall::Raw(raw) => Some(raw),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(raw, [(808, 101), (820, 101)]);
    assert!(live.follow.follow_counters().raw >= 2);
    live.follow.stop();
}

// ------------------------------------------------------------ fast reorder

const PANEL: u32 = 70_001;

fn above_target(n: u64) -> FollowTarget {
    FollowTarget {
        overlay_window: Some(PANEL + u32::try_from(n).unwrap() - 1),
        above: true,
        fast_reorder: true,
        ..target(n, Some(D1))
    }
}

/// Drives one `Reorder` through the core with a recording order call that
/// also notes how many sink calls existed at the time of the call.
fn fast_event(
    rig: &mut Rig,
    ms: u64,
    cg: u32,
    result: FastOrderResult,
    orders: &mut Vec<(u32, u32, usize)>,
) {
    let sink = rig.sink.clone();
    let now = rig.base + Duration::from_millis(ms);
    rig.core.on_event_ordered(
        now,
        event(cg, &[WindowEventKind::Reorder]),
        &mut |_| None,
        Some(&mut |panel, target| {
            orders.push((panel, target, sink.calls().len()));
            result
        }),
    );
}

fn fast_reports(sink: &FakeSink) -> Vec<FastReorder> {
    sink.calls()
        .into_iter()
        .filter_map(|call| match call {
            SinkCall::FastReorder(report) => Some(report),
            _ => None,
        })
        .collect()
}

#[test]
fn a_reorder_of_a_shown_above_target_orders_its_panel_before_the_sink_note() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[above_target(1)]);
    let mut orders = Vec::new();
    fast_event(&mut rig, 0, 101, FastOrderResult::Ordered, &mut orders);
    assert_eq!(orders, [(PANEL, 101, 0)], "one call, before any sink call");
    assert_eq!(rig.sink.calls(), vec![
        SinkCall::FastReorder(FastReorder {
            cg_window_id: 101,
            panel: PANEL,
            result: FastOrderResult::Ordered,
            disabled: false,
        }),
        SinkCall::Reorder(window(1)),
    ]);
    assert_eq!(rig.core.counters().fast_orders, 1);
}

#[test]
fn below_hidden_suspended_or_unknown_panels_get_no_fast_order() {
    let below = FollowTarget {
        above: false,
        ..above_target(1)
    };
    let hidden = FollowTarget {
        fast_reorder: false,
        ..above_target(2)
    };
    let unknown = FollowTarget {
        overlay_window: None,
        ..above_target(3)
    };
    let own_panel = FollowTarget {
        overlay_window: Some(104),
        ..above_target(4)
    };
    let mut rig = Rig::new();
    rig.core.set_targets(&[below, hidden, unknown, own_panel]);
    let mut orders = Vec::new();
    for cg in 101..=104 {
        fast_event(&mut rig, 0, cg, FastOrderResult::Ordered, &mut orders);
    }
    assert!(orders.is_empty(), "no fast call: {orders:?}");
    assert_eq!(
        rig.sink.calls().iter().filter(|c| matches!(c, SinkCall::Reorder(_))).count(),
        4,
        "the main-thread path is still notified"
    );
    // Without an order function (plain `on_event`) nothing is fast either.
    rig.core.set_targets(&[above_target(1)]);
    rig.quiet_event(0, 101, &[WindowEventKind::Reorder]);
    assert_eq!(rig.core.counters().fast_orders, 0);
}

#[test]
fn fast_reorders_are_capped_per_window_per_second() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[above_target(1), above_target(2)]);
    let mut orders = Vec::new();
    for ms in 0..6 {
        fast_event(&mut rig, ms * 10, 101, FastOrderResult::Ordered, &mut orders);
    }
    fast_event(&mut rig, 70, 102, FastOrderResult::Ordered, &mut orders);
    assert_eq!(
        orders.len(),
        FAST_REORDER_CAP + 1,
        "the other window is not capped"
    );
    assert_eq!(rig.core.counters().fast_capped, 2);
    fast_event(&mut rig, 1_000, 101, FastOrderResult::Ordered, &mut orders);
    assert_eq!(orders.len(), FAST_REORDER_CAP + 2, "the window slides");
    assert_eq!(
        rig.sink.calls().iter().filter(|c| matches!(c, SinkCall::Reorder(_))).count(),
        8,
        "every reorder still reaches the sink"
    );
}

#[test]
fn a_missing_symbol_or_refused_call_disables_the_fast_path_with_one_report() {
    for failure in [
        FastOrderResult::Missing("SLSOrderWindow"),
        FastOrderResult::Refused(1000),
    ] {
        let mut rig = Rig::new();
        rig.core.set_targets(&[above_target(1), above_target(2)]);
        let mut orders = Vec::new();
        fast_event(&mut rig, 0, 101, failure, &mut orders);
        fast_event(&mut rig, 10, 102, FastOrderResult::Ordered, &mut orders);
        fast_event(&mut rig, 20, 101, FastOrderResult::Ordered, &mut orders);
        assert_eq!(orders.len(), 1, "disabled after the first failure");
        let reports = fast_reports(&rig.sink);
        assert_eq!(reports.len(), 1, "one diagnostic");
        assert!(reports[0].disabled);
        assert_eq!(reports[0].result, failure);
        let counters = rig.core.counters();
        assert!(counters.fast_disabled);
        assert_eq!(counters.fast_failures, 1);
        assert_eq!(
            rig.sink.calls().iter().filter(|c| matches!(c, SinkCall::Reorder(_))).count(),
            3,
            "the main-thread path is kept"
        );
    }
}

#[test]
fn a_panel_the_main_thread_records_as_not_shown_is_skipped_without_disabling() {
    let mut rig = Rig::new();
    rig.core.set_targets(&[above_target(1)]);
    let mut orders = Vec::new();
    fast_event(&mut rig, 0, 101, FastOrderResult::NotShown, &mut orders);
    fast_event(&mut rig, 10, 101, FastOrderResult::Ordered, &mut orders);
    assert_eq!(orders.len(), 2);
    let counters = rig.core.counters();
    assert_eq!((counters.fast_not_shown, counters.fast_orders), (1, 1));
    assert!(!counters.fast_disabled);
}

#[test]
fn the_owner_thread_makes_the_fast_order_from_the_targets_update() {
    let mut live = live(60_000);
    start(&mut live);
    let own_pid = i32::try_from(std::process::id()).unwrap();
    let own = FollowTarget {
        owner_pid: Some(own_pid),
        ..above_target(2)
    };
    let below = FollowTarget {
        above: false,
        ..above_target(3)
    };
    live.follow.set_targets(&[above_target(1), own, below]);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert!(live.session.inject_raw(808, 101));
    assert!(live.session.inject_raw(808, 102), "own window: never watched");
    assert!(live.session.inject_raw(808, 103));
    wait_until("both reorders", || {
        live.sink.calls().iter().filter(|c| matches!(c, SinkCall::Reorder(_))).count() >= 2
    });
    assert_eq!(live.session.orders(), [(PANEL, 101)]);
    assert_eq!(live.session.order_threads(), [OWNER_THREAD_NAME]);
    let calls = live.sink.calls();
    let fast = calls.iter().position(|c| matches!(c, SinkCall::FastReorder(_))).unwrap();
    let note = calls.iter().position(|c| *c == SinkCall::Reorder(window(1))).unwrap();
    assert!(fast < note, "the fast order precedes the route note");
    assert_eq!(live.follow.follow_counters().fast_orders, 1);
    live.follow.stop();
}

#[test]
fn the_owner_disables_the_fast_path_when_the_session_has_no_order_symbol() {
    let mut live = live(60_000);
    start(&mut live);
    live.session.set_order_result(FastOrderResult::Missing("SLSOrderWindow"));
    live.follow.set_targets(&[above_target(1)]);
    wait_until("subscription", || !live.session.subscriptions().is_empty());
    assert!(live.session.inject_raw(808, 101));
    wait_until("the reorder", || {
        live.sink.calls().contains(&SinkCall::Reorder(window(1)))
    });
    assert!(live.session.inject_raw(808, 101));
    wait_until("the second reorder", || {
        live.sink.calls().iter().filter(|c| matches!(c, SinkCall::Reorder(_))).count() >= 2
    });
    assert_eq!(live.session.orders().len(), 1);
    assert_eq!(fast_reports(&live.sink).len(), 1);
    assert!(live.follow.follow_counters().fast_disabled);
    live.follow.stop();
}

static ORDER_LOOKUPS: AtomicBool = AtomicBool::new(false);

fn order_lookup_missing(_: &CStr) -> *mut c_void {
    ORDER_LOOKUPS.store(true, Ordering::SeqCst);
    std::ptr::null_mut()
}

#[test]
fn the_native_fast_order_needs_a_registered_connection_and_resolves_lazily() {
    let mut notify =
        SkyLightWindowNotify::with_order_lookup(WindowSymbols::missing(), order_lookup_missing);
    assert_eq!(
        notify.order_above(1, 2),
        FastOrderResult::Missing("SLSMainConnectionID"),
        "never registered: no connection, nothing looked up or called"
    );
    assert!(!ORDER_LOOKUPS.load(Ordering::SeqCst));
}
