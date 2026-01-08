use serde::Serialize;
use tauri::Manager;

use crate::bar::constants::{BAR_HEIGHT, PADDING};
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
    let (x, y, width, height) = calculate_window_frame(logical_width);

    set_position(webview_window, x, y, width, height);
}

const fn calculate_window_frame(logical_width: f64) -> (f64, f64, f64, f64) {
    let width = PADDING.mul_add(-2.0, logical_width);
    let height = BAR_HEIGHT;
    (PADDING, PADDING, width, height)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_bar_window_frame(app: tauri::AppHandle) -> Result<WindowFrame, String> {
    let Some(window) = app.get_webview_window("bar") else {
        return Err("Failed to get bar window".into());
    };
    let Ok((screen_width, _screen_height)) = get_screen_size(&window) else {
        return Err("Failed to get screen size for window positioning".into());
    };
    let (x, y, width, height) = calculate_window_frame(screen_width);

    Ok(WindowFrame { x, y, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculate_window_frame_returns_correct_dimensions() {
        let logical_width = 1920.0;
        let (x, y, width, height) = calculate_window_frame(logical_width);

        // x should be PADDING
        assert!((x - PADDING).abs() < f64::EPSILON);

        // y should be PADDING
        assert!((y - PADDING).abs() < f64::EPSILON);

        // width should be logical_width - 2 * PADDING
        let expected_width = 2.0f64.mul_add(-PADDING, logical_width);
        assert!((width - expected_width).abs() < f64::EPSILON);

        // height should be BAR_HEIGHT
        assert!((height - BAR_HEIGHT).abs() < f64::EPSILON);
    }

    #[test]
    fn calculate_window_frame_with_small_screen() {
        let logical_width = 800.0;
        let (x, y, width, height) = calculate_window_frame(logical_width);

        assert!((x - PADDING).abs() < f64::EPSILON);
        assert!((y - PADDING).abs() < f64::EPSILON);
        assert!((width - 2.0f64.mul_add(-PADDING, 800.0)).abs() < f64::EPSILON);
        assert!((height - BAR_HEIGHT).abs() < f64::EPSILON);
    }

    #[test]
    fn calculate_window_frame_with_4k_screen() {
        let logical_width = 3840.0;
        let (x, y, width, height) = calculate_window_frame(logical_width);

        assert!((x - PADDING).abs() < f64::EPSILON);
        assert!((y - PADDING).abs() < f64::EPSILON);
        assert!((width - 2.0f64.mul_add(-PADDING, 3840.0)).abs() < f64::EPSILON);
        assert!((height - BAR_HEIGHT).abs() < f64::EPSILON);
    }
}
