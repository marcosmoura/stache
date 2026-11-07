//! The native menu backend the runtime is built over.
//!
//! [`MenuHost`] is the one seam between module policy and the platform. The
//! system implementation wires the Part 02 pieces (the dedicated AX owner
//! thread, the `NSMenu` presenter, `NSScreen` geometry and tracking-compatible
//! main dispatch) from `stache-macos`; tests supply the same real
//! [`MenuControl`] over the fakes from `stache_macos::menu::testing` and
//! `testing_native`, so the production worker code is what runs under test and
//! no default test creates an AX element, an `NSMenu` or a prompt.
//!
//! Building a host is inert. The AX owner thread and every native object exist
//! only after [`MenuHost::open`], which the runtime calls from an enabled
//! module's `start`. Native reads start on a trigger, never on `open`.

use std::sync::Arc;

use stache_macos::menu::{
    AxOwner, ControlConfig, MenuClock, MenuControl, OwnerConfig, RunLoopDispatcher, SystemGeometry,
    SystemMenuClock, SystemPresenter, TargetValidator,
};

/// Creates the native menu stack.
pub trait MenuHost: Send + Sync {
    /// Opens one menu stack: the AX owner thread, presenter, geometry and
    /// dispatcher behind a [`MenuControl`]. `validator` checks the exact
    /// captured application instance for the owner's reads.
    ///
    /// # Errors
    /// Returns a text diagnostic when the stack cannot be created; nothing is
    /// retained after an error.
    fn open(&self, validator: Arc<dyn TargetValidator>) -> Result<MenuControl, String>;

    /// This process id, for the self-target exclusion.
    fn own_pid(&self) -> i32;

    /// The monotonic clock the menu stack and the runtime share.
    fn clock(&self) -> Arc<dyn MenuClock>;
}

/// The real stack: system AX, `NSMenu`, `NSScreen` and run-loop dispatch.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMenuHost;

impl MenuHost for SystemMenuHost {
    fn open(&self, validator: Arc<dyn TargetValidator>) -> Result<MenuControl, String> {
        let clock: Arc<dyn MenuClock> = Arc::new(SystemMenuClock);
        let owner = Arc::new(AxOwner::system(OwnerConfig {
            clock: Arc::clone(&clock),
            validator,
            own_pid: self.own_pid(),
        }));
        Ok(MenuControl::new(
            owner,
            Arc::new(SystemPresenter),
            Arc::new(SystemGeometry),
            Arc::new(RunLoopDispatcher),
            clock,
            ControlConfig::default(),
        ))
    }

    fn own_pid(&self) -> i32 { i32::try_from(std::process::id()).unwrap_or(0) }

    fn clock(&self) -> Arc<dyn MenuClock> { Arc::new(SystemMenuClock) }
}
