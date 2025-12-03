//! Wallpaper manager for handling wallpaper selection, processing, and cycling.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rand::Rng;

use super::macos;
use super::processing::{self, ProcessingError};
use crate::config::{WallpaperConfig, WallpaperMode};

/// Global wallpaper manager instance.
static MANAGER: OnceLock<Arc<WallpaperManager>> = OnceLock::new();

/// Expands `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(path)
}

/// Actions that can be performed on the wallpaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperAction {
    /// Set the next wallpaper in sequence.
    Next,
    /// Set the previous wallpaper in sequence.
    Previous,
    /// Set a random wallpaper.
    Random,
    /// Set a specific wallpaper by index (0-based).
    Index(usize),
}

/// Errors that can occur in wallpaper management.
#[derive(Debug)]
pub enum WallpaperManagerError {
    /// No wallpapers available.
    NoWallpapers,
    /// Invalid wallpaper index.
    InvalidIndex(usize),
    /// Processing error.
    Processing(ProcessingError),
    /// macOS API error.
    MacOS(macos::WallpaperError),
    /// Invalid path specified in configuration.
    InvalidPath(String),
    /// Manager not initialized.
    NotInitialized,
}

impl std::fmt::Display for WallpaperManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWallpapers => write!(f, "No wallpapers available"),
            Self::InvalidIndex(idx) => write!(f, "Invalid wallpaper index: {idx}"),
            Self::Processing(err) => write!(f, "Image processing error: {err}"),
            Self::MacOS(err) => write!(f, "macOS wallpaper error: {err}"),
            Self::InvalidPath(path) => write!(f, "Invalid wallpaper path: {path}"),
            Self::NotInitialized => write!(f, "Wallpaper manager not initialized"),
        }
    }
}

impl std::error::Error for WallpaperManagerError {}

impl From<ProcessingError> for WallpaperManagerError {
    fn from(err: ProcessingError) -> Self { Self::Processing(err) }
}

impl From<macos::WallpaperError> for WallpaperManagerError {
    fn from(err: macos::WallpaperError) -> Self { Self::MacOS(err) }
}

/// Manages wallpaper selection, processing, and automatic cycling.
pub struct WallpaperManager {
    /// List of wallpaper file paths.
    wallpapers: Vec<PathBuf>,
    /// Configuration for processing.
    config: WallpaperConfig,
    /// Current wallpaper index (for sequential mode).
    current_index: AtomicUsize,
    /// Whether the cycling timer is running.
    timer_running: AtomicBool,
    /// Mutex for thread-safe wallpaper changes.
    change_lock: Mutex<()>,
}

impl WallpaperManager {
    /// Creates a new wallpaper manager from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is invalid or no wallpapers are found.
    pub fn new(config: &WallpaperConfig) -> Result<Self, WallpaperManagerError> {
        let wallpapers = Self::load_wallpapers(config)?;

        if wallpapers.is_empty() {
            return Err(WallpaperManagerError::NoWallpapers);
        }

        Ok(Self {
            wallpapers,
            config: config.clone(),
            current_index: AtomicUsize::new(0),
            timer_running: AtomicBool::new(false),
            change_lock: Mutex::new(()),
        })
    }

    /// Loads wallpaper paths from the configuration.
    fn load_wallpapers(config: &WallpaperConfig) -> Result<Vec<PathBuf>, WallpaperManagerError> {
        // If path is specified, read all images from that directory
        if !config.path.is_empty() {
            let path = expand_tilde(&config.path);
            if !path.exists() {
                return Err(WallpaperManagerError::InvalidPath(config.path.clone()));
            }
            if !path.is_dir() {
                return Err(WallpaperManagerError::InvalidPath(format!(
                    "{} is not a directory",
                    config.path
                )));
            }

            let images = processing::list_images_in_directory(&path);
            return Ok(images);
        }

        // Otherwise, use the list (treating them as full paths or relative to home)
        let mut wallpapers = Vec::new();
        for item in &config.list {
            let path = expand_tilde(item);
            if path.exists() && processing::is_supported_image(&path) {
                wallpapers.push(path);
            }
        }

        Ok(wallpapers)
    }

    /// Returns the number of available wallpapers.
    #[must_use]
    #[allow(dead_code)]
    pub const fn count(&self) -> usize { self.wallpapers.len() }

