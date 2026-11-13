//! Fake-driven proof for the window adapter. No real AX, run loop effect on
//! other applications, event tap or desktop effect is involved.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::adapter::run_owner;
use super::commands::{Command, Reply};
use super::core::{Core, Turn, classify_frame_write};
use super::driver::{Driver, RunLoopDriver, Wakeup};
use super::testing::{FakeObservers, FakeWindowSystem, ScriptedDriver, SteppingClock};
use super::*;
use crate::applications::{ACKNOWLEDGEMENT_BUDGET, ApplicationId, CANCELLATION_QUANTUM};
use crate::ax::testing::{Call, FakeNode, FakeWindowAx, NodeId};
use crate::ax::{
    AxError, AxNotification, AxPoint, AxReading, AxSize, BoolAttribute, ELEMENT_TIMEOUT,
    PRESS_TIMEOUT, WindowString, WriteStatus,
};

// ---- fixtures --------------------------------------------------------------

fn window_node(ax: &FakeWindowAx, cg: u32) -> NodeId {
    let mut node = FakeNode {
        position: Ok(AxPoint { x: 10.0, y: 20.0 }),
        size: Ok(AxSize { width: 300.0, height: 200.0 }),
        position_settable: Ok(true),
        size_settable: Ok(true),
        window_id: Ok(cg),
        ..FakeNode::default()
    };
    node.strings.insert(WindowString::Title, Ok("w".to_owned()));
    node.strings.insert(WindowString::Role, Ok("AXWindow".to_owned()));
    node.strings.insert(WindowString::Subrole, Ok("AXStandardWindow".to_owned()));
    node.bools.insert(BoolAttribute::Minimized, Ok(false));
    node.bools.insert(BoolAttribute::FullScreen, Ok(false));
    ax.add_node(node)
}

fn app_node(ax: &FakeWindowAx, pid: i32, windows: Vec<NodeId>) -> NodeId {
    let node = ax.add_node(FakeNode {
        windows: Ok(windows),
        ..FakeNode::default()
    });
    ax.set_application(pid, node);
    node
}

fn app(token: u64) -> ApplicationId { ApplicationId::for_testing(token) }

struct H {
    core: Core<FakeWindowSystem>,
    tx: SyncSender<Command>,
    rx: Receiver<Command>,
    ax: FakeWindowAx,
    obs: FakeObservers,
    clock: Arc<SteppingClock>,
    queue: Arc<EventQueue>,
    gate: Arc<GenerationCell>,
    adapter: AdapterId,
    epoch: RuntimeEpoch,
}

type Slot<T> = Arc<Mutex<Option<T>>>;

impl H {
    fn with_queue(capacity: usize, read_tabs: bool) -> Self {
        Self::with_options(capacity, read_tabs, false)
    }

    fn with_options(capacity: usize, read_tabs: bool, tab_bars: bool) -> Self {
        let ax = FakeWindowAx::new();
        let (system, obs) = FakeWindowSystem::new(ax.clone());
        let clock = SteppingClock::new(Duration::ZERO);
        let adapter = AdapterId::allocate();
        let core = Core::new(system, adapter, clock.clone(), read_tabs).with_tab_bars(tab_bars);
        let (tx, rx) = mpsc::sync_channel(64);
        let gate = Arc::new(GenerationCell::new());
        gate.publish(1, 1);
        let mut harness = Self {
            core,
            tx,
            rx,
            ax,
            obs,
            clock,
            queue: EventQueue::new(capacity),
            gate,
            adapter,
            epoch: RuntimeEpoch::new(0),
        };
        harness.epoch = harness.install().expect("install");
        harness
    }

    fn new() -> Self { Self::with_queue(1024, false) }

    fn install(&mut self) -> Result<RuntimeEpoch, InstallError> {
        let request = InstallRequest {
            queue: Arc::clone(&self.queue),
            gate: Arc::clone(&self.gate),
        };
        self.run(|reply| Command::Install { request, reply })
    }

    fn send<T: Send + 'static>(&self, build: impl FnOnce(Reply<T>) -> Command) -> Slot<T> {
        let slot: Slot<T> = Arc::new(Mutex::new(None));
        let writer = Arc::clone(&slot);
        let command = build(Box::new(move |value| {
            *writer.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
        }));
        self.tx.try_send(command).expect("queue");
        slot
    }

    fn turn(&mut self) -> Turn { self.core.turn(&self.rx) }

    fn run<T: Send + 'static>(&mut self, build: impl FnOnce(Reply<T>) -> Command) -> T {
        let slot = self.send(build);
        for _ in 0..8 {
            self.turn();
            let reply = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(value) = reply {
                return value;
            }
        }
        panic!("no reply");
    }

    fn settle(&mut self) {
        for _ in 0..500 {
            if !self.turn().active {
                return;
            }
        }
        panic!("never settled");
    }

    fn events(&self) -> Vec<WindowEvent> { self.queue.drain().events }

    fn track(&mut self, application: ApplicationId, pid: i32) -> TrackOutcome {
        let epoch = self.epoch;
        let request = TrackRequest {
            application,
            pid,
            cancel: CancellationToken::new(),
            deadline: None,
        };
        self.run(|reply| Command::Track { epoch, request, reply })
    }

    /// Creates an application with `count` windows (cg ids 100..), tracks it
    /// and settles; returns its node and the tokens in `AXWindows` order.
    fn app_with_windows(
        &mut self,
        application: ApplicationId,
        pid: i32,
        count: u32,
    ) -> (NodeId, Vec<NodeId>, Vec<WindowInstance>) {
        let nodes: Vec<NodeId> = (0..count).map(|n| window_node(&self.ax, 100 + n)).collect();
        let node = app_node(&self.ax, pid, nodes.clone());
        assert_eq!(self.track(application, pid), TrackOutcome::Tracking);
        self.settle();
        let tokens = snapshots(&self.events()).iter().map(|s| s.window).collect();
        (node, nodes, tokens)
    }

    fn issuance_at(window: WindowInstance, generation: u64, deadline: Instant) -> WindowIssuance {
        WindowIssuance::new(window, generation, 1, 1, deadline)
    }

    fn issuance(&self, window: WindowInstance) -> WindowIssuance {
        Self::issuance_at(window, 1, self.clock.peek() + Duration::from_secs(30))
    }

    fn act_with(&mut self, issuance: WindowIssuance, action: WindowAction) -> WindowActionOutcome {
        let request = ActionRequest { issuance, action };
        self.run(|reply| Command::Act { request, reply })
    }

    fn act(&mut self, window: WindowInstance, action: WindowAction) -> WindowActionOutcome {
        let issuance = self.issuance(window);
        self.act_with(issuance, action)
    }

    fn stop(&mut self) -> StopOutcome {
        let epoch = self.epoch;
        self.run(|reply| Command::Stop { epoch, reply })
    }
}

fn snapshots(events: &[WindowEvent]) -> Vec<WindowSnapshot> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            WindowEventKind::Snapshot { snapshot, .. } => Some(snapshot.clone()),
            _ => None,
        })
        .collect()
}

fn kinds(events: &[WindowEvent]) -> Vec<WindowEventKind> {
    events.iter().map(|event| event.kind.clone()).collect()
}

const fn frame(x: f64) -> WindowAction {
    WindowAction::SetFrame {
        position: Some(AxPoint { x, y: 5.0 }),
        size: Some(AxSize { width: 400.0, height: 300.0 }),
    }
}

fn count_calls(ax: &FakeWindowAx, wanted: impl Fn(&Call) -> bool) -> usize {
    ax.calls().iter().filter(|call| wanted(call)).count()
}

// ---- constants and pure helpers ---------------------------------------------

#[test]
fn documented_limits_are_the_existing_constants() {
    assert_eq!(ELEMENT_TIMEOUT, Duration::from_millis(500));
    assert_eq!(PRESS_TIMEOUT, Duration::from_secs(1));
    assert_eq!(ACKNOWLEDGEMENT_BUDGET, Duration::from_secs(2));
    assert_eq!(CANCELLATION_QUANTUM, Duration::from_millis(10));
    assert_eq!(ENUMERATION_CHUNK_ELEMENTS, 64);
    assert_eq!(ENUMERATION_CHUNK_BUDGET, Duration::from_millis(50));
}

#[test]
fn a_hop_is_the_shared_budget_capped_by_the_remaining_deadline() {
    let now = Instant::now();
    assert_eq!(hop_wait(now, None), ACKNOWLEDGEMENT_BUDGET);
    assert_eq!(
        hop_wait(now, Some(now + Duration::from_secs(60))),
        ACKNOWLEDGEMENT_BUDGET
    );
    assert_eq!(
        hop_wait(now, Some(now + Duration::from_millis(300))),
        Duration::from_millis(300)
    );
    assert_eq!(hop_wait(now, Some(now)), Duration::ZERO);
    assert_eq!(hop_wait(now + Duration::from_secs(1), Some(now)), Duration::ZERO);
}

#[test]
fn frame_write_outcomes_are_classified_truthfully() {
    use IssuedKind::{Applied, Partial, Rejected, Uncertain};
    let ok = WriteStatus::Written;
    let none = WriteStatus::NotRequested;
    let bad = WriteStatus::Failed(AxError::Other(-1));
    let lost = WriteStatus::Failed(AxError::CannotComplete);
    assert_eq!(classify_frame_write(ok, ok), Applied);
    assert_eq!(classify_frame_write(ok, none), Applied);
    assert_eq!(classify_frame_write(ok, bad), Partial);
    assert_eq!(classify_frame_write(bad, ok), Partial);
    assert_eq!(classify_frame_write(bad, bad), Rejected);
    assert_eq!(classify_frame_write(bad, none), Rejected);
    assert_eq!(classify_frame_write(ok, lost), Uncertain);
    assert_eq!(classify_frame_write(lost, bad), Uncertain);
}

// ---- observation and enumeration ----------------------------------------------

#[test]
fn observation_is_installed_before_the_first_enumeration_read() {
    let mut h = H::new();
    let (node, ..) = h.app_with_windows(app(1), 500, 2);
    let calls = h.ax.calls();
    let first_read = calls.iter().position(|c| matches!(c, Call::Windows(..))).expect("read");
    let observed: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, Call::Observe(n, _) if *n == node))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(observed.len(), 5, "all application notifications registered");
    assert!(observed.iter().all(|i| *i < first_read));
    assert!(h.obs.is_attached(500));
}

#[test]
fn enumeration_snapshots_are_pure_values_with_explicit_unknowns() {
    let mut h = H::new();
    let good = window_node(&h.ax, 42);
    let bare = h.ax.add_node(FakeNode {
        position: Err(AxError::WrongType),
        window_id: Err(crate::ax::WindowIdError::Unsupported),
        ..FakeNode::default()
    });
    app_node(&h.ax, 7, vec![good, bare]);
    assert_eq!(h.track(app(1), 7), TrackOutcome::Tracking);
    h.settle();
    let events = h.events();
    let shots = snapshots(&events);
    assert_eq!(shots.len(), 2);
    assert_eq!(shots[0].cg_window_id, Some(42));
    assert_eq!(shots[0].title, AxReading::Value("w".to_owned()));
    assert_eq!(shots[0].frame.complete().map(|f| f.size.width), Some(300.0));
    assert_eq!(shots[0].resizable, AxReading::Value(true));
    assert_eq!(shots[0].tabs, TabInfo::Unknown, "tabs are not read by default");
    assert!(shots[0].observed);
    // Missing and malformed attributes are explicit, never zero.
    assert_eq!(shots[1].cg_window_id, None);
    assert_eq!(shots[1].title, AxReading::Unsupported);
    assert_eq!(shots[1].frame.position, AxReading::Unknown(AxError::WrongType));
    assert_eq!(shots[1].frame.size, AxReading::Unsupported);
    assert_eq!(shots[1].frame.complete(), None);
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(WindowEventKind::EnumerationComplete {
            windows: 2,
            truncated: false,
            ..
        })
    ));
    // Sequences are strictly increasing within the epoch.
    assert!(events.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
    assert!(events.iter().all(|e| e.epoch == h.epoch));
}

