//! The typed wire DTOs of the Tiling socket methods.
//!
//! Requests are decoded once by the handler and responses are encoded once;
//! every request type denies unknown fields so a malformed or newer request
//! fails instead of silently doing less. Query responses keep the legacy field
//! sets (camelCase) and the actor snapshot order. Operation responses are
//! module-owned and tagged: [`OperationResponse`] distinguishes admission
//! (`enqueued`), confirmed scope (`applied`), `partial` and `uncertain`, and
//! carries structured evidence. No field says "successful" for actor admission.
//!
//! Free-form focus targets and resize dimension/amount stay raw strings here;
//! their semantic validation belongs to the execution point (exit-1 class), not
//! to decoding.

use serde::{Deserialize, Serialize};
use stache_macos::windows::WindowInstance;

use crate::config::LayoutType;
use crate::geometry::Rect;
use crate::outcome::{
    ComponentEvidence, FailureReason, NotAttemptedReason, OperationOutcome, StaleCause,
    TargetEvidence as NativeTarget,
};

/// The socket method names of the Tiling module.
pub mod method {
    /// Stops the module through the registry and waits for teardown.
    pub const PAUSE: &str = "pause";
    /// Clears a pause through the registry.
    pub const RESUME: &str = "resume";
    /// Reads screens, workspaces, windows or apps.
    pub const QUERY: &str = "query";
    /// Runs an ordered window action list.
    pub const WINDOW: &str = "window";
    /// Runs an ordered workspace action list.
    pub const WORKSPACE: &str = "workspace";
}

// ---------------------------------------------------------------- requests

/// What a query reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueryKind {
    /// Attached displays.
    Screens,
    /// Virtual workspaces.
    Workspaces,
    /// Tracked windows.
    Windows,
    /// Running applications.
    Apps,
}

