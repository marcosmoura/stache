//! The `stache tiling` command line.
//!
//! The module crate owns the clap tree, the executor, the table/JSON renderers
//! and the error adapters; the application composition root contributes it and
//! injects the environment, so `stache-cli` stays SDK-only. The CLI never
//! starts the desktop application, the Accessibility owner or the module: it
//! talks to a running same-config host over the control socket and otherwise
//! fails (queries print the legacy offline body on stdout and exit 1;
//! mutations print the adapter on stderr and exit 1).
//!
//! # Exit and stream contract
//!
//! * Parse, arity, value-enum and flag-conflict errors exit 2 (clap).
//! * Semantic and action failures exit 1 on stderr as `stache: IPC error:
//!   {body}` (argument bodies such as the empty-request and resize validation
//!   bodies keep the legacy `stache: {body}` form).
//! * A mutation answered `enqueued` or `applied` is silent and exits 0. That
//!   is **admission**: the actor took the request, not proof that a window
//!   moved. `partial` and `uncertain` exit 1 with their fixed bodies.
//! * Query failures print their legacy body on **stdout** and exit 1.

pub(crate) mod render;

#[cfg(test)]
mod render_tests;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::builder::{PossibleValue, PossibleValuesParser};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use stache_sdk::ipc::{
    ClientError, CoreStatus, IO_TIMEOUT, IpcClient, Request, Response, RetryPolicy, WireError,
};
use stache_sdk::{
    AppEnvironment, AppPaths, CliContribution, CliExecutor, ConfigEnvironment, ConfigPaths,
};

use crate::config::LayoutType;
use crate::dto::{
    AppDto, OperationResponse, QueryKind, QueryParams, ResizeArg, ScreenDto, StatusDto, WindowDto,
    WindowParams, WorkspaceDto, WorkspaceParams, method,
};
use crate::ipc::body;

/// Applies the CLI's JSON terminal styling.
pub type JsonHighlighter = Arc<dyn Fn(&str, bool) -> String + Send + Sync>;

/// The wire module name of the Tiling handler.
const MODULE: &str = "tiling";

/// The typed body for a partially applied operation.
pub const PARTIAL_BODY: &str = "Tiling operation partially applied.";
/// The typed body for an operation whose outcome is unknown.
pub const UNCERTAIN_BODY: &str =
    "Tiling operation outcome is uncertain; it may already have taken effect.";
/// The body for a request that cannot be serialized.
pub const SERIALIZE_BODY: &str = "Cannot serialize tiling request";
/// The body for response data that does not decode into the typed DTOs. It
/// never echoes the payload.
pub const DECODE_BODY: &str = "Invalid response: the Tiling response could not be decoded";

const DIRECTIONS: [&str; 6] = ["up", "down", "left", "right", "previous", "next"];
/// Layout values with the pre-snake_case kebab spellings kept as CLI-only aliases.
fn layout_values() -> PossibleValuesParser {
    PossibleValuesParser::new([
        PossibleValue::new("dwindle"),
        PossibleValue::new("split"),
        PossibleValue::new("split_vertical").alias("split-vertical"),
        PossibleValue::new("split_horizontal").alias("split-horizontal"),
        PossibleValue::new("monocle"),
        PossibleValue::new("master"),
        PossibleValue::new("grid"),
        PossibleValue::new("floating"),
    ])
}

// -------------------------------------------------------------- transport

/// Talks to the control socket. Tests substitute fixed failures.
pub trait TilingTransport: Send + Sync {
    /// Reads the host's core status, used to validate its config identity.
    ///
    /// # Errors
    ///
    /// The client error, unchanged.
    fn status(&self, socket: &Path) -> Result<CoreStatus, ClientError>;

    /// Sends one request.
    ///
    /// # Errors
    ///
    /// The client error, unchanged.
    fn send(&self, socket: &Path, request: &Request) -> Result<Response, ClientError>;
}

/// The production transport over [`IpcClient`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpcTransport {
    retries: RetryPolicy,
    timeout: Duration,
}

impl IpcTransport {
    /// The shipped route: the legacy absent-only retry and the shared 5 s
    /// client budget.
    #[must_use]
    pub const fn production() -> Self {
        Self {
            retries: RetryPolicy::legacy(),
            timeout: IO_TIMEOUT,
        }
    }

    /// Explicit limits for tests that must not sleep.
    #[must_use]
    pub const fn with_options(retries: RetryPolicy, timeout: Duration) -> Self {
        Self { retries, timeout }
    }

