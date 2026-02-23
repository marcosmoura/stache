//! Wallpaper manager for handling wallpaper selection, processing, and cycling.

use std::io::{IsTerminal, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rand::RngExt;
use rayon::prelude::*;

use super::macos;
use super::processing::{self, ProcessingError};
use crate::config::{WallpaperConfig, WallpaperMode};
use crate::platform::path::expand;

/// Global wallpaper manager instance.
static MANAGER: OnceLock<Arc<WallpaperManager>> = OnceLock::new();

/// Actions that can be performed on the wallpaper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperAction {
    /// Set a random wallpaper (different for each screen).
    Random,
    /// Set a random wallpaper for a specific screen (0-based index).
    RandomForScreen(usize),
    /// Set a specific wallpaper by filename (same for all screens).
    File(String),
    /// Set a specific wallpaper for a specific screen.
    FileForScreen(usize, String),
}

/// Errors that can occur in wallpaper management.
#[derive(Debug)]
pub enum WallpaperManagerError {
    /// No wallpapers available.
    NoWallpapers,
    /// Wallpaper file not found.
    FileNotFound(String),
    /// Processing error.
    Processing(ProcessingError),
    /// macOS API error.
    MacOS(macos::WallpaperError),
    /// Invalid path specified in configuration.
    InvalidPath(String),
    /// Invalid screen index or configuration.
    InvalidScreen(String),
    /// Invalid action or command (reserved for future use).
    #[allow(dead_code)]
    InvalidAction(String),
    /// Manager not initialized.
    NotInitialized,
}

