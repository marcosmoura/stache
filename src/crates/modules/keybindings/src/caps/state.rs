//! The pure, preallocated Caps chord state.
//!
//! Caps Lock is remapped to F18 at the HID layer, so macOS never sees Caps
//! Lock: capitalization and the LED never toggle. This state decides only
//! whether a key pressed while the surrogate is held forms a bound chord.
//!
//! Two atomics hold everything, so it needs no lock, no allocation and no
//! command cloning inside the tap callback. Which events *reach* the state, and
//! that repeats and the matching key-up of a suppressed press are swallowed even
//! after Caps is released, is the shared press ledger's job
//! (`stache_sdk::input`); this machine only keeps the legacy chord rules:
//!
//! * a bare Caps tap does nothing, F18 down, repeat and up are always swallowed
//!   and a duplicate Caps down never re-arms a chord;
//! * one dispatch per discrete (non-repeat) bound key down while Caps is held;
//! * only one chord is active at a time (legacy), and it stays active until its
//!   key-up even after Caps is released;
//! * unbound keys pass.
//!
//! A real F18 key is indistinguishable from the surrogate: a global HID remap
//! carries no per-keyboard identity, so a physical F18 is swallowed too while
//! the mapping is active.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use stache_sdk::input::KeyCode;

use super::parser::CapsKey;

/// `kVK_F18`, where the remapped physical Caps Lock arrives.
pub const F18_KEY: KeyCode = KeyCode(0x4F);

/// One key event for the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsInput {
    /// A key went down; `repeat` marks auto-repeat.
    Down {
        /// The virtual key code.
        key: u16,
        /// Auto-repeat marker.
        repeat: bool,
    },
    /// A key went up.
    Up {
        /// The virtual key code.
        key: u16,
    },
}

/// What to do with the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsDecision {
    /// Let the event continue.
    Pass,
    /// Consume the event.
    Suppress,
    /// Consume the event and dispatch the binding once.
    Execute(CapsKey),
}

/// The lock-free chord state.
#[derive(Debug, Default)]
pub struct CapsState {
    held: AtomicBool,
    /// `0` for none, otherwise the active chord key code plus one.
    active: AtomicU32,
}

impl CapsState {
    /// An idle state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            held: AtomicBool::new(false),
            active: AtomicU32::new(0),
        }
    }

    /// Whether the Caps surrogate is held.
    #[must_use]
    pub fn is_held(&self) -> bool { self.held.load(Ordering::SeqCst) }

    /// The active chord key, when a chord is in progress.
    #[must_use]
    pub fn active_key(&self) -> Option<u16> {
        match self.active.load(Ordering::SeqCst) {
            0 => None,
            raw => u16::try_from(raw - 1).ok(),
        }
    }

    /// Whether any press is armed.
    #[must_use]
    pub fn is_armed(&self) -> bool { self.is_held() || self.active_key().is_some() }

    /// Advances the machine. `dispatch` is `false` once dispatch was
    /// invalidated: the surrogate is still swallowed but no chord executes.
    pub fn step(
        &self,
        input: CapsInput,
        is_bound: impl Fn(u16) -> bool,
        dispatch: bool,
    ) -> CapsDecision {
        let surrogate = F18_KEY.0;
        match input {
            CapsInput::Down { key, .. } if key == surrogate => {
                // A duplicate down only keeps the held flag: it never re-arms.
                self.held.store(true, Ordering::SeqCst);
                CapsDecision::Suppress
            }
            CapsInput::Up { key } if key == surrogate => {
                self.held.store(false, Ordering::SeqCst);
                CapsDecision::Suppress
            }
            CapsInput::Down { key, repeat: true } => {
                if self.active_key() == Some(key) {
                    CapsDecision::Suppress
                } else {
                    CapsDecision::Pass
                }
            }
            CapsInput::Down { key, repeat: false } => {
                if dispatch && self.is_held() && self.active_key().is_none() && is_bound(key) {
                    self.active.store(u32::from(key) + 1, Ordering::SeqCst);
                    CapsDecision::Execute(CapsKey(key))
                } else {
                    CapsDecision::Pass
                }
            }
            CapsInput::Up { key } => {
                if self.active_key() == Some(key) {
                    self.active.store(0, Ordering::SeqCst);
                    CapsDecision::Suppress
                } else {
                    CapsDecision::Pass
                }
            }
        }
    }

    /// Drops every armed press (tap loss, invalidation, stop).
    pub fn clear(&self) {
        self.held.store(false, Ordering::SeqCst);
        self.active.store(0, Ordering::SeqCst);
    }

    /// Drops the armed state of one cancelled press.
    pub fn cancel_key(&self, key: u16) {
        if key == F18_KEY.0 {
            self.held.store(false, Ordering::SeqCst);
        } else if self.active_key() == Some(key) {
            self.active.store(0, Ordering::SeqCst);
        }
    }
}
