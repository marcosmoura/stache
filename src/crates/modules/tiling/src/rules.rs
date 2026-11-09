//! Window rule matching and the pure "never tile" predicates.
//!
//! Matching semantics (port of legacy `rules/mod.rs`, with its fallback bug
//! fixed):
//!
//! - A rule matches when **every** specified criterion matches (AND). A rule
//!   with no criterion never matches.
//! - `appId` is an exact, case-insensitive comparison of the bundle id.
//! - `appName` and `title` are case-insensitive **substring** tests.
//! - Case-insensitivity is Unicode-aware: both sides go through
//!   [`str::to_lowercase`] (full Unicode lowercase mapping). Legacy used ASCII
//!   folding for `appId` only on one of its two code paths; the single
//!   Unicode-aware rule here removes that divergence. A window value that is
//!   missing is the empty string, so it only satisfies a criterion that is
//!   itself empty (an empty substring is contained in everything).
//! - Workspaces are searched in order, rules within a workspace in order, and
//!   the first match wins.
//!
//! [`PreparedRule`] pre-lowercases the rule once; [`matches_unprepared`] lowers
//! on the fly. Both route through one comparison core, so they cannot diverge.

use crate::config::WindowRule;

/// Window metadata a rule can see. Missing values are the empty string.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowMetadata<'a> {
    /// Bundle identifier.
    pub app_id: &'a str,
    /// Application name.
    pub app_name: &'a str,
    /// Window title.
    pub title: &'a str,
}

/// Window metadata lowered once for repeated matching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoweredMetadata {
    app_id: String,
    app_name: String,
    title: String,
}

impl WindowMetadata<'_> {
    /// Lowercases every field once.
    #[must_use]
    pub fn lowered(&self) -> LoweredMetadata {
        LoweredMetadata {
            app_id: self.app_id.to_lowercase(),
            app_name: self.app_name.to_lowercase(),
            title: self.title.to_lowercase(),
        }
    }
}

/// A rule with its criteria pre-lowercased.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreparedRule {
    app_id: Option<String>,
    app_name: Option<String>,
    title: Option<String>,
}

impl PreparedRule {
    /// Prepares `rule`.
    #[must_use]
    pub fn new(rule: &WindowRule) -> Self {
        Self {
            app_id: rule.app_id.as_deref().map(str::to_lowercase),
            app_name: rule.app_name.as_deref().map(str::to_lowercase),
            title: rule.title.as_deref().map(str::to_lowercase),
        }
    }

    /// Whether any criterion is set. A rule without one never matches.
    #[must_use]
    pub const fn has_criteria(&self) -> bool {
        self.app_id.is_some() || self.app_name.is_some() || self.title.is_some()
    }

    /// Matches against already lowered metadata.
    #[must_use]
    pub fn matches_lowered(&self, window: &LoweredMetadata) -> bool {
        self.has_criteria()
            && self.app_id.as_ref().is_none_or(|id| *id == window.app_id)
            && self
                .app_name
                .as_ref()
                .is_none_or(|name| window.app_name.contains(name.as_str()))
            && self.title.as_ref().is_none_or(|title| window.title.contains(title.as_str()))
    }

    /// Matches against raw metadata.
    #[must_use]
    pub fn matches(&self, window: &WindowMetadata<'_>) -> bool {
        self.matches_lowered(&window.lowered())
    }
}

/// Matches `rule` without preparation. Equivalent to
/// `PreparedRule::new(rule).matches(window)` by construction.
#[must_use]
pub fn matches_unprepared(rule: &WindowRule, window: &WindowMetadata<'_>) -> bool {
    PreparedRule::new(rule).matches(window)
}

/// An ordered, prepared rule list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreparedRules(Vec<PreparedRule>);

impl PreparedRules {
    /// Prepares every rule, preserving order.
    #[must_use]
    pub fn new(rules: &[WindowRule]) -> Self { Self(rules.iter().map(PreparedRule::new).collect()) }

    /// Index of the first matching rule.
    #[must_use]
    pub fn first_match(&self, window: &WindowMetadata<'_>) -> Option<usize> {
        let lowered = window.lowered();
        self.0.iter().position(|rule| rule.matches_lowered(&lowered))
    }

