//! Helper functions for tiling that are used by other modules.
//!
//! These are internal helpers, not Tauri commands. The actual Tauri commands
//! for tiling are in `bar::components::tiling`.

use super::init;
use super::state::LayoutType;

/// Checks if tiling is initialized and enabled.
#[must_use]
pub fn is_tiling_enabled() -> bool { init::is_initialized() && init::is_enabled() }

/// Converts a layout type to a lowercase string.
#[must_use]
pub fn layout_to_string_pub(layout: LayoutType) -> String {
    match layout {
        LayoutType::Floating => "floating",
        LayoutType::Dwindle => "dwindle",
        LayoutType::Monocle => "monocle",
        LayoutType::Master => "master",
        LayoutType::Split | LayoutType::SplitVertical => "split",
        LayoutType::SplitHorizontal => "split-horizontal",
        LayoutType::Grid => "grid",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_to_string_works() {
        assert_eq!(layout_to_string_pub(LayoutType::Floating), "floating");
        assert_eq!(layout_to_string_pub(LayoutType::Dwindle), "dwindle");
        assert_eq!(layout_to_string_pub(LayoutType::Monocle), "monocle");
        assert_eq!(layout_to_string_pub(LayoutType::Master), "master");
        assert_eq!(layout_to_string_pub(LayoutType::Split), "split");
        assert_eq!(layout_to_string_pub(LayoutType::SplitVertical), "split");
        assert_eq!(
            layout_to_string_pub(LayoutType::SplitHorizontal),
            "split-horizontal"
        );
        assert_eq!(layout_to_string_pub(LayoutType::Grid), "grid");
    }
}