    /// Returns the current wallpaper index.
    #[must_use]
    #[allow(dead_code)]
    pub fn current_index(&self) -> usize { self.current_index.load(Ordering::SeqCst) }

    /// Selects the initial wallpaper based on mode.
    #[cfg_attr(debug_assertions, allow(dead_code))]
    fn select_initial_index(&self) -> usize {
        match self.config.mode {
            WallpaperMode::Random => {
                let mut rng = rand::rng();
                rng.random_range(0..self.wallpapers.len())
            }
            WallpaperMode::Sequential => 0,
        }
    }

    /// Selects the next wallpaper index based on mode.
    fn select_next_index(&self) -> usize {
        match self.config.mode {
            WallpaperMode::Random => {
                let mut rng = rand::rng();
                rng.random_range(0..self.wallpapers.len())
            }
            WallpaperMode::Sequential => {
                let current = self.current_index.load(Ordering::SeqCst);
                (current + 1) % self.wallpapers.len()
            }
        }
    }

    /// Sets the wallpaper at the given index.
    fn set_wallpaper_at_index(&self, index: usize) -> Result<(), WallpaperManagerError> {
        if index >= self.wallpapers.len() {
            return Err(WallpaperManagerError::InvalidIndex(index));
        }

        let _lock = self.change_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        let source = &self.wallpapers[index];

        // Process the image (applies blur and rounded corners, uses cache if available)
        let processed_path = processing::process_image(source, &self.config)?;

        // Set the wallpaper using macOS APIs
        macos::set_wallpaper(&processed_path)?;

        // Update the current index
        self.current_index.store(index, Ordering::SeqCst);

        Ok(())
    }

    /// Sets the initial wallpaper on startup.
    #[cfg_attr(debug_assertions, allow(dead_code))]
    pub fn set_initial_wallpaper(&self) -> Result<(), WallpaperManagerError> {
        let index = self.select_initial_index();
        self.set_wallpaper_at_index(index)
    }

    /// Performs a wallpaper action.
    pub fn perform_action(&self, action: WallpaperAction) -> Result<(), WallpaperManagerError> {
        let index = match action {
            WallpaperAction::Next => {
                let current = self.current_index.load(Ordering::SeqCst);
                (current + 1) % self.wallpapers.len()
            }
            WallpaperAction::Previous => {
                let current = self.current_index.load(Ordering::SeqCst);
                if current == 0 {
                    self.wallpapers.len() - 1
                } else {
                    current - 1
                }
            }
            WallpaperAction::Random => {
                let mut rng = rand::rng();
                rng.random_range(0..self.wallpapers.len())
            }
            WallpaperAction::Index(idx) => idx,
        };

        self.set_wallpaper_at_index(index)
    }

    /// Starts the automatic wallpaper cycling timer.
    ///
    /// Does nothing if the interval is 0.
    pub fn start_timer(self: &Arc<Self>) {
        if self.config.interval == 0 {
            return;
        }

        if self.timer_running.swap(true, Ordering::SeqCst) {
            // Timer already running
            return;
        }

        let manager = Arc::clone(self);
        let interval = Duration::from_secs(self.config.interval);

        std::thread::spawn(move || {
            loop {
                std::thread::sleep(interval);

                if !manager.timer_running.load(Ordering::SeqCst) {
                    break;
                }

                let next_index = manager.select_next_index();
                if let Err(err) = manager.set_wallpaper_at_index(next_index) {
                    eprintln!("barba: wallpaper timer error: {err}");
                }
            }
        });
    }

    /// Stops the automatic wallpaper cycling timer.
    pub fn stop_timer(&self) { self.timer_running.store(false, Ordering::SeqCst); }

    /// Resets the timer (stops and starts it again).
    pub fn reset_timer(self: &Arc<Self>) {
        self.stop_timer();
        // Small delay to ensure the old timer thread has exited
        std::thread::sleep(Duration::from_millis(100));
        self.start_timer();
    }
}

