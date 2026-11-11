//! Core Animation descriptions as plain data, and the pure planners that make them.
//!
//! The native host turns a [`LayerAnimation`] into a `CABasicAnimation`,
//! `CAKeyframeAnimation` (or a group of two) added with `addAnimation:forKey:`
//! inside the batch's transaction; the fake records the description. Explicit
//! animations still run with implicit actions disabled. Nothing here has a
//! completion callback, timer or delegate: model values are set to the final
//! state when an animation is added, so nothing runs when it ends.

use std::time::Duration;

use super::geometry::{UnitPoint, gradient_points};
use super::layers::{FillSpec, RingShape};
use super::motion::{MotionRepeat, OverlayEasing, clamp_motion};
use super::{BorderColor, BorderMotion};

/// Key of the rotation animation on the gradient layer.
pub const KEY_ROTATE: &str = "stache.rotate";
/// Key of the ping-pong animation on the gradient layer.
pub const KEY_PING_PONG: &str = "stache.ping_pong";
/// Key of the focus width animation on the ring layer's `path` (part 09).
pub const KEY_WIDTH_RING: &str = "stache.width.ring";
/// Key of the focus width animation on the gradient mask's `path` (part 09).
pub const KEY_WIDTH_MASK: &str = "stache.width.mask";
/// Key of the focus width animation on the glow layer's `opacity` (part 09).
pub const KEY_WIDTH_GLOW: &str = "stache.width.glow";
/// Diagnostic published when a motion cannot be described and the border is drawn statically.
pub const ANIMATION_DIAGNOSTIC: &str =
    "borders: an animation could not be described; the border is drawn statically";
/// Largest angular step between two rotation keyframes, in degrees.
pub const ROTATION_STEP_DEGREES: f64 = 1.0;

/// Which layer an animation or override targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnimLayer {
    /// The ring shape layer (solid fill; hidden for a glow).
    Ring,
    /// The masked gradient layer.
    Gradient,
    /// The glow layer holding the rendered soft-band bitmap.
    Glow,
    /// The shape layer masking the gradient layer (its `path` is the ring).
    GradientMask,
}

/// The animated property.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnimProperty {
    /// `CAGradientLayer.colors` (two colors).
    Colors,
    /// `CAShapeLayer.fillColor`.
    FillColor,
    /// `CALayer.opacity`.
    Opacity,
    /// `CAGradientLayer.startPoint` and `endPoint` together.
    Direction,
    /// `CAShapeLayer.path` (the ring).
    Path,
}

impl AnimProperty {
    /// The key path string (`Direction` names its two paths joined by `+`).
    #[must_use]
    pub const fn key_path(self) -> &'static str {
        match self {
            Self::Colors => "colors",
            Self::FillColor => "fillColor",
            Self::Opacity => "opacity",
            Self::Direction => "startPoint+endPoint",
            Self::Path => "path",
        }
    }
}

/// One animated value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimValue {
    /// A scalar (opacity).
    Number(f64),
    /// One color.
    Color(BorderColor),
    /// The two gradient colors.
    Colors(BorderColor, BorderColor),
    /// Gradient start and end points.
    Direction(UnitPoint, UnitPoint),
    /// A ring path (two rounded rectangles, even-odd).
    Ring(RingShape),
}

/// How the value moves.
#[derive(Clone, Debug, PartialEq)]
pub enum AnimKind {
    /// A `CABasicAnimation`.
    Basic {
        /// Start value.
        from: AnimValue,
        /// End value.
        to: AnimValue,
    },
    /// A `CAKeyframeAnimation`; `key_times` are fractions of the duration.
    Keyframes {
        /// Values, one per key time.
        values: Vec<AnimValue>,
        /// Strictly the same length as `values`, from 0 to 1.
        key_times: Vec<f64>,
    },
}

/// Repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimRepeat {
    /// Plays once.
    Once,
    /// `repeatCount = infinity`.
    Forever,
}

/// A complete animation description for one layer.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerAnimation {
    /// Target layer.
    pub layer: AnimLayer,
    /// `addAnimation:forKey:` key.
    pub key: &'static str,
    /// Animated property.
    pub property: AnimProperty,
    /// Basic or keyframes.
    pub kind: AnimKind,
    /// Duration of one iteration, seconds (greater than zero).
    pub duration: f64,
    /// `CAMediaTimingFunction` control points; for keyframes, applied to each segment.
    pub timing: [f64; 4],
    /// Repetition.
    pub repeat: AnimRepeat,
    /// `timeOffset` in seconds: the phase an animation rebuilt mid-run resumes from.
    pub time_offset: f64,
}