    fn client(&self, socket: &Path) -> IpcClient {
        IpcClient::with_options(socket, self.retries, self.timeout)
    }
}

impl TilingTransport for IpcTransport {
    fn status(&self, socket: &Path) -> Result<CoreStatus, ClientError> {
        self.client(socket).status()
    }

    fn send(&self, socket: &Path, request: &Request) -> Result<Response, ClientError> {
        self.client(socket).request(request)
    }
}

/// Everything the executor needs from its composition root.
pub struct TilingCliEnv {
    /// Configuration directory discovery.
    pub config: ConfigEnvironment,
    /// Identity cache and runtime roots.
    pub app: AppEnvironment,
    /// The invocation working directory.
    pub current_dir: Arc<dyn Fn() -> io::Result<PathBuf> + Send + Sync>,
    /// The control-socket transport.
    pub transport: Arc<dyn TilingTransport>,
    /// JSON terminal styling.
    pub highlight: JsonHighlighter,
}

impl TilingCliEnv {
    /// Production defaults around an injected styling policy.
    #[must_use]
    pub fn new(config: ConfigEnvironment, app: AppEnvironment, highlight: JsonHighlighter) -> Self {
        Self {
            config,
            app,
            current_dir: Arc::new(std::env::current_dir),
            transport: Arc::new(IpcTransport::production()),
            highlight,
        }
    }
}

/// Builds the `tiling` contribution.
#[must_use]
pub fn contribution(env: TilingCliEnv) -> CliContribution {
    CliContribution::new(command, TilingCli { env })
}

// ---------------------------------------------------------------- grammar

fn flag(name: &'static str, help: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue).help(help)
}

fn text(name: &'static str, value_name: &'static str, help: &'static str) -> Arg {
    Arg::new(name).long(name).value_name(value_name).help(help)
}

fn query_command() -> Command {
    Command::new("query")
        .about("Query tiling state (screens, workspaces, windows, apps, status)")
        .next_display_order(None)
        .arg(
            Arg::new("json")
                .long("json")
                .short('j')
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Output in JSON format instead of table format"),
        )
        .arg(
            Arg::new("detailed")
                .long("detailed")
                .short('d')
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Show detailed information (more columns/fields)"),
        )
        .subcommand(
            Command::new("screens").about("List all connected screens").after_long_help(
                "Examples:\n  stache tiling query screens         # List all screens\n  stache \
                 tiling query --json screens  # Output as JSON",
            ),
        )
        .subcommand(
            Command::new("workspaces")
                .about("List workspaces")
                .after_long_help(
                    "Examples:\n  stache tiling query workspaces                    # List all \
                     workspaces\n  stache tiling query workspaces --focused-screen   # \
                     Workspaces on focused screen\n  stache tiling query workspaces --screen \
                     main      # Workspaces on main screen",
                )
                .arg(
                    flag("focused-screen", "Only show workspaces on the focused screen")
                        .conflicts_with("screen"),
                )
                .arg(text(
                    "screen",
                    "SCREEN",
                    "Filter by screen name (main, secondary, or screen name)",
                )),
        )
        .subcommand(
            Command::new("windows")
                .about("List managed windows")
                .after_long_help(
                    "Examples:\n  stache tiling query windows                       # List all \
                     windows\n  stache tiling query windows --focused-workspace   # Windows in \
                     focused workspace\n  stache tiling query windows --workspace coding    # \
                     Windows in 'coding' workspace\n  stache tiling query -d windows              \
                     \x20     # Show detailed window info",
                )
                .arg(
                    flag("focused-screen", "Only show windows on the focused screen")
                        .conflicts_with_all(["screen", "workspace", "focused-workspace"]),
                )
                .arg(
                    flag("focused-workspace", "Only show windows in the focused workspace")
                        .conflicts_with_all(["screen", "workspace", "focused-screen"]),
                )
                .arg(
                    text("screen", "SCREEN", "Filter by screen name")
                        .conflicts_with_all(["focused-screen", "focused-workspace"]),
                )
                .arg(
                    text("workspace", "WORKSPACE", "Filter by workspace name")
                        .conflicts_with_all(["focused-screen", "focused-workspace"]),
                ),
        )
        .subcommand(
            Command::new("status")
                .about("Show why windows are or are not being laid out")
                .after_long_help(
                    "Shows the runtime phase, global faults, applications that failed to \
                     track, displays without frames, hidden applications and the latest \
                     intent. Read-only.\n\nExamples:\n  stache tiling query status          # \
                     Summary\n  stache tiling query --json status  # Output as JSON",
                ),
        )
        .subcommand(
            Command::new("apps").about("List all running applications").after_long_help(
                "Examples:\n  stache tiling query apps            # List all running apps\n  \
                 stache tiling query --json apps     # Output as JSON\n  stache tiling query -d \
                 apps         # Show detailed app info",
            ),
        )
}

