//! Pure Quartz -> `AppKit` global point conversion and display validation.
//!
//! Quartz event points (top-left origin, y down) and `AppKit` screen frames
//! (bottom-left origin, y up) are distinct global spaces that share one unscaled
//! point-based desktop. Both are in points; `NSScreen.frame` and
//! `CGDisplayBounds` already agree per display, so **no coordinate is multiplied
//! by a backing scale** (the scale is carried only as information).
//!
//! Conversion is anchored on the **primary** display, never on
//! `NSScreen.mainScreen` or the destination screen's height:
//!
//! ```text
//! xA = primaryA.minX + (xQ - primaryQ.minX)
//! yA = primaryA.maxY - (yQ - primaryQ.minY)
//! ```
//!
//! Displays are paired by `CGDirectDisplayID`; each pair must describe the same
//! rectangle in both spaces (within [`PAIR_TOLERANCE`]) or the whole snapshot is
//! rejected. There is no 1080 fallback. Borders: a Quartz point belongs to a
//! display when `min <= v < max` on both axes (the shared edge belongs to the
//! display to the right/below, the outer max edge is outside). The converted
//! `AppKit` y of the top row equals `maxY`, so the converted point is validated
//! against the paired `AppKit` frame inclusively (`min <= v <= max`).

use crate::applications::DisplayRect;
use crate::desktop::DisplayId;

/// Allowed disagreement (points) between the paired Quartz and `AppKit` rects.
pub const PAIR_TOLERANCE: f64 = 0.5;

/// A point in the Quartz global space (top-left origin, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuartzPoint {
    /// Horizontal position.
    pub x: f64,
    /// Vertical position, growing downward.
    pub y: f64,
}

/// A point in the `AppKit` global space (bottom-left origin, y up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppKitPoint {
    /// Horizontal position.
    pub x: f64,
    /// Vertical position, growing upward.
    pub y: f64,
}

/// One display described in both spaces, snapshotted together on main.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayGeometry {
    /// The shared `CGDirectDisplayID`.
    pub id: DisplayId,
    /// `CGDisplayBounds` (Quartz global space).
    pub quartz: DisplayRect,
    /// `NSScreen.frame` (`AppKit` global space).
    pub appkit: DisplayRect,
    /// `NSScreen.backingScaleFactor`; informational, never multiplied in.
    pub backing_scale: f64,
}

/// An immutable geometry snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct GeometrySnapshot {
    /// The primary display (`CGMainDisplayID`).
    pub primary: DisplayId,
    /// Every attached display.
    pub displays: Vec<DisplayGeometry>,
}

/// Why geometry was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryError {
    /// The geometry could not be read (not on main, or the system failed).
    Unavailable,
    /// No displays.
    NoDisplays,
    /// The primary display is not in the snapshot.
    MissingPrimary,
    /// A display id appears twice.
    DuplicateDisplay(DisplayId),
    /// A rect is not finite or has no area.
    InvalidGeometry(DisplayId),
    /// Quartz and `AppKit` disagree about the display's rectangle.
    InconsistentPair(DisplayId),
    /// The point is not finite.
    InvalidPoint,
    /// The point is outside the union of displays.
    OutsideDesktop,
    /// More than one display contains the point (overlap).
    AmbiguousDisplay,
    /// The geometry differs from the geometry the anchor was captured against.
    Changed,
}

/// The converted anchor and the display that contains it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The `AppKit` global location to pop the menu up at.
    pub location: AppKitPoint,
    /// The validated containing display.
    pub display: DisplayId,
}

/// Snapshots geometry. Called on the main thread only.
pub trait GeometrySource: Send + Sync {
    /// Reads the current paired geometry.
    ///
    /// # Errors
    ///
    /// [`GeometryError::Unavailable`] when it cannot be read.
    fn snapshot(&self) -> Result<GeometrySnapshot, GeometryError>;
}

fn near(a: f64, b: f64) -> bool { (a - b).abs() <= PAIR_TOLERANCE }

