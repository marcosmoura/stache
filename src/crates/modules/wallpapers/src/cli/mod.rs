//! The `stache wallpaper` command line.
//!
//! The module crate owns the clap tree and the executor; the application
//! composition root contributes it and injects every environment dependency,
//! so `stache-cli` stays SDK-only and tests never touch a real desktop, home,
//! socket or sleep.
//!
//! - `set` validates its arguments first, resolves a file token on the client
//!   side (see `resolve_token`), then sends `wallpapers.set` once through a
//!   [`SetTransport`]. Only a proven absence ([`ClientError::NotRunning`])
//!   permits exactly one standalone application on the process main thread.
//!   Every other transport failure is reported as an uncertain outcome without
//!   a local application.
//! - `list` and `generate-all` are local read/cache work using the current
//!   configuration file, irrespective of `enabled`.
//!
//! # Threading
//!
//! The standalone path never builds a [`crate::WallpapersService`]: its
//! coordinator dispatches native work to a main queue that nothing drains in a
//! CLI process. Instead the process main thread snapshots, prepares and applies
//! inline through the injected guarded backend, whose production executor runs
//! inline on main.

mod adapters;
mod generate;
mod set;
mod token;

#[cfg(test)]
mod tests;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Arg, ArgAction, ArgMatches, Command};
use stache_macos::GuardedDesktopBackend;
use stache_sdk::ipc::{ClientError, IpcClient, Request, Response, RetryPolicy};
use stache_sdk::{
    AppEnvironment, CancellationToken, CliContribution, CliExecutor, ConfigEnvironment,
    ConfigFileId, ConfigPaths,
};

pub use self::adapters::{render_outcome, response_uncertainty_body};
pub use self::token::{TokenError, TokenResolution, resolve_token};
use crate::runtime::{
    Processor, RandomSource, RealProcessor, RuntimeClock, SystemRandom, SystemRuntimeClock,
    WALLPAPERS_CLIENT_BUDGET, WallpaperScreen,
};
use crate::{WallpapersConfig, WallpapersSetParams};

/// Applies the CLI's JSON terminal styling.
pub type JsonHighlighter = Arc<dyn Fn(&str, bool) -> String + Send + Sync>;

/// The legacy parser error text for a bad `--screen` value.
fn screen_error(value: &str) -> String {
    format!("Invalid screen value '{value}'. Expected 'all', 'main', or a positive integer.")
}

/// Parses `--screen`: `all` and `main` ignore case, anything else is a `u32`.
///
/// `0` keeps the legacy primary alias. A value that does not fit `u32` is a
/// parse error, so no processing happens for it.
///
/// # Errors
/// Returns the legacy `Invalid screen value` text.
pub fn parse_screen(value: &str) -> Result<WallpaperScreen, String> {
    match value.to_lowercase().as_str() {
        "all" => Ok(WallpaperScreen::All),
        "main" => Ok(WallpaperScreen::Main),
        _ => value
            .parse::<u32>()
            .map(|index| WallpaperScreen::Index { index })
            .map_err(|_| screen_error(value)),
    }
}

/// One cancellation scope for a command run.
///
/// The composition root decides how a signal reaches the token; the guard is
/// dropped when the command finishes so a handler can be released.
pub struct CancelGuard {
    token: CancellationToken,
    _release: Option<Box<dyn Send>>,
}

impl CancelGuard {
    /// A scope whose token is cancelled by the holder of a clone.
    #[must_use]
    pub fn new(token: CancellationToken) -> Self { Self { token, _release: None } }

    /// A scope that keeps `release` alive until the command finishes.
    #[must_use]
    pub fn with_release(token: CancellationToken, release: Box<dyn Send>) -> Self {
        Self { token, _release: Some(release) }
    }

    /// The token observed by processing and application.
    #[must_use]
    pub const fn token(&self) -> &CancellationToken { &self.token }
}

/// Opens a cancellation scope when a command starts.
pub type CancelScope = Arc<dyn Fn() -> CancelGuard + Send + Sync>;

/// A scope that is never cancelled.
#[must_use]
pub fn never_cancelled() -> CancelScope { Arc::new(|| CancelGuard::new(CancellationToken::new())) }

