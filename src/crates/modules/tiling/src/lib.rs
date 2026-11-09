//! The Tiling configuration.
//!
//! [`TilingConfig`] is the single authoritative source for `tiling.toml`,
//! schema, template and migration. The crate also holds a pure layout library,
//! the owner-confined window adapter seam, an unregistered tracking harness
//! with a gated frame-write executor and the [`TilingModule`] lifecycle. The
//! host registers the module default disabled; construction is inert, `enabled`
//! starts it, and a window moves only after a runtime owner publishes the action
//! authority.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod actor;
mod animation;
mod backend;
mod borders;
pub mod cli;
mod config;
mod displays;
mod drag;
pub mod dto;
mod events;
mod geometry;
mod intent;
pub mod ipc;
mod layout;
mod logclock;
mod minimum_size;
mod module;
mod outcome;
mod presets;
mod reservations;
mod rules;
mod state;
mod status;
mod tabs;
mod tracking;
mod visibility;
mod workspaces;

#[cfg(test)]
mod cli_tests;
#[cfg(test)]
mod drag_tests;
#[cfg(test)]
mod ipc_tests;
#[cfg(test)]
mod module_tests;
#[cfg(test)]
mod pure_tests;
#[cfg(test)]
mod runtime_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod visibility_tests;

#[cfg(any(test, feature = "testing"))]
pub use actor::ManualClock;
pub use actor::{
    ACTOR_THREAD_NAME, AppDirectory, BACKEND_BUDGET, Clock, DRAIN_QUANTUM, DrainOutcome, EffectEnv,
    EffectRunner, Engine, FRAME_PLAN_DEADLINE, FrameControl, FrameJob, InlineRunner,
    MAX_INPUTS_PER_TURN, RUNNER_CAPACITY, RunnerError, SnapshotCell, SystemClock, ThreadRunner,
    Turn, spawn_actor,
};
pub use animation::{
    AnimationControl, AnimationJob, AnimationSettings, AnimationSink, AnimationWorker, Animator,
    LiveSink, MAX_ANIMATED, MIN_ANIMATION_DISTANCE, SETTLE, SpringParams, SpringState, Transition,
    apply_easing, wants_animation,
};
pub use backend::{
    BackendError, LazySystemBackend, SystemWindowBackend, TILING_AX_OWNER_THREAD_NAME,
    WindowBackend,
};
#[cfg(any(test, feature = "testing"))]
pub use backend::{FakeBackendCall, FakeWindowBackend, SendHold};
pub use borders::{
    ANIMATION_FPS, BORDERS_THREAD_NAME, BordersControl, BordersParts, BordersWorker,
    ExecutorFactory, FRAME_PERIOD, FRAME_SEND_DEADLINE, Fault, FaultKind, FaultSink, FocusInput,
    JOIN_BOUND, REAP_BOUND, TransportFactory,
};
pub use config::{
    AnimationConfig, BorderAnimationConfig, BorderStateConfig, BorderStyle, BordersConfig,
    DimensionValue, EasingType, FloatingConfig, FloatingPosition, FloatingPreset, GapValue,
    GapsConfig, GapsConfigValue, GradientConfig, LayoutType, MasterConfig, MasterPosition,
    ScreenGapsConfig, TilingConfig, WindowRule, WorkspaceConfig,
};
pub use displays::{DISPLAY_THREAD_NAME, DisplayFeed};
pub use drag::{
    DRAG_QUEUE_CAPACITY, DragFeed, DragPoint, DragSignal, DragUnavailable, GESTURE_TIMEOUT,
    LostCause, MIN_DRAG_DISTANCE,
};
pub use events::{
    AdmissionStats, COMPLETION_CAPACITY, CONTROL_CAPACITY, DEFAULT_REFRESH_HZ, GEOMETRY_CAPACITY,
    GeometryCoalescer, Ingress, IntentSubmitError, MAX_REFRESH_HZ, MIN_REFRESH_HZ, Pipeline,
    RETIME_EPSILON, Record, SEMANTIC_CAPACITY, SubmitError, Wake, clamp_refresh_hz, flush_period,
};
pub use geometry::{GeometryError, Insets, Rect, usable_area};
pub use intent::{
    Cycle, FocusDirection, FocusTarget, Intent, IntentNoOp, IntentOutcome, IntentRecord,
    IntentRejection, ResizeDimension, ScreenTarget, SwapDirection,
};
pub use layout::{
    Exclusion, GapDisplay, Gaps, GridOverflow, LayoutDiagnostic, LayoutPlan, LayoutRequest,
    MAX_GRID_TILES, PlanStatus, UntouchedReason, WindowEntry, WindowFlags, insert_window,
    insertion_index, master_ratio, plan_layout, swap_windows,
};
pub use minimum_size::{
    Change, FrameObservation, INFERENCE_TOLERANCE, InfeasibleReason, MAX_PASSES, MinimumConstraint,
    MinimumRequest, MinimumResult, MinimumStatus, RatioState, Size, VIOLATION_TOLERANCE, Violation,
    default_ratios, enforce, find_violations, guarded_ratio_change, infer_minimum, observe_write,
    redistribute, resize_cumulative,
};
pub use module::{
    RevisionHook, StageHook, Teardown, TeardownTrace, TilingModule, TilingParts, TilingRuntime,
    TilingTiming,
};
pub use outcome::{
    ComponentEvidence, FailureReason, FrameComponents, NotAttemptedReason, OperationOutcome,
    StaleCause, TargetEvidence, classify_frame,
};
pub use presets::{
    FloatingContext, FloatingTarget, FloatingTrigger, PositionDecision, PositionNoOp, PresetNoOp,
    PresetOutcome, PresetRequest, center_frame, choose_floating_target, default_position,
    find_preset, preset_frame, resolve_dimension, resolve_preset,
};
pub use reservations::{
    DisplayProjection, HostDisplay, HostLookup, HostReservations, MAX_HOST_DISPLAYS,
    ReservationSource, ReservationWatcher, WATCH_QUANTUM, insets_from_sdk,
};
pub use rules::{
    ExclusionInput, LoweredMetadata, ManagementExclusion, PIP_SUBROLE, PreparedRule, PreparedRules,
    SKIP_TILING_APP_NAMES, SKIP_TILING_BUNDLE_IDS, WindowMetadata, WorkspaceMatch,
    find_matching_workspace, find_matching_workspace_prepared, is_pip_window, is_system_app_name,
    is_system_bundle, management_exclusion, matches_unprepared,
};
pub use state::{
    AppInfo, AppRecord, AttemptStatus, Binding, Cause, CompletionResult, DesiredWindow,
    Diagnostics, DragOutcome, DragRecord, DragReport, Effect, FocusState, FrameAttempt, FrameBook,
    FrameRecord, FrameStatus, Health, HealthFault, Input, MAX_EXTERNAL_ATTEMPTS,
    MAX_IN_FLIGHT_FRAMES, MAX_INFERENCES, NoFrameReason, ObservedWindow, Phase, Scope, ScreenState,
    Stamp, TilingSnapshot, TilingState, WindowState, WorkspaceId, WorkspaceState, transition,
};
pub use status::{
    TilingRuntimeState, TilingRuntimeStatus, TilingStatusSource, unsupported_features,
};
pub use tabs::{TabStats, TabsStatus};
pub use tracking::{
    HOP, SINK_CAPACITY, StageProbe, StartError, StartStage, TrackingOptions, TrackingRuntime,
};
pub use visibility::{
    ActionsDirectory, ApplicationActions, ApplicationActionsSource, ControlActionsSource, Exec,
    FocusReport, FocusStep, HideEvidence, LOG_CAPACITY, Ledger, MAX_UNHIDE_FAILURES, Observation,
    OwnedApp, Ownership, Planned, Report, RestoreOutcome, UnhideEvidence, VISIBILITY_DEADLINE,
    VisibilityControl,
};
pub use workspaces::{
    AppVisibility, ConflictKind, Mapping, ReloadError, VisibilityConflict, VisibilityRequirement,
    WorkspaceSpec, prepare_reload, prepare_reload_from_snapshot, resolve_screen, resolve_target,
    specs, visibility_requirement,
};
