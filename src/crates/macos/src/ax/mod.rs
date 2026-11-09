//! The neutral Accessibility (AX) adapter shared by `MenuAnywhere` and Tiling.
//!
//! All AX FFI lives here. [`AxApi`] and [`SystemAx`] are the pure move of the
//! former `menu::ax` module (the `menu` module re-exports the same names);
//! [`WindowAx`] adds the window capabilities additively behind the same
//! trait/fake pattern.
//!
//! # Ownership
//!
//! Every AX object ([`AxElement`], [`SystemAx`], [`AxObserver`]) is `!Send` and
//! `!Sync`, statically asserted below. It is created, used and dropped on the
//! one AX owner thread of its consumer (`MenuAnywhere` keeps `stache-ax-owner`;
//! Tiling will own a separate one). Nothing here is moved across threads by
//! `unsafe impl Send`/`Sync` or an integer-cast pointer, and nothing here
//! dispatches to or waits on the main thread.
//!
//! # Landed capabilities
//!
//! | Capability | Surface | Proof |
//! | --- | --- | --- |
//! | Element tree, press | [`AxApi`] | unchanged `MenuAnywhere` tests |
//! | Position/size read and separate write, settable | [`WindowAx`], [`read_frame`], [`write_frame`], [`resizable`] | fake + `AXValue` round trip |
//! | Bool/string flags, windows, focused/main window | [`WindowAx`] | fake; native unverified |
//! | `AXObserver` create/add/remove/run-loop source | [`AxObserver`] | audited callback + mailbox tests; native unverified |
//! | `CGWindowID` | [`WindowIdResolver`] (`dlsym` only) | injected lookup; native unverified |
//! | Tab extraction reads | [`extract_tabs`] | fake; does **not** claim tabs are supported |
//!
//! Timeouts reuse [`ELEMENT_TIMEOUT`] and [`PRESS_TIMEOUT`].

use std::time::Duration;

mod api;
mod ffi;
mod names;
mod observer;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod value;
mod window;
mod window_id;

pub use api::{AxApi, AxElement, AxError, StringAttribute, SystemAx};
pub use observer::{
    AxDrain, AxEvent, AxNotification, AxObserver, ElementKey, MAX_INTERNED_ELEMENTS,
    MAX_PENDING_EVENTS,
};
pub use window::{
    AxFrame, AxPoint, AxReading, AxSize, BoolAttribute, FrameRead, FrameWriteOutcome,
    GeometryAttribute, TAB_BAR_MAX_DEPTH, TAB_BAR_NODE_BUDGET, TAB_GROUP_ROLE,
    TAB_GROUP_SCAN_LIMIT, TabBarShape, TabExtraction, TabGroupRead, TabGroupSearch, TabMemberRead,
    WindowAx, WindowString, WriteStatus, extract_tabs, find_tab_group, read_frame,
    read_tab_bar_shape, read_tab_group, resizable, write_frame, write_frame_gated,
};
pub use window_id::{WindowIdCapability, WindowIdError, WindowIdResolver};

/// Messaging timeout set on every element before it is queried.
pub const ELEMENT_TIMEOUT: Duration = Duration::from_millis(500);

/// Messaging timeout set on an element before a setting or action call
/// (`AXPress`, `AXPosition`/`AXSize` writes).
pub const PRESS_TIMEOUT: Duration = Duration::from_secs(1);

// Native AX types never cross threads: only the owner thread touches them.
mod owner_local {
    use super::{AxElement, AxObserver, SystemAx};

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
        <AxObserver as NotSend<_>>::check();
        <AxObserver as NotSync<_>>::check();
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_keep_their_documented_values() {
        assert_eq!(ELEMENT_TIMEOUT, Duration::from_millis(500));
        assert_eq!(PRESS_TIMEOUT, Duration::from_secs(1));
    }

    #[test]
    fn menu_reexports_the_same_constants_and_types() {
        assert_eq!(crate::menu::ELEMENT_TIMEOUT, ELEMENT_TIMEOUT);
        assert_eq!(crate::menu::PRESS_TIMEOUT, PRESS_TIMEOUT);
        let _: Option<crate::menu::AxError> = None::<AxError>;
    }
}
