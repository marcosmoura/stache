//! Configuration file watcher for hot-reloading.
//!
//! This module provides functionality to watch the configuration file
//! for changes and restart the application when changes are detected.

use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::AppHandle;

use super::get_config_path;

/// Debounce duration for config file changes.
/// Some editors trigger multiple events per save (write to temp, rename, etc.).
const CONFIG_DEBOUNCE_MS: u64 = 200;

/// Starts watching the configuration file for changes.
///
/// When the config file is modified, the app will restart to apply the new configuration.
/// This function spawns a background thread that watches the file.
///
/// # Arguments
///
/// * `app_handle` - The Tauri app handle used to trigger a restart (release builds only)
#[allow(unused_variables, clippy::needless_pass_by_value)]
pub fn watch_config_file<R: tauri::Runtime>(app_handle: AppHandle<R>) {
    let Some(config_path) = get_config_path().cloned() else {
        // No config file loaded, nothing to watch
        return;
    };

    let config_filename =
        config_path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();

        // Create a watcher
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => {
                eprintln!("stache: warning: failed to create config watcher: {err}");
                return;
            }
        };

        // Watch the config file's parent directory to catch file replacements
        // (some editors save by writing to a temp file then renaming)
        let watch_path = config_path.parent().unwrap_or(&config_path);

        if let Err(err) = watcher.watch(watch_path, RecursiveMode::NonRecursive) {
            eprintln!("stache: warning: failed to watch config file: {err}");
            return;
        }

        // Track last event time for debouncing (None = no previous event)
        #[allow(unused_variables, unused_mut)]
        let mut last_event_time: Option<Instant> = None;
        let debounce_duration = Duration::from_millis(CONFIG_DEBOUNCE_MS);

        loop {
            match rx.recv() {
                Ok(Ok(event)) => {
                    // Check if this event affects our config file by filename
                    let affects_config = event
                        .paths
                        .iter()
                        .any(|p| p.file_name().is_some_and(|name| name == config_filename));

                    if !affects_config {
                        continue;
                    }

                    // Debounce: ignore events that occur within the debounce window
                    // This prevents multiple restarts when editors trigger several events per save
                    let now = Instant::now();
                    if last_event_time.is_some_and(|t| now.duration_since(t) < debounce_duration) {
                        continue;
                    }

                    // In debug mode, just log a message since restart kills the dev server.
                    // Update last_event_time only in debug mode for debouncing subsequent events.
                    // In release mode, restart the app to apply the new configuration.
                    #[cfg(debug_assertions)]
                    {
                        last_event_time = Some(now);
                        eprintln!(
                            "stache: config file changed. Restart the app to apply new settings."
                        );
                    }

                    #[cfg(not(debug_assertions))]
                    {
                        app_handle.restart();
                    }
                }
                Ok(Err(err)) => {
                    eprintln!("stache: warning: config watch error: {err}");
                }
                Err(_) => {
                    // Channel closed, watcher dropped
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_debounce_duration_is_reasonable() {
        // Debounce should be at least 100ms but not more than 1 second
        const { assert!(CONFIG_DEBOUNCE_MS >= 100) };
        const { assert!(CONFIG_DEBOUNCE_MS <= 1000) };
    }

    #[test]
    fn debounce_duration_creates_valid_duration() {
        let duration = Duration::from_millis(CONFIG_DEBOUNCE_MS);
        assert_eq!(duration.as_millis(), u128::from(CONFIG_DEBOUNCE_MS));
    }
}
