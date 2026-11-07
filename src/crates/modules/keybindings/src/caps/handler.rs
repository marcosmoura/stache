//! The suppressing input handler for Caps chords.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use stache_sdk::input::{
    CallbackScope, Disposition, InputEvent, InputEventKind, InputHandler, PressId, PressStream,
};

use super::parser::CapsKey;
use super::state::{CapsDecision, CapsInput, CapsState};

/// Where an accepted chord goes. Implementations must only enqueue: they run on
/// the event-tap callback and must never block, allocate without a bound, log,
/// query native state or run a command.
pub trait CapsSink: Send + Sync {
    /// Accepts one discrete bound chord press.
    fn execute(&self, key: CapsKey);
}

/// A sink that drops everything, for the safety-only subscription and tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSink;

impl CapsSink for NoSink {
    fn execute(&self, _key: CapsKey) {}
}

/// The Caps handler.
///
/// Once [`CapsHandler::invalidate`] runs it can never enqueue a command again,
/// but it keeps swallowing the F18 surrogate and the key-ups of presses it
/// already owns, so a pending restore never leaks F18 to applications.
pub struct CapsHandler {
    state: CapsState,
    /// Sorted, deduplicated bound key codes; allocated once.
    bound: Box<[u16]>,
    dispatch: AtomicBool,
    tap_lost: AtomicBool,
    sink: Arc<dyn CapsSink>,
}

impl CapsHandler {
    /// A handler for `bound` keys that dispatches into `sink`.
    #[must_use]
    pub fn new(bound: impl IntoIterator<Item = CapsKey>, sink: Arc<dyn CapsSink>) -> Self {
        let mut keys: Vec<u16> = bound.into_iter().map(|key| key.0).collect();
        keys.sort_unstable();
        keys.dedup();
        Self {
            state: CapsState::new(),
            bound: keys.into_boxed_slice(),
            dispatch: AtomicBool::new(true),
            tap_lost: AtomicBool::new(false),
            sink,
        }
    }

    /// The bound key codes, ascending.
    #[must_use]
    pub fn bound_keys(&self) -> &[u16] { &self.bound }

    /// Stops dispatch immediately and clears every armed press.
    pub fn invalidate(&self) {
        self.dispatch.store(false, Ordering::SeqCst);
        self.state.clear();
    }

    /// Whether the handler may still enqueue commands.
    #[must_use]
    pub fn is_dispatching(&self) -> bool { self.dispatch.load(Ordering::SeqCst) }

    /// Whether the tap reported a discontinuity for a held press.
    #[must_use]
    pub fn tap_lost(&self) -> bool { self.tap_lost.load(Ordering::SeqCst) }

    /// The chord state.
    #[must_use]
    pub const fn state(&self) -> &CapsState { &self.state }
}

impl InputHandler for CapsHandler {
    fn on_event(&self, event: &InputEvent, _scope: &mut CallbackScope) -> Disposition {
        let PressStream::Key(code) = event.stream() else {
            return Disposition::Pass;
        };
        let input = match event.kind() {
            InputEventKind::KeyDown => CapsInput::Down {
                key: code.0,
                repeat: event.repeat(),
            },
            InputEventKind::KeyUp => CapsInput::Up { key: code.0 },
            _ => return Disposition::Pass,
        };
        let decision = self.state.step(
            input,
            |key| self.bound.binary_search(&key).is_ok(),
            self.dispatch.load(Ordering::SeqCst),
        );
        match decision {
            CapsDecision::Pass => Disposition::Pass,
            CapsDecision::Suppress => Disposition::Suppress,
            CapsDecision::Execute(key) => {
                // Re-read: an invalidation that raced the step still wins.
                if self.dispatch.load(Ordering::SeqCst) {
                    self.sink.execute(key);
                }
                Disposition::Suppress
            }
        }
    }

    fn on_cancel(&self, press: PressId) {
        // Tap loss: cancel dispatch and the armed press; no later re-enable can
        // resume an old chord.
        self.dispatch.store(false, Ordering::SeqCst);
        self.tap_lost.store(true, Ordering::SeqCst);
        if let PressStream::Key(code) = press.stream {
            self.state.cancel_key(code.0);
        }
        self.state.clear();
    }
}