#[test]
fn queries_use_the_half_second_timeout() {
    let mut h = H::new();
    let (_, nodes, _) = h.app_with_windows(app(1), 1, 1);
    let calls = h.ax.calls();
    assert!(calls.contains(&Call::SetTimeout(nodes[0], ELEMENT_TIMEOUT)));
    assert!(!calls.iter().any(|c| matches!(c, Call::SetTimeout(_, d) if *d == PRESS_TIMEOUT)));
}

#[test]
fn tabs_are_read_only_when_enabled_and_never_assumed() {
    let mut h = H::with_queue(64, true);
    let window = window_node(&h.ax, 1);
    let group = h.ax.add_node(FakeNode {
        children: Ok(vec![]),
        tabs: Ok(vec![window, window]),
        ..FakeNode::default()
    });
    h.ax.edit(group, |n| {
        n.strings.insert(WindowString::Role, Ok("AXTabGroup".to_owned()));
    });
    h.ax.edit(window, |n| n.children = Ok(vec![group]));
    let plain = window_node(&h.ax, 2);
    h.ax.edit(plain, |n| n.children = Err(AxError::AttributeUnsupported));
    app_node(&h.ax, 9, vec![window, plain]);
    h.track(app(1), 9);
    h.settle();
    let shots = snapshots(&h.events());
    assert_eq!(shots[0].tabs, TabInfo::Tabs(2));
    assert_eq!(shots[1].tabs, TabInfo::None);
}

#[test]
fn tab_members_carry_ids_and_selection_and_a_failed_read_is_explicit() {
    use crate::windows::TabMember;
    let mut h = H::with_queue(64, true);
    let window = window_node(&h.ax, 1);
    let tab_a = h.ax.add_node(FakeNode {
        window_id: Ok(1),
        selected: Ok(true),
        ..FakeNode::default()
    });
    let tab_b = h.ax.add_node(FakeNode {
        window_id: Ok(2),
        selected: Ok(false),
        ..FakeNode::default()
    });
    let group = h.ax.add_node(FakeNode {
        children: Ok(vec![]),
        tabs: Ok(vec![tab_a, tab_b]),
        ..FakeNode::default()
    });
    h.ax.edit(group, |n| {
        n.strings.insert(WindowString::Role, Ok("AXTabGroup".to_owned()));
    });
    h.ax.edit(window, |n| n.children = Ok(vec![group]));
    let broken = window_node(&h.ax, 3);
    h.ax.edit(broken, |n| n.children = Err(AxError::CannotComplete));
    app_node(&h.ax, 9, vec![window, broken]);
    h.track(app(1), 9);
    h.settle();
    let shots = snapshots(&h.events());
    assert_eq!(shots[0].tabs, TabInfo::Tabs(2));
    assert_eq!(shots[0].tab_members, vec![
        TabMember {
            cg_window_id: Some(1),
            selected: Some(true)
        },
        TabMember {
            cg_window_id: Some(2),
            selected: Some(false)
        },
    ]);
    assert_eq!(shots[1].tabs, TabInfo::Failed, "a failed read is not 'no tabs'");
    assert!(shots[1].tab_members.is_empty());
}

#[test]
fn a_missing_window_id_symbol_leaves_the_id_absent() {
    let ax = FakeWindowAx::without_window_id_symbol();
    let (system, _obs) = FakeWindowSystem::new(ax.clone());
    let clock = SteppingClock::new(Duration::ZERO);
    let mut core = Core::new(system, AdapterId::allocate(), clock, false);
    let (tx, rx) = mpsc::sync_channel(8);
    let queue = EventQueue::new(16);
    let gate = Arc::new(GenerationCell::new());
    tx.send(Command::Install {
        request: InstallRequest {
            queue: Arc::clone(&queue),
            gate,
        },
        reply: Box::new(|_| {}),
    })
    .expect("send");
    core.turn(&rx);
    let window = window_node(&ax, 5);
    app_node(&ax, 3, vec![window]);
    tx.send(Command::Track {
        epoch: RuntimeEpoch::new(1),
        request: TrackRequest {
            application: app(1),
            pid: 3,
            cancel: CancellationToken::new(),
            deadline: None,
        },
        reply: Box::new(|_| {}),
    })
    .expect("send");
    core.turn(&rx);
    assert_eq!(snapshots(&queue.drain().events)[0].cg_window_id, None);
}

#[test]
fn enumeration_yields_at_sixty_four_elements() {
    let mut h = H::new();
    let nodes: Vec<NodeId> = (0..130).map(|n| window_node(&h.ax, n)).collect();
    app_node(&h.ax, 1, nodes);
    // The track turn runs the first chunk.
    assert_eq!(h.track(app(1), 1), TrackOutcome::Tracking);
    assert_eq!(snapshots(&h.events()).len(), 64);
    let second = h.turn();
    assert!(second.active, "more windows: the chunk is re-posted");
    assert_eq!(snapshots(&h.events()).len(), 64);
    let third = h.turn();
    assert!(!third.active);
    let events = h.events();
    assert_eq!(snapshots(&events).len(), 2);
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(WindowEventKind::EnumerationComplete { windows: 130, .. })
    ));
    assert_eq!(h.core.tracked_windows(), 130);
}

#[test]
fn enumeration_yields_when_the_fifty_millisecond_budget_is_spent() {
    let mut h = H::new();
    let nodes: Vec<NodeId> = (0..10).map(|n| window_node(&h.ax, n)).collect();
    app_node(&h.ax, 1, nodes);
    h.clock.set_step(Duration::from_millis(20));
    // Readings at +0, +20, +40 are within budget; +60 yields: three elements.
    h.track(app(1), 1);
    assert_eq!(snapshots(&h.events()).len(), 3);
    assert!(h.turn().active);
    assert_eq!(snapshots(&h.events()).len(), 3);
}

#[test]
fn enumeration_stops_between_elements_on_cancel_deadline_and_seal() {
    // Cancellation.
    let mut h = H::new();
    let nodes: Vec<NodeId> = (0..130).map(|n| window_node(&h.ax, n)).collect();
    app_node(&h.ax, 1, nodes);
    let cancel = CancellationToken::new();
    let epoch = h.epoch;
    let request = TrackRequest {
        application: app(1),
        pid: 1,
        cancel: cancel.clone(),
        deadline: None,
    };
    h.run(|reply| Command::Track { epoch, request, reply });
    cancel.cancel();
    h.settle();
    let events = h.events();
    assert_eq!(snapshots(&events).len(), 64);
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(WindowEventKind::EnumerationAborted {
            reason: EnumerationAbort::Cancelled,
            ..
        })
    ));

    // Deadline already passed.
    let mut h = H::new();
    let nodes = vec![window_node(&h.ax, 1)];
    app_node(&h.ax, 1, nodes);
    let epoch = h.epoch;
    let request = TrackRequest {
        application: app(1),
        pid: 1,
        cancel: CancellationToken::new(),
        deadline: Some(h.clock.peek()),
    };
    h.run(|reply| Command::Track { epoch, request, reply });
    assert!(matches!(
        h.events().last().map(|e| &e.kind),
        Some(WindowEventKind::EnumerationAborted {
            reason: EnumerationAbort::DeadlineExceeded,
            ..
        })
    ));

    // A sealed gate cancels an in-flight enumeration.
    let mut h = H::new();
    let nodes: Vec<NodeId> = (0..130).map(|n| window_node(&h.ax, n)).collect();
    app_node(&h.ax, 1, nodes);
    h.track(app(1), 1);
    h.gate.seal();
    h.settle();
    assert!(
        h.events().iter().any(|e| matches!(e.kind, WindowEventKind::EnumerationAborted {
            reason: EnumerationAbort::Sealed,
            ..
        }))
    );
}

#[test]
fn an_unreadable_window_list_aborts_enumeration_explicitly() {
    let mut h = H::new();
    let node = app_node(&h.ax, 1, vec![]);
    h.ax.edit(node, |n| n.windows = Err(AxError::CannotComplete));
    h.track(app(1), 1);
    assert!(matches!(
        h.events().last().map(|e| &e.kind),
        Some(WindowEventKind::EnumerationAborted {
            reason: EnumerationAbort::Failed(AxError::CannotComplete),
            ..
        })
    ));
}

#[test]
fn an_idle_owner_parks_without_a_timeout_and_a_busy_one_polls_with_zero() {
    let ax = FakeWindowAx::new();
    let nodes: Vec<NodeId> = (0..130).map(|n| window_node(&ax, n)).collect();
    app_node(&ax, 1, nodes);
    let (system, _obs) = FakeWindowSystem::new(ax);
    let clock = SteppingClock::new(Duration::ZERO);
    let core = Core::new(system, AdapterId::allocate(), clock, false);
    let (tx, rx) = mpsc::sync_channel(64);
    let queue = EventQueue::new(1024);
    let gate = Arc::new(GenerationCell::new());
    tx.send(Command::Install {
        request: InstallRequest { queue, gate },
        reply: Box::new(|_| {}),
    })
    .expect("send");
    let sender = tx.clone();
    let track = Box::new(move || {
        sender
            .send(Command::Track {
                epoch: RuntimeEpoch::new(1),
                request: TrackRequest {
                    application: app(1),
                    pid: 1,
                    cancel: CancellationToken::new(),
                    deadline: None,
                },
                reply: Box::new(|_| {}),
            })
            .expect("send");
    });
    let sender = tx;
    let terminate = Box::new(move || {
        sender.send(Command::Terminate { reply: Box::new(|_| {}) }).expect("send");
    });
    let (driver, parks) =
        ScriptedDriver::new(vec![track, Box::new(|| {}), Box::new(|| {}), terminate]);
    let wakeup = Wakeup::new(driver.waker());
    run_owner(core, &rx, driver, &wakeup);
    let parks = parks.lock().unwrap_or_else(PoisonError::into_inner).clone();
    // install (idle) -> track (64) -> 64 -> last 2 + complete (idle).
    assert_eq!(parks, vec![
        None,
        Some(Duration::ZERO),
        Some(Duration::ZERO),
        None
    ]);
    assert!(
        parks.iter().flatten().all(Duration::is_zero),
        "no duration timer is ever armed"
    );
}

// ---- sink ---------------------------------------------------------------------

#[test]
fn a_full_sink_drops_and_reports_overflow_explicitly() {
    let mut h = H::with_queue(2, false);
    let nodes: Vec<NodeId> = (0..5).map(|n| window_node(&h.ax, n)).collect();
    app_node(&h.ax, 1, nodes);
    h.track(app(1), 1);
    h.settle();
    let batch = h.queue.drain();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(
        batch.dropped, 4,
        "five snapshots plus the completion minus two kept"
    );
    assert!(batch.overflowed());
    assert_eq!(batch.events[1].sequence, 2);
    // The drop count resets and later events carry later sequences (a visible gap).
    h.turn();
    assert!(!h.queue.drain().overflowed());
}

