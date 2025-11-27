use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tauri_plugin_cli::{ArgData, CliExt, Matches, SubcommandMatches};

const CLI_EVENT_CHANNEL: &str = "tauri_cli_event";
const SYNTHETIC_BIN_NAME: &str = "barba";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CliEventPayload {
    pub name: String,
    pub data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpMessage(pub String);

impl std::fmt::Display for HelpMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CliParseError {
    /// User requested help text (global or subcommand).
    Help(HelpMessage),
    /// Help should be displayed because no args were provided in release builds.
    MissingArguments,
    /// `workspace-changed` missing required workspace name.
    MissingWorkspaceName,
    /// The CLI invocation did not match a known subcommand.
    UnknownCommand,
    /// Internal clap error surfaced through plugin config.
    InvalidInvocation(String),
}

impl std::fmt::Display for CliParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Help(message) => f.write_str(&message.0),
            Self::MissingArguments => write!(
                f,
                "No CLI arguments provided. Run `barba --help` to discover available commands."
            ),
            Self::MissingWorkspaceName => {
                write!(
                    f,
                    "Missing workspace name. Usage: `barba workspace-changed <name>`"
                )
            }
            Self::UnknownCommand => write!(
                f,
                "Unknown command. Run `barba --help` to list all supported commands."
            ),
            Self::InvalidInvocation(message) => {
                write!(f, "CLI invocation could not be parsed: {message}")
            }
        }
    }
}

impl std::error::Error for CliParseError {}

pub fn handle_cli_invocation(app_handle: &AppHandle, args: &[String]) {
    if consume_version_flag(args) {
        return;
    }

    match parse_cli_event(app_handle, args) {
        // Valid CLI event detected
        Ok(Some(event)) => {
            app_handle.emit(CLI_EVENT_CHANNEL, event).unwrap_or_else(|err| {
                eprintln!("barba: failed to emit CLI event: {err}");
            });
        }

        // No CLI event detected
        Ok(None) => {
            #[cfg(not(debug_assertions))]
            {
                if !print_default_help(app_handle) {
                    eprintln!("barba: {}", CliParseError::MissingArguments);
                }
            }
        }
        // Version flag detected (handled internally by clap)

        // CLI parsing error
        Err(CliParseError::Help(help)) => {
            println!("{help}");
        }

        // No arguments provided when required
        Err(CliParseError::MissingArguments) => {
            if !print_default_help(app_handle) {
                eprintln!("barba: {}", CliParseError::MissingArguments);
            }
        }

        // Other CLI parsing errors
        Err(err) => {
            eprintln!("barba: {err}");
        }
    }
}

pub fn parse_cli_event(
    app_handle: &AppHandle,
    args: &[String],
) -> Result<Option<CliEventPayload>, CliParseError> {
    let Some(normalized_args) = normalize_cli_args(args) else {
        return Ok(None);
    };

    let matches = app_handle
        .cli()
        .matches_from(normalized_args)
        .map_err(|err| CliParseError::InvalidInvocation(err.to_string()))?;

    build_cli_event(&matches)
}

pub fn preview_cli_event(args: &[String]) -> Result<Option<CliEventPayload>, CliParseError> {
    let Some(normalized_args) = normalize_cli_args(args) else {
        return Ok(None);
    };

    preview_cli_event_with(&normalized_args, should_render_help_on_empty_invocation())
}

fn consume_version_flag(args: &[String]) -> bool {
    let Some(normalized_args) = normalize_cli_args(args) else {
        return false;
    };

    if is_version_request(&normalized_args) {
        println!("{SYNTHETIC_BIN_NAME} {APP_VERSION}");
        return true;
    }

    false
}

fn is_version_request(normalized_args: &[String]) -> bool {
    normalized_args.len() == 2 && matches!(normalized_args[1].as_str(), "--version" | "-V")
}