impl LayerAnimation {
    /// Keyframe count (`None` for a basic animation).
    #[must_use]
    pub const fn keyframe_count(&self) -> Option<usize> {
        match &self.kind {
            AnimKind::Basic { .. } => None,
            AnimKind::Keyframes { values, .. } => Some(values.len()),
        }
    }
}

/// A model-value override applied after a layer write while a fade runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerOverride {
    /// Layer.
    pub layer: AnimLayer,
    /// Model `hidden`.
    pub hidden: bool,
    /// Model `opacity`.
    pub opacity: f64,
}

/// What is currently shown, as the presentation layers report it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Presented {
    /// A flat ring color.
    Solid(BorderColor),
    /// The two gradient colors.
    Gradient(BorderColor, BorderColor),
    /// A glow color.
    Glow(BorderColor),
}

/// Why a motion could not be described.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescribeError {
    /// The gradient layer has no usable size.
    EmptySize,
}

const fn seconds(duration: Duration) -> f64 { duration.as_secs_f64() }

/// The rotation track: the gradient direction from `angle` to `angle + 360`.
///
/// Keyframes sit at every whole degree (so at most 1° between two), each at the
/// time the eased progress reaches it, which bakes the easing into the track
/// and keeps the timing function linear. `phase` resumes the track mid-turn
/// (`timeOffset`).
///
/// # Errors
/// [`DescribeError::EmptySize`] when `size` is not finite and positive.
pub fn rotation_animation(
    size: (f64, f64),
    angle: f64,
    motion: BorderMotion,
    phase: Duration,
) -> Result<LayerAnimation, DescribeError> {
    let BorderMotion::Rotate { easing, repeat, .. } = motion else {
        return Err(DescribeError::EmptySize);
    };
    if !(size.0.is_finite() && size.1.is_finite() && size.0 > 0.0 && size.1 > 0.0) {
        return Err(DescribeError::EmptySize);
    }
    let steps: u16 = 360;
    let mut values = Vec::with_capacity(usize::from(steps) + 1);
    let mut key_times = Vec::with_capacity(usize::from(steps) + 1);
    let mut previous = 0.0_f64;
    for step in 0..=steps {
        let (start, end) = gradient_points(size, angle + f64::from(step));
        values.push(AnimValue::Direction(start, end));
        let time = if step == steps {
            1.0
        } else {
            easing.time_for_progress(f64::from(step) / f64::from(steps)).max(previous)
        };
        previous = time;
        key_times.push(time);
    }
    let period = clamp_motion(motion.period());
    Ok(LayerAnimation {
        layer: AnimLayer::Gradient,
        key: KEY_ROTATE,
        property: AnimProperty::Direction,
        kind: AnimKind::Keyframes { values, key_times },
        duration: seconds(period),
        timing: OverlayEasing::Linear.control_points(),
        repeat: match repeat {
            MotionRepeat::Once => AnimRepeat::Once,
            MotionRepeat::Loop => AnimRepeat::Forever,
        },
        time_offset: seconds(phase),
    })
}

/// The ping-pong track: the two gradient colors swap and swap back, forever.
///
/// Values `[(from,to), (to,from), (from,to)]` at key times `0, 0.5, 1` with the
/// leg easing on each segment reproduce the legacy cursor exactly (the return
/// leg is the same eased curve from `to` back to `from`).
#[must_use]
pub fn ping_pong_animation(
    from: BorderColor,
    to: BorderColor,
    motion: BorderMotion,
    phase: Duration,
) -> LayerAnimation {
    let period = seconds(clamp_motion(motion.period()));
    LayerAnimation {
        layer: AnimLayer::Gradient,
        key: KEY_PING_PONG,
        property: AnimProperty::Colors,
        kind: AnimKind::Keyframes {
            values: vec![
                AnimValue::Colors(from, to),
                AnimValue::Colors(to, from),
                AnimValue::Colors(from, to),
            ],
            key_times: vec![0.0, 0.5, 1.0],
        },
        duration: 2.0 * period,
        timing: motion.easing().control_points(),
        repeat: AnimRepeat::Forever,
        time_offset: seconds(phase) % (2.0 * period),
    }
}

/// The animations and model overrides of one cross-fade.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FadePlan {
    /// Animations to add.
    pub animations: Vec<LayerAnimation>,
    /// Model overrides to apply after the layer write.
    pub overrides: Vec<LayerOverride>,
}

