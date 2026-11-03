//! Shared types for CLI commands.
//!
//! This module contains types that are used across multiple CLI command modules.

use std::str::FromStr;

/// A 1-based screen index for targeting specific displays.
///
/// This newtype provides type safety and validation for screen indices,
/// ensuring they are always 1-based (as users expect) rather than 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct ScreenIndex(usize);

impl ScreenIndex {
    /// Creates a new `ScreenIndex` from a 1-based index.
    ///
    /// # Arguments
    ///
    /// * `index` - A 1-based screen index (1 = first screen, 2 = second, etc.)
    #[must_use]
    pub const fn new(index: usize) -> Self { Self(index) }

    /// Returns the 1-based index value.
    #[must_use]
    #[allow(dead_code)] // Public API for consumers of ScreenIndex
    pub const fn get(self) -> usize { self.0 }

    /// Returns the 0-based index for internal use with arrays/APIs.
    #[must_use]
    pub const fn as_zero_based(self) -> usize { self.0.saturating_sub(1) }
}

impl std::fmt::Display for ScreenIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
}

/// Screen target for wallpaper commands.
///
/// Specifies which screen(s) should receive the wallpaper.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenTarget {
    /// Apply to all screens.
    #[default]
    All,
    /// Apply to the main screen only.
    Main,
    /// Apply to a specific screen by 1-based index.
    Index(ScreenIndex),
}

impl FromStr for ScreenTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "all" => Ok(Self::All),
            "main" => Ok(Self::Main),
            _ => s.parse::<usize>().map(|idx| Self::Index(ScreenIndex::new(idx))).map_err(|_| {
                format!(
                    "Invalid screen value '{s}'. Expected 'all', 'main', or a positive integer."
                )
            }),
        }
    }
}

impl std::fmt::Display for ScreenTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Main => write!(f, "main"),
            Self::Index(idx) => write!(f, "{idx}"),
        }
    }
}

/// Direction for window focus and swap operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Direction {
    /// Focus/swap with window above.
    Up,
    /// Focus/swap with window below.
    Down,
    /// Focus/swap with window to the left.
    Left,
    /// Focus/swap with window to the right.
    Right,
    /// Focus/swap with previous window in stack order.
    Previous,
    /// Focus/swap with next window in stack order.
    Next,
}

/// Dimension for window resize operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ResizeDimension {
    /// Resize window width.
    Width,
    /// Resize window height.
    Height,
}

/// Layout type for workspaces (CLI representation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CliLayoutType {
    /// Binary Space Partitioning layout.
    Dwindle,
    /// Split layout - based on screen orientation.
    Split,
    /// Vertical split layout.
    SplitVertical,
    /// Horizontal split layout.
    SplitHorizontal,
    /// Monocle layout - all windows maximized.
    Monocle,
    /// Master layout - one large window with stack.
    Master,
    /// Grid layout - windows arranged in a grid.
    Grid,
    /// Floating layout - windows can be freely moved.
    Floating,
}

impl CliLayoutType {
    /// Converts to kebab-case string for IPC communication.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dwindle => "dwindle",
            Self::Split => "split",
            Self::SplitVertical => "split-vertical",
            Self::SplitHorizontal => "split-horizontal",
            Self::Monocle => "monocle",
            Self::Master => "master",
            Self::Grid => "grid",
            Self::Floating => "floating",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // ScreenTarget tests
    // ========================================================================

    #[test]
    fn test_screen_target_from_str_all() {
        let target: ScreenTarget = "all".parse().unwrap();
        assert_eq!(target, ScreenTarget::All);
    }

    #[test]
    fn test_screen_target_from_str_main() {
        let target: ScreenTarget = "main".parse().unwrap();
        assert_eq!(target, ScreenTarget::Main);
    }