/// Sends the one `wallpapers.set` mutation to the control socket.
///
/// The CLI calls [`SetTransport::send`] at most once per invocation.
pub trait SetTransport: Send + Sync {
    /// Sends one request and returns the decoded response.
    ///
    /// # Errors
    /// Returns the client error unchanged; the caller classifies it.
    fn send(&self, socket: &Path, request: &Request) -> Result<Response, ClientError>;
}

/// The production transport over [`IpcClient`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpcTransport {
    retries: RetryPolicy,
    timeout: Duration,
}

impl IpcTransport {
    /// The shipped route: the legacy absent-only retry policy and the
    /// [`WALLPAPERS_CLIENT_BUDGET`] response wait.
    #[must_use]
    pub const fn production() -> Self {
        Self {
            retries: RetryPolicy::legacy(),
            timeout: WALLPAPERS_CLIENT_BUDGET,
        }
    }

    /// Explicit limits for tests that must not sleep.
    #[must_use]
    pub const fn with_options(retries: RetryPolicy, timeout: Duration) -> Self {
        Self { retries, timeout }
    }
}

impl SetTransport for IpcTransport {
    fn send(&self, socket: &Path, request: &Request) -> Result<Response, ClientError> {
        IpcClient::with_options(socket, self.retries, self.timeout).request(request)
    }
}

/// Everything the executor needs from its composition root.
pub struct WallpapersCliEnv {
    /// Configuration directory discovery (including the home for `~`).
    pub config: ConfigEnvironment,
    /// Identity cache and runtime roots.
    pub app: AppEnvironment,
    /// The invocation working directory.
    pub current_dir: Arc<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
    /// The control-socket transport.
    pub transport: Arc<dyn SetTransport>,
    /// The guarded desktop used for the local application and snapshots.
    pub desktop: Arc<dyn GuardedDesktopBackend>,
    /// The image processor.
    pub processor: Arc<dyn Processor>,
    /// Random draws for standalone `--random`.
    pub random: Arc<dyn RandomSource>,
    /// Monotonic time for `generate-all` timing.
    pub clock: Arc<dyn RuntimeClock>,
    /// Opens the cancellation scope of one run.
    pub cancel: CancelScope,
    /// JSON terminal styling.
    pub highlight: JsonHighlighter,
    /// Spinner frame interval for a terminal `generate-all`.
    pub progress_interval: Duration,
    /// Observes batch workers; a test seam that is a no-op in production.
    pub(crate) observer: Arc<dyn crate::batch::BatchObserver + Send + Sync>,
}

impl WallpapersCliEnv {
    /// Production defaults around an injected desktop, styling and
    /// cancellation scope.
    #[must_use]
    pub fn new(
        config: ConfigEnvironment,
        app: AppEnvironment,
        desktop: Arc<dyn GuardedDesktopBackend>,
        highlight: JsonHighlighter,
        cancel: CancelScope,
    ) -> Self {
        Self {
            config,
            app,
            current_dir: Arc::new(std::env::current_dir),
            transport: Arc::new(IpcTransport::production()),
            desktop,
            processor: Arc::new(RealProcessor),
            random: Arc::new(SystemRandom),
            clock: Arc::new(SystemRuntimeClock),
            cancel,
            highlight,
            progress_interval: Duration::from_millis(80),
            observer: Arc::new(crate::batch::NoObserver),
        }
    }
}

/// Builds the `wallpaper` contribution.
#[must_use]
pub fn contribution(env: WallpapersCliEnv) -> CliContribution {
    CliContribution::new(command, WallpapersCli { env })
}

/// Builds the `wallpaper` command tree.
#[must_use]
pub fn command() -> Command {
    Command::new("wallpaper")
        .about("Wallpaper management commands")
        .next_display_order(None)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("set")
                .about("Set the desktop wallpaper")
                .long_about(
                    "Set the desktop wallpaper.\n\nSet a specific wallpaper by providing a path, \
                     or use --random to set\na random wallpaper from the configured wallpaper \
                     directory.",
                )
                .after_long_help(SET_EXAMPLES)
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .help("The path to the image to use as wallpaper"),
                )
                .arg(
                    Arg::new("random")
                        .long("random")
                        .short('r')
                        .action(ArgAction::SetTrue)
                        .help("Set a random wallpaper from the configured wallpaper directory"),
                )
                .arg(
                    Arg::new("screen")
                        .long("screen")
                        .short('s')
                        .value_name("SCREEN")
                        .default_value("all")
                        .value_parser(parse_screen)
                        .help(
                            "Specify which screen(s) to set the wallpaper on. Values: all, main, \
                             <index> (1-based). Default: all",
                        ),
                ),
        )
        .subcommand(
            Command::new("generate-all").about("Pre-generate all wallpapers").long_about(
                "Pre-generate all wallpapers.\n\nProcesses all wallpapers from the \
                     configuration and stores them in the cache directory. Useful for \
                     pre-caching wallpapers to avoid delays when switching.",
            ),
        )
        .subcommand(
            Command::new("list").about("List available wallpapers").long_about(
                "List available wallpapers.\n\nReturns a JSON array of wallpaper paths from the \
                 configured wallpaper directory or list.",
            ),
        )
}