const fn fade_key(layer: AnimLayer, property: AnimProperty) -> &'static str {
    match (layer, property) {
        (AnimLayer::Gradient, AnimProperty::Colors) => "stache.fade.gradient.colors",
        (AnimLayer::Gradient, AnimProperty::Opacity) => "stache.fade.gradient.opacity",
        (AnimLayer::Ring, AnimProperty::FillColor) => "stache.fade.ring.fill_color",
        (AnimLayer::Ring, AnimProperty::Opacity) => "stache.fade.ring.opacity",
        (AnimLayer::Glow, AnimProperty::Opacity) => "stache.fade.glow.opacity",
        _ => "stache.fade.other",
    }
}

const fn basic(
    layer: AnimLayer,
    property: AnimProperty,
    from: AnimValue,
    to: AnimValue,
    duration: f64,
    timing: [f64; 4],
) -> LayerAnimation {
    LayerAnimation {
        layer,
        key: fade_key(layer, property),
        property,
        kind: AnimKind::Basic { from, to },
        duration,
        timing,
        repeat: AnimRepeat::Once,
        time_offset: 0.0,
    }
}

/// Shared inputs of one fade.
#[derive(Clone, Copy)]
struct Fade {
    seconds: f64,
    eased: [f64; 4],
    linear: [f64; 4],
}

impl Fade {
    const fn between(
        self,
        layer: AnimLayer,
        property: AnimProperty,
        from: AnimValue,
        to: AnimValue,
    ) -> LayerAnimation {
        basic(layer, property, from, to, self.seconds, self.eased)
    }

    const fn opacity(self, layer: AnimLayer, from: f64, to: f64) -> LayerAnimation {
        self.between(
            layer,
            AnimProperty::Opacity,
            AnimValue::Number(from),
            AnimValue::Number(to),
        )
    }

    /// Holds an opacity for the fade's length with a linear timing, so a layer
    /// the model already hides (or shows) stays as it is until the fade ends.
    const fn hold(self, layer: AnimLayer, level: f64) -> LayerAnimation {
        basic(
            layer,
            AnimProperty::Opacity,
            AnimValue::Number(level),
            AnimValue::Number(level),
            self.seconds,
            self.linear,
        )
    }
}

const fn invisible_at_rest(layer: AnimLayer) -> LayerOverride {
    LayerOverride {
        layer,
        hidden: false,
        opacity: 0.0,
    }
}

fn fade_to_gradient(
    prev: Presented,
    from: BorderColor,
    to: BorderColor,
    fade: Fade,
    skip_colors: bool,
) -> FadePlan {
    let mut plan = FadePlan::default();
    let (a, b) = match prev {
        Presented::Glow(_) => {
            plan.animations.push(fade.opacity(AnimLayer::Glow, 1.0, 0.0));
            plan.animations.push(fade.opacity(AnimLayer::Gradient, 0.0, 1.0));
            plan.overrides.push(invisible_at_rest(AnimLayer::Glow));
            return plan;
        }
        Presented::Solid(color) => (color, color),
        Presented::Gradient(a, b) => (a, b),
    };
    if !skip_colors && (a != from || b != to) {
        plan.animations.push(fade.between(
            AnimLayer::Gradient,
            AnimProperty::Colors,
            AnimValue::Colors(a, b),
            AnimValue::Colors(from, to),
        ));
    }
    plan
}

fn fade_to_solid(prev: Presented, color: BorderColor, fade: Fade) -> FadePlan {
    let mut plan = FadePlan::default();
    match prev {
        Presented::Gradient(a, b) => {
            plan.animations.push(fade.between(
                AnimLayer::Gradient,
                AnimProperty::Colors,
                AnimValue::Colors(a, b),
                AnimValue::Colors(color, color),
            ));
            plan.animations.push(fade.hold(AnimLayer::Gradient, 1.0));
            plan.animations.push(fade.hold(AnimLayer::Ring, 0.0));
            plan.overrides.push(invisible_at_rest(AnimLayer::Gradient));
        }
        Presented::Solid(a) => {
            if a != color {
                plan.animations.push(fade.between(
                    AnimLayer::Ring,
                    AnimProperty::FillColor,
                    AnimValue::Color(a),
                    AnimValue::Color(color),
                ));
            }
        }
        Presented::Glow(_) => {
            // The glow has no ring: the band fades out (the model hides it, so
            // it stays drawable until the fade ends) while the ring fades in.
            plan.animations.push(fade.opacity(AnimLayer::Glow, 1.0, 0.0));
            plan.animations.push(fade.opacity(AnimLayer::Ring, 0.0, 1.0));
            plan.overrides.push(invisible_at_rest(AnimLayer::Glow));
        }
    }
    plan
}