    /// Whether any rule matches.
    #[must_use]
    pub fn any_match(&self, window: &WindowMetadata<'_>) -> bool {
        self.first_match(window).is_some()
    }
}

/// The workspace and rule index that claimed a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceMatch {
    /// Index of the workspace in the searched sequence.
    pub workspace_index: usize,
    /// Index of the matching rule within that workspace.
    pub rule_index: usize,
}

/// Finds the first workspace (in order) whose first matching rule claims
/// `window`, without preparation.
#[must_use]
pub fn find_matching_workspace<'a, I>(
    window: &WindowMetadata<'_>,
    workspaces: I,
) -> Option<WorkspaceMatch>
where
    I: IntoIterator<Item = &'a [WindowRule]>,
{
    let lowered = window.lowered();
    workspaces.into_iter().enumerate().find_map(|(workspace_index, rules)| {
        rules
            .iter()
            .position(|rule| PreparedRule::new(rule).matches_lowered(&lowered))
            .map(|rule_index| WorkspaceMatch { workspace_index, rule_index })
    })
}

/// [`find_matching_workspace`] over prepared rule lists.
#[must_use]
pub fn find_matching_workspace_prepared<'a, I>(
    window: &WindowMetadata<'_>,
    workspaces: I,
) -> Option<WorkspaceMatch>
where
    I: IntoIterator<Item = &'a PreparedRules>,
{
    let lowered = window.lowered();
    workspaces.into_iter().enumerate().find_map(|(workspace_index, rules)| {
        rules
            .0
            .iter()
            .position(|rule| rule.matches_lowered(&lowered))
            .map(|rule_index| WorkspaceMatch { workspace_index, rule_index })
    })
}

/// Bundle ids of system components that are never tiled (legacy
/// `rules/mod.rs:163+`). Stache's own identity is supplied by the caller, never
/// hard coded here.
pub const SKIP_TILING_BUNDLE_IDS: &[&str] = &[
    "com.apple.dock",
    "com.apple.SystemUIServer",
    "com.apple.controlcenter",
    "com.apple.notificationcenterui",
    "com.apple.Spotlight",
    "com.apple.WindowManager",
    "com.apple.loginwindow",
    "com.apple.screencaptureui",
    "com.apple.screensaver",
    "com.apple.SecurityAgent",
    "com.apple.UserNotificationCenter",
    "com.apple.universalcontrol",
    "com.apple.TouchBarServer",
    "com.apple.AirPlayUIAgent",
    "com.apple.wifi.WiFiAgent",
    "com.apple.bluetoothUIServer",
    "com.apple.CoreLocationAgent",
    "com.apple.VoiceOver",
    "com.apple.AssistiveControl",
    "com.apple.SpeechRecognitionCore",
    "com.apple.accessibility.universalAccessAuthWarn",
    "com.apple.launchpad.launcher",
    "com.apple.FolderActionsDispatcher",
];

/// Application names skipped when no bundle id is available.
pub const SKIP_TILING_APP_NAMES: &[&str] = &[
    "Dock",
    "SystemUIServer",
    "Control Center",
    "Notification Center",
    "Spotlight",
    "Window Manager",
    "WindowManager",
    "loginwindow",
    "borders",
];

/// AX subrole of Picture-in-Picture windows.
pub const PIP_SUBROLE: &str = "AXFloatingWindow";

/// Why a window is never managed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementExclusion {
    /// The window belongs to Stache itself.
    SelfApplication,
    /// A system component, by bundle id.
    SystemBundle,
    /// A system component, by application name (no bundle id match).
    SystemAppName,
    /// A Picture-in-Picture window.
    PictureInPicture,
    /// A Finder "Get Info" window (legacy popup exclusion).
    FinderInfo,
    /// A sheet, drawer or `AXUnknown` panel: attached or popup-like.
    NonStandardSubrole,
    /// An `AXDialog` smaller than 400x300 (alerts, pickers).
    DialogTooSmall,
    /// An ordinary window smaller than 200x150 (helper or invisible windows).
    TooSmall,
    /// A configured global `ignore` rule matched.
    IgnoreRule {
        /// Index of the matching rule.
        index: usize,
    },
}