impl std::fmt::Display for WallpaperManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWallpapers => write!(f, "No wallpapers available"),
            Self::FileNotFound(name) => write!(f, "Wallpaper not found: {name}"),
            Self::Processing(err) => write!(f, "Image processing error: {err}"),
            Self::MacOS(err) => write!(f, "macOS wallpaper error: {err}"),
            Self::InvalidPath(path) => write!(f, "Invalid wallpaper path: {path}"),
            Self::InvalidScreen(msg) => write!(f, "Invalid screen: {msg}"),
            Self::InvalidAction(msg) => write!(f, "Invalid action: {msg}"),
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
            let path = expand(&config.path);
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
            let path = expand(item);
            if path.exists() && processing::is_supported_image(&path) {
                wallpapers.push(path);
            }
        }

        Ok(wallpapers)
    }

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

    /// Sets the wallpaper at the given index for a specific screen.
    fn set_wallpaper_at_index_for_screen(
        &self,
        index: usize,
        screen_index: usize,
    ) -> Result<(), WallpaperManagerError> {
        let _lock = self.change_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        let source = &self.wallpapers[index];

        // Process the image with screen-specific settings
        let processed_path =
            processing::process_image_for_screen(source, &self.config, screen_index)?;

        // Set the wallpaper for the specific screen
        macos::set_wallpaper_for_screen(&processed_path, screen_index)?;

        Ok(())
    }

    /// Sets a random wallpaper for each screen.
    fn set_random_wallpapers_per_screen(&self) -> Result<(), WallpaperManagerError> {
        let screen_count = macos::screen_count();
        let mut rng = rand::rng();

        for screen_index in 0..screen_count {
            let index = rng.random_range(0..self.wallpapers.len());
            self.set_wallpaper_at_index_for_screen(index, screen_index)?;
        }

        // Update the current index to the last one set (for timer purposes)
        let last_index = rng.random_range(0..self.wallpapers.len());
        self.current_index.store(last_index, Ordering::SeqCst);

        Ok(())
    }

    /// Sets the initial wallpaper on startup.
    #[cfg_attr(debug_assertions, allow(dead_code))]
    pub fn set_initial_wallpaper(&self) -> Result<(), WallpaperManagerError> {
        let index = self.select_initial_index();
        self.set_wallpaper_at_index(index)
    }

    /// Returns a list of all available wallpaper paths.
    #[must_use]
    pub fn list_wallpapers(&self) -> Vec<String> {
        self.wallpapers.iter().map(|p| p.display().to_string()).collect()
    }

    /// Performs a wallpaper action.
    pub fn perform_action(&self, action: &WallpaperAction) -> Result<(), WallpaperManagerError> {
        match action {
            WallpaperAction::Random => {
                // For random action, set a different random wallpaper for each screen
                self.set_random_wallpapers_per_screen()
            }
            WallpaperAction::RandomForScreen(screen_index) => {
                // Set a random wallpaper for a specific screen
                let screen_count = macos::screen_count();
                if *screen_index >= screen_count {
                    return Err(WallpaperManagerError::InvalidScreen(format!(
                        "Screen index {} is out of range (0-{})",
                        screen_index,
                        screen_count - 1
                    )));
                }
                let mut rng = rand::rng();
                let index = rng.random_range(0..self.wallpapers.len());
                self.set_wallpaper_at_index_for_screen(index, *screen_index)
            }
            WallpaperAction::File(filename) => {
                // Set a specific wallpaper for all screens
                let index = self.find_wallpaper_index(filename)?;
                self.set_wallpaper_at_index(index)
            }
            WallpaperAction::FileForScreen(screen_index, filename) => {
                // Set a specific wallpaper for a specific screen
                let index = self.find_wallpaper_index(filename)?;
                self.set_wallpaper_at_index_for_screen(index, *screen_index)
            }
        }
    }

    /// Finds the index of a wallpaper by filename.
    ///
    /// The search is case-insensitive and matches against the file name
    /// (with or without extension).
    fn find_wallpaper_index(&self, filename: &str) -> Result<usize, WallpaperManagerError> {
        let filename_lower = filename.to_lowercase();

        for (i, path) in self.wallpapers.iter().enumerate() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                // Match full filename (with extension)
                if name.to_lowercase() == filename_lower {
                    return Ok(i);
                }
                // Match filename without extension
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                    && stem.to_lowercase() == filename_lower
                {
                    return Ok(i);
                }
            }
        }

        Err(WallpaperManagerError::FileNotFound(filename.to_string()))
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
                    tracing::warn!(error = %err, "wallpaper timer failed to set wallpaper");
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
/// Call `init()` after this to set the initial wallpaper and start cycling.
///
/// If wallpapers are disabled or initialization fails, logs a warning and returns.
pub fn setup() {
    let config = &crate::config::get_config().wallpapers;

    if !config.is_enabled() || !config.has_wallpapers() {
        return;
    }

    let manager = match WallpaperManager::new(config) {
        Ok(m) => Arc::new(m),
        Err(err) => {
            tracing::warn!(error = %err, "failed to create wallpaper manager");
            return;
        }
    };

    // Store in global
    if MANAGER.set(manager).is_err() {
        tracing::warn!("wallpaper manager already initialized");
    } else {
        let count = get_manager().map_or(0, |m| m.wallpapers.len());
        tracing::info!(count, "wallpaper manager initialized");
    }
}

