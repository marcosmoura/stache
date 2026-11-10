//! KB-04-01: the chord state machine, first alone and then through the shared
//! press ledger.

use std::sync::{Arc, Mutex, PoisonError};

use stache_sdk::CancellationToken;
use stache_sdk::input::testing::{CapacityWork, FakeBackend, FakeClock};
use stache_sdk::input::{
    Disposition, HandlerSpec, InputBackend, InputClock, InputEventKind, InputFilter, InputPriority,
    InputService, KeyCode, KindSet, ModifierFlags, PressStream, RawInput, TapKind, WorkRequirement,
    WorkReservations,
};

use crate::caps::handler::{CapsHandler, CapsSink};
use crate::caps::parser::CapsKey;
use crate::caps::state::{CapsDecision, CapsInput, CapsState, F18_KEY};

const F18: u16 = 0x4F;
const S: u16 = 1;
const A: u16 = 0;
const X: u16 = 7;

fn down(key: u16) -> CapsInput { CapsInput::Down { key, repeat: false } }

fn repeat(key: u16) -> CapsInput { CapsInput::Down { key, repeat: true } }

fn up(key: u16) -> CapsInput { CapsInput::Up { key } }

fn bound(key: u16) -> bool { key == S || key == A }

use CapsDecision::{Execute, Pass, Suppress};

fn run(trace: &[(CapsInput, CapsDecision)], dispatch: bool) {
    let state = CapsState::new();
    for (index, (input, expected)) in trace.iter().enumerate() {
        assert_eq!(
            state.step(*input, bound, dispatch),
            *expected,
            "step {index}: {input:?}"
        );
    }
}

#[test]
fn a_bare_caps_tap_does_nothing_and_never_leaks_f18() {
    run(&[(down(F18), Suppress), (up(F18), Suppress)], true);
    // Auto-repeat of the surrogate is swallowed too, and so is a stray up.
    run(
        &[
            (down(F18), Suppress),
            (repeat(F18), Suppress),
            (repeat(F18), Suppress),
            (up(F18), Suppress),
        ],
        true,
    );
    run(&[(up(F18), Suppress)], true);
}

#[test]
fn one_dispatch_per_discrete_chord_press() {
    run(
        &[
            (down(F18), Suppress),
            (down(S), Execute(CapsKey(S))),
            (repeat(S), Suppress),
            (repeat(S), Suppress),
            (up(S), Suppress),
            (up(F18), Suppress),
        ],
        true,
    );
}

#[test]
fn releasing_and_repressing_a_chord_under_held_caps_dispatches_again() {
    run(
        &[
            (down(F18), Suppress),
            (down(S), Execute(CapsKey(S))),
            (up(S), Suppress),
            (down(S), Execute(CapsKey(S))),
            (up(S), Suppress),
            (down(A), Execute(CapsKey(A))),
            (up(A), Suppress),
        ],
        true,
    );
}

#[test]
fn the_chord_key_up_is_suppressed_after_caps_is_released() {
    run(
        &[
            (down(F18), Suppress),
            (down(S), Execute(CapsKey(S))),
            (up(F18), Suppress),
            (repeat(S), Suppress),
            (up(S), Suppress),
            // Afterwards the key behaves like any other without Caps.
            (down(S), Pass),
            (up(S), Pass),
        ],
        true,
    );
}

#[test]
fn a_duplicate_caps_down_does_not_rearm_and_one_chord_is_active_at_a_time() {
    run(
        &[
            (down(F18), Suppress),
            (down(S), Execute(CapsKey(S))),
            (down(F18), Suppress),
            (down(A), Pass),
            (up(A), Pass),
            (repeat(S), Suppress),
            (up(S), Suppress),
        ],
        true,
    );
}

#[test]
fn unbound_keys_and_keys_without_caps_pass() {
    run(
        &[
            (down(S), Pass),
            (up(S), Pass),
            (down(F18), Suppress),
            (down(X), Pass),
            (repeat(X), Pass),
            (up(X), Pass),
            (up(F18), Suppress),
        ],
        true,
    );
}

#[test]
fn with_dispatch_invalidated_the_surrogate_is_swallowed_but_nothing_executes() {
    run(
        &[
            (down(F18), Suppress),
            (down(S), Pass),
            (up(S), Pass),
            (repeat(F18), Suppress),
            (up(F18), Suppress),
        ],
        false,
    );
}

#[test]
fn clearing_and_cancelling_drop_armed_state() {
    let state = CapsState::new();
    state.step(down(F18), bound, true);
    state.step(down(S), bound, true);
    assert!(state.is_held());
    assert_eq!(state.active_key(), Some(S));
    state.cancel_key(S);
    assert_eq!(state.active_key(), None);
    assert!(state.is_held());
    state.cancel_key(F18);
    assert!(!state.is_armed());
    state.step(down(F18), bound, true);
    state.step(down(A), bound, true);
    state.clear();
    assert!(!state.is_armed());
}

// ---- the same traces through the real router and press ledger ----

#[derive(Default)]
struct Sink {
    keys: Mutex<Vec<CapsKey>>,
}

impl Sink {
    fn taken(&self) -> Vec<u16> {
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|k| k.0)
            .collect()
    }
}

impl CapsSink for Sink {
    fn execute(&self, key: CapsKey) {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner).push(key);
    }
}

struct Rig {
    service: Arc<InputService>,
    backend: Arc<FakeBackend>,
    handler: Arc<CapsHandler>,
    sink: Arc<Sink>,
    _subscription: stache_sdk::input::Subscription,
}