impl GeometrySnapshot {
    /// Validates the snapshot and returns the primary display.
    ///
    /// # Errors
    ///
    /// The first structural problem found.
    pub fn validate(&self) -> Result<&DisplayGeometry, GeometryError> {
        if self.displays.is_empty() {
            return Err(GeometryError::NoDisplays);
        }
        for (index, display) in self.displays.iter().enumerate() {
            if self.displays[..index].iter().any(|other| other.id == display.id) {
                return Err(GeometryError::DuplicateDisplay(display.id));
            }
            if !display.quartz.is_usable() || !display.appkit.is_usable() {
                return Err(GeometryError::InvalidGeometry(display.id));
            }
        }
        let primary = self
            .displays
            .iter()
            .find(|display| display.id == self.primary)
            .ok_or(GeometryError::MissingPrimary)?;
        for display in &self.displays {
            let (q, a) = (display.quartz, display.appkit);
            let expected_x = primary.appkit.x + (q.x - primary.quartz.x);
            let expected_y =
                primary.appkit.y + primary.appkit.height - (q.y - primary.quartz.y) - q.height;
            if !(near(a.x, expected_x)
                && near(a.y, expected_y)
                && near(a.width, q.width)
                && near(a.height, q.height))
            {
                return Err(GeometryError::InconsistentPair(display.id));
            }
        }
        Ok(primary)
    }

