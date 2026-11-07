//! The main-thread native seam.
//!
//! [`Native`] is everything the hotkey owner needs from Carbon, Text Input Sources
//! and the distributed notification center. It is created *on the main thread*
//! and is deliberately not `Send`: the real implementation holds raw Carbon
//! references and an Objective-C observer. Only plain values cross threads.

use std::rc::Rc;
use std::sync::Arc;

use crate::keyboard_layout::LayoutRead;

/// The Stache signature (`'stnx'`) of every native hotkey ID this crate registers.
pub const SIGNATURE: u32 = u32::from_be_bytes(*b"stnx");

/// `eventHotKeyExistsErr`: the chord is already registered by someone.
pub const STATUS_HOT_KEY_EXISTS: i32 = -9878;

/// A native hotkey ID. Allocated from a process-monotonic counter and never
/// reused, so a queued event for a retired registration can never be
/// reinterpreted as a successor's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NativeHotKeyId(pub u32);

/// What kind of Carbon event arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawEventKind {
    /// `kEventHotKeyPressed`.
    Pressed,
    /// `kEventHotKeyReleased`.
    Released,
    /// Any other event kind.
    Other,
}

/// One Carbon event, already read by the native layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    /// The event kind.
    pub kind: RawEventKind,
    /// The hotkey ID `(signature, id)`, or the non-zero `OSStatus` of the failed read.
    pub hot_key: Result<(u32, u32), i32>,
}

/// What the Carbon callback does with an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// This crate owns the event; it stops here.
    Handled,
    /// Not ours (or not decidable): pass to the next handler.
    Forward,
}

/// The decision function the native handler calls on the main thread. It is an
/// `Rc` on purpose: it is main-thread state.
pub type Dispatch = Rc<dyn Fn(RawEvent) -> Disposition>;

/// Why one chord could not be registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterFailure {
    /// Another registration holds the chord.
    Occupied(i32),
    /// Any other native failure.
    Failed(i32),
}

/// The main-thread native layer.
pub trait Native {
    /// Installs the single application event handler.
    ///
    /// # Errors
    /// The `OSStatus` of the failed install.
    fn install_handler(&mut self, dispatch: Dispatch) -> Result<(), i32>;

    /// Removes the handler. A failure keeps it (and its context) alive.
    ///
    /// # Errors
    /// The `OSStatus` of the failed removal.
    fn remove_handler(&mut self) -> Result<(), i32>;

    /// Registers one chord under `id`.
    ///
    /// # Errors
    /// A [`RegisterFailure`] naming an occupied chord or another failure.
    fn register(
        &mut self,
        key_code: u16,
        carbon_modifiers: u32,
        id: NativeHotKeyId,
    ) -> Result<(), RegisterFailure>;

    /// Unregisters `id`. Unknown IDs succeed.
    ///
    /// # Errors
    /// The `OSStatus` of the failed removal; the registration then remains.
    fn unregister(&mut self, id: NativeHotKeyId) -> Result<(), i32>;

    /// Reads the current keyboard layout.
    fn read_layout(&mut self) -> LayoutRead;

    /// Starts calling `hint` (from any thread) on input-source changes.
    ///
    /// # Errors
    /// A description of the failed subscription.
    fn observe_layout(&mut self, hint: Arc<dyn Fn() + Send + Sync>) -> Result<(), String>;

    /// Stops the layout observer. A failure keeps it installed.
    ///
    /// # Errors
    /// A description of the failed removal.
    fn unobserve_layout(&mut self) -> Result<(), String>;
}

/// Creates the native layer on the main thread.
pub type NativeFactory = Arc<dyn Fn() -> Box<dyn Native> + Send + Sync>;
