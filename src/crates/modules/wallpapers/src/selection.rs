//! Pure target selection and per-display pixel geometry.
//!
//! Selection never decodes, reads a source or touches the cache. A caller
//! resolves a [`ScreenTarget`] against one [`DisplaySnapshot`] before any
//! expensive work, so an invalid selector, an empty screen list or unusable
//! geometry fails first. `main` is entry 0 of the snapshot, never the
//! focus-dependent `mainScreen`, and numeric selection is 1-based with `0`
//! retaining the legacy primary alias.

use std::fmt;

use stache_macos::{DisplayEntry, DisplayId, DisplaySnapshot};

use crate::limits::MAX_TARGET_DIMENSION;

/// Which displays a wallpaper operation addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenTarget {
    /// Every display in the snapshot.
    All,
    /// Entry 0 of the snapshot.
    Main,
    /// A 1-based display index; `0` is the primary alias.
    Index(u32),
}

impl ScreenTarget {
    /// Parses `all`, `main` or a decimal index.
    ///
    /// Parsing is deliberately strict: only these three spellings are
    /// accepted, and the value is not trimmed beyond surrounding whitespace.
    ///
    /// # Errors
    /// Returns [`SelectionError::InvalidSelector`] for any other text and
    /// [`SelectionError::OutOfRange`] for an index that cannot fit a `u32`.
    pub fn parse(value: &str) -> Result<Self, SelectionError> {
        match value.trim() {
            "all" => Ok(Self::All),
            "main" => Ok(Self::Main),
            text => text
                .parse::<u32>()
                .map(Self::Index)
                .map_err(|_| SelectionError::InvalidSelector { value: value.to_owned() }),
        }
    }
}

/// A display selected for processing, with its validated target pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectedTarget {
    /// The snapshot entry this output was prepared for.
    pub entry: DisplayEntry,
    /// The validated target pixel size.
    pub pixels: TargetPixels,
}

impl SelectedTarget {
    /// The stable native display identity.
    #[must_use]
    pub const fn id(self) -> DisplayId { self.entry.id }
}

/// An exact output size in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetPixels {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Which part of a display's geometry made it unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryProblem {
    /// The frame or scale is not a finite number.
    NonFinite,
    /// The frame or scale is zero or negative.
    NonPositive,
    /// The scaled value does not fit `u32` or the pixel product overflowed.
    Overflow,
    /// The value exceeds an internal target bound.
    TooLarge,
}

/// Why a selector or display geometry cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionError {
    /// The selector text is not `all`, `main` or a decimal index.
    InvalidSelector {
        /// The rejected text.
        value: String,
    },
    /// The snapshot has no displays, so there is nothing to address.
    NoDisplays,
    /// A numeric selector names a display that is not in this snapshot.
    OutOfRange {
        /// The requested 1-based index (`0` means primary).
        requested: u32,
        /// How many displays the snapshot has.
        available: usize,
    },
    /// A selected display reports unusable geometry.
    Geometry {
        /// The offending display.
        display: DisplayId,
        /// Why its geometry was rejected.
        problem: GeometryProblem,
    },
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSelector { value } => {
                write!(
                    f,
                    "invalid display selector `{value}`; use all, main or a number"
                )
            }
            Self::NoDisplays => f.write_str("no displays are available"),
            Self::OutOfRange { requested, available } => write!(
                f,
                "display {requested} is out of range for {available} connected display(s)"
            ),
            Self::Geometry { display, problem } => write!(
                f,
                "display {} has unusable geometry ({problem:?})",
                display.raw()
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

/// Resolves a selector against one snapshot, in snapshot order.
///
/// # Errors
/// Fails with [`SelectionError`] for an empty snapshot, an out-of-range index
/// or unusable geometry on any selected display. No fallback display or
/// resolution is invented.
pub fn resolve_targets(
    snapshot: &DisplaySnapshot,
    target: &ScreenTarget,
) -> Result<Vec<SelectedTarget>, SelectionError> {
    if snapshot.is_empty() {
        return Err(SelectionError::NoDisplays);
    }
    let entries: Vec<&DisplayEntry> = match target {
        ScreenTarget::All => snapshot.iter().collect(),
        ScreenTarget::Main => {
            let entry = snapshot.get(0).ok_or(SelectionError::NoDisplays)?;
            vec![entry]
        }
        ScreenTarget::Index(index) => {
            let zero_based = usize::try_from(*index).map_err(|_| SelectionError::OutOfRange {
                requested: *index,
                available: snapshot.len(),
            })?;
            // Numeric selection is 1-based; 0 aliases the primary display.
            let position = if zero_based == 0 { 0 } else { zero_based - 1 };
            let entry = snapshot.get(position).ok_or(SelectionError::OutOfRange {
                requested: *index,
                available: snapshot.len(),
            })?;
            vec![entry]
        }
    };
    entries
        .into_iter()
        .map(|entry| {
            let pixels = target_pixels(entry)?;
            Ok(SelectedTarget { entry: *entry, pixels })
        })
        .collect()
}

/// Computes the exact pixel size of one display.
///
/// The frame is in points and is multiplied by the backing scale, truncated
/// like the legacy adapter, then checked for positivity, finiteness, `u32`
/// range and the internal target bounds.
///
/// # Errors
/// Returns [`SelectionError::Geometry`] instead of clamping or substituting a
/// default resolution.
pub fn target_pixels(entry: &DisplayEntry) -> Result<TargetPixels, SelectionError> {
    let width = scaled(entry, entry.frame.width)?;
    let height = scaled(entry, entry.frame.height)?;
    if width == 0 || height == 0 {
        return Err(SelectionError::Geometry {
            display: entry.id,
            problem: GeometryProblem::NonPositive,
        });
    }
    if width > MAX_TARGET_DIMENSION || height > MAX_TARGET_DIMENSION {
        return Err(SelectionError::Geometry {
            display: entry.id,
            problem: GeometryProblem::TooLarge,
        });
    }
    if crate::limits::check_target(width, height).is_err() {
        return Err(SelectionError::Geometry {
            display: entry.id,
            problem: GeometryProblem::TooLarge,
        });
    }
    Ok(TargetPixels { width, height })
}

fn scaled(entry: &DisplayEntry, points: f64) -> Result<u32, SelectionError> {
    if !points.is_finite() || !entry.scale.is_finite() {
        return Err(SelectionError::Geometry {
            display: entry.id,
            problem: GeometryProblem::NonFinite,
        });
    }
    if points <= 0.0 || entry.scale <= 0.0 {
        return Err(SelectionError::Geometry {
            display: entry.id,
            problem: GeometryProblem::NonPositive,
        });
    }
    let pixels = points * entry.scale;
    if !pixels.is_finite() || pixels > f64::from(u32::MAX) {
        return Err(SelectionError::Geometry {
            display: entry.id,
            problem: GeometryProblem::Overflow,
        });
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixels = pixels.trunc() as u32;
    Ok(pixels)
}

#[cfg(test)]
mod tests;