#[test]
fn the_sink_notifies_only_on_the_empty_to_non_empty_edge() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let queue = EventQueue::with_notify(
        8,
        Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let event = |sequence| WindowEvent {
        epoch: RuntimeEpoch::new(1),
        sequence,
        kind: WindowEventKind::CapabilityChanged {
            capability: Capability::Accessibility,
            available: true,
        },
    };
    assert!(queue.push(event(1)));
    assert!(queue.push(event(2)));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let _ = queue.drain();
    assert!(queue.push(event(3)));
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

#[test]
fn observer_overflow_is_reported_so_the_consumer_can_refresh() {
    let mut h = H::new();
    h.app_with_windows(app(1), 500, 1);
    h.obs.set_overflow(500);
    h.turn();
    assert!(h.events().iter().any(
        |e| matches!(e.kind, WindowEventKind::ObserverOverflow { application } if application == app(1))
    ));
    // Refresh reports the still-live window again (same token, no duplicate
    // adoption) so the consumer sees it in the listing.
    let before = h.core.tracked_windows();
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Refresh {
        epoch,
        application: app(1),
        cancel: CancellationToken::new(),
        deadline: None,
        reply,
    });
    assert_eq!(outcome, TrackOutcome::Tracking);
    h.settle();
    let events = h.events();
    let listed = snapshots(&events);
    assert_eq!(listed.len(), 1);
    assert_eq!(h.core.tracked_windows(), before);
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(WindowEventKind::EnumerationComplete {
            windows: 1,
            truncated: false,
            ..
        })
    ));
}

fn refresh(h: &mut H, application: ApplicationId) -> Vec<WindowEvent> {
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Refresh {
        epoch,
        application,
        cancel: CancellationToken::new(),
        deadline: None,
        reply,
    });
    assert_eq!(outcome, TrackOutcome::Tracking);
    h.settle();
    h.events()
}

#[test]
fn a_refresh_reports_every_live_window_with_its_original_token() {
    let mut h = H::new();
    let (_, _, tokens) = h.app_with_windows(app(1), 500, 3);
    let events = refresh(&mut h, app(1));
    let listed = snapshots(&events);
    assert_eq!(listed.iter().map(|s| s.window).collect::<Vec<_>>(), tokens);
    assert!(events.iter().all(|e| !matches!(e.kind, WindowEventKind::Window {
        change: WindowChange::Destroyed,
        ..
    })));
    assert!(events.iter().all(|e| match &e.kind {
        WindowEventKind::Snapshot { origin, .. } => *origin == SnapshotOrigin::Enumeration,
        _ => true,
    }));
}

#[test]
fn a_complete_refresh_retires_a_closed_window_and_releases_its_registrations() {
    let mut h = H::new();
    let (node, nodes, tokens) = h.app_with_windows(app(1), 500, 3);
    h.ax.edit(node, |n| n.windows = Ok(vec![nodes[0], nodes[2]]));
    h.ax.kill(nodes[1]);
    let events = refresh(&mut h, app(1));
    let kinds = kinds(&events);
    assert_eq!(
        snapshots(&events).iter().map(|s| s.window).collect::<Vec<_>>(),
        vec![tokens[0], tokens[2]]
    );
    let destroyed = kinds.iter().position(|k| matches!(
        k,
        WindowEventKind::Window { window, change: WindowChange::Destroyed } if *window == tokens[1]
    ));
    let complete = kinds
        .iter()
        .position(|k| matches!(k, WindowEventKind::EnumerationComplete { truncated: false, .. }));
    assert!(destroyed.is_some() && destroyed < complete, "{kinds:?}");
    assert_eq!(h.core.tracked_windows(), 2);
    assert!(h.obs.registrations(500).iter().all(|(n, _)| *n != nodes[1]));
    // The retired token is dead for every later command.
    let outcome = h.run(|reply| Command::Snapshot { window: tokens[1], reply });
    assert!(matches!(outcome, SnapshotOutcome::Stale(StaleReason::Destroyed)));
}

#[test]
fn a_failed_registration_removal_on_retire_is_kept_for_cleanup() {
    let mut h = H::new();
    let (node, nodes, tokens) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(node, |n| n.windows = Ok(vec![nodes[0]]));
    h.ax.kill(nodes[1]);
    h.obs.set_remove_error(500, Some(AxError::CannotComplete));
    let events = refresh(&mut h, app(1));
    assert!(kinds(&events).iter().any(|k| matches!(
        k,
        WindowEventKind::Window { window, change: WindowChange::Destroyed } if *window == tokens[1]
    )));
    // The registration could not be removed: it stays owned for teardown.
    assert!(h.obs.registrations(500).iter().any(|(n, _)| *n == nodes[1]));
    assert!(matches!(h.stop(), StopOutcome::Quarantined { .. }));
}

#[test]
fn an_aborted_or_truncated_refresh_destroys_nothing() {
    let mut h = H::new();
    let (node, nodes, _) = h.app_with_windows(app(1), 500, 3);
    h.ax.edit(node, |n| n.windows = Ok(vec![nodes[0]]));
    let epoch = h.epoch;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let _ = h.run(|reply| Command::Refresh {
        epoch,
        application: app(1),
        cancel,
        deadline: None,
        reply,
    });
    h.settle();
    let events = h.events();
    assert!(
        kinds(&events).iter().any(|k| matches!(k, WindowEventKind::EnumerationAborted {
            reason: EnumerationAbort::Cancelled,
            ..
        }))
    );
    assert!(
        kinds(&events).iter().all(|k| !matches!(k, WindowEventKind::Window {
            change: WindowChange::Destroyed,
            ..
        }))
    );
    assert_eq!(h.core.tracked_windows(), 3);

    // A list at the per-application cap is truncated: nothing is retired.
    let mut h = H::new();
    let many = u32::try_from(MAX_WINDOWS_PER_APPLICATION + 10).expect("small");
    let (node, nodes, _) = h.app_with_windows(app(1), 500, many);
    assert_eq!(h.core.tracked_windows(), MAX_WINDOWS_PER_APPLICATION);
    h.ax.edit(node, |n| n.windows = Ok(nodes[1..].to_vec()));
    let events = refresh(&mut h, app(1));
    assert!(matches!(
        events.last().map(|e| &e.kind),
        Some(WindowEventKind::EnumerationComplete { truncated: true, .. })
    ));
    assert!(
        kinds(&events).iter().all(|k| !matches!(k, WindowEventKind::Window {
            change: WindowChange::Destroyed,
            ..
        }))
    );
}

// ---- notifications -------------------------------------------------------------

#[test]
fn notifications_become_pure_events_outside_the_callback() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 1);
    h.ax.clear_calls();
    h.obs.inject(500, AxNotification::WindowMoved, nodes[0]);
    h.obs.inject(500, AxNotification::WindowResized, nodes[0]);
    h.obs.inject(500, AxNotification::WindowMiniaturized, nodes[0]);
    h.obs.inject(500, AxNotification::WindowDeminiaturized, nodes[0]);
    h.obs.inject(500, AxNotification::TitleChanged, nodes[0]);
    h.obs.inject(500, AxNotification::FocusedWindowChanged, nodes[0]);
    h.obs.inject(500, AxNotification::MainWindowChanged, app_el);
    h.obs.inject(500, AxNotification::ApplicationHidden, app_el);
    h.obs.inject(500, AxNotification::ApplicationShown, app_el);
    h.turn();
    let window = tokens[0];
    assert_eq!(kinds(&h.events()), vec![
        WindowEventKind::Window {
            window,
            change: WindowChange::Moved
        },
        WindowEventKind::Window {
            window,
            change: WindowChange::Resized
        },
        WindowEventKind::Window {
            window,
            change: WindowChange::Miniaturized
        },
        WindowEventKind::Window {
            window,
            change: WindowChange::Deminiaturized
        },
        WindowEventKind::Window {
            window,
            change: WindowChange::TitleChanged
        },
        WindowEventKind::Window {
            window,
            change: WindowChange::Focused
        },
        WindowEventKind::Application {
            application: app(1),
            change: ApplicationChange::MainWindowChanged
        },
        WindowEventKind::Application {
            application: app(1),
            change: ApplicationChange::Hidden
        },
        WindowEventKind::Application {
            application: app(1),
            change: ApplicationChange::Shown
        },
    ]);
    // Events carry no attribute reads: the owner read nothing to produce them.
    assert!(h.ax.calls().is_empty());
}

#[test]
fn a_created_window_is_adopted_and_snapshotted_on_the_owner() {
    let mut h = H::new();
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 1);
    let fresh = window_node(&h.ax, 777);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0], fresh]));
    h.obs.inject(500, AxNotification::WindowCreated, fresh);
    h.turn();
    let events = h.events();
    let shots = snapshots(&events);
    assert_eq!(shots.len(), 1);
    assert_eq!(shots[0].cg_window_id, Some(777));
    assert!(matches!(events[0].kind, WindowEventKind::Snapshot {
        origin: SnapshotOrigin::Created,
        ..
    }));
    assert_eq!(h.core.tracked_windows(), 2);
}

#[test]
fn a_creation_for_an_element_no_longer_in_the_window_list_is_dropped() {
    let mut h = H::new();
    let (_, _, _) = h.app_with_windows(app(1), 500, 1);
    let ghost = window_node(&h.ax, 9);
    h.obs.inject(500, AxNotification::WindowCreated, ghost);
    h.turn();
    assert!(h.events().is_empty());
    assert_eq!(h.core.tracked_windows(), 1);
}

#[test]
fn a_focus_notification_before_the_creation_in_one_drain_does_not_lose_the_window() {
    let mut h = H::new();
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 1);
    let fresh = window_node(&h.ax, 777);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0], fresh]));
    // The application-level delivery names the window element and arrives
    // before the creation of that same window.
    h.obs.inject(500, AxNotification::FocusedWindowChanged, fresh);
    h.obs.inject(500, AxNotification::MainWindowChanged, fresh);
    h.obs.inject(500, AxNotification::WindowCreated, fresh);
    h.turn();
    let events = h.events();
    let shots = snapshots(&events);
    assert_eq!(shots.len(), 1, "the creation was not dropped: {events:?}");
    let token = shots[0].window;
    assert_eq!(h.core.tracked_windows(), 2);
    // The focus is reported for the token, after the snapshot that mints it.
    let kinds = kinds(&events);
    let created = kinds.iter().position(|k| matches!(k, WindowEventKind::Snapshot { .. }));
    let focused = kinds.iter().position(|k| {
        matches!(
            k,
            WindowEventKind::Window { window, change: WindowChange::Focused } if *window == token
        )
    });
    assert!(created.is_some() && focused > created, "{kinds:?}");
    assert!(kinds.iter().any(|k| matches!(
        k,
        WindowEventKind::Window { window, change: WindowChange::Main } if *window == token
    )));
    assert!(h.obs.registrations(500).iter().any(|(node, _)| *node == fresh));
}

#[test]
fn a_focus_for_a_window_that_never_becomes_tracked_is_released_not_reported() {
    let mut h = H::new();
    let (_, _, _) = h.app_with_windows(app(1), 500, 1);
    let ghost = window_node(&h.ax, 9);
    h.obs.inject(500, AxNotification::FocusedWindowChanged, ghost);
    h.turn();
    assert!(h.events().is_empty());
    assert!(!h.obs.is_interned(500, ghost), "released after the backlog");
}

#[test]
fn a_creation_whose_element_is_no_longer_held_asks_for_a_refresh() {
    let mut h = H::new();
    let (_, nodes, _) = h.app_with_windows(app(1), 500, 1);
    // The window is destroyed and then (stale) created in one drain: the
    // destroy forgets the element, so the creation cannot be resolved.
    h.obs.inject(500, AxNotification::ElementDestroyed, nodes[0]);
    h.obs.inject(500, AxNotification::WindowCreated, nodes[0]);
    h.turn();
    let kinds = kinds(&h.events());
    assert!(kinds.iter().any(|k| matches!(k, WindowEventKind::Window {
        change: WindowChange::Destroyed,
        ..
    })));
    assert!(
        kinds.iter().any(|k| matches!(
            k,
            WindowEventKind::ObserverOverflow { application } if *application == app(1)
        )),
        "{kinds:?}"
    );
}

