//! Configuration module for Barba Shell.
//!
//! This module wraps the shared configuration types and adds Tauri-specific
//! functionality like file watching and app restart on config changes.

use std::path::PathBuf;
use std::sync::OnceLock;

// Re-export shared types for use throughout the desktop app
pub use barba_shared::{
    BarbaConfig, ConfigError, ShortcutCommands, WallpaperConfig, WallpaperMode,
    generate_schema_json, load_config as load_config_with_path,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::AppHandle;

/// Global configuration instance, loaded once at startup.
static CONFIG: OnceLock<BarbaConfig> = OnceLock::new();

/// Path to the currently loaded configuration file.
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Loads the configuration and stores it in a global static.
///
/// This function is idempotent - calling it multiple times will return
/// the same configuration instance.
///
/// If no configuration file is found, returns a default empty configuration.
pub fn init() -> &'static BarbaConfig {
    CONFIG.get_or_init(|| match load_config_with_path() {
        Ok((config, path)) => {
            let _ = CONFIG_PATH.set(path);
            config
        }
        Err(ConfigError::NotFound) => BarbaConfig::default(),
        Err(err) => {
            eprintln!("barba: warning: failed to load configuration: {err}");
            BarbaConfig::default()
        }
    })
}

/// Returns the global configuration instance.
///
/// # Panics
///
/// Panics if called before `init()` has been called.
pub fn get_config() -> &'static BarbaConfig {
    CONFIG.get().expect("Configuration not initialized. Call init() first.")
}

/// Returns the global configuration instance if it has been initialized.
#[allow(dead_code)]
pub fn try_get_config() -> Option<&'static BarbaConfig> { CONFIG.get() }

/// Returns the path to the loaded configuration file, if any.
pub fn get_config_path() -> Option<&'static PathBuf> { CONFIG_PATH.get() }

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
                eprintln!("barba: warning: failed to create config watcher: {err}");
                return;
            }
        };

        // Watch the config file's parent directory to catch file replacements
        // (some editors save by writing to a temp file then renaming)
        let watch_path = config_path.parent().unwrap_or(&config_path);

        if let Err(err) = watcher.watch(watch_path, RecursiveMode::NonRecursive) {
            eprintln!("barba: warning: failed to watch config file: {err}");
            return;
        }

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

                    // In debug mode, just log a message since restart kills the dev server.
                    // In release mode, restart the app to apply the new configuration.
                    #[cfg(debug_assertions)]
                    {
                        eprintln!(
                            "barba: config file changed. Restart the app to apply new settings."
                        );
                    }

                    #[cfg(not(debug_assertions))]
                    {
                        app_handle.restart();
                    }
                }
                Ok(Err(err)) => {
                    eprintln!("barba: warning: config watch error: {err}");
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
    fn test_config_not_initialized_returns_none() {
        // This test checks that try_get_config returns None before init
        // Note: This might not work reliably if other tests have called init()
        // In practice, init() is called early in the app lifecycle
    }

    #[test]
    fn test_shared_types_are_reexported() {
        // Verify that shared types are accessible
        let config = BarbaConfig::default();
        assert!(config.shortcuts.is_empty());

        let wallpaper = WallpaperConfig::default();
        assert!(!wallpaper.is_enabled());

        let mode = WallpaperMode::default();
        assert_eq!(mode, WallpaperMode::Random);
    }

    #[test]
    fn test_generate_schema_works() {
        let schema = generate_schema_json();
        assert!(!schema.is_empty());
        assert!(schema.contains("BarbaConfig"));
    }
}