/// Initializes the global wallpaper manager.
///
/// Reads the wallpaper configuration from the global config and creates
/// the manager instance. This only loads wallpapers but does NOT set the
/// initial wallpaper or start the timer.
///
/// Call `start()` after this to set the initial wallpaper and start cycling.
///
/// If wallpapers are disabled or initialization fails, logs a warning and returns.
pub fn init() {
    let config = &crate::config::get_config().wallpapers;

    if !config.is_enabled() {
        return;
    }

    let manager = match WallpaperManager::new(config) {
        Ok(m) => Arc::new(m),
        Err(err) => {
            eprintln!("barba: warning: failed to create wallpaper manager: {err}");
            return;
        }
    };

    // Store in global
    if MANAGER.set(manager).is_err() {
        eprintln!("barba: warning: wallpaper manager already initialized");
    }
}

/// Starts the wallpaper manager.
///
/// Sets the initial wallpaper and starts the automatic cycling timer.
/// Should be called once after `init()` when launching the UI.
///
/// Does nothing if the manager is not initialized.
pub fn start() {
    let Some(manager) = get_manager() else {
        return;
    };

    // Set initial wallpaper (only in release builds to avoid slow dev startup)
    #[cfg(not(debug_assertions))]
    if let Err(err) = manager.set_initial_wallpaper() {
        eprintln!("barba: warning: failed to set initial wallpaper: {err}");
        return;
    }

    // Start the timer if interval is set
    manager.start_timer();
}

/// Returns the global wallpaper manager instance.
pub fn get_manager() -> Option<&'static Arc<WallpaperManager>> { MANAGER.get() }

/// Performs a wallpaper action using the global manager.
///
/// This is the main entry point for CLI commands.
pub fn perform_action(action: WallpaperAction) -> Result<(), WallpaperManagerError> {
    let manager = get_manager().ok_or(WallpaperManagerError::NotInitialized)?;

    manager.perform_action(action)?;

    // Reset timer if interval is set (to restart from current moment)
    if manager.config.interval > 0 {
        manager.reset_timer();
    }

    Ok(())
}

/// Generates all wallpapers and stores them in the cache directory.
///
/// This pre-processes all wallpapers so they're ready for instant switching.
/// Useful for pre-caching wallpapers to avoid delays when cycling.
///
/// # Errors
///
/// Returns an error if the manager is not initialized or processing fails.
pub fn generate_all() -> Result<(), WallpaperManagerError> {
    let manager = get_manager().ok_or(WallpaperManagerError::NotInitialized)?;

    let total = manager.wallpapers.len();
    println!("Generating {total} wallpapers...");

    for (i, wallpaper) in manager.wallpapers.iter().enumerate() {
        let name = wallpaper.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");

        print!("[{}/{}] Processing {}... ", i + 1, total, name);

        match processing::process_image(wallpaper, &manager.config) {
            Ok(cached_path) => {
                println!(
                    "done ({})",
                    cached_path.file_name().and_then(|n| n.to_str()).unwrap_or("cached")
                );
            }
            Err(err) => {
                println!("error: {err}");
            }
        }
    }

    println!("Wallpaper generation complete.");
    Ok(())
}

/// Parses a CLI argument into a wallpaper action.
///
/// # Arguments
///
/// * `arg` - The CLI argument: "next", "previous", "random", or an index number
///
/// # Returns
///
/// The corresponding `WallpaperAction`, or `None` if the argument is invalid.
pub fn parse_action(arg: &str) -> Option<WallpaperAction> {
    match arg.to_lowercase().as_str() {
        "next" => Some(WallpaperAction::Next),
        "previous" | "prev" => Some(WallpaperAction::Previous),
        "random" => Some(WallpaperAction::Random),
        _ => arg.parse::<usize>().ok().map(WallpaperAction::Index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_action() {
        assert_eq!(parse_action("next"), Some(WallpaperAction::Next));
        assert_eq!(parse_action("NEXT"), Some(WallpaperAction::Next));
        assert_eq!(parse_action("previous"), Some(WallpaperAction::Previous));
        assert_eq!(parse_action("prev"), Some(WallpaperAction::Previous));
        assert_eq!(parse_action("random"), Some(WallpaperAction::Random));
        assert_eq!(parse_action("0"), Some(WallpaperAction::Index(0)));
        assert_eq!(parse_action("5"), Some(WallpaperAction::Index(5)));
        assert_eq!(parse_action("invalid"), None);
    }
}