/// An application with `count` windows to create, none tracked yet.
fn creation_burst(h: &mut H, count: u32) -> (Vec<NodeId>, Vec<NodeId>) {
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 1);
    let fresh: Vec<NodeId> = (0..count).map(|n| window_node(&h.ax, 1000 + n)).collect();
    let mut all = nodes;
    all.extend(fresh.iter().copied());
    h.ax.edit(app_el, |n| n.windows = Ok(all.clone()));
    for node in &fresh {
        h.obs.inject(500, AxNotification::WindowCreated, *node);
    }
    (all, fresh)
}

#[test]
fn a_burst_of_creations_yields_after_the_element_budget_and_keeps_order() {
    let mut h = H::new();
    let (_, fresh) = creation_burst(&mut h, 150);
    let turn = h.turn();
    let first = snapshots(&h.events());
    assert_eq!(first.len(), ENUMERATION_CHUNK_ELEMENTS, "one budget per turn");
    assert!(turn.active, "the rest is re-posted, not dropped");
    let mut cg: Vec<u32> = first.iter().filter_map(|s| s.cg_window_id).collect();
    h.settle();
    cg.extend(snapshots(&h.events()).iter().filter_map(|s| s.cg_window_id));
    let wanted: Vec<u32> =
        (0..u32::try_from(fresh.len()).expect("small")).map(|n| 1000 + n).collect();
    assert_eq!(
        cg, wanted,
        "every creation adopted exactly once, in arrival order"
    );
}

#[test]
fn a_burst_of_creations_also_yields_after_the_time_budget() {
    let mut h = H::new();
    creation_burst(&mut h, 20);
    // Every clock reading costs 30 ms, so the 50 ms budget is spent quickly.
    h.clock.set_step(Duration::from_millis(30));
    let turn = h.turn();
    let adopted = snapshots(&h.events()).len();
    assert!((1..20).contains(&adopted), "{adopted}");
    assert!(turn.active);
}

fn focus_tokens(events: &[WindowEvent], wanted: WindowChange) -> Vec<WindowInstance> {
    kinds(events)
        .into_iter()
        .filter_map(|k| match k {
            WindowEventKind::Window { window, change } if change == wanted => Some(window),
            _ => None,
        })
        .collect()
}

#[test]
fn a_deferred_focus_keeps_its_notification_order_and_the_latest_wins() {
    for (notification, change) in [
        (AxNotification::FocusedWindowChanged, WindowChange::Focused),
        (AxNotification::MainWindowChanged, WindowChange::Main),
    ] {
        let mut h = H::new();
        let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 1);
        let fresh = window_node(&h.ax, 777);
        h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0], fresh]));
        // Focus(K) (untracked) -> Created(K) -> Focus(J) (tracked).
        h.obs.inject(500, notification, fresh);
        h.obs.inject(500, AxNotification::WindowCreated, fresh);
        h.obs.inject(500, notification, nodes[0]);
        h.turn();
        let events = h.events();
        let focused = focus_tokens(&events, change);
        assert_eq!(focused.len(), 2, "{events:?}");
        assert_eq!(focused[1], tokens[0], "the consumer ends on J, not K");
        assert_ne!(focused[0], tokens[0]);
    }
}

#[test]
fn an_untracked_focus_superseded_by_a_later_one_is_never_reported() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 1);
    let fresh = window_node(&h.ax, 777);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[0], fresh]));
    // Focus(K) is deferred, Focus(J) supersedes it, then K is created.
    h.obs.inject(500, AxNotification::FocusedWindowChanged, fresh);
    h.obs.inject(500, AxNotification::FocusedWindowChanged, nodes[0]);
    h.obs.inject(500, AxNotification::WindowCreated, fresh);
    h.turn();
    let focused = focus_tokens(&h.events(), WindowChange::Focused);
    assert_eq!(focused, vec![tokens[0]]);
}

#[test]
fn a_stale_backlog_never_survives_untrack_and_retrack() {
    let mut h = H::new();
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 1);
    let fresh: Vec<NodeId> = (0..250).map(|n| window_node(&h.ax, 1000 + n)).collect();
    let mut all = nodes.clone();
    all.extend(fresh.iter().copied());
    h.ax.edit(app_el, |n| n.windows = Ok(all.clone()));
    for node in &fresh {
        h.obs.inject(500, AxNotification::WindowCreated, *node);
    }
    h.obs.inject(500, AxNotification::ElementDestroyed, nodes[0]);
    h.obs.inject(500, AxNotification::FocusedWindowChanged, fresh[249]);
    let turn = h.turn();
    assert!(turn.active, "part of the backlog is still queued");
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    assert_eq!(outcome, StopOutcome::Clean { applications: 1 });
    let _ = h.events();
    // The same application again: a new observer, its keys start over.
    assert_eq!(h.track(app(1), 500), TrackOutcome::Tracking);
    h.settle();
    let events = h.events();
    assert!(
        kinds(&events).iter().all(|k| !matches!(k, WindowEventKind::Window {
            change: WindowChange::Destroyed,
            ..
        })),
        "{events:?}"
    );
    assert_eq!(h.core.tracked_windows(), all.len());
}

#[test]
fn a_key_with_a_pending_mailbox_event_is_not_released_across_turns() {
    let mut h = H::new();
    let (app_el, nodes, _) = h.app_with_windows(app(1), 500, 1);
    let burst: Vec<NodeId> = (0..100).map(|n| window_node(&h.ax, 1000 + n)).collect();
    let late = window_node(&h.ax, 777);
    let mut all = nodes;
    all.extend(burst.iter().copied());
    all.push(late);
    h.ax.edit(app_el, |n| n.windows = Ok(all.clone()));
    // A focus for a not yet tracked window, then a multi-turn creation burst.
    h.obs.inject(500, AxNotification::FocusedWindowChanged, late);
    for node in &burst {
        h.obs.inject(500, AxNotification::WindowCreated, *node);
    }
    assert!(h.turn().active);
    let _ = h.events();
    // Between turns the creation of that window is recorded.
    h.obs.inject(500, AxNotification::WindowCreated, late);
    h.settle();
    let events = h.events();
    let kinds = kinds(&events);
    assert!(
        kinds.iter().all(|k| !matches!(k, WindowEventKind::ObserverOverflow { .. })),
        "{kinds:?}"
    );
    let token = snapshots(&events)
        .iter()
        .find(|s| s.cg_window_id == Some(777))
        .map(|s| s.window)
        .expect("the late creation was adopted");
    assert_eq!(focus_tokens(&events, WindowChange::Focused), vec![token]);
}

// ---- retirement is budgeted ------------------------------------------------------

fn destroyed(events: &[WindowEvent]) -> usize {
    focus_tokens(events, WindowChange::Destroyed).len()
}

/// `count` tracked windows, then a refresh whose listing is empty.
fn start_mass_retirement(
    h: &mut H,
    count: u32,
    cancel: CancellationToken,
    deadline: Option<Instant>,
    step: Duration,
) {
    let (node, nodes, _) = h.app_with_windows(app(1), 500, count);
    h.ax.edit(node, |n| n.windows = Ok(Vec::new()));
    for dead in nodes {
        h.ax.kill(dead);
    }
    h.clock.set_step(step);
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Refresh {
        epoch,
        application: app(1),
        cancel,
        deadline,
        reply,
    });
    assert_eq!(outcome, TrackOutcome::Tracking);
}

#[test]
fn many_missed_windows_are_retired_within_the_element_budget_per_turn() {
    let mut h = H::new();
    start_mass_retirement(&mut h, 150, CancellationToken::new(), None, Duration::ZERO);
    let mut total = 0;
    let mut turns = 0;
    loop {
        let events = h.events();
        let n = destroyed(&events);
        assert!(n <= ENUMERATION_CHUNK_ELEMENTS, "one chunk retired {n}");
        total += n;
        turns += 1;
        if kinds(&events)
            .iter()
            .any(|k| matches!(k, WindowEventKind::EnumerationComplete { truncated: false, .. }))
        {
            break;
        }
        assert!(h.turn().active || turns < 10);
        assert!(turns < 10, "never completed");
    }
    assert_eq!(total, 150);
    assert!(turns >= 3, "spread over several turns: {turns}");
    assert_eq!(h.core.tracked_windows(), 0);
}

#[test]
fn retirement_also_yields_when_the_time_budget_is_spent() {
    let mut h = H::new();
    start_mass_retirement(
        &mut h,
        10,
        CancellationToken::new(),
        None,
        Duration::from_millis(20),
    );
    let first = destroyed(&h.events());
    assert!((1..10).contains(&first), "{first}");
    h.clock.set_step(Duration::ZERO);
    h.settle();
    assert_eq!(h.core.tracked_windows(), 0);
}

#[test]
fn cancellation_deadline_and_seal_abort_a_listing_truthfully() {
    for case in 0..3 {
        let mut h = H::new();
        let cancel = CancellationToken::new();
        let deadline = Some(h.clock.peek() + Duration::from_secs(5));
        start_mass_retirement(&mut h, 150, cancel.clone(), deadline, Duration::ZERO);
        let first = destroyed(&h.events());
        assert!(first > 0 && first < 150);
        let expected = match case {
            0 => {
                cancel.cancel();
                EnumerationAbort::Cancelled
            }
            1 => {
                h.clock.advance(Duration::from_secs(10));
                EnumerationAbort::DeadlineExceeded
            }
            _ => {
                h.gate.seal();
                EnumerationAbort::Sealed
            }
        };
        h.settle();
        let events = h.events();
        let kinds = kinds(&events);
        assert!(
            kinds.iter().any(|k| matches!(
                k,
                WindowEventKind::EnumerationAborted { reason, .. } if *reason == expected
            )),
            "case {case}: {kinds:?}"
        );
        assert!(kinds.iter().all(|k| !matches!(k, WindowEventKind::EnumerationComplete { .. })));
        assert_eq!(destroyed(&events), 0, "nothing retired after the interruption");
        assert_eq!(h.core.tracked_windows(), 150 - first);
    }
}

// ---- identity, destruction, reuse ----------------------------------------------

#[test]
fn destroy_invalidates_the_token_and_forgets_registrations_without_an_ax_call() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![nodes[1]]));
    h.ax.clear_calls();
    h.obs.inject(500, AxNotification::ElementDestroyed, nodes[0]);
    h.turn();
    assert_eq!(kinds(&h.events()), vec![WindowEventKind::Window {
        window: tokens[0],
        change: WindowChange::Destroyed
    }]);
    assert_eq!(count_calls(&h.ax, |c| matches!(c, Call::Unobserve(..))), 0);
    assert!(h.obs.registrations(500).iter().all(|(node, _)| *node != nodes[0]));
    assert_eq!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::Destroyed)
    );
    assert_eq!(count_calls(&h.ax, |c| matches!(c, Call::Raise(_))), 0);
}

#[test]
fn a_destroy_callback_beats_an_already_queued_action() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 1);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![]));
    let request = ActionRequest {
        issuance: h.issuance(tokens[0]),
        action: frame(1.0),
    };
    let slot = h.send(|reply| Command::Act { request, reply });
    h.obs.inject(500, AxNotification::ElementDestroyed, nodes[0]);
    h.turn();
    let outcome = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
    assert_eq!(outcome, Some(WindowActionOutcome::Stale(StaleReason::Destroyed)));
    assert_eq!(count_calls(&h.ax, |c| matches!(c, Call::SetPosition(..))), 0);
}