fn window_command() -> Command {
    Command::new("window")
        .about("Window manipulation commands")
        .long_about(
            "Window manipulation commands.\n\nUse flags to specify the window operation to \
             perform. Operations are executed in a fixed order whatever the flag order: focus, \
             swap, toggle floating, preset, resize, send to screen, send to workspace.",
        )
        .next_display_order(None)
        .after_long_help(
            "Examples:\n  stache tiling window --focus left                            # Focus \
             window to the left\n  stache tiling window --swap down                             \
             # Swap with window below\n  stache tiling window --resize width 100                \
                   # Increase width by 100px\n  stache tiling window --resize width 100 --resize \
             height 50   # Resize both dimensions\n  stache tiling window --swap right --resize \
             width 150         # Swap then resize\n  stache tiling window --send-to-screen main   \
             \x20                # Send to main screen",
        )
        .arg(text(
            "focus",
            "DIRECTION|WINDOW_ID",
            "Focus a window in a direction (up, down, left, right, previous, next) or by ID",
        ))
        .arg(
            text(
                "swap",
                "DIRECTION",
                "Swap focused window with another in a direction (up, down, left, right, \
                 previous, next)",
            )
            .value_parser(DIRECTIONS),
        )
        .arg(flag(
            "toggle-floating",
            "Toggle the focused window between tiled and floating",
        ))
        .arg(text(
            "preset",
            "PRESET_NAME",
            "Apply a floating preset from the configuration to the focused window",
        ))
        .arg(
            Arg::new("resize")
                .long("resize")
                .value_names(["DIMENSION", "AMOUNT"])
                .num_args(2)
                .action(ArgAction::Append)
                .allow_negative_numbers(true)
                .help(
                    "Resize the focused window: dimension (width/height) and amount in pixels; \
                     negative values decrease. Repeat to resize both dimensions",
                ),
        )
        .arg(text(
            "send-to-screen",
            "SCREEN",
            "Send focused window to another screen (main, secondary, or screen name)",
        ))
        .arg(text(
            "send-to-workspace",
            "WORKSPACE",
            "Send focused window to another workspace",
        ))
}

fn workspace_command() -> Command {
    Command::new("workspace")
        .about("Workspace commands")
        .long_about(
            "Workspace commands.\n\nUse flags to specify the workspace operation to perform. \
             Operations are executed in a fixed order: focus, layout, balance, send to screen.",
        )
        .next_display_order(None)
        .after_long_help(
            "Examples:\n  stache tiling workspace --balance                    # Balance windows \
             in focused workspace\n  stache tiling workspace --focus coding               # \
             Switch to 'coding' workspace\n  stache tiling workspace --layout dwindle             \
             \x20    # Use DWINDLE layout\n  stache tiling workspace --layout dwindle --balance   \
             \x20   # Change layout then balance\n  stache tiling workspace --send-to-screen main   \
             \x20     # Move workspace to main screen",
        )
        .arg(text("focus", "WORKSPACE", "Focus a workspace by name"))
        .arg(
            text(
                "layout",
                "LAYOUT",
                "Change the layout of the focused workspace (dwindle, split, split_vertical, \
                 split_horizontal, monocle, master, grid, floating)",
            )
            .value_parser(layout_values()),
        )
        .arg(flag("balance", "Balance windows in the focused workspace"))
        .arg(text(
            "send-to-screen",
            "SCREEN",
            "Send focused workspace to another screen (main, secondary, or screen name)",
        ))
}

/// Builds the `tiling` command tree.
#[must_use]
pub fn command() -> Command {
    Command::new("tiling")
        .about("Tiling window manager commands")
        .next_display_order(None)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(Command::new("pause").about("Pause tiling and wait for native effects to stop"))
        .subcommand(Command::new("resume").about("Resume a paused tiling runtime"))
        .subcommand(query_command())
        .subcommand(window_command())
        .subcommand(workspace_command())
}

