//! Floating presets and the one-shot floating positioning policy.
//!
//! Pure decisions only: nothing here moves a window. A caller (a later part)
//! turns a [`PositionDecision`] or [`PresetOutcome`] into an effect.
//!
//! Policy (see the Tiling overview, decision 4):
//!
//! - Initial adoption is not a new-window event and never positions.
//! - A new floating window (including any new window in a Floating layout) and
//!   an explicit tiled-to-user-floating toggle get `floating.defaultPosition`
//!   **once**. `center` keeps the observed size and centres on the owning
//!   display; `default` leaves the frame alone.
//! - An explicit preset in a Floating workspace outranks that default. Outside a
//!   Floating layout `--preset` is a silent, admitted no-op (an internal
//!   [`PresetNoOp`] reason exists for diagnostics only), including for an
//!   individually floating window.
//! - Grid overflow is exempt from automatic positioning.
//! - Layout change, reload, focus, refresh and reconcile never reposition.
//! - `preset-on-open` is parsed configuration only. No function in this crate
//!   consumes it.

use crate::config::{DimensionValue, FloatingPosition, FloatingPreset, LayoutType};
use crate::geometry::{GeometryError, Rect};

/// Finds a preset by name, ignoring case.
///
/// Uses Unicode lowercasing, the same comparison part 01's duplicate-name
/// validation uses (legacy lookup folded ASCII only; names are unique under the
/// stricter Unicode comparison, so this never picks a different preset).
#[must_use]
pub fn find_preset<'a>(presets: &'a [FloatingPreset], name: &str) -> Option<&'a FloatingPreset> {
    let wanted = name.to_lowercase();
    presets.iter().find(|preset| preset.name.to_lowercase() == wanted)
}

/// Resolves a dimension against `reference` logical points. Pixels are logical
/// points; an unparsable percentage resolves to `0.0` like legacy.
#[must_use]
pub fn resolve_dimension(value: &DimensionValue, reference: f64) -> f64 {
    match value {
        DimensionValue::Pixels(pixels) => f64::from(*pixels),
        DimensionValue::Percentage(text) => text
            .trim()
            .trim_end_matches('%')
            .parse::<f64>()
            .map_or(0.0, |percent| (percent / 100.0) * reference),
    }
}

fn is_half(value: &DimensionValue) -> bool {
    match value {
        DimensionValue::Percentage(text) => text
            .trim()
            .trim_end_matches('%')
            .parse::<f64>()
            .is_ok_and(|percent| (percent - 50.0).abs() < 0.001),
        DimensionValue::Pixels(_) => false,
    }
}

/// Computes a preset's frame inside `usable` (the normalized display usable
/// area, outer gaps already applied).
///
/// Port of legacy `layout/floating.rs:63-125`: a `50%` size uses
/// `(usable - inner gap) / 2` so two halves leave exactly one inner gap; a
/// `50%` position paired with a `50%` size sits after the first half plus the
/// gap; sizes clamp to `1..=usable` and the position clamps inside the usable
/// area. `center` ignores `x`/`y`.
///
/// # Errors
///
/// Returns [`GeometryError`] for non-finite inputs or a usable area smaller than
/// one point in either axis.
pub fn preset_frame(
    preset: &FloatingPreset,
    usable: &Rect,
    inner_h: f64,
    inner_v: f64,
) -> Result<Rect, GeometryError> {
    if !usable.is_finite() || !inner_h.is_finite() || !inner_v.is_finite() {
        return Err(GeometryError::NonFinite { what: "preset inputs" });
    }
    if inner_h < 0.0 || inner_v < 0.0 {
        return Err(GeometryError::Negative { what: "gaps" });
    }
    if usable.width < 1.0 || usable.height < 1.0 {
        return Err(GeometryError::NoUsableArea);
    }

    let width_half = is_half(&preset.width);
    let height_half = is_half(&preset.height);
    let width = if width_half {
        (usable.width - inner_h) / 2.0
    } else {
        resolve_dimension(&preset.width, usable.width)
    };
    let height = if height_half {
        (usable.height - inner_v) / 2.0
    } else {
        resolve_dimension(&preset.height, usable.height)
    };
    let width = width.min(usable.width).max(1.0);
    let height = height.min(usable.height).max(1.0);

    let (x, y) = if preset.center {
        (
            usable.x + (usable.width - width) / 2.0,
            usable.y + (usable.height - height) / 2.0,
        )
    } else {
        let x = preset.x.as_ref().map_or(usable.x, |dimension| {
            if width_half && is_half(dimension) {
                usable.x + width + inner_h
            } else {
                usable.x + resolve_dimension(dimension, usable.width)
            }
        });
        let y = preset.y.as_ref().map_or(usable.y, |dimension| {
            if height_half && is_half(dimension) {
                usable.y + height + inner_v
            } else {
                usable.y + resolve_dimension(dimension, usable.height)
            }
        });
        (x, y)
    };
    let x = x.max(usable.x).min(usable.x + usable.width - width);
    let y = y.max(usable.y).min(usable.y + usable.height - height);

    let frame = Rect::new(x, y, width, height);
    if frame.has_area() {
        Ok(frame)
    } else {
        Err(GeometryError::DegenerateFrame)
    }
}