fn preview_cli_event_with(
    normalized_args: &[String],
    require_command: bool,
) -> Result<Option<CliEventPayload>, CliParseError> {
    if normalized_args.len() <= 1 {
        return if require_command {
            Err(CliParseError::MissingArguments)
        } else {
            Ok(None)
        };
    }

    let event_candidate = normalized_args[1].trim().to_string();
    if event_candidate.is_empty() {
        return Err(CliParseError::UnknownCommand);
    }

    let data = if normalized_args.len() > 2 {
        Some(normalized_args[2..].join(" "))
    } else {
        None
    };

    Ok(Some(CliEventPayload { name: event_candidate, data }))
}

fn build_cli_event(matches: &Matches) -> Result<Option<CliEventPayload>, CliParseError> {
    build_cli_event_with(matches, should_render_help_on_empty_invocation())
}

fn build_cli_event_with(
    matches: &Matches,
    require_command: bool,
) -> Result<Option<CliEventPayload>, CliParseError> {
    if let Some(help) = matches.args.get("help")
        && let Value::String(help_text) = &help.value
    {
        return Err(CliParseError::Help(HelpMessage(help_text.clone())));
    }

    if require_command && matches.subcommand.is_none() {
        return Err(CliParseError::MissingArguments);
    }

    match extract_subcommand(matches.subcommand.as_deref())? {
        CommandMatch::None => Ok(None),
        CommandMatch::FocusChanged => Ok(Some(CliEventPayload {
            name: "focus-changed".to_string(),
            data: None,
        })),
        CommandMatch::WorkspaceChanged(workspace) => Ok(Some(CliEventPayload {
            name: "workspace-changed".to_string(),
            data: Some(workspace),
        })),
    }
}

#[derive(Debug)]
enum CommandMatch {
    None,
    FocusChanged,
    WorkspaceChanged(String),
}

fn extract_subcommand(
    subcommand: Option<&SubcommandMatches>,
) -> Result<CommandMatch, CliParseError> {
    let Some(matches) = subcommand else {
        return Ok(CommandMatch::None);
    };

    parse_command(matches.name.as_str(), &matches.matches.args)
}

fn parse_command(
    name: &str,
    args: &HashMap<String, ArgData>,
) -> Result<CommandMatch, CliParseError> {
    match name {
        "focus-changed" => Ok(CommandMatch::FocusChanged),
        "workspace-changed" => {
            let workspace = args
                .get("name")
                .and_then(|arg| match &arg.value {
                    Value::String(value) if !value.is_empty() => Some(value.clone()),
                    _ => None,
                })
                .ok_or(CliParseError::MissingWorkspaceName)?;
            Ok(CommandMatch::WorkspaceChanged(workspace))
        }
        _ => Err(CliParseError::UnknownCommand),
    }
}

fn normalize_cli_args(args: &[String]) -> Option<Vec<String>> {
    if args.is_empty() {
        return None;
    }

    if looks_like_binary(&args[0]) {
        if args.len() == 1 {
            return None;
        }

        return Some(args.to_vec());
    }

    let mut normalized = Vec::with_capacity(args.len() + 1);
    normalized.push(SYNTHETIC_BIN_NAME.to_string());
    normalized.extend_from_slice(args);
    Some(normalized)
}

fn looks_like_binary(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }

    let normalized = path.trim_matches('"');
    let lowered = normalized.to_ascii_lowercase();
    let mentions_binary = lowered.contains(SYNTHETIC_BIN_NAME);
    let has_separator = normalized.contains('/') || normalized.contains('\\');

    mentions_binary && (normalized == SYNTHETIC_BIN_NAME || has_separator)
}

pub const fn should_render_help_on_empty_invocation() -> bool { cfg!(not(debug_assertions)) }

fn print_default_help(app_handle: &AppHandle) -> bool {
    resolve_help_text(app_handle, None).is_some_and(|help| {
        println!("{help}");
        true
    })
}

