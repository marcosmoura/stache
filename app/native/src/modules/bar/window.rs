use serde::Serialize;
use tauri::Manager;

use crate::config::get_config;
use crate::error::StacheError;
use crate::utils::window::{get_screen_size, set_position};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn set_window_position(webview_window: &tauri::WebviewWindow) {
    let Ok((logical_width, _)) = get_screen_size(webview_window) else {
        eprintln!("Failed to get screen size for window positioning");
        return;
    };
    let config = get_config();
    let (x, y, width, height) = calculate_window_frame(
        logical_width,
        f64::from(config.bar.height),
        f64::from(config.bar.padding),
    );

    set_position(webview_window, x, y, width, height);
}

const fn calculate_window_frame(
    logical_width: f64,
    bar_height: f64,
    padding: f64,
) -> (f64, f64, f64, f64) {
    // Note: Can't use f64::mul_add in const fn, so use manual calculation
    let width = logical_width - (2.0 * padding);
    let height = bar_height;
    (padding, padding, width, height)
}

/// Gets the current bar window frame dimensions.
///
/// # Errors
///
/// Returns an error if the bar window or screen size cannot be determined.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_bar_window_frame(app: tauri::AppHandle) -> Result<WindowFrame, StacheError> {
    let window = app
        .get_webview_window("bar")
        .ok_or_else(|| StacheError::CommandError("Failed to get bar window".to_string()))?;
    let (screen_width, _screen_height) = get_screen_size(&window)
        .map_err(|_| StacheError::CommandError("Failed to get screen size".to_string()))?;
    let config = get_config();
    let (x, y, width, height) = calculate_window_frame(
        screen_width,
        f64::from(config.bar.height),
        f64::from(config.bar.padding),
    );

    Ok(WindowFrame { x, y, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Default test values matching BarConfig defaults
    const TEST_BAR_HEIGHT: f64 = 28.0;
    const TEST_PADDING: f64 = 12.0;

    #[test]
    fn calculate_window_frame_returns_correct_dimensions() {
        let logical_width = 1920.0;
        let (x, y, width, height) =
            calculate_window_frame(logical_width, TEST_BAR_HEIGHT, TEST_PADDING);

        // x should be padding
        assert!((x - TEST_PADDING).abs() < f64::EPSILON);

        // y should be padding
        assert!((y - TEST_PADDING).abs() < f64::EPSILON);

        // width should be logical_width - 2 * padding
        let expected_width = 2.0f64.mul_add(-TEST_PADDING, logical_width);
        assert!((width - expected_width).abs() < f64::EPSILON);

        // height should be bar_height
        assert!((height - TEST_BAR_HEIGHT).abs() < f64::EPSILON);
    }

    #[test]
    fn calculate_window_frame_with_small_screen() {
        let logical_width = 800.0;
        let (x, y, width, height) =
            calculate_window_frame(logical_width, TEST_BAR_HEIGHT, TEST_PADDING);

        assert!((x - TEST_PADDING).abs() < f64::EPSILON);
        assert!((y - TEST_PADDING).abs() < f64::EPSILON);
        assert!((width - 2.0f64.mul_add(-TEST_PADDING, 800.0)).abs() < f64::EPSILON);
        assert!((height - TEST_BAR_HEIGHT).abs() < f64::EPSILON);
    }

    #[test]
    fn calculate_window_frame_with_4k_screen() {
        let logical_width = 3840.0;
        let (x, y, width, height) =
            calculate_window_frame(logical_width, TEST_BAR_HEIGHT, TEST_PADDING);

        assert!((x - TEST_PADDING).abs() < f64::EPSILON);
        assert!((y - TEST_PADDING).abs() < f64::EPSILON);
        assert!((width - 2.0f64.mul_add(-TEST_PADDING, 3840.0)).abs() < f64::EPSILON);
        assert!((height - TEST_BAR_HEIGHT).abs() < f64::EPSILON);
    }

    #[test]
    fn calculate_window_frame_with_custom_config() {
        let logical_width = 1920.0;
        let custom_height = 40.0;
        let custom_padding = 20.0;
        let (x, y, width, height) =
            calculate_window_frame(logical_width, custom_height, custom_padding);

        assert!((x - custom_padding).abs() < f64::EPSILON);
        assert!((y - custom_padding).abs() < f64::EPSILON);
        assert!((width - 2.0f64.mul_add(-custom_padding, logical_width)).abs() < f64::EPSILON);
        assert!((height - custom_height).abs() < f64::EPSILON);
    }
}