/// What the exclusion predicates see about a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExclusionInput<'a> {
    /// Rule-visible metadata.
    pub metadata: WindowMetadata<'a>,
    /// The AX subrole, when known.
    pub subrole: Option<&'a str>,
}

/// Whether `subrole` marks a Picture-in-Picture window.
#[must_use]
pub fn is_pip_window(subrole: Option<&str>) -> bool { subrole == Some(PIP_SUBROLE) }

/// Whether the bundle id is a system component (ASCII case-insensitive).
#[must_use]
pub fn is_system_bundle(bundle_id: &str) -> bool {
    !bundle_id.is_empty()
        && SKIP_TILING_BUNDLE_IDS.iter().any(|id| bundle_id.eq_ignore_ascii_case(id))
}

/// Whether the application name is a skipped system name (ASCII
/// case-insensitive).
#[must_use]
pub fn is_system_app_name(name: &str) -> bool {
    !name.is_empty() && SKIP_TILING_APP_NAMES.iter().any(|skip| name.eq_ignore_ascii_case(skip))
}

/// Smallest ordinary window that is tiled (legacy `MIN_STANDARD_*`).
pub const MIN_STANDARD_SIZE: (f64, f64) = (200.0, 150.0);
/// Smallest dialog that is tiled (legacy `MIN_DIALOG_*`).
pub const MIN_DIALOG_SIZE: (f64, f64) = (400.0, 300.0);

impl ManagementExclusion {
    /// A short stable label for diagnostics and the status query.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::SelfApplication => "stache itself",
            Self::SystemBundle => "system component (bundle id)",
            Self::SystemAppName => "system component (name)",
            Self::PictureInPicture => "picture-in-picture",
            Self::FinderInfo => "finder info window",
            Self::NonStandardSubrole => "sheet, drawer or unknown-subrole panel",
            Self::DialogTooSmall => "dialog below 400x300",
            Self::TooSmall => "window below 200x150",
            Self::IgnoreRule { .. } => "ignore rule",
        }
    }
}

/// The legacy enumeration filter a window must pass to be *adopted*: sheets,
/// drawers and `AXUnknown` panels are refused, dialogs must be at least
/// 400x300 and every other window at least 200x150. An unread frame cannot be
/// judged and does not exclude (it is judged again by its next snapshot). Only
/// new windows are judged, so a layout that shrinks a tile never untracks it.
#[must_use]
pub fn adoption_exclusion(
    subrole: Option<&str>,
    size: Option<(f64, f64)>,
) -> Option<ManagementExclusion> {
    let below = |minimum: (f64, f64)| size.is_some_and(|(w, h)| w < minimum.0 || h < minimum.1);
    match subrole {
        Some("AXSheet" | "AXDrawer" | "AXUnknown") => Some(ManagementExclusion::NonStandardSubrole),
        Some("AXDialog") => below(MIN_DIALOG_SIZE).then_some(ManagementExclusion::DialogTooSmall),
        _ => below(MIN_STANDARD_SIZE).then_some(ManagementExclusion::TooSmall),
    }
}

/// Returns why a window must never be managed, or `None` if it may be.
///
/// Order: self, system bundle, system name, Picture-in-Picture, then global
/// ignore rules. `self_bundle_id` is Stache's own bundle id (empty disables the
/// self check).
#[must_use]
pub fn management_exclusion(
    input: &ExclusionInput<'_>,
    self_bundle_id: &str,
    ignore: &PreparedRules,
) -> Option<ManagementExclusion> {
    let bundle = input.metadata.app_id;
    if !self_bundle_id.is_empty() && bundle.to_lowercase() == self_bundle_id.to_lowercase() {
        return Some(ManagementExclusion::SelfApplication);
    }
    if is_system_bundle(bundle) {
        return Some(ManagementExclusion::SystemBundle);
    }
    if is_system_app_name(input.metadata.app_name) {
        return Some(ManagementExclusion::SystemAppName);
    }
    if is_pip_window(input.subrole) {
        return Some(ManagementExclusion::PictureInPicture);
    }
    if bundle.eq_ignore_ascii_case("com.apple.finder") && input.metadata.title.ends_with(" Info") {
        return Some(ManagementExclusion::FinderInfo);
    }
    ignore
        .first_match(&input.metadata)
        .map(|index| ManagementExclusion::IgnoreRule { index })
}