fn rig() -> Rig {
    let clock = FakeClock::new();
    let backend = FakeBackend::new(&clock);
    let service = InputService::with_clock(
        Arc::clone(&backend) as Arc<dyn InputBackend>,
        CapacityWork::new(8) as Arc<dyn WorkReservations>,
        Arc::clone(&clock) as Arc<dyn InputClock>,
    );
    let sink = Arc::new(Sink::default());
    let handler = Arc::new(CapsHandler::new(
        [CapsKey(S), CapsKey(A)],
        Arc::clone(&sink) as _,
    ));
    let filter = InputFilter::new(KindSet::of(InputEventKind::KeyDown).with(InputEventKind::KeyUp))
        .with_keys([F18_KEY, KeyCode(S), KeyCode(A)]);
    let subscription = service
        .subscribe_handler(
            HandlerSpec {
                filter,
                priority: InputPriority::default(),
                work: WorkRequirement::None,
            },
            Arc::clone(&handler) as _,
            &CancellationToken::new(),
        )
        .expect("subscribe");
    Rig {
        service,
        backend,
        handler,
        sink,
        _subscription: subscription,
    }
}

fn raw(kind: InputEventKind, key: u16, repeat: bool) -> RawInput {
    RawInput {
        kind,
        stream: PressStream::Key(KeyCode(key)),
        flags: ModifierFlags::NONE,
        repeat,
        timestamp: std::time::Duration::ZERO,
        location: None,
    }
}

impl Rig {
    fn send(&self, kind: InputEventKind, key: u16, repeat: bool) -> Disposition {
        self.service.dispatch(TapKind::Suppressing, raw(kind, key, repeat))
    }

    fn down(&self, key: u16) -> Disposition { self.send(InputEventKind::KeyDown, key, false) }

    fn repeat(&self, key: u16) -> Disposition { self.send(InputEventKind::KeyDown, key, true) }

    fn up(&self, key: u16) -> Disposition { self.send(InputEventKind::KeyUp, key, false) }
}

#[test]
fn the_ledger_swallows_repeats_and_matched_ups_even_after_caps_release() {
    let rig = rig();
    assert_eq!(rig.down(F18), Disposition::Suppress);
    assert_eq!(rig.down(S), Disposition::Suppress);
    assert_eq!(rig.sink.taken(), [S]);
    assert_eq!(rig.up(F18), Disposition::Suppress);
    assert_eq!(rig.repeat(S), Disposition::Suppress);
    assert_eq!(rig.up(S), Disposition::Suppress);
    // A plain S afterwards passes and dispatches nothing.
    assert_eq!(rig.down(S), Disposition::Pass);
    assert_eq!(rig.up(S), Disposition::Pass);
    assert_eq!(rig.sink.taken(), [S]);
    assert_eq!(rig.service.ledger_len(), 0, "no press leaks in the ledger");
}

#[test]
fn bare_taps_repeats_and_re_presses_through_the_service() {
    let rig = rig();
    assert_eq!(rig.down(F18), Disposition::Suppress);
    assert_eq!(rig.repeat(F18), Disposition::Suppress);
    assert_eq!(rig.up(F18), Disposition::Suppress);
    assert!(rig.sink.taken().is_empty());

    rig.down(F18);
    for _ in 0..3 {
        assert_eq!(rig.down(A), Disposition::Suppress);
        assert_eq!(rig.repeat(A), Disposition::Suppress);
        assert_eq!(rig.up(A), Disposition::Suppress);
    }
    assert_eq!(rig.sink.taken(), [A, A, A]);
    // Unbound keys pass entirely.
    assert_eq!(rig.down(X), Disposition::Pass);
    assert_eq!(rig.up(X), Disposition::Pass);
    rig.up(F18);
    assert_eq!(rig.service.ledger_len(), 0);
}

#[test]
fn a_real_f18_press_is_swallowed_like_the_surrogate() {
    // The global remap has no device identity, so a physical F18 key behaves
    // exactly like the Caps surrogate while the mapping is active.
    let rig = rig();
    assert_eq!(rig.down(F18), Disposition::Suppress);
    assert_eq!(rig.up(F18), Disposition::Suppress);
    assert!(rig.sink.taken().is_empty());
}

#[test]
fn tap_loss_cancels_dispatch_and_a_re_enable_never_resumes_an_armed_chord() {
    let rig = rig();
    rig.down(F18);
    rig.down(S);
    assert_eq!(rig.sink.taken(), [S]);
    rig.service.tap_disabled(TapKind::Suppressing);
    assert!(rig.handler.tap_lost());
    assert!(!rig.handler.is_dispatching());
    assert!(!rig.handler.state().is_armed());
    // Events after the discontinuity are orphans and pass; nothing resumes.
    assert_eq!(rig.repeat(S), Disposition::Pass);
    assert_eq!(rig.up(S), Disposition::Pass);
    rig.down(F18);
    assert_eq!(rig.down(A), Disposition::Pass);
    assert_eq!(rig.sink.taken(), [S], "no command after the discontinuity");
    drop(rig.backend);
}

#[test]
fn reload_mid_press_keeps_swallowing_the_owned_press_and_starts_fresh() {
    let rig = rig();
    rig.down(F18);
    rig.down(S);
    assert_eq!(rig.sink.taken(), [S]);
    // A reload invalidates the old handler; its owned presses are tombstoned.
    rig.handler.invalidate();
    assert_eq!(rig.repeat(S), Disposition::Suppress);
    assert_eq!(rig.up(S), Disposition::Suppress);
    assert_eq!(rig.up(F18), Disposition::Suppress);
    // The old handler can no longer enqueue anything.
    rig.down(F18);
    assert_eq!(rig.down(S), Disposition::Pass);
    assert_eq!(rig.sink.taken(), [S]);
}