/// The help of `stache tiling query`, as printed without a subcommand.
fn query_help() -> String {
    let mut root = Command::new("stache").subcommand(command());
    match root.try_get_matches_from_mut(["stache", "tiling", "query", "--help"]) {
        Err(error) => error.to_string(),
        Ok(_) => String::new(),
    }
}

// ---------------------------------------------------------------- failures

/// Escapes control characters other than newline and tab, so no text from a
/// lower layer can write a raw escape sequence to the terminal.
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

/// JSON string quoting, so control characters become `\u00XX`.
fn quoted(text: &str) -> String { Value::String(text.to_owned()).to_string() }

/// Bodies that the legacy CLI raised as argument errors (`stache: {body}`)
/// rather than IPC errors.
fn is_argument_body(body: &str) -> bool {
    body.starts_with("Invalid resize dimension '")
        || body.starts_with("Invalid resize amount '")
        || body == body::NO_WINDOW_OPERATION
        || body == body::NO_WORKSPACE_OPERATION
}

/// Why an exchange did not produce data.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// The host is absent (the legacy offline body).
    Offline,
    /// An IPC-class body (`IPC error: ` in action output).
    Ipc(String),
    /// A CLI-side body (`stache: ` only).
    Cli(String),
}

impl Failure {
    /// The typed SDK client error through this module's compatibility adapter.
    fn client(error: ClientError) -> Self {
        match error {
            ClientError::NotRunning => Self::Offline,
            ClientError::Timeout => Self::Ipc("Connection timed out".to_owned()),
            ClientError::Io(source) => Self::Ipc(format!("IO error: {source}")),
            ClientError::Serialize(_) => Self::Ipc(SERIALIZE_BODY.to_owned()),
            ClientError::InvalidResponse(detail) => {
                Self::Ipc(format!("Invalid response: {detail}"))
            }
            ClientError::Remote(error) => Self::remote(&error),
        }
    }

    fn remote(error: &WireError) -> Self {
        if is_argument_body(&error.message) {
            Self::Cli(error.message.clone())
        } else {
            Self::Ipc(error.message.clone())
        }
    }

    /// The body, for the query (stdout) forms.
    fn query_body(&self) -> &str {
        match self {
            Self::Offline => "Stache app is not running",
            Self::Ipc(body) | Self::Cli(body) => body,
        }
    }

    /// The `stache: ...` stderr line of an action failure.
    fn action_line(&self) -> String {
        match self {
            Self::Offline => "stache: IPC error: Stache app is not running".to_owned(),
            Self::Ipc(body) => format!("stache: IPC error: {}", visible_controls(body)),
            Self::Cli(body) => format!("stache: {}", visible_controls(body)),
        }
    }
}

// --------------------------------------------------------------- execution

/// The executor behind [`contribution`].
struct TilingCli {
    env: TilingCliEnv,
}