/// Why an explicit `--preset` request does nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresetNoOp {
    /// The workspace layout is not Floating. Admitted silently to the user.
    NotFloatingLayout,
    /// No preset has that name (ignoring case).
    UnknownPreset,
    /// There is no focused window in the workspace.
    NoFocusedWindow,
    /// The window is a Grid overflow tile or otherwise ineligible.
    NotEligible,
}

/// The result of an explicit preset request.
///
/// `Target` is a computed desired frame, **not** a report that a native write
/// happened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PresetOutcome {
    /// The desired frame to attempt.
    Target(Rect),
    /// Nothing to do, with an internal reason.
    NoOp(PresetNoOp),
    /// Geometry was infeasible.
    Infeasible(GeometryError),
}

/// Inputs of an explicit preset request.
#[derive(Clone, Copy, Debug)]
pub struct PresetRequest<'a> {
    /// The configured presets.
    pub presets: &'a [FloatingPreset],
    /// The requested name.
    pub name: &'a str,
    /// The owning workspace layout.
    pub layout: LayoutType,
    /// Whether a focused window exists in that workspace.
    pub has_focused_window: bool,
    /// Whether the window is eligible (not Grid overflow, fullscreen, ...).
    pub window_eligible: bool,
    /// The normalized usable area of the owning display.
    pub usable: Rect,
    /// Horizontal inner gap.
    pub inner_h: f64,
    /// Vertical inner gap.
    pub inner_v: f64,
}

/// Resolves an explicit preset request. Only a Floating workspace applies one;
/// every other layout, including an individually floating window in a tiled
/// workspace, is [`PresetNoOp::NotFloatingLayout`].
#[must_use]
pub fn resolve_preset(request: &PresetRequest<'_>) -> PresetOutcome {
    let Some(preset) = find_preset(request.presets, request.name) else {
        return PresetOutcome::NoOp(PresetNoOp::UnknownPreset);
    };
    if request.layout != LayoutType::Floating {
        return PresetOutcome::NoOp(PresetNoOp::NotFloatingLayout);
    }
    if !request.has_focused_window {
        return PresetOutcome::NoOp(PresetNoOp::NoFocusedWindow);
    }
    if !request.window_eligible {
        return PresetOutcome::NoOp(PresetNoOp::NotEligible);
    }
    match preset_frame(preset, &request.usable, request.inner_h, request.inner_v) {
        Ok(frame) => PresetOutcome::Target(frame),
        Err(error) => PresetOutcome::Infeasible(error),
    }
}

/// What happened to a window that may need default positioning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatingTrigger {
    /// First adoption of an already-existing window. Not a new-window event.
    InitialAdoption,
    /// A genuinely new window appeared.
    NewWindow,
    /// The user explicitly toggled a tiled window to floating.
    ExplicitFloatToggle,
    /// The workspace layout changed.
    LayoutChange,
    /// Configuration reloaded.
    ConfigReload,
    /// Focus moved.
    FocusChange,
    /// A refresh or reconcile pass.
    Reconcile,
}

