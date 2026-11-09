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
pub mod ax;
pub mod borders;
pub mod caps_remap;
pub mod desktop;
pub mod display;
pub mod displays;
pub mod hold_hud;
pub mod hotkeys;
pub mod input;
pub mod keep_awake;
pub mod keyboard_layout;
pub mod main_thread;
pub mod menu;
pub mod settings;
pub mod termination;
pub mod windows;

pub use accessibility::{AccessibilityPermission, is_trusted, request_trust};
pub use activation::ActivationObserver;
#[cfg(any(test, feature = "testing"))]
pub use applications::testing::{
    FakeAppKey, FakeApplicationBackend, FakeBarrier, FakeLaunch, ManualExecutor,
};
pub use applications::{
    ACKNOWLEDGEMENT_BUDGET, AcceptedActionToken, ActivationSink, ApplicationBackend,
    ApplicationControl, ApplicationError, ApplicationId, ApplicationObservation, BackendHandle,
    CANCELLATION_QUANTUM, DisplayHint, DisplayRect, DisplaySource, FINDER_BUNDLE_IDENTIFIER,
    FrontmostApplication, FrontmostClass, FrontmostListener, FrontmostPreflight, FrontmostSnapshot,
    FrontmostState, FrontmostWatch, GracefulOutcome, IssuancePermit, LifecycleEvent, LifecycleKind,
    LifecycleSink, NativeOpenCompletion, OpenCompletion, OpenCompletionContext, OpenRequest,
    PreflightRejection, PressAuthorization, PressCancel, RefreshTicket, ScreenRecord,
    SystemApplicationBackend, TerminateOutcome, Unconfirmed, VisibilityAuthorization,
    VisibilityOperation, VisibilityOutcome, WindowRecord, appkit_rect_from_cg, classify_preflight,
    select_display,
};
pub use audio::{
    AudioBackend, AudioChangeSubscription, AudioDevice, AudioDeviceId, AudioDirection, AudioError,
    AudioWakeup, CoreAudioBackend,
};
#[cfg(any(test, feature = "testing"))]
pub use borders::testing::{FakeSend, FakeTransport};
pub use borders::{
    BORDERS_BINARY, BordersConnection, BordersTransport, CLI_CHILD_DEADLINE, CLI_OUTPUT_CAP,
    CliOutcome, CliRunner, EncodeError, MACH_SEND_DEADLINE_MAX, MAX_PAYLOAD_BYTES, MissingService,
    Rejection, SERVICE_NAME, SendOutcome, ServiceAttempt, encode_mach_args,
};
#[cfg(any(test, feature = "testing"))]
pub use desktop::testing::{FakeDesktop, FakeSetCall};
pub use desktop::{
    DESKTOP_ACKNOWLEDGEMENT_BUDGET, DesktopBackend, DesktopClock, DesktopError, DesktopKeepalive,
    DesktopOperation, DisplayEntry, DisplayFrame, DisplayId, DisplaySnapshot,
    GuardedDesktopBackend, NativeDesktop, NativeOperationAccount, NativeOperationTracker,
    SystemDesktopClock,
};
pub use display::is_screen_mirroring_active;
#[cfg(any(test, feature = "testing"))]
pub use displays::testing::FakeDisplayBackend;
pub use displays::{
    DisplayBackend, DisplayControl, DisplayError, DisplayInfo, DisplayLayout, DisplayObserver,
    SystemDisplayBackend, TopologyHint, appkit_point_from_ax, appkit_rect_from_ax,
    ax_point_from_appkit, ax_rect_from_appkit, refresh_hz_from_native,
};
pub use input::MacInputBackend;
#[cfg(any(test, feature = "testing"))]
pub use keep_awake::testing::{
    BarrierWait, FakeAssertionBackend, FakeLockObservationBackend, FakeLockProbe, FakeNativeBarrier,
};
pub use keep_awake::{
    AcquireError, AssertionBackend, AssertionError, AssertionId, AssertionKind, AssertionLease,
    AssertionOwner, AssertionOwnerId, AssertionRequest, LeaseState, LockObservation,
    LockObservationBackend, LockObservationError, LockObservationInstaller, LockObserverHandle,
    LockProbe, LockState, LockUnknown, OBSERVATION_BUDGET, OBSERVATION_CANCELLATION_QUANTUM,
    OwnedAssertion, OwnerReleaseState, PendingAssertion, ReleaseReport, ReleaseStatus,
    SystemAssertionBackend, SystemLockObservationBackend, SystemLockProbe,
};
pub use main_thread::{MainThread, is_main_thread, run_on_main};
pub use menu::{
    ActionToken, AxOwner, MenuItemKind, MenuItemSnapshot, MenuReadOutcome, MenuReleaseOutcome,
    MenuSnapshot, MenuTarget, PresentationId, ReadRequest, TargetCheck, TargetValidator,
};
pub use settings::{SettingsError, open_accessibility_settings};
#[cfg(feature = "test")]
pub use termination::terminate_after;
pub use termination::{TerminateAnswer, TerminateError, TerminateObservation, reply_to_terminate};