#[test]
fn delayed_callbacks_after_destroy_never_revive_or_mint_a_token() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 1);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![]));
    h.obs.inject(500, AxNotification::ElementDestroyed, nodes[0]);
    h.turn();
    h.events();
    for notification in [
        AxNotification::WindowMoved,
        AxNotification::WindowResized,
        AxNotification::FocusedWindowChanged,
        AxNotification::WindowCreated,
        AxNotification::ElementDestroyed,
    ] {
        h.obs.inject(500, notification, nodes[0]);
    }
    h.turn();
    assert!(h.events().is_empty());
    assert_eq!(h.core.tracked_windows(), 0);
    assert_eq!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::Destroyed)
    );
}

#[test]
fn a_window_that_left_the_window_list_is_stale_before_any_send() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 2);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![]));
    h.ax.kill(nodes[1]);
    h.ax.clear_calls();
    let outcome = h.act(tokens[1], frame(1.0));
    assert_eq!(outcome, WindowActionOutcome::Stale(StaleReason::NotMember));
    assert!(outcome.proves_no_send());
    assert_eq!(
        count_calls(&h.ax, |c| matches!(c, Call::SetPosition(..) | Call::SetSize(..))),
        0
    );
    // The consumer is told, and the token is now simply destroyed.
    assert!(
        h.events().iter().any(|e| matches!(e.kind, WindowEventKind::Window {
            change: WindowChange::Destroyed,
            ..
        }))
    );
    assert_eq!(
        h.act(tokens[1], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::Destroyed)
    );
}

#[test]
fn a_reused_cg_window_id_never_revives_a_destroyed_token() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 500, 1);
    let cg = 100;
    assert_eq!(h.core_resolve(cg), Some(tokens[0]));
    // Destroy, then a new window reuses the same CGWindowID.
    let reused = window_node(&h.ax, cg);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![reused]));
    h.obs.inject(500, AxNotification::ElementDestroyed, nodes[0]);
    h.obs.inject(500, AxNotification::WindowCreated, reused);
    h.turn();
    let fresh = snapshots(&h.events())[0].window;
    assert_ne!(fresh, tokens[0]);
    assert_eq!(fresh.serial(), tokens[0].serial() + 1);
    assert_eq!(h.core_resolve(cg), Some(fresh));
    assert_eq!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::Destroyed)
    );
    assert!(matches!(
        h.act(fresh, WindowAction::Raise),
        WindowActionOutcome::Issued { .. }
    ));
}

impl H {
    fn core_resolve(&mut self, cg_window_id: u32) -> Option<WindowInstance> {
        self.run(|reply| Command::Resolve { cg_window_id, reply })
    }
}

#[test]
fn duplicate_cg_ids_in_one_app_are_ambiguous_and_resolve_to_nothing() {
    let mut h = H::new();
    let a = window_node(&h.ax, 55);
    let b = window_node(&h.ax, 55);
    let c = window_node(&h.ax, 56);
    let app_el = app_node(&h.ax, 1, vec![a, b, c]);
    h.track(app(1), 1);
    h.settle();
    let tokens: Vec<_> = snapshots(&h.events()).iter().map(|s| s.window).collect();
    assert_eq!(h.core_resolve(55), None);
    assert_eq!(h.core_resolve(56), Some(tokens[2]));
    assert_eq!(h.core_resolve(999), None);
    // Once one duplicate dies the survivor resolves.
    h.ax.edit(app_el, |n| n.windows = Ok(vec![b, c]));
    h.obs.inject(1, AxNotification::ElementDestroyed, a);
    h.turn();
    assert_eq!(h.core_resolve(55), Some(tokens[1]));
}

#[test]
fn resolving_a_window_that_silently_left_the_list_yields_nothing() {
    let mut h = H::new();
    let (app_el, _, _) = h.app_with_windows(app(1), 1, 1);
    h.ax.edit(app_el, |n| n.windows = Ok(vec![]));
    assert_eq!(h.core_resolve(100), None);
}

#[test]
fn a_replaced_application_instance_never_inherits_the_old_tokens() {
    let mut h = H::new();
    let (_, _, old) = h.app_with_windows(app(1), 100, 1);
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    assert_eq!(outcome, StopOutcome::Clean { applications: 1 });
    // Same PID, new exact application instance.
    let nodes = vec![window_node(&h.ax, 100)];
    app_node(&h.ax, 100, nodes);
    assert_eq!(h.track(app(2), 100), TrackOutcome::Tracking);
    h.settle();
    let new = snapshots(&h.events())[0].window;
    assert_ne!(old[0], new);
    assert_eq!(
        h.act(old[0], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::ApplicationGone)
    );
    assert!(matches!(
        h.act(new, WindowAction::Raise),
        WindowActionOutcome::Issued { .. }
    ));
    // Tracking the same instance twice is refused.
    assert_eq!(h.track(app(2), 100), TrackOutcome::AlreadyTracked);
}

#[test]
fn tokens_from_another_adapter_or_epoch_cannot_issue() {
    let mut first = H::new();
    let (_, _, tokens) = first.app_with_windows(app(1), 1, 1);
    let mut second = H::new();
    second.app_with_windows(app(1), 1, 1);
    assert_eq!(
        second.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::WrongAdapter)
    );
    assert_ne!(first.adapter, second.adapter);

    // A new epoch of the same adapter refuses the old epoch's token.
    assert_eq!(first.stop(), StopOutcome::Clean { applications: 1 });
    first.gate = Arc::new(GenerationCell::new());
    first.gate.publish(1, 1);
    first.queue = EventQueue::new(16);
    let next = first.install().expect("second epoch");
    assert!(next > tokens[0].epoch());
    first.epoch = next;
    assert_eq!(
        first.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Stale(StaleReason::WrongEpoch)
    );
}

// ---- actions ------------------------------------------------------------------

#[test]
fn set_frame_writes_size_position_size_under_the_one_second_timeout() {
    let mut h = H::new();
    let (_, nodes, tokens) = h.app_with_windows(app(1), 1, 1);
    h.ax.clear_calls();
    let outcome = h.act(tokens[0], frame(50.0));
    let calls = h.ax.calls();
    let timeout = calls
        .iter()
        .position(|c| *c == Call::SetTimeout(nodes[0], PRESS_TIMEOUT))
        .expect("press timeout");
    let position = calls.iter().position(|c| matches!(c, Call::SetPosition(..))).expect("pos");
    let size = calls.iter().position(|c| matches!(c, Call::SetSize(..))).expect("size");
    let last_size = calls.iter().rposition(|c| matches!(c, Call::SetSize(..))).expect("size");
    assert!(timeout < size && size < position && position < last_size);
    let WindowActionOutcome::Issued { kind, native, observed } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(kind, IssuedKind::Applied);
    assert_eq!(native, NativeStatus::Frame {
        position: WriteStatus::Written,
        size: WriteStatus::Written
    });
    let frame = observed.and_then(|read| read.complete()).expect("observed frame");
    assert_eq!(frame.origin.x, 50.0);
    assert_eq!(frame.size.width, 400.0);
}

#[test]
fn partial_lost_and_rejected_writes_are_reported_per_half() {
    let mut h = H::new();
    let (_, nodes, tokens) = h.app_with_windows(app(1), 1, 1);
    h.ax.edit(nodes[0], |n| n.set_size_result = Err(AxError::Other(-1)));
    let WindowActionOutcome::Issued { kind, native, observed } = h.act(tokens[0], frame(1.0))
    else {
        panic!("issued");
    };
    assert_eq!(kind, IssuedKind::Partial);
    assert_eq!(native, NativeStatus::Frame {
        position: WriteStatus::Written,
        size: WriteStatus::Failed(AxError::Other(-1))
    });
    // The actual geometry is read back: the position moved, the size did not.
    let read = observed.and_then(|r| r.complete()).expect("read back");
    assert_eq!((read.origin.x, read.size.width), (1.0, 300.0));

    h.ax.edit(nodes[0], |n| {
        n.set_position_result = Err(AxError::CannotComplete);
    });
    let WindowActionOutcome::Issued { kind, .. } = h.act(tokens[0], frame(2.0)) else {
        panic!("issued");
    };
    assert_eq!(
        kind,
        IssuedKind::Uncertain,
        "CannotComplete may follow an applied write"
    );

    h.ax.edit(nodes[0], |n| n.set_position_result = Err(AxError::Other(-2)));
    let WindowActionOutcome::Issued { kind, .. } = h.act(tokens[0], frame(3.0)) else {
        panic!("issued");
    };
    assert_eq!(kind, IssuedKind::Rejected);
}

#[test]
fn raise_sends_axraise_only_and_never_activates() {
    let mut h = H::new();
    let (_, nodes, tokens) = h.app_with_windows(app(1), 1, 1);
    h.ax.clear_calls();
    let outcome = h.act(tokens[0], WindowAction::Raise);
    assert_eq!(outcome, WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Action(Ok(())),
        observed: None
    });
    let calls = h.ax.calls();
    let timeout = calls.iter().position(|c| *c == Call::SetTimeout(nodes[0], PRESS_TIMEOUT));
    let raise = calls.iter().position(|c| *c == Call::Raise(nodes[0]));
    assert!(timeout < raise && raise.is_some());

    h.ax.edit(nodes[0], |n| n.raise_result = Err(AxError::CannotComplete));
    assert!(matches!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Issued {
            kind: IssuedKind::Uncertain,
            ..
        }
    ));
    h.ax.edit(nodes[0], |n| n.raise_result = Err(AxError::ActionUnsupported));
    assert!(matches!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Issued { kind: IssuedKind::Rejected, .. }
    ));
}

#[test]
fn every_gate_fact_refuses_before_any_native_call() {
    let mut h = H::new();
    let (_, _, tokens) = h.app_with_windows(app(1), 1, 1);
    let window = tokens[0];
    h.ax.clear_calls();
    let deadline = h.clock.peek() + Duration::from_secs(30);
    let writes = |h: &H| {
        count_calls(&h.ax, |c| {
            matches!(c, Call::SetPosition(..) | Call::SetSize(..) | Call::Raise(_))
        })
    };

    // Wrong generation.
    let outcome = h.act_with(H::issuance_at(window, 9, deadline), frame(1.0));
    assert_eq!(
        outcome,
        WindowActionOutcome::NotIssued(NotIssuedReason::GenerationInvalid)
    );
    // Changed accepted revision.
    h.gate.accept_revision(2);
    let outcome = h.act_with(H::issuance_at(window, 1, deadline), frame(1.0));
    assert_eq!(
        outcome,
        WindowActionOutcome::NotIssued(NotIssuedReason::RevisionChanged)
    );
    h.gate.accept_revision(1);
    // Cancelled issuance.
    let cancelled = H::issuance_at(window, 1, deadline);
    assert_eq!(cancelled.cancel(), IssuanceCancel::Cancelled);
    assert_eq!(
        h.act_with(cancelled, frame(1.0)),
        WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled)
    );
    // Expired deadline.
    let expired = H::issuance_at(window, 1, h.clock.peek());
    assert_eq!(
        h.act_with(expired, frame(1.0)),
        WindowActionOutcome::ExpiredBeforeSend
    );
    // Empty request.
    let outcome = h.act_with(H::issuance_at(window, 1, deadline), WindowAction::SetFrame {
        position: None,
        size: None,
    });
    assert_eq!(
        outcome,
        WindowActionOutcome::NotIssued(NotIssuedReason::EmptyRequest)
    );
    assert_eq!(writes(&h), 0);

    // One-shot: the same issuance cannot send twice.
    let once = H::issuance_at(window, 1, deadline);
    assert!(matches!(
        h.act_with(once.clone(), WindowAction::Raise),
        WindowActionOutcome::Issued { .. }
    ));
    assert!(once.is_issued());
    assert_eq!(
        h.act_with(once, WindowAction::Raise),
        WindowActionOutcome::NotIssued(NotIssuedReason::AlreadyUsed)
    );
    assert_eq!(writes(&h), 1);

    // A newer plan supersedes older sequences (the floor only rises).
    h.gate.supersede_below(5);
    let outcome = h.act_with(H::issuance_at(window, 1, deadline), frame(1.0));
    assert_eq!(
        outcome,
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
    );
    assert_eq!(writes(&h), 1);
}