/// Facts about the window at the time of the trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloatingContext {
    /// What happened.
    pub trigger: FloatingTrigger,
    /// The workspace layout the window belongs to.
    pub layout: LayoutType,
    /// The window is explicitly user-floating.
    pub explicit_floating: bool,
    /// The window is a Grid overflow tile.
    pub grid_overflow: bool,
    /// Default positioning already happened for this window.
    pub already_positioned: bool,
}

/// Why default positioning did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionNoOp {
    /// The trigger is not one that positions (adoption, reload, focus, ...).
    TriggerDoesNotPosition,
    /// The window is not floating.
    NotFloating,
    /// Grid overflow is exempt from automatic positioning.
    GridOverflow,
    /// It was already positioned once.
    AlreadyPositioned,
}

/// The default-position decision for a newly floating window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionDecision {
    /// Centre on the owning display, preserving the observed size.
    Center,
    /// Leave the application's frame (`defaultPosition = default`).
    KeepFrame,
    /// Do nothing, with the reason.
    NoOp(PositionNoOp),
}

/// Decides default positioning for `context`. Grants it at most once per window.
#[must_use]
pub const fn default_position(
    setting: FloatingPosition,
    context: &FloatingContext,
) -> PositionDecision {
    match context.trigger {
        FloatingTrigger::NewWindow | FloatingTrigger::ExplicitFloatToggle => {}
        _ => return PositionDecision::NoOp(PositionNoOp::TriggerDoesNotPosition),
    }
    if context.grid_overflow {
        return PositionDecision::NoOp(PositionNoOp::GridOverflow);
    }
    if !(context.explicit_floating || matches!(context.layout, LayoutType::Floating)) {
        return PositionDecision::NoOp(PositionNoOp::NotFloating);
    }
    if context.already_positioned {
        return PositionDecision::NoOp(PositionNoOp::AlreadyPositioned);
    }
    match setting {
        FloatingPosition::Center => PositionDecision::Center,
        FloatingPosition::Default => PositionDecision::KeepFrame,
    }
}

/// Centres `observed` (keeping its size) inside `usable`.
///
/// A window larger than the usable area is pinned to the top-left of the area
/// rather than pushed off screen.
///
/// # Errors
///
/// Returns [`GeometryError`] for non-finite input or a degenerate observed
/// size.
pub fn center_frame(observed: &Rect, usable: &Rect) -> Result<Rect, GeometryError> {
    if !observed.is_finite() || !usable.is_finite() {
        return Err(GeometryError::NonFinite { what: "centering inputs" });
    }
    if !observed.has_area() || !usable.has_area() {
        return Err(GeometryError::DegenerateFrame);
    }
    let x = (usable.x + (usable.width - observed.width) / 2.0).max(usable.x);
    let y = (usable.y + (usable.height - observed.height) / 2.0).max(usable.y);
    Ok(Rect::new(x, y, observed.width, observed.height))
}

/// The single frame decision for a floating window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FloatingTarget {
    /// Apply the explicit preset frame.
    Preset(Rect),
    /// Centre per default positioning.
    Center,
    /// Keep the current frame.
    Keep,
}

/// Combines an explicit preset request with default positioning. An explicit
/// preset that produces a target wins; otherwise the default decision applies;
/// no-ops keep the current frame.
#[must_use]
pub const fn choose_floating_target(
    explicit: Option<PresetOutcome>,
    default: PositionDecision,
) -> FloatingTarget {
    if let Some(PresetOutcome::Target(frame)) = explicit {
        return FloatingTarget::Preset(frame);
    }
    match default {
        PositionDecision::Center => FloatingTarget::Center,
        PositionDecision::KeepFrame | PositionDecision::NoOp(_) => FloatingTarget::Keep,
    }
}