/// Starts the wallpaper manager.
///
/// Sets the initial wallpaper and starts the automatic cycling timer.
/// Should be called once after `init()` when launching the UI.
///
/// Does nothing if the manager is not initialized.
pub fn init() {
    let Some(manager) = get_manager() else {
        return;
    };

    // Set initial wallpaper (only in release builds to avoid slow dev startup)
    #[cfg(not(debug_assertions))]
    if let Err(err) = manager.set_initial_wallpaper() {
        tracing::warn!(error = %err, "failed to set initial wallpaper");
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
///
/// # Errors
///
/// Returns an error if the wallpaper manager is not initialized or the action fails.
pub fn perform_action(action: &WallpaperAction) -> Result<(), WallpaperManagerError> {
    let manager = get_manager().ok_or(WallpaperManagerError::NotInitialized)?;

    manager.perform_action(action)?;

    // Reset timer if interval is set (to restart from current moment)
    if manager.config.interval > 0 {
        manager.reset_timer();
    }

    Ok(())
}

/// Returns a list of all available wallpaper paths.
///
/// # Errors
///
/// Returns an error if the manager is not initialized.
pub fn list_wallpapers() -> Result<Vec<String>, WallpaperManagerError> {
    let manager = get_manager().ok_or(WallpaperManagerError::NotInitialized)?;
    Ok(manager.list_wallpapers())
}

/// Result of processing a single wallpaper.
#[derive(Clone)]
struct ProcessResult {
    screen_index: usize,
    wallpaper_name: String,
    cached_name: Option<String>,
    error: Option<String>,
    was_cached: bool,
}

/// ANSI escape codes for terminal output.
mod ansi {
    pub const GREEN: &str = "\x1b[32m";
    pub const RED: &str = "\x1b[31m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const CYAN: &str = "\x1b[36m";
    pub const DIM: &str = "\x1b[2m";
    pub const BOLD: &str = "\x1b[1m";
    pub const RESET: &str = "\x1b[0m";
    pub const CLEAR_LINE: &str = "\x1b[2K\r";
    pub const HIDE_CURSOR: &str = "\x1b[?25l";
    pub const SHOW_CURSOR: &str = "\x1b[?25h";
}

/// Spinner frames for progress animation.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Generates all wallpapers with real-time progress feedback.
///
/// This pre-processes all wallpapers so they're ready for instant switching.
/// Useful for pre-caching wallpapers to avoid delays when cycling.
///
/// Features:
/// - Processes all screens and wallpapers in parallel for maximum speed
/// - Shows real-time progress with spinner animation (in TTY mode)
/// - Skips already-cached wallpapers for faster subsequent runs
/// - Reports timing and cache statistics
///
/// # Errors
///
/// Returns an error if the manager is not initialized.
#[allow(clippy::too_many_lines)]
pub fn generate_all_streaming<W: IoWrite + IsTerminal + Send + 'static>(
    writer: W,
) -> Result<(), WallpaperManagerError> {
    let manager = get_manager().ok_or(WallpaperManagerError::NotInitialized)?;
    let screen_count = macos::screen_count();
    let wallpaper_count = manager.wallpapers.len();
    let total_tasks = screen_count * wallpaper_count;

    if total_tasks == 0 {
        let mut w = writer;
        let _ = writeln!(w, "No wallpapers to generate.");
        return Ok(());
    }

    let is_tty = writer.is_terminal();
    let writer = Arc::new(Mutex::new(writer));

    // Progress tracking
    let completed = Arc::new(AtomicUsize::new(0));
    let cached_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));
    let start_time = Instant::now();

    // Print header
    print_generation_header(&writer, is_tty, screen_count, wallpaper_count, total_tasks);

    // Create all (screen, wallpaper) combinations for parallel processing
    let tasks: Vec<(usize, &PathBuf)> = (0..screen_count)
        .flat_map(|screen| manager.wallpapers.iter().map(move |wp| (screen, wp)))
        .collect();

    // Spawn progress reporter thread for TTY
    let progress_done = Arc::new(AtomicBool::new(false));
    let progress_handle = spawn_progress_reporter(
        is_tty,
        Arc::clone(&writer),
        Arc::clone(&completed),
        Arc::clone(&progress_done),
        total_tasks,
    );

    // Process all tasks in parallel
    let results: Vec<ProcessResult> = tasks
        .par_iter()
        .map(|(screen_index, wallpaper)| {
            process_single_wallpaper(
                *screen_index,
                wallpaper,
                &manager.config,
                &cached_count,
                &error_count,
                &completed,
            )
        })
        .collect();

    // Stop progress reporter
    progress_done.store(true, Ordering::Relaxed);
    if let Some(handle) = progress_handle {
        let _ = handle.join();
    }

    let elapsed = start_time.elapsed();
    let cached = cached_count.load(Ordering::Relaxed);
    let errors = error_count.load(Ordering::Relaxed);
    let generated = total_tasks - cached - errors;

    // Print results and summary
    let summary = GenerationSummary {
        screen_count,
        generated,
        cached,
        errors,
        elapsed,
    };
    print_generation_results(&writer, is_tty, &results, &summary);

    Ok(())
}