impl CliExecutor for TilingCli {
    fn execute(
        &self,
        matches: &ArgMatches,
        ansi: bool,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> ExitCode {
        match matches.subcommand() {
            Some(("pause", _)) => self.lifecycle(matches, method::PAUSE, err),
            Some(("resume", _)) => self.lifecycle(matches, method::RESUME, err),
            Some(("query", sub)) => self.query(matches, sub, ansi, out),
            Some(("window", sub)) => self.window(matches, sub, err),
            Some(("workspace", sub)) => self.workspace(matches, sub, err),
            _ => cli_failure(err, "tiling requires a subcommand"),
        }
    }
}

fn cli_failure(err: &mut dyn Write, message: &str) -> ExitCode {
    let _ = writeln!(err, "stache: {}", visible_controls(message));
    ExitCode::FAILURE
}

fn action_failure(err: &mut dyn Write, failure: &Failure) -> ExitCode {
    let _ = writeln!(err, "{}", failure.action_line());
    ExitCode::FAILURE
}

fn physical_failure(err: &mut dyn Write, body: &str) -> ExitCode {
    let _ = writeln!(err, "stache: IPC error: {body}");
    ExitCode::FAILURE
}

/// The global `-c/--config` value propagated into a subcommand's matches.
fn config_override(matches: &ArgMatches) -> Option<PathBuf> {
    matches.try_get_one::<PathBuf>("config").ok().flatten().cloned()
}

/// The directory as the host would compare it: canonical when it exists.
fn normalized(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

impl TilingCli {
    /// Resolves the socket and validates the host's config identity, then
    /// sends one request and returns its `Ok` data.
    fn exchange(&self, matches: &ArgMatches, request: &Request) -> Result<Value, Failure> {
        let paths = ConfigPaths::resolve(config_override(matches), &self.env.config)
            .map_err(|error| Failure::Cli(error.to_string()))?;
        let app = AppPaths::resolve(&self.env.app).map_err(|e| Failure::Cli(e.to_string()))?;
        let socket = app.socket_path().map_err(|e| Failure::Cli(e.to_string()))?;
        let requested = if paths.directory().is_absolute() {
            paths.directory().to_path_buf()
        } else {
            let cwd = (self.env.current_dir)().map_err(|error| {
                Failure::Cli(format!("cannot determine the current directory: {error}"))
            })?;
            cwd.join(paths.directory())
        };

        let status = self.env.transport.status(&socket).map_err(Failure::client)?;
        let host = normalized(&status.directory);
        let requested = normalized(&requested);
        if host != requested {
            return Err(Failure::Ipc(format!(
                "Running host uses config directory {}; requested {}. Use the same config \
                 directory or stop that host.",
                quoted(&host.to_string_lossy()),
                quoted(&requested.to_string_lossy())
            )));
        }

        match self.env.transport.send(&socket, request).map_err(Failure::client)? {
            Response::Ok { data } => Ok(data),
            Response::Error { error } => Err(Failure::remote(&error)),
        }
    }

    fn request(method: &str, params: &impl Serialize) -> Result<Request, Failure> {
        Request::new(MODULE, method, params).map_err(|_| Failure::Ipc(SERIALIZE_BODY.to_owned()))
    }

    // ---------------------------------------------------------- mutations

    fn mutate(
        &self,
        matches: &ArgMatches,
        method: &str,
        params: &impl Serialize,
        err: &mut dyn Write,
    ) -> ExitCode {
        let data =
            Self::request(method, params).and_then(|request| self.exchange(matches, &request));
        match data {
            Ok(data) => finish_operation(data, method == self::method::PAUSE, err),
            Err(failure) => action_failure(err, &failure),
        }
    }

    fn lifecycle(&self, matches: &ArgMatches, name: &str, err: &mut dyn Write) -> ExitCode {
        self.mutate(matches, name, &serde_json::json!({}), err)
    }

    fn window(&self, matches: &ArgMatches, sub: &ArgMatches, err: &mut dyn Write) -> ExitCode {
        let params = window_params(sub);
        if params == WindowParams::default() {
            return cli_failure(err, body::NO_WINDOW_OPERATION);
        }
        self.mutate(matches, method::WINDOW, &params, err)
    }

    fn workspace(&self, matches: &ArgMatches, sub: &ArgMatches, err: &mut dyn Write) -> ExitCode {
        let params = workspace_params(sub);
        if params == WorkspaceParams::default() {
            return cli_failure(err, body::NO_WORKSPACE_OPERATION);
        }
        self.mutate(matches, method::WORKSPACE, &params, err)
    }

    // ------------------------------------------------------------ queries

    fn query(
        &self,
        matches: &ArgMatches,
        sub: &ArgMatches,
        ansi: bool,
        out: &mut dyn Write,
    ) -> ExitCode {
        let json = sub.get_flag("json");
        let detailed = sub.get_flag("detailed");
        let Some((kind, filters)) = sub.subcommand() else {
            let _ = writeln!(out, "{}", query_help());
            return ExitCode::SUCCESS;
        };
        let params = query_params(kind, filters);
        let outcome = Self::request(method::QUERY, &params)
            .and_then(|request| self.exchange(matches, &request))
            .and_then(|data| self.render(params.kind, data, json, detailed, ansi));
        match outcome {
            Ok(text) => {
                let _ = out.write_all(text.as_bytes());
                ExitCode::SUCCESS
            }
            Err(failure) => {
                let text = if json {
                    render::json_error(failure.query_body())
                } else {
                    render::table_error(failure.query_body(), failure == Failure::Offline, ansi)
                };
                let _ = out.write_all(text.as_bytes());
                ExitCode::FAILURE
            }
        }
    }

    fn render(
        &self,
        kind: QueryKind,
        data: Value,
        json: bool,
        detailed: bool,
        ansi: bool,
    ) -> Result<String, Failure> {
        let highlight = self.env.highlight.as_ref();
        match kind {
            QueryKind::Screens => {
                let rows: Vec<ScreenDto> = decode(data)?;
                present(&rows, json, ansi, highlight, |rows| render::screens(rows, ansi))
            }
            QueryKind::Workspaces => {
                let rows: Vec<WorkspaceDto> = decode(data)?;
                present(&rows, json, ansi, highlight, |rows| {
                    render::workspaces(rows, ansi)
                })
            }
            QueryKind::Windows => {
                let rows: Vec<WindowDto> = decode(data)?;
                present(&rows, json, ansi, highlight, |rows| {
                    render::windows(rows, detailed, ansi)
                })
            }
            QueryKind::Apps => {
                let rows: Vec<AppDto> = decode(data)?;
                present(&rows, json, ansi, highlight, |rows| render::apps(rows, ansi))
            }
            QueryKind::Status => {
                let status: StatusDto = decode(data)?;
                if json {
                    render::json(&status, ansi, highlight)
                        .map_err(|_| Failure::Ipc(DECODE_BODY.to_owned()))
                } else {
                    Ok(render::status(&status, ansi))
                }
            }
        }
    }
}

/// Decodes response data once into typed rows; never echoes the payload.
fn decode<T: DeserializeOwned>(data: Value) -> Result<T, Failure> {
    serde_json::from_value(data).map_err(|_| Failure::Ipc(DECODE_BODY.to_owned()))
}

fn present<T: Serialize>(
    rows: &[T],
    json: bool,
    ansi: bool,
    highlight: &dyn Fn(&str, bool) -> String,
    table: impl FnOnce(&[T]) -> String,
) -> Result<String, Failure> {
    if json {
        render::json(rows, ansi, highlight).map_err(|_| Failure::Ipc(DECODE_BODY.to_owned()))
    } else {
        Ok(table(rows))
    }
}

/// Maps a decoded mutation response to streams and an exit code.
///
/// `enqueued` and `applied` are silent exit 0 (admission, not native success).
/// A pause that is not `applied` never reports success.
fn finish_operation(data: Value, pause: bool, err: &mut dyn Write) -> ExitCode {
    match decode::<OperationResponse>(data) {
        Ok(OperationResponse::Applied(_)) => ExitCode::SUCCESS,
        Ok(OperationResponse::Enqueued(_)) if !pause => ExitCode::SUCCESS,
        Ok(OperationResponse::Enqueued(_) | OperationResponse::Uncertain(_)) => {
            physical_failure(err, UNCERTAIN_BODY)
        }
        Ok(OperationResponse::Partial(_)) => physical_failure(err, PARTIAL_BODY),
        Err(failure) => action_failure(err, &failure),
    }
}

fn query_params(kind: &str, filters: &ArgMatches) -> QueryParams {
    let get = |name: &str| filters.try_get_one::<String>(name).ok().flatten().cloned();
    let on =
        |name: &str| filters.try_get_one::<bool>(name).ok().flatten().copied().unwrap_or(false);
    let kind = match kind {
        "screens" => QueryKind::Screens,
        "workspaces" => QueryKind::Workspaces,
        "windows" => QueryKind::Windows,
        "status" => QueryKind::Status,
        _ => QueryKind::Apps,
    };
    QueryParams {
        kind,
        screen: get("screen"),
        workspace: get("workspace"),
        focused_screen: on("focused-screen"),
        focused_workspace: on("focused-workspace"),
    }
}

fn window_params(sub: &ArgMatches) -> WindowParams {
    let resize: Vec<String> = sub
        .get_many::<String>("resize")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    WindowParams {
        focus: sub.get_one::<String>("focus").cloned(),
        swap: sub.get_one::<String>("swap").cloned(),
        toggle_floating: sub.get_flag("toggle-floating"),
        preset: sub.get_one::<String>("preset").cloned(),
        resize: resize
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[dimension, amount]| ResizeArg {
                dimension: dimension.clone(),
                amount: amount.clone(),
            })
            .collect(),
        send_to_screen: sub.get_one::<String>("send-to-screen").cloned(),
        send_to_workspace: sub.get_one::<String>("send-to-workspace").cloned(),
    }
}

fn workspace_params(sub: &ArgMatches) -> WorkspaceParams {
    WorkspaceParams {
        focus: sub.get_one::<String>("focus").cloned(),
        layout: sub.get_one::<String>("layout").and_then(|name| {
            // Clap passes the matched spelling through, so fold the kebab aliases here.
            serde_json::from_value::<LayoutType>(Value::String(name.replace('-', "_"))).ok()
        }),
        balance: sub.get_flag("balance"),
        send_to_screen: sub.get_one::<String>("send-to-screen").cloned(),
    }
}