fn fade_to_glow(prev: Presented, color: BorderColor, fade: Fade) -> FadePlan {
    let mut plan = FadePlan::default();
    match prev {
        Presented::Gradient(..) => {
            plan.animations.push(fade.opacity(AnimLayer::Gradient, 1.0, 0.0));
            plan.animations.push(fade.opacity(AnimLayer::Glow, 0.0, 1.0));
            plan.overrides.push(invisible_at_rest(AnimLayer::Gradient));
        }
        Presented::Solid(_) => {
            // The model hides the ring (a glow has none); it keeps its old
            // color and fades out while the band fades in.
            plan.animations.push(fade.opacity(AnimLayer::Ring, 1.0, 0.0));
            plan.animations.push(fade.opacity(AnimLayer::Glow, 0.0, 1.0));
            plan.overrides.push(invisible_at_rest(AnimLayer::Ring));
        }
        Presented::Glow(a) => {
            // A glow-to-glow color change swaps the band bitmap; the new band
            // fades in.
            if a != color {
                plan.animations.push(fade.opacity(AnimLayer::Glow, 0.0, 1.0));
            }
        }
    }
    plan
}

/// Plans the cross-fade from what is `prev`iously presented to `next`.
///
/// Solid counts as a two-stop gradient of one color, so solid and gradient fade
/// color by color on the gradient layer. A change to or from glow cross-fades
/// the opacity of the glow layer (a glow has no ring) against the other layer.
/// Where a layer must stay visible while the model already hides it, the
/// model gets an opacity-0 override and an opacity animation that holds 1
/// for the fade (a layer reverts to its model value when its animation ends,
/// so nothing needs to run at the end). `skip_colors` leaves the gradient
/// `colors` key path to a ping-pong that owns it.
#[must_use]
pub fn plan_fade(
    prev: Presented,
    next: &FillSpec,
    duration: Duration,
    easing: OverlayEasing,
    skip_colors: bool,
) -> FadePlan {
    let fade = Fade {
        seconds: seconds(clamp_motion(duration)),
        eased: easing.control_points(),
        linear: OverlayEasing::Linear.control_points(),
    };
    match *next {
        FillSpec::Gradient { from, to, .. } => fade_to_gradient(prev, from, to, fade, skip_colors),
        FillSpec::Solid(color) => fade_to_solid(prev, color, fade),
        FillSpec::Glow { color, .. } => fade_to_glow(prev, color, fade),
        FillSpec::None => FadePlan::default(),
    }
}

/// The animations and model overrides of one focus width change (part 09).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WidthPlan {
    /// Animations to add.
    pub animations: Vec<LayerAnimation>,
    /// Model overrides to apply after the layer write.
    pub overrides: Vec<LayerOverride>,
}

/// Plans a focus width grow or shrink from the ring `from` to the ring `to`.
///
/// The ring layer's and the gradient mask's `path` both animate (only the one
/// the fill shows is visible), so solid and gradient share one track. A glow
/// has no ring and its band is a fixed bitmap: only the glow layer animates,
/// fading (`glow` is the band opacity at the start and the end) instead of
/// scaling. The caller sets the model to `to` (the ring path override, and for
/// a glow an opacity override at the end value), so nothing runs when the
/// animation ends. `seconds` is the
/// already clamped duration.
#[must_use]
pub fn plan_width(
    from: RingShape,
    to: RingShape,
    glow: Option<(f64, f64)>,
    seconds: f64,
    easing: OverlayEasing,
) -> WidthPlan {
    let timing = easing.control_points();
    let path = |layer, key| LayerAnimation {
        layer,
        key,
        property: AnimProperty::Path,
        kind: AnimKind::Basic {
            from: AnimValue::Ring(from),
            to: AnimValue::Ring(to),
        },
        duration: seconds,
        timing,
        repeat: AnimRepeat::Once,
        time_offset: 0.0,
    };
    let Some((start, end)) = glow else {
        return WidthPlan {
            animations: vec![
                path(AnimLayer::Ring, KEY_WIDTH_RING),
                path(AnimLayer::GradientMask, KEY_WIDTH_MASK),
            ],
            overrides: Vec::new(),
        };
    };
    let mut plan = WidthPlan::default();
    {
        plan.animations.push(LayerAnimation {
            layer: AnimLayer::Glow,
            key: KEY_WIDTH_GLOW,
            property: AnimProperty::Opacity,
            kind: AnimKind::Basic {
                from: AnimValue::Number(start),
                to: AnimValue::Number(end),
            },
            duration: seconds,
            timing,
            repeat: AnimRepeat::Once,
            time_offset: 0.0,
        });
        plan.overrides.push(LayerOverride {
            layer: AnimLayer::Glow,
            hidden: false,
            opacity: end,
        });
    }
    plan
}