/// Parameters of `tiling.query`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryParams {
    /// What to read.
    pub kind: QueryKind,
    /// Screen filter: exact `main`/`primary`/`secondary`, else an exact name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// Exact workspace name filter (windows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Only the focused workspace's screen (workspaces and windows).
    #[serde(default, skip_serializing_if = "is_false")]
    pub focused_screen: bool,
    /// Only the focused workspace (windows).
    #[serde(default, skip_serializing_if = "is_false")]
    pub focused_workspace: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` signature
const fn is_false(value: &bool) -> bool { !*value }

impl QueryParams {
    /// A query of `kind` without filters.
    #[must_use]
    pub const fn new(kind: QueryKind) -> Self {
        Self {
            kind,
            screen: None,
            workspace: None,
            focused_screen: false,
            focused_workspace: false,
        }
    }
}

/// One `--resize DIMENSION AMOUNT` pair, unvalidated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResizeArg {
    /// `width` or `height` (case-insensitive), checked when the pair executes.
    pub dimension: String,
    /// A signed 32-bit integer in decimal, checked when the pair executes.
    pub amount: String,
}

/// Parameters of `tiling.window`. Executes in the fixed order focus, swap,
/// toggle-floating, preset, resize pairs (as supplied), send-to-screen,
/// send-to-workspace, whatever the order of the fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct WindowParams {
    /// A direction (case-insensitive, `prev` accepted) or a numeric window id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// A swap direction (exact lowercase).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap: Option<String>,
    /// Toggle the focused window's floating state.
    #[serde(skip_serializing_if = "is_false")]
    pub toggle_floating: bool,
    /// A floating preset name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Resize pairs, in supplied order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resize: Vec<ResizeArg>,
    /// A target screen (`main`, `primary`, `secondary` or a name).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_to_screen: Option<String>,
    /// A destination workspace name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_to_workspace: Option<String>,
}

/// Parameters of `tiling.workspace`. Executes in the fixed order focus, layout,
/// balance, send-to-screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct WorkspaceParams {
    /// A workspace name to show and focus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// The layout for the focused workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutType>,
    /// Reset the focused workspace's ratios.
    #[serde(skip_serializing_if = "is_false")]
    pub balance: bool,
    /// A target screen for the focused workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_to_screen: Option<String>,
}

// ----------------------------------------------------------- query results

/// A rectangle as `x,y,width,height`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameDto {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl FrameDto {
    /// The frame of `rect`, or `None` when any value is not finite.
    #[must_use]
    pub fn from_rect(rect: Rect) -> Option<Self> {
        Self::finite(rect.x, rect.y, rect.width, rect.height)
    }

    /// A frame from four values, or `None` when any is not finite.
    #[must_use]
    pub fn finite(x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        [x, y, width, height].iter().all(|value| value.is_finite()).then_some(Self {
            x,
            y,
            width,
            height,
        })
    }
}

/// One `screens` row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScreenDto {
    /// The native display id.
    pub id: u32,
    /// The display name (`screen-{id}` when the system reports none).
    pub name: String,
    /// Whether this is the main (primary) display.
    pub is_main: bool,
    /// Whether this is a built-in panel.
    pub is_builtin: bool,
    /// The backing scale factor.
    pub scale_factor: f64,
    /// The full frame.
    pub frame: FrameDto,
    /// The frame without menu bar and Dock.
    pub visible_frame: FrameDto,
}

/// One `workspaces` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceDto {
    /// The runtime workspace id, as a string.
    pub id: String,
    /// The workspace name.
    pub name: String,
    /// The name of the screen it is on.
    pub screen_name: String,
    /// The layout in effect (kebab-case).
    pub layout: String,
    /// Whether it is visible on its screen.
    pub is_visible: bool,
    /// Whether it is the focused workspace.
    pub is_focused: bool,
    /// Tracked windows assigned to it.
    pub window_count: usize,
}

/// One `windows` row.
// The wire shape is the legacy row: four independent booleans.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowDto {
    /// The `CGWindowID` (display and selector only, never action authority).
    pub id: u32,
    /// The owning process id.
    pub pid: i32,
    /// The application bundle identifier.
    pub app_id: String,
    /// The application name.
    pub app_name: String,
    /// The window title.
    pub title: String,
    /// The workspace name.
    pub workspace: String,
    /// The window frame.
    pub frame: FrameDto,
    /// Whether the window is minimized.
    pub is_minimized: bool,
    /// Whether the window is native fullscreen.
    pub is_fullscreen: bool,
    /// Whether the window floats now (explicitly or as Grid overflow).
    pub is_floating: bool,
    /// Whether the window is the focused one.
    pub is_focused: bool,
}

/// One `apps` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppDto {
    /// The process id.
    pub pid: i32,
    /// The application name.
    pub name: String,
    /// The bundle identifier.
    pub bundle_id: String,
    /// Whether the application is hidden.
    pub is_hidden: bool,
}

// ---------------------------------------------------------- operation result

/// Which request an operation response answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    /// `tiling.pause`.
    Pause,
    /// `tiling.resume`.
    Resume,
    /// `tiling.window`.
    Window,
    /// `tiling.workspace`.
    Workspace,
}

/// What became of one requested action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionStatus {
    /// The actor admitted it and changed desired state. Native effects, if any,
    /// are reported separately and later.
    AdmittedChanged,
    /// The actor admitted it and had nothing to change (a silent no-op).
    AdmittedUnchanged,
    /// It was sent but its result is not known.
    SentUnconfirmed,
    /// It was never sent.
    NotAttempted,
    /// The lifecycle operation finished with acknowledgement.
    Acknowledged,
}

/// One requested action and its status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionEvidence {
    /// The action name (`focus`, `swap`, `toggleFloating`, `preset`, `resize`,
    /// `sendToScreen`, `sendToWorkspace`, `layout`, `balance`, `pause`,
    /// `resume`).
    pub action: String,
    /// What became of it.
    pub status: ActionStatus,
}

/// The request an operation response answers, with its ordered actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestEvidence {
    /// The request.
    pub operation: OperationKind,
    /// The actions in execution order.
    pub actions: Vec<ActionEvidence>,
}

/// How a frame component ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComponentDto {
    /// Not requested.
    NotRequested,
    /// Confirmed by readback.
    Confirmed,
    /// Written but the readback differs.
    Mismatch,
    /// Definitively refused.
    Refused,
    /// Possibly written, no usable readback.
    Unobserved,
    /// Nothing was sent.
    NotSent,
}

impl From<ComponentEvidence> for ComponentDto {
    fn from(value: ComponentEvidence) -> Self {
        match value {
            ComponentEvidence::NotRequested => Self::NotRequested,
            ComponentEvidence::Confirmed => Self::Confirmed,
            ComponentEvidence::Mismatch => Self::Mismatch,
            ComponentEvidence::Refused(_) => Self::Refused,
            ComponentEvidence::Unobserved => Self::Unobserved,
            ComponentEvidence::NotSent => Self::NotSent,
        }
    }
}

/// Per-target, per-component evidence of a native write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetDto {
    /// The `CGWindowID` when known (display only).
    pub window: Option<u32>,
    /// The layout plan sequence, as a decimal string.
    pub plan_sequence: String,
    /// The position component.
    pub position: ComponentDto,
    /// The size component.
    pub size: ComponentDto,
    /// The frame the write wanted.
    pub expected: Option<FrameDto>,
    /// The frame the window actually has, when read.
    pub actual: Option<FrameDto>,
}

/// A named subject with a fixed machine-readable reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasonEvidence {
    /// What the reason is about (`action:focus`, `stage:actor`, `window:42`).
    pub subject: String,
    /// A fixed camelCase reason code.
    pub reason: String,
}

impl ReasonEvidence {
    /// A reason for a subject.
    #[must_use]
    pub fn new(subject: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            reason: reason.into(),
        }
    }
}

/// The evidence every operation outcome carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationEvidence {
    /// The request and its ordered actions.
    pub request: RequestEvidence,
    /// Native per-target and per-component evidence.
    pub targets: Vec<TargetDto>,
    /// Subjects definitively refused.
    pub failed: Vec<ReasonEvidence>,
    /// Subjects provably never sent.
    pub not_attempted: Vec<ReasonEvidence>,
    /// Subjects sent whose result is unknown.
    pub unknown: Vec<ReasonEvidence>,
}

impl OperationEvidence {
    /// Evidence with only a request.
    #[must_use]
    pub const fn new(request: RequestEvidence) -> Self {
        Self {
            request,
            targets: Vec::new(),
            failed: Vec::new(),
            not_attempted: Vec::new(),
            unknown: Vec::new(),
        }
    }
}

/// The module-owned result of a mutation, tagged by `outcome`.
///
/// `enqueued` means the actor admitted the request; it is not proof that a
/// window moved. `applied` needs a confirmed scope (for example an
/// acknowledged teardown). `partial` and `uncertain` are the physical failure
/// classes a client turns into a non-zero exit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum OperationResponse {
    /// Admitted by the actor; nothing is known about the windows.
    Enqueued(OperationEvidence),
    /// The stated scope is confirmed.
    Applied(OperationEvidence),
    /// Some of the scope is confirmed and some is not.
    Partial(OperationEvidence),
    /// A send was issued and its result is unknown.
    Uncertain(OperationEvidence),
}

impl OperationResponse {
    /// The evidence, whatever the tag.
    #[must_use]
    pub const fn evidence(&self) -> &OperationEvidence {
        match self {
            Self::Enqueued(evidence)
            | Self::Applied(evidence)
            | Self::Partial(evidence)
            | Self::Uncertain(evidence) => evidence,
        }
    }

    /// Whether this is a physical failure class (`partial` or `uncertain`).
    #[must_use]
    pub const fn is_physical_failure(&self) -> bool {
        matches!(self, Self::Partial(_) | Self::Uncertain(_))
    }
}

/// A native outcome that is a known immediate failure: it becomes a typed wire
/// error, never an `Ok` body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationFailure {
    /// The fixed, actionable body.
    pub message: String,
}

fn target_dto(target: &NativeTarget, cg: &dyn Fn(WindowInstance) -> Option<u32>) -> TargetDto {
    TargetDto {
        window: cg(target.window),
        plan_sequence: target.plan_sequence.to_string(),
        position: target.position.into(),
        size: target.size.into(),
        expected: FrameDto::from_rect(target.expected),
        actual: target.actual.and_then(FrameDto::from_rect),
    }
}

const fn not_attempted_code(reason: NotAttemptedReason) -> &'static str {
    match reason {
        NotAttemptedReason::Cancelled => "cancelled",
        NotAttemptedReason::AlreadyUsed => "alreadyUsed",
        NotAttemptedReason::GenerationInvalid => "generationInvalid",
        NotAttemptedReason::RevisionChanged => "revisionChanged",
        NotAttemptedReason::Superseded => "superseded",
        NotAttemptedReason::Sealed => "sealed",
        NotAttemptedReason::OwnerUnavailable => "ownerUnavailable",
        NotAttemptedReason::EmptyRequest => "emptyRequest",
        NotAttemptedReason::PermissionDenied => "permissionDenied",
        NotAttemptedReason::TimeoutSetFailed(_) => "timeoutSetFailed",
        NotAttemptedReason::MembershipUnverified(_) => "membershipUnverified",
        NotAttemptedReason::Dispatch(_) => "dispatchRefused",
        NotAttemptedReason::AbandonedBeforeSend => "abandonedBeforeSend",
        NotAttemptedReason::Expired => "expired",
        NotAttemptedReason::QueueFull => "queueFull",
    }
}

fn components(target: &NativeTarget, subject: &str, evidence: &mut OperationEvidence) {
    for (name, component) in [("position", target.position), ("size", target.size)] {
        match component {
            ComponentEvidence::Refused(_) => {
                evidence
                    .failed
                    .push(ReasonEvidence::new(format!("{subject}:{name}"), "refused"));
            }
            ComponentEvidence::Unobserved => {
                evidence
                    .unknown
                    .push(ReasonEvidence::new(format!("{subject}:{name}"), "unobserved"));
            }
            ComponentEvidence::Mismatch => {
                evidence.unknown.push(ReasonEvidence::new(
                    format!("{subject}:{name}"),
                    "readbackMismatch",
                ));
            }
            ComponentEvidence::NotRequested
            | ComponentEvidence::Confirmed
            | ComponentEvidence::NotSent => {}
        }
    }
}

/// Maps a native [`OperationOutcome`] to the wire.
///
/// `cg` resolves an exact window token to its display `CGWindowID`.
///
/// # Errors
///
/// An [`OperationFailure`] for a known immediate failure: a definitive
/// refusal, a proven no-send or a dead target. These never become `Ok` bodies.
pub fn from_native(
    request: RequestEvidence,
    outcome: &OperationOutcome,
    cg: &dyn Fn(WindowInstance) -> Option<u32>,
) -> Result<OperationResponse, OperationFailure> {
    let mut evidence = OperationEvidence::new(request);
    let collect = |targets: &[NativeTarget], evidence: &mut OperationEvidence| {
        for target in targets {
            let dto = target_dto(target, cg);
            let subject = format!(
                "window:{}",
                dto.window.map_or_else(|| "unknown".to_owned(), |id| id.to_string())
            );
            components(target, &subject, evidence);
            evidence.targets.push(dto);
        }
    };
    match outcome {
        OperationOutcome::Enqueued => Ok(OperationResponse::Enqueued(evidence)),
        OperationOutcome::Applied { targets } => {
            collect(targets, &mut evidence);
            Ok(OperationResponse::Applied(evidence))
        }
        OperationOutcome::Partial { targets } => {
            collect(targets, &mut evidence);
            Ok(OperationResponse::Partial(evidence))
        }
        OperationOutcome::Uncertain { targets } => {
            collect(targets, &mut evidence);
            Ok(OperationResponse::Uncertain(evidence))
        }
        OperationOutcome::Failed { reason, .. } => Err(OperationFailure {
            message: match reason {
                FailureReason::Refused => "Tiling operation was refused by the window".to_owned(),
            },
        }),
        OperationOutcome::NotAttempted { reason, .. } => Err(OperationFailure {
            message: format!(
                "Tiling operation was not attempted ({})",
                not_attempted_code(*reason)
            ),
        }),
        OperationOutcome::Stale { cause, .. } => Err(OperationFailure {
            message: match cause {
                StaleCause::Window(_) => "Tiling operation target no longer exists".to_owned(),
                StaleCause::Plan => "Tiling operation was superseded by a newer plan".to_owned(),
            },
        }),
    }
}