fn resolve_help_text(app_handle: &AppHandle, subcommand: Option<&str>) -> Option<String> {
    let mut args = vec![SYNTHETIC_BIN_NAME.to_string()];
    if let Some(name) = subcommand {
        args.push(name.to_string());
    }
    args.push("--help".to_string());

    let matches = app_handle.cli().matches_from(args).ok()?;
    let help = matches.args.get("help")?;

    if let Value::String(help_text) = &help.value {
        Some(help_text.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tauri_plugin_cli::ArgData;

    use super::*;

    fn arg_map(pairs: &[(&str, Value)]) -> HashMap<String, ArgData> {
        let mut map = HashMap::new();
        for (key, value) in pairs {
            let mut arg = ArgData::default();
            arg.value = value.clone();
            arg.occurrences = 1;
            map.insert((*key).to_string(), arg);
        }
        map
    }

    fn matches_with_help(text: &str) -> Matches {
        let mut matches = Matches::default();
        let mut data = ArgData::default();
        data.value = Value::String(text.to_string());
        matches.args.insert("help".to_string(), data);
        matches
    }

    #[test]
    fn parse_command_focus_changed() {
        let args = HashMap::new();
        let command = parse_command("focus-changed", &args).unwrap();

        assert!(matches!(command, CommandMatch::FocusChanged));
    }

    #[test]
    fn parse_command_workspace_extracts_name() {
        let args = arg_map(&[("name", Value::String("coding".to_string()))]);
        let command = parse_command("workspace-changed", &args).unwrap();

        match command {
            CommandMatch::WorkspaceChanged(name) => assert_eq!(name, "coding"),
            _ => panic!("expected workspace command"),
        }
    }

    #[test]
    fn parse_command_requires_workspace_name() {
        let err = parse_command("workspace-changed", &HashMap::new()).unwrap_err();

        assert_eq!(err, CliParseError::MissingWorkspaceName);
    }

    #[test]
    fn build_cli_event_returns_help_error() {
        let matches = matches_with_help("Usage");
        let err = build_cli_event_with(&matches, false).unwrap_err();

        assert!(matches!(err, CliParseError::Help(_)));
    }

    #[test]
    fn build_cli_event_requires_command_when_enforced() {
        let matches = Matches::default();
        let err = build_cli_event_with(&matches, true).unwrap_err();

        assert_eq!(err, CliParseError::MissingArguments);
    }

    #[test]
    fn preview_cli_event_detects_command() {
        let args = vec!["barba".to_string(), "focus-changed".to_string()];
        let preview = preview_cli_event_with(&args, false).unwrap().unwrap();

        assert_eq!(preview.name, "focus-changed");
    }

    #[test]
    fn preview_cli_event_requires_command_when_enforced() {
        let args = vec!["barba".to_string()];
        let err = preview_cli_event_with(&args, true).unwrap_err();

        assert_eq!(err, CliParseError::MissingArguments);
    }

    #[test]
    fn normalizes_binary_prefix() {
        assert!(normalize_cli_args(&[]).is_none());
        assert!(normalize_cli_args(&["barba".to_string()]).is_none());

        let args = vec!["focus-changed".to_string()];
        let normalized = normalize_cli_args(&args).unwrap();

        assert_eq!(normalized[0], SYNTHETIC_BIN_NAME);
        assert_eq!(&normalized[1..], args.as_slice());
    }

    #[test]
    fn detects_version_request_flags() {
        let args = vec!["barba".to_string(), "--version".to_string()];
        assert!(is_version_request(&args));

        let short = vec!["barba".to_string(), "-V".to_string()];
        assert!(is_version_request(&short));
    }

    #[test]
    fn rejects_version_request_with_extra_args() {
        let args = vec![
            "barba".to_string(),
            "--version".to_string(),
            "extra".to_string(),
        ];
        assert!(!is_version_request(&args));
    }
}