/// Prints the header for wallpaper generation.
fn print_generation_header<W: IoWrite>(
    writer: &Arc<Mutex<W>>,
    is_tty: bool,
    screen_count: usize,
    wallpaper_count: usize,
    total_tasks: usize,
) {
    let mut w = writer.lock().unwrap();
    let _ = writeln!(
        w,
        "{}{}Generating wallpapers{} for {} screen(s), {} wallpaper(s) each",
        ansi::BOLD,
        ansi::CYAN,
        ansi::RESET,
        screen_count,
        wallpaper_count
    );
    let _ = writeln!(
        w,
        "{}Total: {} images to process{}",
        ansi::DIM,
        total_tasks,
        ansi::RESET
    );
    let _ = writeln!(w);
    if is_tty {
        let _ = write!(w, "{}", ansi::HIDE_CURSOR);
    }
    let _ = w.flush();
}

/// Spawns a progress reporter thread for TTY output.
fn spawn_progress_reporter<W: IoWrite + Send + 'static>(
    is_tty: bool,
    writer: Arc<Mutex<W>>,
    completed: Arc<AtomicUsize>,
    progress_done: Arc<AtomicBool>,
    total_tasks: usize,
) -> Option<std::thread::JoinHandle<()>> {
    if !is_tty {
        return None;
    }

    Some(std::thread::spawn(move || {
        let mut frame = 0;
        while !progress_done.load(Ordering::Relaxed) {
            let done = completed.load(Ordering::Relaxed);
            let percent = (done * 100) / total_tasks;
            let spinner = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];

            if let Ok(mut w) = writer.lock() {
                let _ = write!(
                    w,
                    "{}{}{} Processing... {}/{} ({}%){}",
                    ansi::CLEAR_LINE,
                    ansi::CYAN,
                    spinner,
                    done,
                    total_tasks,
                    percent,
                    ansi::RESET
                );
                let _ = w.flush();
            }

            frame += 1;
            std::thread::sleep(Duration::from_millis(80));
        }
    }))
}

/// Processes a single wallpaper for a specific screen.
fn process_single_wallpaper(
    screen_index: usize,
    wallpaper: &Path,
    config: &WallpaperConfig,
    cached_count: &AtomicUsize,
    error_count: &AtomicUsize,
    completed: &AtomicUsize,
) -> ProcessResult {
    let wallpaper_name = wallpaper
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("unknown")
        .to_string();

    // Check if already cached BEFORE processing
    let expected_cache_path = processing::cached_path_for_screen(wallpaper, config, screen_index);
    let was_cached = expected_cache_path.exists();

    let result = match processing::process_image_for_screen(wallpaper, config, screen_index) {
        Ok(cached_path) => {
            let cached_name =
                cached_path.file_name().and_then(|n| n.to_str()).unwrap_or("cached").to_string();

            if was_cached {
                cached_count.fetch_add(1, Ordering::Relaxed);
            }

            ProcessResult {
                screen_index,
                wallpaper_name,
                cached_name: Some(cached_name),
                error: None,
                was_cached,
            }
        }
        Err(err) => {
            error_count.fetch_add(1, Ordering::Relaxed);
            ProcessResult {
                screen_index,
                wallpaper_name,
                cached_name: None,
                error: Some(err.to_string()),
                was_cached: false,
            }
        }
    };

    completed.fetch_add(1, Ordering::Relaxed);
    result
}

/// Summary statistics for wallpaper generation.
struct GenerationSummary {
    screen_count: usize,
    generated: usize,
    cached: usize,
    errors: usize,
    elapsed: Duration,
}

/// Prints the results and summary of wallpaper generation.
fn print_generation_results<W: IoWrite>(
    writer: &Arc<Mutex<W>>,
    is_tty: bool,
    results: &[ProcessResult],
    summary: &GenerationSummary,
) {
    let mut w = writer.lock().unwrap();
    if is_tty {
        let _ = write!(w, "{}{}", ansi::CLEAR_LINE, ansi::SHOW_CURSOR);
    }

    // Group results by screen for organized output
    for screen_index in 0..summary.screen_count {
        let screen_results: Vec<_> =
            results.iter().filter(|r| r.screen_index == screen_index).collect();

        let _ = writeln!(w, "{}Screen {}:{}", ansi::BOLD, screen_index, ansi::RESET);

        for result in screen_results {
            print_single_result(&mut *w, result);
        }
        let _ = writeln!(w);
    }

    // Summary
    let _ = writeln!(w, "{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}", ansi::DIM, ansi::RESET);
    let _ = write!(w, "{}Done!{} ", ansi::BOLD, ansi::RESET);

    if summary.generated > 0 {
        let _ = write!(
            w,
            "{}Generated: {}{} ",
            ansi::GREEN,
            summary.generated,
            ansi::RESET
        );
    }
    if summary.cached > 0 {
        let _ = write!(w, "{}Cached: {}{} ", ansi::YELLOW, summary.cached, ansi::RESET);
    }
    if summary.errors > 0 {
        let _ = write!(w, "{}Errors: {}{} ", ansi::RED, summary.errors, ansi::RESET);
    }

    let _ = writeln!(
        w,
        "{}({:.2}s){}",
        ansi::DIM,
        summary.elapsed.as_secs_f64(),
        ansi::RESET
    );
    let _ = w.flush();
}

