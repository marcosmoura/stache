//! Hotkey Daemon for Stache.
//!
//! This module provides a background daemon that listens for global keyboard shortcuts
//! and executes configured commands when those shortcuts are activated.
//!
//! The daemon reads its configuration from the global Stache configuration file
//! and uses Tauri's global-shortcut plugin to register system-wide hotkeys.

mod caps_lock;

use std::collections::HashMap;
use std::process::Command;

use tauri::{AppHandle, Runtime};
use tauri_plugin_global_shortcut::{Builder, GlobalShortcutExt, Shortcut, ShortcutState};

use crate::config::{ShortcutCommands, get_config};
use crate::platform::command::resolve_binary;

/// Creates the global-shortcut plugin.
///
/// Configured shortcuts are registered separately during application setup via
/// [`register_configured_hotkeys`] so individual registration failures do not
/// abort startup.
///
/// # Returns
///
/// Returns a configured `TauriPlugin` that can be added to the Tauri app builder.
#[must_use]
pub fn create_hotkey_plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    Builder::<R>::new().build()
}

/// Registers configured global shortcuts after the plugin has initialized.
///
/// Registration is performed one shortcut at a time so a single unavailable macOS
/// hotkey does not abort application startup.
pub fn register_configured_hotkeys<R: Runtime>(app: &AppHandle<R>) {
    let config = get_config();
    let keybindings = &config.keybindings;

    if keybindings.is_empty() {
        return;
    }

    let mut planned_shortcuts: HashMap<Shortcut, (String, String, ShortcutCommands)> =
        HashMap::new();

    for (shortcut_key, commands) in keybindings {
        let shortcut_str = normalize_shortcut(shortcut_key);

        match shortcut_str.parse::<Shortcut>() {
            Ok(shortcut) => {
                if let Some((previous_raw, _, _)) = planned_shortcuts.insert(
                    shortcut,
                    (shortcut_key.clone(), shortcut_str.clone(), commands.clone()),
                ) {
                    tracing::warn!(
                        shortcut = %shortcut_key,
                        normalized = %shortcut_str,
                        previous = %previous_raw,
                        "duplicate shortcut after normalization; only one binding will be used"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(shortcut = %shortcut_key, error = %err, "invalid shortcut");
            }
        }
    }

    if planned_shortcuts.is_empty() {
        return;
    }

    tracing::info!(count = planned_shortcuts.len(), "registering global shortcuts");

    let global_shortcut = app.global_shortcut();
    let mut registered = 0usize;
    let mut failed = 0usize;

    for (shortcut, (raw_shortcut, normalized_shortcut, commands)) in planned_shortcuts {
        let description = commands.commands_display();

        match global_shortcut.on_shortcut(shortcut, move |_app, _shortcut, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }

            execute_shortcut_commands(&commands);
        }) {
            Ok(()) => {
                registered += 1;
                tracing::debug!(
                    shortcut = %raw_shortcut,
                    normalized = %normalized_shortcut,
                    command = %description,
                    "registered shortcut"
                );
            }
            Err(err) => {
                failed += 1;
                tracing::warn!(
                    shortcut = %raw_shortcut,
                    normalized = %normalized_shortcut,
                    error = %err,
                    "failed to register shortcut"
                );
            }
        }
    }

    tracing::info!(registered, failed, "finished registering global shortcuts");
}

/// Normalizes a shortcut string to a consistent format for macOS.
///
/// This function handles common variations in shortcut notation using a single-pass
/// approach for efficiency:
/// - "Ctrl" is normalized to "Control"
/// - "Cmd" is normalized to "Command" (macOS Command key)
/// - "Alt" and "Opt" are normalized to "Option" (macOS Option key)
/// - "Super" and "Meta" are normalized to "Command"
/// - backtick (`` ` ``) is normalized to "Backquote"
fn normalize_shortcut(shortcut: &str) -> String {
    let mut result = String::with_capacity(shortcut.len() + 8);

    for part in shortcut.split('+') {
        if !result.is_empty() {
            result.push('+');
        }

        let normalized = match part {
            "Ctrl" => "Control",
            "Cmd" | "Super" | "Meta" => "Command",
            "Alt" | "Opt" => "Option",
            "`" => "Backquote",
            other => other,
        };

        result.push_str(normalized);
    }

    result
}

