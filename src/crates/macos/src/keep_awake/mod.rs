//! Safe Keep Awake native adapters.
//!
//! This module is the complete native boundary the Keep Awake runtime will use.
//! It creates no timer, starts no worker and acquires no assertion on
//! construction. Part 01 exports the adapters only; a later part owns policy and
//! lifecycle.
//!
//! # Ownership
//!
//! ```text
//! module
//!   ├─ AssertionRequest (intent only, Copy)
//!   ├─ AssertionOwner (Arc, Send + Sync, opaque AssertionOwnerId)
//!   │    ├─ ledger: in-flight generation, owned and unresolved IDs
//!   │    └─ Arc<dyn AssertionBackend>
//!   │         └─ SystemAssertionBackend -> IOPMAssertionCreateWithName/Release
//!   ├─ AssertionLease (Send + Sync, exact IDs and truthful release state)
//!   │    └─ release is explicit, fallible and never claims in-flight success
//!   ├─ RECOVERY_BANK (passive process-lifetime map, no thread, no scan)
//!   │    └─ AssertionOwnerId -> retained backend + exact pending IDs
//!   ├─ LockObservationInstaller (Send + Sync, dispatch only)
//!   │    └─ MainThreadExecutor -> SystemLockObservationBackend
//!   │         └─ thread-local OBSERVERS: Retained<MainThreadOnly observer>
//!   ├─ LockObservation (Send + Sync, opaque generation + local refresh slot)
//!   └─ SystemLockProbe (Send + Sync)
//!        └─ CGSessionCopyCurrentDictionary -> CFRetained<CFDictionary>
//! ```
//!
//! Native objects never leave the main thread. The selector observer is a
//! `MainThreadOnly` class retained in a main-thread-local map keyed by an opaque
//! handle and generation; callers only see `Send + Sync` handles and pure
//! signals. There is no `unsafe impl Send`/`Sync`, no static observer and no
//! detached run-loop thread.
//!
//! Assertion evidence is never forgotten by dropping the last handle. A final
//! `Drop` attempt that still cannot confirm every ID retains the exact backend
//! and generation evidence in the passive recovery bank until
//! [`AssertionOwner::recover`] takes back that one owner. Process exit is the
//! ultimate OS boundary, not a dropped handle.
//!
//! # Bounded waits and gates
//!
//! Off-main observation installation, predecessor cleanup and stop dispatch
//! through the host [`MainThreadExecutor`](stache_sdk::MainThreadExecutor) and
//! wait at most [`OBSERVATION_BUDGET`] per hop, rechecking cancellation every
//! [`OBSERVATION_CANCELLATION_QUANTUM`]. Stop takes a separate wait token:
//! invalidation and exact native cleanup are enqueued even when that token is
//! already cancelled, and only the acknowledgement wait returns `Cancelled`
//! promptly. Cancellation never skips required removal. A main-thread call runs
//! inline and never waits on a worker. The observation gate is held only for a
//! few field updates, never across a native callback, a dispatch wait or a
//! backend call. Deactivation uses a compare-exchange so a stale predecessor can
//! never clear a successor's publication.
//!
//! Assertion creation and release are synchronous native calls. The assertion
//! ledger is not held across them: it records an in-flight or cleanup generation
//! first, then releases the IDs with no lock. There is no reaper, detached
//! cleanup thread or busy retry; [`AssertionOwner::cleanup_pending`] performs one
//! bounded attempt per caller and must be driven by the next reconcile or
//! lifecycle transition.
//!
//! # Lock evidence
//!
//! The probe reads the public `CGSessionCopyCurrentDictionary` function and
//! copies the dictionary into an owned `CFRetained`. `CGSSessionScreenIsLocked`
//! is not documented by Apple's header, and `com.apple.screenIsLocked` /
//! `com.apple.screenIsUnlocked` are best-effort refresh hints: registration
//! success does not prove delivery. The mapping "valid dictionary, key absent"
//! to [`LockState::Unlocked`] is the approved legacy compatibility policy, not a
//! public lock-state guarantee. A null/unreadable dictionary or a present
//! non-boolean value is [`LockState::Unknown`] and must suppress acquisition.
//! `NSWorkspace` session activity is deliberately not used as a substitute.
//!
//! Real selector delivery requires a live main run loop. The default test suite
//! exercises only deterministic fakes; a real lock/unlock observation remains
//! manual and unverified.
//!
//! # Binding evidence
//!
//! Compile-proven against the installed crates and exact selected features:
//!
//! - `objc2 0.6.5`; `objc2-foundation 0.3.2` with `NSArray`, `NSBundle`,
//!   `NSDistributedNotificationCenter`, `NSError`, `NSNotification`, `NSObject`,
//!   `NSOperation`, `NSString`, `NSURL`, `block2` and `std`. The `NSNotification`
//!   and `NSDistributedNotificationCenter` features are the selector
//!   prerequisites. The installed Foundation header
//!   `Foundation.framework/Headers/NSDistributedNotificationCenter.h` states
//!   delivery uses the main run loop, can be delayed or dropped, and the center
//!   does not retain selector observers. Only `Retained<ScreenLockObserverObject>`
//!   is `!Send`/`!Sync`; installed `Retained<NSDistributedNotificationCenter>`
//!   itself is `Send + Sync`, so the thread-local map, not that handle, is what
//!   keeps the observer main-owned.
//! - `objc2-core-foundation 0.3.2` with `CFBase`, `CFDictionary`, `CFNumber`,
//!   `CFString` and `std`, for the owned session dictionary and CF type checks.
//! - `objc2-core-graphics 0.3.2` with `CGSession` and `std`; the public
//!   `CGSessionCopyCurrentDictionary` returns `Option<CFRetained<CFDictionary>>`
//!   and the wrapper takes ownership of the copied dictionary.
//! - `objc2-io-kit 0.3.2` with `pwr_mgt` and `std`, exposing
//!   `IOPMAssertionCreateWithName` and `IOPMAssertionRelease`. Only
//!   `kIOReturnSuccess` proves release; every other return is retained as an
//!   unresolved ID.
//!
//! # Gate result
//!
//! Pass: the installed public bindings support owned main-thread selector
//! observation, a cancellable dispatcher with eventual cleanup, an owned
//! CoreGraphics session copy, and fallible IOPM creation/release with retained
//! generation evidence. No private symbol, `unsafe impl Send`/`Sync`, leaked
//! observer, detached run-loop thread or real native call is involved.
//! `CGSSessionScreenIsLocked` and the two legacy notification names remain
//! best-effort compatibility evidence, not public guarantees.

mod assertion;
mod lock;
mod observation;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use assertion::{
    ASSERTION_REASON, AcquireError, AssertionBackend, AssertionError, AssertionId, AssertionKind,
    AssertionLease, AssertionOwner, AssertionOwnerId, AssertionRequest, LeaseState, OwnedAssertion,
    PendingAssertion, ReleaseReport, ReleaseStatus, SystemAssertionBackend,
};
pub use lock::{
    LockProbe, LockState, LockUnknown, SCREEN_LOCK_NOTIFICATION, SCREEN_LOCKED_KEY,
    SCREEN_UNLOCK_NOTIFICATION, SystemLockProbe,
};
pub use observation::{
    LockObservation, LockObservationBackend, LockObservationError, LockObservationInstaller,
    LockObserverHandle, OBSERVATION_BUDGET, OBSERVATION_CANCELLATION_QUANTUM,
    SystemLockObservationBackend,
};