#[test]
fn a_deadline_passing_during_revalidation_expires_before_send() {
    let mut h = H::new();
    let (_, _, tokens) = h.app_with_windows(app(1), 1, 1);
    h.ax.clear_calls();
    // Each clock reading advances 10 ms: the pre-check passes, the commit does not.
    h.clock.set_step(Duration::from_millis(10));
    let deadline = h.clock.peek() + Duration::from_millis(5);
    let outcome = h.act_with(H::issuance_at(tokens[0], 1, deadline), frame(1.0));
    assert_eq!(outcome, WindowActionOutcome::ExpiredBeforeSend);
    assert_eq!(
        count_calls(&h.ax, |c| matches!(c, Call::SetPosition(..) | Call::SetSize(..))),
        0
    );
}

#[test]
fn unverifiable_membership_and_timeout_failures_send_nothing() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 1, 1);
    h.ax.clear_calls();
    h.ax.edit(app_el, |n| n.windows = Err(AxError::CannotComplete));
    assert_eq!(
        h.act(tokens[0], frame(1.0)),
        WindowActionOutcome::NotIssued(NotIssuedReason::MembershipUnverified(
            AxError::CannotComplete
        ))
    );
    h.ax.edit(app_el, |n| n.windows = Ok(nodes.clone()));
    h.ax.edit(nodes[0], |n| n.timeout_result = Err(AxError::CannotComplete));
    assert_eq!(
        h.act(tokens[0], frame(1.0)),
        WindowActionOutcome::NotIssued(NotIssuedReason::TimeoutSetFailed(AxError::CannotComplete))
    );
    assert_eq!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::NotIssued(NotIssuedReason::TimeoutSetFailed(AxError::CannotComplete))
    );
    assert_eq!(
        count_calls(&h.ax, |c| matches!(c, Call::SetPosition(..) | Call::Raise(_))),
        0
    );
}

#[test]
fn permission_loss_degrades_the_capability_once_and_recovers() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 1, 1);
    h.events();
    h.ax.edit(app_el, |n| n.windows = Err(AxError::ApiDisabled));
    assert_eq!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::NotIssued(NotIssuedReason::PermissionDenied)
    );
    assert_eq!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::NotIssued(NotIssuedReason::PermissionDenied)
    );
    let degraded: Vec<_> = kinds(&h.events());
    assert_eq!(
        degraded,
        vec![WindowEventKind::CapabilityChanged {
            capability: Capability::Accessibility,
            available: false
        }],
        "reported once, not per failure"
    );
    h.ax.edit(app_el, |n| n.windows = Ok(nodes));
    assert!(matches!(
        h.act(tokens[0], WindowAction::Raise),
        WindowActionOutcome::Issued { .. }
    ));
    assert_eq!(kinds(&h.events()), vec![WindowEventKind::CapabilityChanged {
        capability: Capability::Accessibility,
        available: true
    }]);
}

#[test]
fn tracking_without_permission_is_denied_and_degrades() {
    let mut h = H::new();
    app_node(&h.ax, 1, vec![]);
    h.obs.set_create_error(Some(AxError::ApiDisabled));
    assert_eq!(h.track(app(1), 1), TrackOutcome::PermissionDenied);
    assert!(matches!(
        h.events()[0].kind,
        WindowEventKind::CapabilityChanged { available: false, .. }
    ));
    h.obs.set_create_error(None);
    h.obs.set_add_error(1, Some(AxError::CannotComplete));
    assert_eq!(h.track(app(1), 1), TrackOutcome::Failed(AxError::CannotComplete));
    h.obs.set_add_error(1, None);
    assert_eq!(
        h.track(app(1), 1),
        TrackOutcome::Tracking,
        "a failed install left nothing tracked"
    );
}

#[test]
fn a_snapshot_query_revalidates_and_reads_fresh_values() {
    let mut h = H::new();
    let (app_el, nodes, tokens) = h.app_with_windows(app(1), 1, 1);
    h.ax.edit(nodes[0], |n| n.position = Ok(AxPoint { x: 99.0, y: 1.0 }));
    let window = tokens[0];
    let outcome = h.run(|reply| Command::Snapshot { window, reply });
    let SnapshotOutcome::Snapshot(shot) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(shot.frame.complete().map(|f| f.origin.x), Some(99.0));
    h.ax.edit(app_el, |n| n.windows = Ok(vec![]));
    // Unlisted but alive: suspended, never retired.
    let outcome = h.run(|reply| Command::Snapshot { window, reply });
    assert_eq!(outcome, SnapshotOutcome::Stale(StaleReason::Suspended));
    // Unlisted and dead: retired.
    h.ax.kill(nodes[0]);
    let outcome = h.run(|reply| Command::Snapshot { window, reply });
    assert_eq!(outcome, SnapshotOutcome::Stale(StaleReason::NotMember));
}

// ---- cleanup, quarantine, epochs ---------------------------------------------------

#[test]
fn stop_seals_cancels_enumeration_and_releases_everything() {
    let mut h = H::new();
    let nodes: Vec<NodeId> = (0..130).map(|n| window_node(&h.ax, n)).collect();
    app_node(&h.ax, 1, nodes);
    h.track(app(1), 1);
    assert_eq!(h.stop(), StopOutcome::Clean { applications: 1 });
    assert!(h.gate.is_sealed());
    assert_eq!(h.gate.live_generation(), 0);
    h.settle();
    assert_eq!(snapshots(&h.events()).len(), 64, "no chunk ran after stop");
    assert!(h.obs.registrations(1).is_empty());
    assert!(!h.obs.is_attached(1));
    assert_eq!(h.ax.live_elements(), 0, "elements are released on the owner");
    assert_eq!(h.core.tracked_windows(), 0);
}

#[test]
fn a_failed_removal_is_quarantined_and_retries_exactly_once_per_call() {
    let mut h = H::new();
    h.app_with_windows(app(1), 500, 1);
    let registered = h.obs.registrations(500).len();
    assert_eq!(registered, 5 + 6);
    h.obs.set_remove_error(500, Some(AxError::CannotComplete));
    let attempts_before = h.obs.remove_attempts(500);
    let StopOutcome::Quarantined { applications, registrations } = h.stop() else {
        panic!("expected quarantine");
    };
    assert_eq!((applications, registrations), (1, registered));
    assert_eq!(
        h.obs.remove_attempts(500) - attempts_before,
        registered,
        "one attempt each"
    );
    assert_eq!(
        h.obs.registrations(500).len(),
        registered,
        "nothing was dropped silently"
    );
    assert!(h.ax.live_elements() > 0, "elements stay owned while quarantined");
    assert!(!h.obs.is_attached(500), "quarantined observers are inert");

    // A quarantined adapter refuses a new epoch.
    assert_eq!(
        h.install(),
        Err(InstallError::Quarantined {
            applications: 1,
            registrations: registered
        })
    );

    // A retry that still fails attempts every registration once and reports it.
    let report = h.run(|reply| Command::Retry { reply });
    assert_eq!(report.remaining_applications, 1);
    assert_eq!(report.remaining_registrations, registered);
    assert_eq!(h.obs.remove_attempts(500) - attempts_before, registered * 2);

    // Once the system cooperates the retry cleans up and installs resume.
    h.obs.set_remove_error(500, None);
    let report = h.run(|reply| Command::Retry { reply });
    assert_eq!(report, CleanupReport {
        cleaned_applications: 1,
        remaining_applications: 0,
        remaining_registrations: 0
    });
    assert!(h.obs.registrations(500).is_empty());
    assert_eq!(h.ax.live_elements(), 0);
    h.gate = Arc::new(GenerationCell::new());
    assert!(h.install().expect("install after cleanup") > h.epoch);
}

#[test]
fn registrations_of_an_exited_application_are_not_quarantined() {
    // Native run 12: System Settings terminated, its untrack failed every
    // removal with `CannotComplete` and the five registrations stayed
    // quarantined until the backend close reported them.
    let mut h = H::new();
    h.app_with_windows(app(1), 500, 0);
    h.obs.set_remove_error(500, Some(AxError::CannotComplete));
    h.obs.set_exited(500, true);
    assert_eq!(h.stop(), StopOutcome::Clean { applications: 1 });
    assert!(h.obs.registrations(500).is_empty());
    assert_eq!(h.ax.live_elements(), 0);
}

#[test]
fn a_hung_application_stays_quarantined_until_it_exits() {
    let mut h = H::new();
    h.app_with_windows(app(1), 500, 0);
    h.obs.set_remove_error(500, Some(AxError::CannotComplete));
    let StopOutcome::Quarantined {
        applications: 1,
        registrations: 5,
    } = h.stop()
    else {
        panic!("a live application that refuses the removal stays owed");
    };
    let report = h.run(|reply| Command::Retry { reply });
    assert_eq!(report.remaining_registrations, 5);
    // The application exits: the next retry finds nothing left to remove.
    h.obs.set_exited(500, true);
    let report = h.run(|reply| Command::Retry { reply });
    assert_eq!(report, CleanupReport {
        cleaned_applications: 1,
        remaining_applications: 0,
        remaining_registrations: 0
    });
}

#[test]
fn an_invalid_element_during_removal_counts_as_gone() {
    let mut h = H::new();
    h.app_with_windows(app(1), 500, 1);
    h.obs.set_remove_error(500, Some(AxError::InvalidElement));
    assert_eq!(h.stop(), StopOutcome::Clean { applications: 1 });
}

#[test]
fn old_epoch_cleanup_never_touches_a_successor() {
    let mut h = H::new();
    h.app_with_windows(app(1), 1, 1);
    let old = h.epoch;
    assert_eq!(h.stop(), StopOutcome::Clean { applications: 1 });
    h.gate = Arc::new(GenerationCell::new());
    h.gate.publish(1, 1);
    h.epoch = h.install().expect("successor");
    let nodes = vec![window_node(&h.ax, 7)];
    app_node(&h.ax, 2, nodes);
    h.track(app(2), 2);
    h.settle();
    assert_eq!(h.core.tracked_windows(), 1);
    let outcome = h.run(|reply| Command::Stop { epoch: old, reply });
    assert_eq!(outcome, StopOutcome::NotCurrent);
    assert_eq!(h.core.tracked_windows(), 1);
    assert!(!h.gate.is_sealed());
}

#[test]
fn untracking_an_application_releases_only_its_resources() {
    let mut h = H::new();
    h.app_with_windows(app(1), 1, 1);
    let nodes = vec![window_node(&h.ax, 7)];
    app_node(&h.ax, 2, nodes);
    h.track(app(2), 2);
    h.settle();
    let epoch = h.epoch;
    let outcome = h.run(|reply| Command::Untrack {
        epoch,
        application: app(1),
        reply,
    });
    assert_eq!(outcome, StopOutcome::Clean { applications: 1 });
    assert!(h.obs.registrations(1).is_empty());
    assert!(!h.obs.registrations(2).is_empty());
    assert_eq!(h.core.tracked_windows(), 1);
}