/// Executes all commands associated with a shortcut sequentially.
///
/// This function handles both Stache CLI commands (starting with "stache")
/// and external shell commands. Commands are executed in order, one after
/// another, never in parallel.
///
/// If no commands are configured (empty string or empty array), the function
/// returns immediately without executing anything. This is useful for capturing
/// shortcuts to disable global OS shortcuts.
pub(crate) fn execute_shortcut_commands(shortcut_commands: &ShortcutCommands) {
    let commands = shortcut_commands.get_commands();

    // No commands configured - this shortcut is just for capturing/blocking
    if commands.is_empty() {
        return;
    }

    let description = shortcut_commands.commands_display();

    // Clone commands for the thread
    let commands_owned: Vec<String> = commands.iter().map(|s| (*s).to_string()).collect();
    let description_owned = description;

    // Execute all commands in a background thread to avoid blocking the UI
    std::thread::spawn(move || {
        for (index, command) in commands_owned.iter().enumerate() {
            if !execute_single_command(command, &description_owned, index + 1, commands_owned.len())
            {
                // Stop executing remaining commands if one fails
                break;
            }
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandParseError {
    UnterminatedQuote(char),
    TrailingEscape,
}

impl std::fmt::Display for CommandParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnterminatedQuote(quote) => write!(formatter, "unterminated {quote} quote"),
            Self::TrailingEscape => formatter.write_str("trailing escape character"),
        }
    }
}

fn split_command(command: &str) -> Result<Vec<String>, CommandParseError> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut has_token = false;

    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            has_token = true;
            continue;
        }

        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else if active_quote == '"' && character == '\\' {
                escaped = true;
            } else {
                current.push(character);
            }
            has_token = true;
            continue;
        }

        match character {
            '\'' | '"' => {
                quote = Some(character);
                has_token = true;
            }
            '\\' => {
                escaped = true;
                has_token = true;
            }
            character if character.is_whitespace() => {
                if has_token {
                    parts.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            _ => {
                current.push(character);
                has_token = true;
            }
        }
    }

    if escaped {
        return Err(CommandParseError::TrailingEscape);
    }

    if let Some(active_quote) = quote {
        return Err(CommandParseError::UnterminatedQuote(active_quote));
    }

    if has_token {
        parts.push(current);
    }

    Ok(parts)
}

