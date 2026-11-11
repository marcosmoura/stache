//! The plain motion model of a border.
//!
//! Plain data only: no layer, clock or timer lives here. The overlay turns a
//! [`BorderMotion`] and an engine-wide [`Transition`] into Core Animation
//! descriptions (see `animation.rs`) that the render server runs on its own.
//!
//! Behavioral reference only (decision 8): Hyprland's `border` and
//! `borderangle` animations (wiki "Animations"). Approach studied, never code
//! (decision 5): the reference border tool's file `src/animation.c`, whose display-link
//! driven proxy animation Stache does not reproduce; here every animation is a
//! render-server animation added once, so Stache schedules nothing per frame.

use std::time::Duration;

/// The shortest motion period or fade, as the legacy ping-pong leg (`MIN_LEG_MS`).
pub const MIN_MOTION: Duration = Duration::from_millis(16);

/// Clamps a motion duration to at least [`MIN_MOTION`].
#[must_use]
pub fn clamp_motion(duration: Duration) -> Duration { duration.max(MIN_MOTION) }

/// The easing set the overlay understands.
///
/// Tiling maps its own `EasingType` onto this (`spring` is linear, as in
/// `apply_easing`), because `stache-macos` cannot depend on Tiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayEasing {
    /// No acceleration.
    Linear,
    /// Slow start.
    EaseIn,
    /// Slow end.
    EaseOut,
    /// Slow start and end.
    EaseInOut,
    /// Very fast start, long tail.
    EaseOutExpo,
}

impl OverlayEasing {
    /// The `CAMediaTimingFunction` control points `[c1x, c1y, c2x, c2y]`.
    ///
    /// Cubic Béziers, the usual approximations of the Tiling curves (chosen
    /// from the common easing catalogue; the Tiling formulas are cubic in `t`):
    /// `ease_in` `t³` as (0.32, 0, 0.67, 0), `ease_out` as (0.33, 1, 0.68, 1),
    /// `ease_in_out` as (0.65, 0, 0.35, 1) and `ease_out_expo` (`1 - 2^(-10t)`)
    /// as (0.16, 1, 0.3, 1). Linear is (0, 0, 1, 1).
    #[must_use]
    pub const fn control_points(self) -> [f64; 4] {
        match self {
            Self::Linear => [0.0, 0.0, 1.0, 1.0],
            Self::EaseIn => [0.32, 0.0, 0.67, 0.0],
            Self::EaseOut => [0.33, 1.0, 0.68, 1.0],
            Self::EaseInOut => [0.65, 0.0, 0.35, 1.0],
            Self::EaseOutExpo => [0.16, 1.0, 0.3, 1.0],
        }
    }

    /// `(x, y)` of the Bézier at parameter `s`.
    fn point(self, along: f64) -> (f64, f64) {
        let [x1, y1, x2, y2] = self.control_points();
        let rest = 1.0 - along;
        let first = 3.0 * rest * rest * along;
        let second = 3.0 * rest * along * along;
        let end = along * along * along;
        (
            first.mul_add(x1, second.mul_add(x2, end)),
            first.mul_add(y1, second.mul_add(y2, end)),
        )
    }

    /// Bisects the monotone Bézier for the parameter whose chosen coordinate
    /// equals `target`.
    fn solve(self, target: f64, use_y: bool) -> (f64, f64) {
        let (mut low, mut high) = (0.0_f64, 1.0_f64);
        for _ in 0..64 {
            let middle = low.midpoint(high);
            let (x, y) = self.point(middle);
            if (if use_y { y } else { x }) < target {
                low = middle;
            } else {
                high = middle;
            }
        }
        self.point(low.midpoint(high))
    }

    /// The eased progress at time fraction `t` (clamped to `0..=1`), as Core
    /// Animation evaluates the timing function.
    #[must_use]
    pub fn progress(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 || t >= 1.0 {
            return t;
        }
        self.solve(t, false).1
    }

    /// The time fraction at which the eased progress reaches `y` (`0..=1`).
    #[must_use]
    pub fn time_for_progress(self, y: f64) -> f64 {
        let y = y.clamp(0.0, 1.0);
        if y <= 0.0 || y >= 1.0 {
            return y;
        }
        self.solve(y, true).0
    }
}

/// Whether a rotation plays one turn or repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionRepeat {
    /// One turn each time the state becomes the window's active appearance.
    Once,
    /// Forever while the state is active.
    Loop,
}

/// What moves on a border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderMotion {
    /// The two gradient colors swap back and forth; `leg` is one swap.
    PingPong {
        /// Duration of one leg (clamped to at least [`MIN_MOTION`]).
        leg: Duration,
        /// Easing of each leg.
        easing: OverlayEasing,
    },
    /// The gradient direction turns from its angle `a` to `a + 360` degrees,
    /// counterclockwise (angles grow counterclockwise, see `gradient_points`).
    Rotate {
        /// Duration of one full turn (clamped to at least [`MIN_MOTION`]).
        turn: Duration,
        /// Easing of each turn.
        easing: OverlayEasing,
        /// Play once or loop.
        repeat: MotionRepeat,
    },
}

impl BorderMotion {
    /// The clamped duration of one leg or turn.
    #[must_use]
    pub fn period(self) -> Duration {
        match self {
            Self::PingPong { leg, .. } => clamp_motion(leg),
            Self::Rotate { turn, .. } => clamp_motion(turn),
        }
    }

    /// The easing.
    #[must_use]
    pub const fn easing(self) -> OverlayEasing {
        match self {
            Self::PingPong { easing, .. } | Self::Rotate { easing, .. } => easing,
        }
    }
}

/// The engine-wide focus color cross-fade.
///
/// It lives on the control (`set_transition`) and rides in every merged batch
/// rather than on each appearance: it is one configuration value for all
/// borders, a reload swaps it atomically and without a `Show`, and carrying it
/// per command would copy it into up to 64 entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    /// Fade duration; zero means instant.
    pub duration: Duration,
    /// Easing of the fade.
    pub easing: OverlayEasing,
}

impl Transition {
    /// The clamped duration, or `None` when the change is instant (0 ms).
    #[must_use]
    pub fn effective(self) -> Option<Duration> {
        (!self.duration.is_zero()).then(|| clamp_motion(self.duration))
    }
}

/// Why an appearance is being applied to a live overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppearanceChange {
    /// A reload, a recolor or a first show: applied instantly; a running
    /// motion with unchanged parameters keeps running.
    #[default]
    Reload,
    /// The window's active appearance changed because focus or its active
    /// state changed: colors cross-fade (when a transition is set) and the
    /// motion restarts from phase 0.
    StateChange,
    /// The window gained focus while unfocused windows draw no border (part
    /// 09): the ring grows from its presented width (0 when the overlay was
    /// hidden) to the configured width over the transition; instant without
    /// one. Colors do not cross-fade; the motion restarts.
    Grow,
    /// The window lost focus while unfocused windows draw no border (part 09):
    /// the ring shrinks from its presented width to 0 over the transition and
    /// the panel stays ordered in, drawing nothing, until a `Hide`, `HideAll`
    /// or `Release` orders it out (each still at once). Without a transition it
    /// orders out at once.
    Shrink,
}