// ---- unwind-only panic containment --------------------------------------------------

/// UNWIND-ONLY: proves containment in unwind-enabled (test/dev) builds. The
/// release profile uses `panic = "abort"`, so this is not release recovery
/// evidence.
#[test]
fn unwind_only_a_panic_in_owner_work_poisons_the_core_and_seals_the_gate() {
    let mut h = H::new();
    let (_, _, tokens) = h.app_with_windows(app(1), 1, 1);
    h.ax.on_write(|| panic!("injected"));
    let request = ActionRequest {
        issuance: h.issuance(tokens[0]),
        action: frame(1.0),
    };
    let slot = h.send(|reply| Command::Act { request, reply });
    h.turn();
    assert!(
        slot.lock().unwrap_or_else(PoisonError::into_inner).is_none(),
        "reply dropped"
    );
    assert!(h.core.is_poisoned());
    assert!(h.gate.is_sealed());
    // Later commands are dropped, never executed.
    let request = ActionRequest {
        issuance: h.issuance(tokens[0]),
        action: WindowAction::Raise,
    };
    let slot = h.send(|reply| Command::Act { request, reply });
    h.turn();
    assert!(slot.lock().unwrap_or_else(PoisonError::into_inner).is_none());
}

// ---- compile-time assertions --------------------------------------------------------

const _: () = {
    const fn send_sync<T: Send + Sync + 'static>() {}
    const fn send<T: Send + 'static>() {}
    send_sync::<WindowAdapter>();
    send_sync::<EventQueue>();
    send_sync::<GenerationCell>();
    send_sync::<WindowIssuance>();
    send_sync::<WindowInstance>();
    send::<Command>();
    send::<WindowEvent>();
    send::<WindowSnapshot>();
    send::<WindowActionOutcome>();
};

// ---- the real thread, the real run loop, fake AX ----------------------------------------

static NAME_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn unique_name() -> String {
    format!(
        "stache-test-ax-owner-{}",
        NAME_SEQUENCE.fetch_add(1, Ordering::SeqCst)
    )
}

struct Live {
    adapter: Arc<WindowAdapter>,
    ax: FakeWindowAx,
    obs: FakeObservers,
    queue: Arc<EventQueue>,
    gate: Arc<GenerationCell>,
    epoch: RuntimeEpoch,
    window: WindowInstance,
}

fn live(windows: u32) -> Live {
    let ax = FakeWindowAx::new();
    let nodes: Vec<NodeId> = (0..windows).map(|n| window_node(&ax, 300 + n)).collect();
    app_node(&ax, 900, nodes);
    let (system, obs) = FakeWindowSystem::factory(ax.clone());
    let adapter = Arc::new(
        WindowAdapter::spawn(
            &unique_name(),
            &AdapterOptions::default(),
            Arc::new(SystemWindowClock),
            system,
            RunLoopDriver::new,
        )
        .expect("spawn"),
    );
    let queue = EventQueue::new(256);
    let gate = Arc::new(GenerationCell::new());
    gate.publish(1, 1);
    let epoch = adapter
        .install(
            InstallRequest {
                queue: Arc::clone(&queue),
                gate: Arc::clone(&gate),
            },
            None,
        )
        .expect("install");
    let outcome = adapter.track_application(
        epoch,
        TrackRequest {
            application: app(1),
            pid: 900,
            cancel: CancellationToken::new(),
            deadline: None,
        },
        None,
    );
    assert_eq!(outcome, TrackOutcome::Tracking);
    let mut seen = Vec::new();
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end
        && !seen
            .iter()
            .any(|e: &WindowEvent| matches!(e.kind, WindowEventKind::EnumerationComplete { .. }))
    {
        seen.extend(queue.drain().events);
        thread::sleep(Duration::from_millis(2));
    }
    let window = snapshots(&seen).first().map(|s| s.window).expect("window");
    Live {
        adapter,
        ax,
        obs,
        queue,
        gate,
        epoch,
        window,
    }
}

impl Live {
    fn request(&self, action: WindowAction, deadline: Duration) -> ActionRequest {
        ActionRequest {
            issuance: WindowIssuance::new(self.window, 1, 1, 1, Instant::now() + deadline),
            action,
        }
    }

    /// Stalls the first native write until `release` is sent.
    fn stall_first_write(&self) -> (Receiver<()>, SyncSender<()>) {
        let (entered_tx, entered_rx) = mpsc::sync_channel::<()>(1);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let release_rx = Mutex::new(release_rx);
        let first = AtomicBool::new(true);
        self.ax.on_write(move || {
            if first.swap(false, Ordering::SeqCst) {
                let _ = entered_tx.try_send(());
                let _ = release_rx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv_timeout(Duration::from_secs(10));
            }
        });
        (entered_rx, release_tx)
    }
}

#[test]
fn the_owner_thread_runs_a_real_run_loop_and_serves_commands() {
    let live = live(1);
    let outcome = live.adapter.act(
        live.request(WindowAction::Raise, Duration::from_secs(5)),
        &CancellationToken::new(),
    );
    assert!(matches!(outcome, WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        ..
    }));
    let resolved = live.adapter.resolve_window_id(300, &CancellationToken::new(), None);
    assert_eq!(resolved, Some(live.window));
    assert!(matches!(
        live.adapter.snapshot(live.window, &CancellationToken::new(), None),
        SnapshotOutcome::Snapshot(_)
    ));
    assert_eq!(
        live.adapter.stop(live.epoch, &live.gate, None),
        StopOutcome::Clean { applications: 1 }
    );
    assert_eq!(live.adapter.close(), CloseOutcome::Closed);
    assert_eq!(live.ax.live_elements(), 0);
}

#[test]
fn invalidation_is_lock_free_while_a_native_send_is_stalled() {
    let live = live(1);
    let (entered, release) = live.stall_first_write();
    let adapter = Arc::clone(&live.adapter);
    let request = live.request(frame(5.0), Duration::from_secs(30));
    let issuance = request.issuance.clone();
    let sender = thread::spawn(move || adapter.act(request, &CancellationToken::new()));
    entered.recv_timeout(Duration::from_secs(5)).expect("send entered");

    let started = Instant::now();
    assert!(live.gate.invalidate(1), "the generation was live");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "returns while the send is stalled"
    );
    assert_eq!(
        issuance.cancel(),
        IssuanceCancel::AlreadyIssued,
        "an issued send cannot be undone"
    );

    // A later action under the dead generation never reaches the native layer.
    let second = live.adapter.act(
        live.request(frame(6.0), Duration::from_millis(200)),
        &CancellationToken::new(),
    );
    assert_eq!(
        second,
        WindowActionOutcome::ExpiredBeforeSend,
        "queued behind the stalled send, abandoned unsent at its deadline"
    );
    release.send(()).expect("release");
    let first = sender.join().expect("sender");
    assert!(matches!(first, WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        ..
    }));
    assert_eq!(count_calls(&live.ax, |c| matches!(c, Call::SetPosition(..))), 1);
    // After release the owner drains the abandoned command without sending.
    let third = live.adapter.act(
        live.request(frame(7.0), Duration::from_secs(5)),
        &CancellationToken::new(),
    );
    assert_eq!(
        third,
        WindowActionOutcome::NotIssued(NotIssuedReason::GenerationInvalid)
    );
    assert_eq!(count_calls(&live.ax, |c| matches!(c, Call::SetPosition(..))), 1);
}

#[test]
fn a_timeout_after_the_send_committed_is_uncertain_and_an_unsent_one_is_not() {
    let live = live(1);
    let (entered, release) = live.stall_first_write();
    let adapter = Arc::clone(&live.adapter);
    let request = live.request(frame(5.0), Duration::from_millis(250));
    let sender = thread::spawn(move || adapter.act(request, &CancellationToken::new()));
    entered.recv_timeout(Duration::from_secs(5)).expect("send entered");

    // Queued behind the stalled send with a short deadline: expires unsent.
    let started = Instant::now();
    let queued = live.adapter.act(
        live.request(frame(6.0), Duration::from_millis(100)),
        &CancellationToken::new(),
    );
    assert_eq!(queued, WindowActionOutcome::ExpiredBeforeSend);
    assert!(started.elapsed() < Duration::from_secs(2));

    // The stalled one passes its deadline after commit: Uncertain, not "not applied".
    let first = sender.join().expect("sender");
    assert_eq!(first, WindowActionOutcome::uncertain_for_tests());
    release.send(()).expect("release");
    // The abandoned queued action never sends once the owner resumes.
    let _ = live.adapter.snapshot(live.window, &CancellationToken::new(), None);
    assert_eq!(count_calls(&live.ax, |c| matches!(c, Call::SetPosition(..))), 1);
}

impl WindowActionOutcome {
    const fn uncertain_for_tests() -> Self {
        Self::Issued {
            kind: IssuedKind::Uncertain,
            native: NativeStatus::Unobserved,
            observed: None,
        }
    }
}

#[test]
fn caller_cancellation_is_observed_within_a_quantum_and_stays_truthful() {
    let live = live(1);
    let (entered, release) = live.stall_first_write();
    let adapter = Arc::clone(&live.adapter);
    let request = live.request(frame(5.0), Duration::from_secs(30));
    let first = thread::spawn(move || adapter.act(request, &CancellationToken::new()));
    entered.recv_timeout(Duration::from_secs(5)).expect("send entered");

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        trigger.cancel();
    });
    let started = Instant::now();
    let outcome = live.adapter.act(live.request(frame(6.0), Duration::from_secs(30)), &cancel);
    assert_eq!(
        outcome,
        WindowActionOutcome::NotIssued(NotIssuedReason::AbandonedBeforeSend)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    canceller.join().expect("canceller");
    release.send(()).expect("release");
    let _ = first.join().expect("first");
    assert_eq!(count_calls(&live.ax, |c| matches!(c, Call::SetPosition(..))), 1);
}

#[test]
fn a_full_queue_and_a_closed_adapter_reject_dispatch_without_queueing() {
    let live = live(1);
    let (entered, release) = live.stall_first_write();
    let adapter = Arc::clone(&live.adapter);
    let request = live.request(frame(5.0), Duration::from_secs(30));
    let first = thread::spawn(move || adapter.act(request, &CancellationToken::new()));
    entered.recv_timeout(Duration::from_secs(5)).expect("send entered");
    let mut queued = 0;
    let rejected = loop {
        let request = live.request(WindowAction::Raise, Duration::from_secs(30));
        match live.adapter.submit_action(request, |_| {}) {
            Ok(()) => queued += 1,
            Err(error) => break error,
        }
        assert!(queued <= OWNER_QUEUE_BOUND, "the queue is bounded");
    };
    assert_eq!(rejected, DispatchError::Full);
    assert_eq!(queued, OWNER_QUEUE_BOUND);
    release.send(()).expect("release");
    let _ = first.join().expect("first");
}

#[test]
fn unwind_only_a_lost_acknowledgement_after_commit_is_uncertain() {
    // UNWIND-ONLY: the owner panics inside the native write (after the commit),
    // its reply is dropped, and the waiting side reports Uncertain. Release
    // builds abort instead; this is not release recovery evidence.
    let live = live(1);
    live.ax.on_write(|| panic!("injected owner panic"));
    let outcome = live.adapter.act(
        live.request(frame(1.0), Duration::from_secs(5)),
        &CancellationToken::new(),
    );
    assert_eq!(outcome, WindowActionOutcome::uncertain_for_tests());
    // The poisoned owner refuses everything afterwards; nothing is sent.
    let after = live.adapter.act(
        live.request(WindowAction::Raise, Duration::from_secs(5)),
        &CancellationToken::new(),
    );
    assert!(after.proves_no_send());
    assert!(live.gate.is_sealed());
}