    /// Converts `point` and validates its containing display.
    ///
    /// # Errors
    ///
    /// A [`GeometryError`] for bad geometry, a non-finite point, a point outside
    /// the display union or an overlapping display layout.
    pub fn place(&self, point: QuartzPoint) -> Result<Placement, GeometryError> {
        let primary = self.validate()?;
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(GeometryError::InvalidPoint);
        }
        let mut containing = self.displays.iter().filter(|display| {
            let q = display.quartz;
            point.x >= q.x && point.x < q.x + q.width && point.y >= q.y && point.y < q.y + q.height
        });
        let display = containing.next().ok_or(GeometryError::OutsideDesktop)?;
        if containing.next().is_some() {
            return Err(GeometryError::AmbiguousDisplay);
        }
        let location = AppKitPoint {
            x: primary.appkit.x + (point.x - primary.quartz.x),
            y: primary.appkit.y + primary.appkit.height - (point.y - primary.quartz.y),
        };
        let a = display.appkit;
        let inside = location.x >= a.x - PAIR_TOLERANCE
            && location.x <= a.x + a.width + PAIR_TOLERANCE
            && location.y >= a.y - PAIR_TOLERANCE
            && location.y <= a.y + a.height + PAIR_TOLERANCE;
        if !inside {
            return Err(GeometryError::InconsistentPair(display.id));
        }
        Ok(Placement { location, display: display.id })
    }

    /// [`Self::place`], additionally rejecting geometry that differs from the
    /// snapshot `expected` that the anchor was captured against.
    ///
    /// # Errors
    ///
    /// [`GeometryError::Changed`] on replacement, otherwise as [`Self::place`].
    pub fn place_checked(
        &self,
        expected: Option<&Self>,
        point: QuartzPoint,
    ) -> Result<Placement, GeometryError> {
        if expected.is_some_and(|expected| expected != self) {
            return Err(GeometryError::Changed);
        }
        self.place(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn rect(x: f64, y: f64, w: f64, h: f64) -> DisplayRect { DisplayRect::new(x, y, w, h) }

    /// A display from its Quartz rect and the primary's `AppKit` height.
    fn display(id: u32, q: DisplayRect, primary_height: f64, scale: f64) -> DisplayGeometry {
        DisplayGeometry {
            id: DisplayId::new(id),
            quartz: q,
            appkit: rect(q.x, primary_height - q.y - q.height, q.width, q.height),
            backing_scale: scale,
        }
    }

    fn snapshot(displays: Vec<DisplayGeometry>) -> GeometrySnapshot {
        GeometrySnapshot {
            primary: DisplayId::new(1),
            displays,
        }
    }

    fn q(x: f64, y: f64) -> QuartzPoint { QuartzPoint { x, y } }

    fn single() -> GeometrySnapshot {
        snapshot(vec![display(1, rect(0.0, 0.0, 1440.0, 900.0), 900.0, 2.0)])
    }

    #[test]
    fn the_primary_display_flips_y_against_its_own_height() {
        let placed = single().place(q(100.0, 100.0)).expect("placed");
        assert_eq!(placed.location, AppKitPoint { x: 100.0, y: 800.0 });
        assert_eq!(placed.display, DisplayId::new(1));
    }

    #[test]
    fn a_display_above_the_primary_uses_the_primary_reference_not_its_own_height() {
        // Secondary 1920x1080 above: Quartz y -1080..0.
        let s = snapshot(vec![
            display(1, rect(0.0, 0.0, 1440.0, 900.0), 900.0, 2.0),
            display(2, rect(0.0, -1080.0, 1920.0, 1080.0), 900.0, 1.0),
        ]);
        let placed = s.place(q(10.0, -1.0)).expect("placed");
        assert_eq!(placed.display, DisplayId::new(2));
        assert_eq!(placed.location, AppKitPoint { x: 10.0, y: 901.0 });
        let top = s.place(q(10.0, -1080.0)).expect("top row");
        assert_eq!(top.location.y, 1980.0);
    }

    #[test]
    fn a_display_below_the_primary_has_negative_appkit_y() {
        let s = snapshot(vec![
            display(1, rect(0.0, 0.0, 1440.0, 900.0), 900.0, 2.0),
            display(2, rect(0.0, 900.0, 1920.0, 1080.0), 900.0, 1.0),
        ]);
        let placed = s.place(q(5.0, 1000.0)).expect("placed");
        assert_eq!(placed.display, DisplayId::new(2));
        assert_eq!(placed.location, AppKitPoint { x: 5.0, y: -100.0 });
    }

    #[test]
    fn a_display_to_the_left_keeps_negative_x() {
        let s = snapshot(vec![
            display(1, rect(0.0, 0.0, 1440.0, 900.0), 900.0, 2.0),
            display(2, rect(-1920.0, -100.0, 1920.0, 1080.0), 900.0, 1.0),
        ]);
        let placed = s.place(q(-1000.0, 50.0)).expect("placed");
        assert_eq!(placed.display, DisplayId::new(2));
        assert_eq!(placed.location, AppKitPoint { x: -1000.0, y: 850.0 });
    }

    #[test]
    fn a_displaced_primary_origin_is_honoured() {
        // Synthetic: the primary's frames are not at the origin in either space.
        let primary = DisplayGeometry {
            id: DisplayId::new(1),
            quartz: rect(100.0, 50.0, 1000.0, 800.0),
            appkit: rect(30.0, 70.0, 1000.0, 800.0),
            backing_scale: 1.0,
        };
        let side = DisplayGeometry {
            id: DisplayId::new(2),
            quartz: rect(1100.0, 50.0, 500.0, 600.0),
            // x = 30 + 1000; y = 70 + 800 - 0 - 600
            appkit: rect(1030.0, 270.0, 500.0, 600.0),
            backing_scale: 2.0,
        };
        let s = snapshot(vec![primary, side]);
        let placed = s.place(q(1200.0, 150.0)).expect("placed");
        // xA = 30 + (1200-100); yA = (70+800) - (150-50)
        assert_eq!(placed.location, AppKitPoint { x: 1130.0, y: 770.0 });
        assert_eq!(placed.display, DisplayId::new(2));
    }

    #[test]
    fn a_point_in_a_gap_between_displays_is_rejected_not_clamped() {
        let s = snapshot(vec![
            display(1, rect(0.0, 0.0, 1000.0, 800.0), 800.0, 2.0),
            display(2, rect(1200.0, 0.0, 1000.0, 800.0), 800.0, 1.0),
        ]);
        assert_eq!(s.place(q(1100.0, 10.0)), Err(GeometryError::OutsideDesktop));
        assert!(s.place(q(1200.0, 10.0)).is_ok());
    }

    #[test]
    fn mixed_scale_displays_are_never_multiplied() {
        let s = snapshot(vec![
            display(1, rect(0.0, 0.0, 1440.0, 900.0), 900.0, 2.0),
            display(2, rect(1440.0, 0.0, 1920.0, 1080.0), 900.0, 1.0),
        ]);
        let retina = s.place(q(700.0, 450.0)).expect("retina");
        let plain = s.place(q(2000.0, 450.0)).expect("plain");
        assert_eq!(retina.location, AppKitPoint { x: 700.0, y: 450.0 });
        assert_eq!(plain.location, AppKitPoint { x: 2000.0, y: 450.0 });
    }

    #[test]
    fn borders_include_the_min_edge_and_exclude_the_max_edge() {
        let s = snapshot(vec![
            display(1, rect(0.0, 0.0, 1000.0, 800.0), 800.0, 1.0),
            display(2, rect(1000.0, 0.0, 1000.0, 800.0), 800.0, 1.0),
        ]);
        // The shared edge belongs to the right display.
        assert_eq!(
            s.place(q(1000.0, 10.0)).expect("edge").display,
            DisplayId::new(2)
        );
        assert_eq!(
            s.place(q(999.999, 10.0)).expect("edge").display,
            DisplayId::new(1)
        );
        // The top row (y = min) is inside and maps to the AppKit maxY.
        assert_eq!(s.place(q(0.0, 0.0)).expect("top-left").location.y, 800.0);
        // The outer max edges are outside.
        assert_eq!(s.place(q(2000.0, 10.0)), Err(GeometryError::OutsideDesktop));
        assert_eq!(s.place(q(10.0, 800.0)), Err(GeometryError::OutsideDesktop));
    }

    #[test]
    fn overlapping_displays_are_ambiguous() {
        let s = snapshot(vec![
            display(1, rect(0.0, 0.0, 1000.0, 800.0), 800.0, 1.0),
            display(2, rect(500.0, 0.0, 1000.0, 800.0), 800.0, 1.0),
        ]);
        assert_eq!(s.place(q(600.0, 10.0)), Err(GeometryError::AmbiguousDisplay));
    }

    #[test]
    fn missing_duplicate_invalid_and_inconsistent_geometry_is_rejected() {
        assert_eq!(
            snapshot(vec![]).place(q(0.0, 0.0)),
            Err(GeometryError::NoDisplays)
        );
        let no_primary = GeometrySnapshot {
            primary: DisplayId::new(9),
            displays: vec![display(1, rect(0.0, 0.0, 100.0, 100.0), 100.0, 1.0)],
        };
        assert_eq!(no_primary.place(q(1.0, 1.0)), Err(GeometryError::MissingPrimary));
        let one = display(1, rect(0.0, 0.0, 100.0, 100.0), 100.0, 1.0);
        assert_eq!(
            snapshot(vec![one, one]).place(q(1.0, 1.0)),
            Err(GeometryError::DuplicateDisplay(DisplayId::new(1)))
        );
        let mut empty = one;
        empty.quartz.width = 0.0;
        assert_eq!(
            snapshot(vec![empty]).place(q(1.0, 1.0)),
            Err(GeometryError::InvalidGeometry(DisplayId::new(1)))
        );
        let mut skewed = display(2, rect(100.0, 0.0, 100.0, 100.0), 100.0, 1.0);
        skewed.appkit.y += 40.0;
        assert_eq!(
            snapshot(vec![one, skewed]).place(q(150.0, 10.0)),
            Err(GeometryError::InconsistentPair(DisplayId::new(2)))
        );
    }

    #[test]
    fn a_non_finite_point_is_rejected_and_there_is_no_1080_fallback() {
        assert_eq!(
            single().place(q(f64::NAN, 1.0)),
            Err(GeometryError::InvalidPoint)
        );
        assert_eq!(
            single().place(q(1.0, f64::INFINITY)),
            Err(GeometryError::InvalidPoint)
        );
        assert_eq!(
            single().place(q(10.0, 2000.0)),
            Err(GeometryError::OutsideDesktop)
        );
    }

    #[test]
    fn geometry_replaced_between_capture_and_show_is_rejected() {
        let captured = single();
        let replaced = snapshot(vec![
            display(1, rect(0.0, 0.0, 1440.0, 900.0), 900.0, 2.0),
            display(2, rect(1440.0, 0.0, 1920.0, 1080.0), 900.0, 1.0),
        ]);
        assert_eq!(
            replaced.place_checked(Some(&captured), q(10.0, 10.0)),
            Err(GeometryError::Changed)
        );
        assert!(captured.place_checked(Some(&captured), q(10.0, 10.0)).is_ok());
        assert!(replaced.place_checked(None, q(10.0, 10.0)).is_ok());
    }
}