const SET_EXAMPLES: &str = "Examples:
  stache wallpaper set /path/to/image.jpg               # Specific wallpaper for all screens
  stache wallpaper set /path/to/image.jpg --screen main # Specific wallpaper for main screen
  stache wallpaper set /path/to/image.jpg --screen 2    # Specific wallpaper for screen 2
  stache wallpaper set --random                         # Random wallpaper for all screens
  stache wallpaper set --random --screen main           # Random wallpaper for main screen
  stache wallpaper set --random --screen 2              # Random wallpaper for screen 2";

/// The executor behind [`contribution`].
struct WallpapersCli {
    env: WallpapersCliEnv,
}

impl CliExecutor for WallpapersCli {
    fn execute(
        &self,
        matches: &ArgMatches,
        ansi: bool,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> ExitCode {
        match matches.subcommand() {
            Some(("set", sub)) => set::run(&self.env, sub, out, err),
            Some(("list", sub)) => generate::list(&self.env, sub, ansi, out, err),
            Some(("generate-all", sub)) => generate::generate_all(&self.env, sub, ansi, out, err),
            _ => fail(err, "wallpaper requires a subcommand"),
        }
    }
}

/// `stache: {message}` on stderr, exit 1.
pub(crate) fn fail(err: &mut dyn Write, message: &str) -> ExitCode {
    let _ = writeln!(err, "stache: {}", visible_controls(message));
    ExitCode::FAILURE
}

/// Escapes control characters other than newline and tab, so no path or OS
/// text from a lower layer can write a raw escape sequence to the terminal.
fn visible_controls(text: &str) -> String {
    use std::fmt::Write as _;
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() && c != '\n' && c != '\t' {
            let _ = write!(escaped, "\\u{:04x}", u32::from(c));
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// `stache: Wallpaper error: {body}` on stderr, exit 1.
pub(crate) fn wallpaper_error(err: &mut dyn Write, body: &str) -> ExitCode {
    let _ = writeln!(err, "stache: Wallpaper error: {}", visible_controls(body));
    ExitCode::FAILURE
}

/// The global `-c/--config` value propagated into a subcommand's matches.
fn config_override(matches: &ArgMatches) -> Option<PathBuf> {
    matches.try_get_one::<PathBuf>("config").ok().flatten().cloned()
}

/// Resolves the selected configuration directory like the built-in commands.
pub(crate) fn config_paths(
    env: &WallpapersCliEnv,
    matches: &ArgMatches,
) -> Result<ConfigPaths, String> {
    ConfigPaths::resolve(config_override(matches), &env.config).map_err(|error| error.to_string())
}

/// Reads the current `wallpapers.jsonc`.
///
/// A missing file means defaults. A present file that cannot be read, parsed
/// or validated is a failure and never replaced by defaults.
pub(crate) fn load_config(paths: &ConfigPaths) -> Result<WallpapersConfig, String> {
    use stache_sdk::config::loader;

    let file = paths.module_file(WallpapersConfig::ID);
    let id = ConfigFileId::module(WallpapersConfig::ID);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(WallpapersConfig::default());
        }
        Err(error) => {
            return Err(format!(
                "{}: {}",
                adapters::quoted(&file.to_string_lossy()),
                adapters::quoted(&error.to_string())
            ));
        }
    };
    let config: WallpapersConfig =
        loader::decode(id, &file, &text).map_err(|diagnostic| diagnostic.to_string())?;
    loader::validate(id, &file, &config).map_err(|diagnostic| diagnostic.to_string())?;
    Ok(config)
}

/// The `wallpapers.set` request for resolved arguments.
pub(crate) fn set_request(params: &WallpapersSetParams) -> Result<Request, serde_json::Error> {
    Request::new("wallpapers", "set", params)
}