/// Executes a single command and returns true if successful.
///
/// # Arguments
/// * `command` - The command string to execute
/// * `description` - Description for logging
/// * `index` - 1-based index of this command in the sequence
/// * `total` - Total number of commands in the sequence
fn execute_single_command(command: &str, description: &str, index: usize, total: usize) -> bool {
    let parts = match split_command(command) {
        Ok(parts) => parts,
        Err(err) => {
            tracing::warn!(command = %command, error = %err, "failed to parse shortcut command");
            return false;
        }
    };

    let Some((binary, args)) = parts.split_first() else {
        tracing::warn!("empty command for shortcut");
        return false;
    };

    // Resolve the binary path
    let binary_path = match resolve_binary(binary) {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(binary = %binary, error = %err, "failed to resolve binary");
            return false;
        }
    };

    match Command::new(&binary_path).args(args).spawn() {
        Ok(mut child) => {
            // Wait for the command to complete before proceeding to the next
            match child.wait() {
                Ok(status) => {
                    if !status.success() {
                        tracing::warn!(
                            command = %description,
                            step = index,
                            total,
                            status = %status,
                            "command exited with non-zero status"
                        );
                        return false;
                    }
                    tracing::trace!(command = %description, "command completed successfully");
                    true
                }
                Err(err) => {
                    tracing::error!(command = %description, error = %err, "failed to wait for command");
                    false
                }
            }
        }
        Err(err) => {
            tracing::error!(
                binary = %binary_path.display(),
                error = %err,
                "failed to execute command"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // normalize_shortcut tests
    // ========================================================================

    #[test]
    fn test_normalize_shortcut_ctrl() {
        assert_eq!(normalize_shortcut("Ctrl+Shift+S"), "Control+Shift+S");
        assert_eq!(normalize_shortcut("Control+Shift+S"), "Control+Shift+S");
    }

    #[test]
    fn test_normalize_shortcut_cmd() {
        assert_eq!(normalize_shortcut("Cmd+K"), "Command+K");
        assert_eq!(normalize_shortcut("Command+K"), "Command+K");
    }

    #[test]
    fn test_normalize_shortcut_option() {
        assert_eq!(normalize_shortcut("Alt+K"), "Option+K");
        assert_eq!(normalize_shortcut("Opt+K"), "Option+K");
        assert_eq!(normalize_shortcut("Option+K"), "Option+K");
    }

    #[test]
    fn test_normalize_shortcut_super_and_meta() {
        assert_eq!(normalize_shortcut("Super+K"), "Command+K");
        assert_eq!(normalize_shortcut("Meta+K"), "Command+K");
    }

    #[test]
    fn test_normalize_shortcut_backtick() {
        assert_eq!(normalize_shortcut("Cmd+`"), "Command+Backquote");
        assert_eq!(normalize_shortcut("Command+Shift+`"), "Command+Shift+Backquote");
    }

    #[test]
    fn test_normalize_shortcut_complex() {
        assert_eq!(normalize_shortcut("Ctrl+Alt+Shift+K"), "Control+Option+Shift+K");
        assert_eq!(
            normalize_shortcut("Cmd+Opt+Shift+`"),
            "Command+Option+Shift+Backquote"
        );
    }

    #[test]
    fn test_normalize_shortcut_passthrough() {
        // Keys that should pass through unchanged
        assert_eq!(normalize_shortcut("A"), "A");
        assert_eq!(normalize_shortcut("F12"), "F12");
        assert_eq!(normalize_shortcut("Space"), "Space");
    }

    // ========================================================================
    // Additional normalize_shortcut tests
    // ========================================================================

    #[test]
    fn test_normalize_shortcut_empty() {
        assert_eq!(normalize_shortcut(""), "");
    }

    #[test]
    fn test_normalize_shortcut_single_key() {
        assert_eq!(normalize_shortcut("A"), "A");
        assert_eq!(normalize_shortcut("Escape"), "Escape");
        assert_eq!(normalize_shortcut("Enter"), "Enter");
    }

    #[test]
    fn test_normalize_shortcut_function_keys() {
        assert_eq!(normalize_shortcut("Cmd+F1"), "Command+F1");
        assert_eq!(normalize_shortcut("Ctrl+F12"), "Control+F12");
        assert_eq!(normalize_shortcut("Alt+F5"), "Option+F5");
    }

    #[test]
    fn test_normalize_shortcut_all_modifiers() {
        assert_eq!(
            normalize_shortcut("Ctrl+Alt+Cmd+Shift+K"),
            "Control+Option+Command+Shift+K"
        );
    }

    #[test]
    fn test_normalize_shortcut_preserves_case() {
        // Key names should preserve their case
        assert_eq!(normalize_shortcut("Cmd+a"), "Command+a");
        assert_eq!(normalize_shortcut("Cmd+A"), "Command+A");
    }

    #[test]
    fn test_normalize_shortcut_arrow_keys() {
        assert_eq!(normalize_shortcut("Cmd+Left"), "Command+Left");
        assert_eq!(normalize_shortcut("Alt+Up"), "Option+Up");
        assert_eq!(normalize_shortcut("Ctrl+Down"), "Control+Down");
    }

    #[test]
    fn test_normalize_shortcut_special_keys() {
        assert_eq!(normalize_shortcut("Cmd+Tab"), "Command+Tab");
        assert_eq!(normalize_shortcut("Cmd+Space"), "Command+Space");
        assert_eq!(normalize_shortcut("Cmd+Escape"), "Command+Escape");
    }

    #[test]
    fn test_normalize_shortcut_numbers() {
        assert_eq!(normalize_shortcut("Cmd+1"), "Command+1");
        assert_eq!(normalize_shortcut("Ctrl+0"), "Control+0");
        assert_eq!(normalize_shortcut("Alt+9"), "Option+9");
    }

    #[test]
    fn test_normalize_shortcut_only_modifiers() {
        // Edge case: only modifiers, no key
        assert_eq!(normalize_shortcut("Ctrl+Cmd"), "Control+Command");
        assert_eq!(normalize_shortcut("Alt+Shift"), "Option+Shift");
    }

    // ========================================================================
    // execute_shortcut_commands tests (empty commands)
    // ========================================================================

    #[test]
    fn test_execute_shortcut_commands_empty_single() {
        let commands = ShortcutCommands::Single(String::new());
        // Should not panic with empty commands
        execute_shortcut_commands(&commands);
    }

    #[test]
    fn test_execute_shortcut_commands_empty_multiple() {
        let commands = ShortcutCommands::Multiple(vec![]);
        // Should not panic with empty array
        execute_shortcut_commands(&commands);
    }

    #[test]
    fn test_execute_shortcut_commands_whitespace_only() {
        let commands = ShortcutCommands::Single("   ".to_string());
        // Should not panic with whitespace-only commands
        execute_shortcut_commands(&commands);
    }

    // ========================================================================
    // execute_single_command tests (parsing)
    // ========================================================================

    #[test]
    fn test_execute_single_command_empty() {
        // Empty command should return false
        let result = execute_single_command("", "test", 1, 1);
        assert!(!result);
    }

    #[test]
    fn test_execute_single_command_whitespace() {
        // Whitespace-only command should return false
        let result = execute_single_command("   ", "test", 1, 1);
        assert!(!result);
    }

    #[test]
    fn test_execute_single_command_nonexistent_binary() {
        // Non-existent binary should return false
        let result = execute_single_command("nonexistent_binary_xyz123", "test", 1, 1);
        assert!(!result);
    }

    #[test]
    fn test_execute_single_command_echo() {
        // Echo should succeed
        let result = execute_single_command("echo hello", "test", 1, 1);
        assert!(result);
    }

    #[test]
    fn test_execute_single_command_true() {
        // /usr/bin/true should succeed
        let result = execute_single_command("true", "test", 1, 1);
        assert!(result);
    }

    #[test]
    fn test_execute_single_command_false() {
        // /usr/bin/false should fail (non-zero exit)
        let result = execute_single_command("false", "test", 1, 1);
        assert!(!result);
    }

    #[test]
    fn test_execute_single_command_with_args() {
        // Command with arguments
        let result = execute_single_command("echo test arg1 arg2", "test", 1, 1);
        assert!(result);
    }

    #[test]
    fn test_split_command_preserves_double_quoted_arg() {
        assert_eq!(
            split_command(r#"open -a "Activity Monitor""#).expect("command should parse"),
            vec![
                "open".to_string(),
                "-a".to_string(),
                "Activity Monitor".to_string()
            ]
        );
    }

    #[test]
    fn test_split_command_preserves_single_quoted_arg() {
        assert_eq!(
            split_command("echo 'hello world'").expect("command should parse"),
            vec!["echo".to_string(), "hello world".to_string()]
        );
    }

    #[test]
    fn test_split_command_preserves_empty_quoted_arg() {
        assert_eq!(
            split_command(r#"echo "" trailing"#).expect("command should parse"),
            vec!["echo".to_string(), String::new(), "trailing".to_string()]
        );
    }

    #[test]
    fn test_split_command_rejects_unterminated_quote() {
        assert_eq!(
            split_command(r#"open -a "Activity Monitor"#),
            Err(CommandParseError::UnterminatedQuote('"'))
        );
    }

    // ========================================================================
    // Integration-like tests
    // ========================================================================

    #[test]
    fn test_normalize_and_parse_shortcut() {
        // Test that normalized shortcuts can be parsed
        let shortcuts = [
            "Ctrl+S",
            "Cmd+K",
            "Alt+Tab",
            "Opt+Space",
            "Super+A",
            "Meta+B",
            "Command+Shift+`",
        ];

        for shortcut in shortcuts {
            let normalized = normalize_shortcut(shortcut);
            let parsed = normalized.parse::<Shortcut>();
            assert!(
                parsed.is_ok(),
                "Failed to parse normalized shortcut: {normalized}"
            );
        }
    }

    #[test]
    fn test_shortcut_commands_get_commands_single() {
        let commands = ShortcutCommands::Single("echo hello".to_string());
        let cmds = commands.get_commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0], "echo hello");
    }

    #[test]
    fn test_shortcut_commands_get_commands_multiple() {
        let commands =
            ShortcutCommands::Multiple(vec!["echo first".to_string(), "echo second".to_string()]);
        let cmds = commands.get_commands();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], "echo first");
        assert_eq!(cmds[1], "echo second");
    }

    #[test]
    fn test_shortcut_commands_display_single() {
        let commands = ShortcutCommands::Single("stache reload".to_string());
        let display = commands.commands_display();
        assert_eq!(display, "stache reload");
    }

    #[test]
    fn test_shortcut_commands_display_multiple() {
        let commands = ShortcutCommands::Multiple(vec![
            "cmd1".to_string(),
            "cmd2".to_string(),
            "cmd3".to_string(),
        ]);
        let display = commands.commands_display();
        assert_eq!(display, "[3 commands]");
    }
}