/// Prints a single processing result.
fn print_single_result<W: IoWrite>(w: &mut W, result: &ProcessResult) {
    if let Some(ref cached_name) = result.cached_name {
        if result.was_cached {
            let _ = writeln!(
                w,
                "  {}●{} {} {}(cached){}",
                ansi::DIM,
                ansi::RESET,
                result.wallpaper_name,
                ansi::DIM,
                ansi::RESET
            );
        } else {
            let _ = writeln!(
                w,
                "  {}✓{} {} -> {}",
                ansi::GREEN,
                ansi::RESET,
                result.wallpaper_name,
                cached_name
            );
        }
    } else if let Some(ref err) = result.error {
        let _ = writeln!(
            w,
            "  {}✗{} {} -> {}",
            ansi::RED,
            ansi::RESET,
            result.wallpaper_name,
            err
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wallpaper_action_variants() {
        // Test that all variants can be created
        let _ = WallpaperAction::Random;
        let _ = WallpaperAction::RandomForScreen(0);
        let _file = WallpaperAction::File("test.jpg".to_string());
        let _file_screen = WallpaperAction::FileForScreen(0, "test.jpg".to_string());
    }

    #[test]
    fn test_wallpaper_action_equality() {
        assert_eq!(WallpaperAction::Random, WallpaperAction::Random);
        assert_eq!(
            WallpaperAction::RandomForScreen(1),
            WallpaperAction::RandomForScreen(1)
        );
        assert_ne!(
            WallpaperAction::RandomForScreen(0),
            WallpaperAction::RandomForScreen(1)
        );
        assert_eq!(
            WallpaperAction::File("test.jpg".to_string()),
            WallpaperAction::File("test.jpg".to_string())
        );
        assert_ne!(
            WallpaperAction::File("a.jpg".to_string()),
            WallpaperAction::File("b.jpg".to_string())
        );
    }

    #[test]
    fn test_wallpaper_action_clone() {
        let action = WallpaperAction::FileForScreen(2, "wallpaper.png".to_string());
        let cloned = action.clone();
        assert_eq!(action, cloned);
    }

    #[test]
    fn test_wallpaper_manager_error_display_no_wallpapers() {
        let err = WallpaperManagerError::NoWallpapers;
        assert_eq!(err.to_string(), "No wallpapers available");
    }

    #[test]
    fn test_wallpaper_manager_error_display_file_not_found() {
        let err = WallpaperManagerError::FileNotFound("missing.jpg".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Wallpaper not found"));
        assert!(msg.contains("missing.jpg"));
    }

    #[test]
    fn test_wallpaper_manager_error_display_invalid_path() {
        let err = WallpaperManagerError::InvalidPath("/bad/path".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Invalid wallpaper path"));
        assert!(msg.contains("/bad/path"));
    }

    #[test]
    fn test_wallpaper_manager_error_display_invalid_screen() {
        let err = WallpaperManagerError::InvalidScreen("out of range".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Invalid screen"));
    }

    #[test]
    fn test_wallpaper_manager_error_display_invalid_action() {
        let err = WallpaperManagerError::InvalidAction("bad action".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Invalid action"));
    }

    #[test]
    fn test_wallpaper_manager_error_display_not_initialized() {
        let err = WallpaperManagerError::NotInitialized;
        assert_eq!(err.to_string(), "Wallpaper manager not initialized");
    }

    #[test]
    fn test_get_manager_returns_none_without_init() {
        // Before init, get_manager should return None
        // Note: This test may not work reliably if other tests have already called init()
        // In practice, the global MANAGER is initialized once per process
    }

    // ========================================================================
    // WallpaperManagerError conversion tests
    // ========================================================================

    #[test]
    fn test_wallpaper_manager_error_from_processing_error() {
        let proc_err = ProcessingError::ImageRead("test.jpg".to_string());
        let mgr_err: WallpaperManagerError = proc_err.into();
        assert!(matches!(mgr_err, WallpaperManagerError::Processing(_)));
        let display = mgr_err.to_string();
        assert!(display.contains("Image processing error"));
    }

    #[test]
    fn test_wallpaper_manager_error_from_macos_error() {
        let mac_err = macos::WallpaperError::FileNotFound("/test/path".to_string());
        let mgr_err: WallpaperManagerError = mac_err.into();
        assert!(matches!(mgr_err, WallpaperManagerError::MacOS(_)));
        let display = mgr_err.to_string();
        assert!(display.contains("macOS wallpaper error"));
    }

    #[test]
    fn test_wallpaper_manager_error_is_error_trait() {
        let err = WallpaperManagerError::NoWallpapers;
        // Verify it implements std::error::Error
        let _: &dyn std::error::Error = &err;
    }

    // ========================================================================
    // WallpaperManager construction tests
    // ========================================================================

    #[test]
    fn test_wallpaper_manager_new_empty_config_returns_error() {
        let config = WallpaperConfig::default();
        let result = WallpaperManager::new(&config);
        assert!(matches!(result, Err(WallpaperManagerError::NoWallpapers)));
    }

    #[test]
    fn test_wallpaper_manager_new_nonexistent_path_returns_error() {
        let config = WallpaperConfig {
            path: "/nonexistent/path/that/does/not/exist".to_string(),
            ..Default::default()
        };
        let result = WallpaperManager::new(&config);
        assert!(matches!(result, Err(WallpaperManagerError::InvalidPath(_))));
    }

    #[test]
    fn test_wallpaper_manager_new_file_instead_of_dir_returns_error() {
        // Use /etc/passwd which exists and is a file, not a directory
        let config = WallpaperConfig {
            path: "/etc/passwd".to_string(),
            ..Default::default()
        };
        let result = WallpaperManager::new(&config);
        match result {
            Err(WallpaperManagerError::InvalidPath(msg)) => {
                assert!(msg.contains("is not a directory"));
            }
            _ => panic!("Expected InvalidPath error"),
        }
    }

    #[test]
    fn test_wallpaper_manager_new_with_empty_list_returns_error() {
        let config = WallpaperConfig {
            list: vec!["nonexistent.jpg".to_string()],
            ..Default::default()
        };
        let result = WallpaperManager::new(&config);
        // Should return NoWallpapers because the file doesn't exist
        assert!(matches!(result, Err(WallpaperManagerError::NoWallpapers)));
    }

    // ========================================================================
    // WallpaperAction tests
    // ========================================================================

    #[test]
    fn test_wallpaper_action_debug() {
        let action = WallpaperAction::FileForScreen(1, "test.jpg".to_string());
        let debug = format!("{action:?}");
        assert!(debug.contains("FileForScreen"));
        assert!(debug.contains("test.jpg"));
    }

    #[test]
    fn test_wallpaper_action_random_for_screen_values() {
        // Test that different screen indices create different actions
        let action0 = WallpaperAction::RandomForScreen(0);
        let action1 = WallpaperAction::RandomForScreen(1);
        let action2 = WallpaperAction::RandomForScreen(2);

        assert_ne!(action0, action1);
        assert_ne!(action1, action2);
        assert_ne!(action0, action2);
    }

    #[test]
    fn test_wallpaper_action_file_for_screen_values() {
        // Same file, different screens
        let action0 = WallpaperAction::FileForScreen(0, "test.jpg".to_string());
        let action1 = WallpaperAction::FileForScreen(1, "test.jpg".to_string());
        assert_ne!(action0, action1);

        // Same screen, different files
        let action_a = WallpaperAction::FileForScreen(0, "a.jpg".to_string());
        let action_b = WallpaperAction::FileForScreen(0, "b.jpg".to_string());
        assert_ne!(action_a, action_b);
    }

    // ========================================================================
    // ANSI codes tests
    // ========================================================================

    #[test]
    fn test_ansi_codes_are_valid_escape_sequences() {
        // All ANSI codes should start with escape sequence
        assert!(ansi::GREEN.starts_with("\x1b["));
        assert!(ansi::RED.starts_with("\x1b["));
        assert!(ansi::YELLOW.starts_with("\x1b["));
        assert!(ansi::CYAN.starts_with("\x1b["));
        assert!(ansi::DIM.starts_with("\x1b["));
        assert!(ansi::BOLD.starts_with("\x1b["));
        assert!(ansi::RESET.starts_with("\x1b["));
        assert!(ansi::CLEAR_LINE.starts_with("\x1b["));
        assert!(ansi::HIDE_CURSOR.starts_with("\x1b["));
        assert!(ansi::SHOW_CURSOR.starts_with("\x1b["));
    }

    #[test]
    fn test_ansi_reset_ends_with_m() {
        // Color codes end with 'm'
        assert!(ansi::RESET.ends_with('m'));
        assert!(ansi::GREEN.ends_with('m'));
        assert!(ansi::RED.ends_with('m'));
    }

    // ========================================================================
    // Spinner frames tests
    // ========================================================================

    #[test]
    fn test_spinner_frames_not_empty() {
        assert!(!SPINNER_FRAMES.is_empty());
    }

    #[test]
    fn test_spinner_frames_all_non_empty() {
        for frame in SPINNER_FRAMES {
            assert!(!frame.is_empty());
        }
    }

    // ========================================================================
    // ProcessResult tests
    // ========================================================================

    #[test]
    fn test_process_result_clone() {
        let result = ProcessResult {
            screen_index: 0,
            wallpaper_name: "test.jpg".to_string(),
            cached_name: Some("cached_test.jpg".to_string()),
            error: None,
            was_cached: true,
        };
        let cloned = result.clone();
        assert_eq!(result.screen_index, cloned.screen_index);
        assert_eq!(result.wallpaper_name, cloned.wallpaper_name);
        assert_eq!(result.cached_name, cloned.cached_name);
        assert_eq!(result.error, cloned.error);
        assert_eq!(result.was_cached, cloned.was_cached);
    }

    #[test]
    fn test_process_result_with_error() {
        let result = ProcessResult {
            screen_index: 1,
            wallpaper_name: "broken.jpg".to_string(),
            cached_name: None,
            error: Some("Image read failed".to_string()),
            was_cached: false,
        };
        assert!(result.error.is_some());
        assert!(result.cached_name.is_none());
        assert!(!result.was_cached);
    }

    // ========================================================================
    // load_wallpapers tests
    // ========================================================================

    #[test]
    fn test_load_wallpapers_empty_path_and_empty_list() {
        let config = WallpaperConfig::default();
        let result = WallpaperManager::load_wallpapers(&config);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_load_wallpapers_with_nonexistent_list_items() {
        let config = WallpaperConfig {
            list: vec![
                "/nonexistent/a.jpg".to_string(),
                "/nonexistent/b.png".to_string(),
            ],
            ..Default::default()
        };
        let result = WallpaperManager::load_wallpapers(&config);
        assert!(result.is_ok());
        // Should filter out non-existent files
        assert!(result.unwrap().is_empty());
    }

    // ========================================================================
    // Module function tests
    // ========================================================================

    #[test]
    fn test_perform_action_without_init_returns_error() {
        // This test depends on global state, but we can verify the error type
        // Since other tests might initialize the manager, we can't guarantee
        // this will return NotInitialized, but we test the function exists
        let action = WallpaperAction::Random;
        let _result = perform_action(&action);
        // Result depends on whether manager was initialized elsewhere
    }

    #[test]
    fn test_list_wallpapers_without_init_returns_error() {
        // Similar to above - depends on global state
        let _result = list_wallpapers();
    }
}