#[test]
fn the_handle_keeps_install_refused_until_a_quarantine_is_retried_away() {
    let live = live(1);
    live.obs.set_remove_error(900, Some(AxError::CannotComplete));
    let outcome = live.adapter.stop(live.epoch, &live.gate, None);
    let StopOutcome::Quarantined { applications: 1, registrations } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(registrations > 0);
    assert!(live.adapter.cleanup_unconfirmed());
    let request = || InstallRequest {
        queue: Arc::clone(&live.queue),
        gate: Arc::new(GenerationCell::new()),
    };
    assert!(matches!(
        live.adapter.install(request(), None),
        Err(InstallError::Quarantined { .. })
    ));
    // Closing keeps the thread alive to hold the quarantined resources.
    assert!(matches!(live.adapter.close(), CloseOutcome::Quarantined { .. }));
    live.obs.set_remove_error(900, None);
    let report = live.adapter.retry_cleanup(&CancellationToken::new(), None).expect("report");
    assert_eq!(report.remaining_applications, 0);
    assert!(!live.adapter.cleanup_unconfirmed());
    let next = live.adapter.install(request(), None).expect("install after retry");
    assert!(next > live.epoch);
}

#[test]
fn thread_names_are_distinct_and_menu_anywheres_is_rejected() {
    let spawn = |name: &str| {
        WindowAdapter::spawn(
            name,
            &AdapterOptions::default(),
            Arc::new(SystemWindowClock),
            || FakeWindowSystem::new(FakeWindowAx::new()).0,
            RunLoopDriver::new,
        )
    };
    assert_eq!(
        spawn("stache-ax-owner").err(),
        Some(SpawnError::ReservedThreadName)
    );
    assert_eq!(spawn("").err(), Some(SpawnError::EmptyThreadName));
    let name = unique_name();
    let first = spawn(&name).expect("first");
    assert_eq!(spawn(&name).err(), Some(SpawnError::ThreadNameInUse));
    assert_eq!(first.close(), CloseOutcome::Closed);
    assert_eq!(first.close(), CloseOutcome::AlreadyClosed);
    // The name is released once the owner thread ends.
    let mut reused = None;
    for _ in 0..200 {
        match spawn(&name) {
            Ok(adapter) => {
                reused = Some(adapter);
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    }
    let reused = reused.expect("name released after close");
    assert_eq!(reused.close(), CloseOutcome::Closed);
}

#[test]
fn tokens_minted_by_two_live_adapters_never_cross() {
    let one = live(1);
    let two = live(1);
    assert_ne!(one.adapter.id(), two.adapter.id());
    assert_eq!(one.window.adapter(), one.adapter.id());
    let outcome = two.adapter.act(
        one.request(WindowAction::Raise, Duration::from_secs(5)),
        &CancellationToken::new(),
    );
    assert_eq!(outcome, WindowActionOutcome::Stale(StaleReason::WrongAdapter));
}

#[test]
fn a_dropped_handle_stops_the_owner_and_releases_elements() {
    let live = live(2);
    let ax = live.ax.clone();
    assert!(ax.live_elements() > 0);
    drop(live);
    let end = Instant::now() + Duration::from_secs(5);
    while ax.live_elements() > 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(ax.live_elements(), 0);
}

// ---- cancellable requests ---------------------------------------------------------

/// A driver whose first park after `armed` is set stalls the owner, so no
/// reply arrives meanwhile.
struct StalledDriver {
    waker: Arc<CountingWaker>,
    armed: Arc<AtomicBool>,
    stalled: bool,
}

use super::testing::CountingWaker;

impl Driver for StalledDriver {
    fn waker(&self) -> Arc<dyn Waker> { self.waker.clone() }

    fn park(&mut self, _timeout: Option<Duration>) {
        if !self.stalled && self.armed.load(Ordering::SeqCst) {
            self.stalled = true;
            thread::sleep(Duration::from_millis(1500));
        }
    }
}

#[test]
fn a_blocking_request_observes_cancellation_within_the_quantum_not_the_hop() {
    let ax = FakeWindowAx::new();
    app_node(&ax, 900, Vec::new());
    let (system, _obs) = FakeWindowSystem::factory(ax);
    let armed = Arc::new(AtomicBool::new(false));
    let shared = Arc::clone(&armed);
    let adapter = Arc::new(
        WindowAdapter::spawn(
            &unique_name(),
            &AdapterOptions::default(),
            Arc::new(SystemWindowClock),
            system,
            move || StalledDriver {
                waker: Arc::new(CountingWaker::default()),
                armed: Arc::clone(&shared),
                stalled: false,
            },
        )
        .expect("spawn"),
    );
    let gate = Arc::new(GenerationCell::new());
    gate.publish(1, 1);
    let epoch = adapter
        .install(
            InstallRequest {
                queue: EventQueue::new(16),
                gate,
            },
            None,
        )
        .expect("install");
    let tracked = adapter.track_application(
        epoch,
        TrackRequest {
            application: app(1),
            pid: 900,
            cancel: CancellationToken::new(),
            deadline: None,
        },
        None,
    );
    assert_eq!(tracked, TrackOutcome::Tracking);
    armed.store(true, Ordering::SeqCst);
    // The idle owner spins through `park`, so the stall begins at once.
    thread::sleep(Duration::from_millis(100));
    let cancel = CancellationToken::new();
    let canceller = cancel.clone();
    let trigger = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        canceller.cancel();
    });
    let window = WindowInstance::for_testing(adapter.id(), epoch, app(1), 1);
    let started = Instant::now();
    let outcome = adapter.snapshot(window, &cancel, None);
    let waited = started.elapsed();
    trigger.join().expect("trigger");
    assert_eq!(outcome, SnapshotOutcome::Unacknowledged);
    assert!(waited < Duration::from_millis(500), "waited {waited:?}");
    assert!(waited < ACKNOWLEDGEMENT_BUDGET);
}

// ---- application-level AXHidden (the public hide/unhide fallback) ------------

impl H {
    fn hidden(
        &mut self,
        application: ApplicationId,
        operation: HiddenOperation,
        issuance: Option<ApplicationIssuance>,
    ) -> HiddenOutcome {
        let request = HiddenRequest {
            epoch: self.epoch,
            application,
            operation,
            issuance,
        };
        self.run(|reply| Command::Hidden { request, reply })
    }

    fn hidden_issuance(&self, application: ApplicationId) -> ApplicationIssuance {
        ApplicationIssuance::new(application, self.clock.peek() + Duration::from_secs(30))
    }
}

fn tracked_app(h: &mut H, token: u64, pid: i32) -> (ApplicationId, NodeId) {
    let application = app(token);
    let (node, _, _) = h.app_with_windows(application, pid, 0);
    h.ax.edit(node, |n| {
        n.bools.insert(BoolAttribute::Hidden, Ok(false));
    });
    (application, node)
}

#[test]
fn axhidden_is_read_from_the_exact_tracked_application_only() {
    let mut h = H::new();
    let (application, node) = tracked_app(&mut h, 1, 10);
    h.ax.clear_calls();
    assert_eq!(
        h.hidden(application, HiddenOperation::Read, None),
        HiddenOutcome::Read(AxReading::Value(false))
    );
    assert_eq!(h.ax.calls(), vec![
        Call::SetTimeout(node, ELEMENT_TIMEOUT),
        Call::Bool(node, BoolAttribute::Hidden),
    ]);
    // A different instance (even a valid token) was never tracked.
    assert_eq!(
        h.hidden(app(2), HiddenOperation::Read, None),
        HiddenOutcome::Stale(StaleReason::ApplicationGone)
    );
}

#[test]
fn an_axhidden_write_is_one_shot_timed_and_read_back() {
    let mut h = H::new();
    let (application, node) = tracked_app(&mut h, 1, 10);
    h.ax.clear_calls();
    let issuance = h.hidden_issuance(application);
    let outcome = h.hidden(application, HiddenOperation::Set(true), Some(issuance.clone()));
    assert_eq!(outcome, HiddenOutcome::Written {
        kind: IssuedKind::Applied,
        status: Ok(()),
        observed: AxReading::Value(true),
    });
    assert!(issuance.is_issued());
    assert_eq!(h.ax.calls(), vec![
        Call::SetTimeout(node, PRESS_TIMEOUT),
        Call::SetHidden(node, true),
        Call::SetTimeout(node, ELEMENT_TIMEOUT),
        Call::Bool(node, BoolAttribute::Hidden),
    ]);
    // The same authorization cannot send twice.
    assert_eq!(
        h.hidden(application, HiddenOperation::Set(false), Some(issuance)),
        HiddenOutcome::NotIssued(NotIssuedReason::AlreadyUsed)
    );
}

#[test]
fn a_cancelled_expired_or_missing_authorization_sends_nothing() {
    let mut h = H::new();
    let (application, node) = tracked_app(&mut h, 1, 10);
    let cancelled = h.hidden_issuance(application);
    assert_eq!(cancelled.cancel(), IssuanceCancel::Cancelled);
    assert_eq!(
        h.hidden(application, HiddenOperation::Set(true), Some(cancelled)),
        HiddenOutcome::NotIssued(NotIssuedReason::Cancelled)
    );
    let expired = ApplicationIssuance::new(application, h.clock.peek());
    assert_eq!(
        h.hidden(application, HiddenOperation::Set(true), Some(expired)),
        HiddenOutcome::ExpiredBeforeSend
    );
    assert_eq!(
        h.hidden(application, HiddenOperation::Set(true), None),
        HiddenOutcome::NotIssued(NotIssuedReason::EmptyRequest)
    );
    assert_eq!(
        count_calls(&h.ax, |call| matches!(call, Call::SetHidden(n, _) if *n == node)),
        0
    );
}

#[test]
fn an_axhidden_write_is_not_refused_by_the_sealed_frame_gate() {
    let mut h = H::new();
    let (application, _) = tracked_app(&mut h, 1, 10);
    h.gate.seal();
    let issuance = h.hidden_issuance(application);
    assert!(matches!(
        h.hidden(application, HiddenOperation::Set(false), Some(issuance)),
        HiddenOutcome::Written { kind: IssuedKind::Applied, .. }
    ));
}

#[test]
fn an_axhidden_failure_is_typed_never_applied() {
    let mut h = H::new();
    let (application, node) = tracked_app(&mut h, 1, 10);
    h.ax.edit(node, |n| n.set_hidden_result = Err(AxError::CannotComplete));
    let uncertain = h.hidden(
        application,
        HiddenOperation::Set(true),
        Some(h.hidden_issuance(application)),
    );
    assert!(matches!(uncertain, HiddenOutcome::Written {
        kind: IssuedKind::Uncertain,
        status: Err(AxError::CannotComplete),
        observed: AxReading::Value(false),
    }));
    h.ax.edit(node, |n| {
        n.set_hidden_result = Err(AxError::AttributeUnsupported);
    });
    let rejected = h.hidden(
        application,
        HiddenOperation::Set(true),
        Some(h.hidden_issuance(application)),
    );
    assert!(matches!(rejected, HiddenOutcome::Written {
        kind: IssuedKind::Rejected,
        ..
    }));
}

#[test]
fn an_axhidden_request_after_the_epoch_ends_is_stale() {
    let mut h = H::new();
    let (application, _) = tracked_app(&mut h, 1, 10);
    let _ = h.stop();
    assert_eq!(
        h.hidden(application, HiddenOperation::Read, None),
        HiddenOutcome::Stale(StaleReason::WrongEpoch)
    );
}

mod frames;
mod lease;
mod shards;
mod suspended;
mod tab_bars;