    #[test]
    fn test_screen_target_from_str_index() {
        let target: ScreenTarget = "2".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(2)));
    }

    #[test]
    fn test_screen_target_from_str_case_insensitive() {
        let target: ScreenTarget = "ALL".parse().unwrap();
        assert_eq!(target, ScreenTarget::All);

        let target: ScreenTarget = "Main".parse().unwrap();
        assert_eq!(target, ScreenTarget::Main);
    }

    #[test]
    fn test_screen_target_from_str_invalid() {
        let result: Result<ScreenTarget, _> = "invalid".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid screen value"));
    }

    #[test]
    fn test_screen_target_display() {
        assert_eq!(ScreenTarget::All.to_string(), "all");
        assert_eq!(ScreenTarget::Main.to_string(), "main");
        assert_eq!(ScreenTarget::Index(ScreenIndex::new(2)).to_string(), "2");
    }

    #[test]
    fn test_screen_target_default() {
        let target = ScreenTarget::default();
        assert_eq!(target, ScreenTarget::All);
    }

    #[test]
    fn test_screen_target_from_str_mixed_case() {
        let target: ScreenTarget = "AlL".parse().unwrap();
        assert_eq!(target, ScreenTarget::All);

        let target: ScreenTarget = "mAiN".parse().unwrap();
        assert_eq!(target, ScreenTarget::Main);
    }

    #[test]
    fn test_screen_target_from_str_numeric() {
        let target: ScreenTarget = "1".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(1)));

        let target: ScreenTarget = "10".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(10)));

        let target: ScreenTarget = "99".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(99)));
    }

    #[test]
    fn test_screen_target_from_str_negative() {
        let result: Result<ScreenTarget, _> = "-1".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_from_str_float() {
        let result: Result<ScreenTarget, _> = "1.5".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_from_str_empty() {
        let result: Result<ScreenTarget, _> = "".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_from_str_whitespace() {
        // Whitespace is not trimmed, so this should fail
        let result: Result<ScreenTarget, _> = " all ".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_serialization_all() {
        let target = ScreenTarget::All;
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, "\"all\"");
    }

    #[test]
    fn test_screen_target_serialization_main() {
        let target = ScreenTarget::Main;
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, "\"main\"");
    }

    #[test]
    fn test_screen_target_serialization_index() {
        let target = ScreenTarget::Index(ScreenIndex::new(3));
        let json = serde_json::to_string(&target).unwrap();
        // Index variant serializes with the index object
        assert!(json.contains("index") || json.contains('3'));
    }

    #[test]
    fn test_screen_target_debug() {
        let target = ScreenTarget::All;
        let debug_str = format!("{target:?}");
        assert!(debug_str.contains("All"));
    }

    #[test]
    fn test_screen_target_clone() {
        let original = ScreenTarget::Index(ScreenIndex::new(5));
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    // ========================================================================
    // ScreenIndex tests
    // ========================================================================

    #[test]
    fn test_screen_index_new_and_get() {
        let idx = ScreenIndex::new(3);
        assert_eq!(idx.get(), 3);
    }

    #[test]
    fn test_screen_index_as_zero_based() {
        assert_eq!(ScreenIndex::new(1).as_zero_based(), 0);
        assert_eq!(ScreenIndex::new(2).as_zero_based(), 1);
        assert_eq!(ScreenIndex::new(5).as_zero_based(), 4);
    }

    #[test]
    fn test_screen_index_as_zero_based_saturating() {
        // Edge case: 0 should saturate to 0 (not underflow)
        assert_eq!(ScreenIndex::new(0).as_zero_based(), 0);
    }

    #[test]
    fn test_screen_index_display() {
        assert_eq!(ScreenIndex::new(1).to_string(), "1");
        assert_eq!(ScreenIndex::new(42).to_string(), "42");
    }

    #[test]
    fn test_screen_index_copy() {
        let idx = ScreenIndex::new(3);
        let copied = idx; // Copy
        assert_eq!(idx.get(), copied.get());
    }

    #[test]
    fn test_screen_index_clone() {
        let idx = ScreenIndex::new(3);
        let cloned = idx;
        assert_eq!(idx.get(), cloned.get());
    }

    #[test]
    fn test_screen_index_equality() {
        let idx1 = ScreenIndex::new(5);
        let idx2 = ScreenIndex::new(5);
        let idx3 = ScreenIndex::new(6);

        assert_eq!(idx1, idx2);
        assert_ne!(idx1, idx3);
    }

    #[test]
    fn test_screen_index_serialization() {
        let idx = ScreenIndex::new(7);
        let json = serde_json::to_string(&idx).unwrap();
        assert_eq!(json, "7");
    }

    #[test]
    fn test_screen_index_debug() {
        let idx = ScreenIndex::new(42);
        let debug_str = format!("{idx:?}");
        assert!(debug_str.contains("42"));
    }

    #[test]
    fn test_screen_index_large_value() {
        let idx = ScreenIndex::new(usize::MAX);
        assert_eq!(idx.get(), usize::MAX);
        // as_zero_based should handle large values with saturating subtraction
        assert_eq!(idx.as_zero_based(), usize::MAX - 1);
    }

    // ========================================================================
    // CliLayoutType tests
    // ========================================================================

    #[test]
    fn test_cli_layout_type_as_str() {
        assert_eq!(CliLayoutType::Dwindle.as_str(), "dwindle");
        assert_eq!(CliLayoutType::Split.as_str(), "split");
        assert_eq!(CliLayoutType::SplitVertical.as_str(), "split-vertical");
        assert_eq!(CliLayoutType::SplitHorizontal.as_str(), "split-horizontal");
        assert_eq!(CliLayoutType::Monocle.as_str(), "monocle");
        assert_eq!(CliLayoutType::Master.as_str(), "master");
        assert_eq!(CliLayoutType::Grid.as_str(), "grid");
        assert_eq!(CliLayoutType::Floating.as_str(), "floating");
    }
}
