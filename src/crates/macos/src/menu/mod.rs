//! The Accessibility menu-bar reader and its dedicated AX owner thread.
//!
//! # Ownership
//!
//! ```text
//! caller threads ── ReadRequest / release ──> AxOwner (Send + Sync handle)
//!                                               └─ thread "stache-ax-owner"
//!                                                    ├─ SystemAx / AxApi   (!Send)
//!                                                    ├─ AxElement          (!Send, RAII CFRelease)
//!                                                    └─ presentation -> retained leaves
//! replies: MenuReadOutcome / MenuSnapshot (pure Send data)
//! ```
//!
//! All AX element creation, copying, retention and release happen on the owner
//! thread; native types are `!Send` and statically asserted so. Reads are
//! budgeted and all-or-nothing (see `reader`). The `NSMenu` presenter
//! (`presenter`), pure geometry (`geometry`), tracking-compatible main dispatch
//! (`dispatch`), exact-instance invocation with `AXPress` (`invocation`) and the
//! combining [`MenuControl`] (`control`) sit on top. The `MenuAnywhere` module runtime
//! drives them. Default tests use the fakes in `testing` and
//! `testing_native` and never create a real `NSMenu`, AX element or activation.

mod ax;
mod control;
#[cfg(test)]
mod control_tests;
mod dispatch;
mod geometry;
mod invocation;
mod owner;
mod presenter;
mod reader;
mod snapshot;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(any(test, feature = "testing"))]
pub mod testing_native;
#[cfg(test)]
mod tests;

pub use ax::{AxApi, AxElement, AxError, StringAttribute, SystemAx};
pub use control::{
    CancelOutcome, CleanupError, CleanupOutcome, ControlConfig, MenuControl, MenuEvent,
    MenuPresentationOutcome, PresentationAuthority, Selection, ShowRequest,
};
pub use dispatch::{DispatchError, MainDispatcher, MainJob, RunLoopDispatcher, run_main_loop};
pub use geometry::{
    AppKitPoint, DisplayGeometry, GeometryError, GeometrySnapshot, GeometrySource, Placement,
    QuartzPoint,
};
pub use invocation::{
    CancelReport, InvocationAuthority, InvocationContext, InvocationTicket, MenuInvocationOutcome,
    MenuInvocationReport, PRESS_TIMEOUT, PressCancel, PressGate, PressOutcome, UncertainStage,
    invoke,
};
pub use owner::{
    AxOwner, Lease, MenuReleaseOutcome, OWNER_THREAD_NAME, OwnerConfig, PressRequest, QUEUE_BOUND,
    ReadRequest, SubmitError,
};
pub use presenter::{
    CancelResult, PlannedEntry, PlannedItem, PresenterBackend, PresenterError, RemoveResult,
    RenderPlan, SelectionSink, SystemGeometry, SystemPresenter, TrackResult, contain_selection,
    modifier_mask, plan,
};
pub use reader::{
    ELEMENT_TIMEOUT, MAX_DEPTH, MAX_NODES, MenuClock, MenuReadOutcome, MenuTarget, READ_DEADLINE,
    ReadContext, ReadSuccess, SystemMenuClock, TargetCheck, TargetValidator, read_menu,
};
pub use snapshot::{
    ActionToken, KeyEquivalent, MarkState, MenuItemKind, MenuItemSnapshot, MenuSnapshot,
    ModifierSet, PresentationId, SubmenuSnapshot, key_equivalent,
};

// Native AX types never cross threads: only the owner thread touches them.
mod owner_local {
    use super::{AxElement, SystemAx};

    /// Compile-time proof that `T` is NOT `Send`: if `T: Send`, both blanket
    /// impls apply and the inferred parameter is ambiguous.
    pub trait NotSend<A> {
        fn check() {}
    }
    impl<T: ?Sized> NotSend<()> for T {}
    impl<T: ?Sized + Send> NotSend<u8> for T {}

    /// Same for `Sync`.
    pub trait NotSync<A> {
        fn check() {}
    }
    impl<T: ?Sized> NotSync<()> for T {}
    impl<T: ?Sized + Sync> NotSync<u8> for T {}

    const _: fn() = || {
        <AxElement as NotSend<_>>::check();
        <AxElement as NotSync<_>>::check();
        <SystemAx as NotSend<_>>::check();
        <SystemAx as NotSync<_>>::check();
    };
}
