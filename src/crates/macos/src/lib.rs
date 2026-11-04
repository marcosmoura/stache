//! Safe macOS adapters.
//!
//! This is the only Stache crate allowed to contain project-authored unsafe
//! code. Each unsafe block states the ownership or thread invariant it relies
//! on; public functions are safe.
#![warn(missing_docs)]

pub mod accessibility;
pub mod activation;
pub mod applications;
pub mod audio;
pub mod display;
pub mod keep_awake;
pub mod main_thread;

pub use accessibility::{AccessibilityPermission, is_trusted, request_trust};
pub use activation::ActivationObserver;
#[cfg(any(test, feature = "testing"))]
pub use applications::testing::{
    FakeAppKey, FakeApplicationBackend, FakeBarrier, FakeLaunch, ManualExecutor,
};
pub use applications::{
    ACKNOWLEDGEMENT_BUDGET, AcceptedActionToken, ApplicationBackend, ApplicationControl,
    ApplicationError, ApplicationId, ApplicationObservation, BackendHandle, CANCELLATION_QUANTUM,
    IssuancePermit, NativeOpenCompletion, OpenCompletion, OpenCompletionContext, OpenRequest,
    SystemApplicationBackend, TerminateOutcome,
};
pub use audio::{
    AudioBackend, AudioChangeSubscription, AudioDevice, AudioDeviceId, AudioDirection, AudioError,
    AudioWakeup, CoreAudioBackend,
};
pub use display::is_screen_mirroring_active;
#[cfg(any(test, feature = "testing"))]
pub use keep_awake::testing::{
    BarrierWait, FakeAssertionBackend, FakeLockObservationBackend, FakeLockProbe, FakeNativeBarrier,
};
pub use keep_awake::{
    AcquireError, AssertionBackend, AssertionError, AssertionId, AssertionKind, AssertionLease,
    AssertionOwner, AssertionOwnerId, AssertionRequest, LeaseState, LockObservation,
    LockObservationBackend, LockObservationError, LockObservationInstaller, LockObserverHandle,
    LockProbe, LockState, LockUnknown, OBSERVATION_BUDGET, OBSERVATION_CANCELLATION_QUANTUM,
    OwnedAssertion, PendingAssertion, ReleaseReport, ReleaseStatus, SystemAssertionBackend,
    SystemLockObservationBackend, SystemLockProbe,
};
pub use main_thread::{MainThread, is_main_thread, run_on_main};
