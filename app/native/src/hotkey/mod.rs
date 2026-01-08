//! Hotkey Daemon for Stache.
//!
//! This module provides a background daemon that listens for global keyboard shortcuts
//! and executes configured commands when those shortcuts are activated.
//!
//! The daemon reads its configuration from the global Stache configuration file
//! and uses Tauri's global-shortcut plugin to register system-wide hotkeys.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use tauri::Runtime;
use tauri_plugin_global_shortcut::{Builder, Shortcut, ShortcutState};

use crate::config::{ShortcutCommands, get_config};
use crate::utils::command::resolve_binary;

/// Maps registered shortcuts to their corresponding commands.
///
/// The key is the parsed `Shortcut` struct, and the value contains the commands to execute.
type ShortcutCommandMap = Arc<HashMap<Shortcut, ShortcutCommands>>;

/// Creates the global-shortcut plugin with all configured shortcuts registered.
///
/// This function reads the shortcuts from the global configuration and sets up
/// a handler that executes the corresponding command when a shortcut is triggered.
///
/// # Returns
///
/// Returns a configured `TauriPlugin` that can be added to the Tauri app builder.
pub fn create_hotkey_plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let config = get_config();
    let keybindings = &config.keybindings;

    if keybindings.is_empty() {
        // No keybindings configured, return a no-op plugin
        return Builder::<R>::new().build();
    }

    // Build the shortcut-to-command mapping
    let mut shortcut_map: HashMap<Shortcut, ShortcutCommands> = HashMap::new();
    let mut valid_shortcuts: Vec<Shortcut> = Vec::new();

    for (shortcut_key, commands) in keybindings {
        // Normalize the shortcut string for consistency
        let shortcut_str = normalize_shortcut(shortcut_key);

        // Try to parse the shortcut to validate it
        match shortcut_str.parse::<Shortcut>() {
            Ok(shortcut) => {
                shortcut_map.insert(shortcut, commands.clone());
                valid_shortcuts.push(shortcut);
            }
            Err(err) => {
                eprintln!("stache: warning: invalid shortcut '{shortcut_key}': {err}");
            }
        }
    }

    if valid_shortcuts.is_empty() {
        return Builder::<R>::new().build();
    }

    let shortcut_map: ShortcutCommandMap = Arc::new(shortcut_map);
    let shortcut_map_handler = Arc::clone(&shortcut_map);

    // Build the plugin with all valid shortcuts using with_shortcuts (batch registration)
    let builder = match Builder::<R>::new().with_shortcuts(valid_shortcuts) {
        Ok(b) => b,
        Err(err) => {
            eprintln!("stache: warning: failed to register shortcuts: {err}");
            return Builder::<R>::new().build();
        }
    };

    builder
        .with_handler(move |_app, shortcut, event| {
            // Only trigger on key press, not release
            if event.state != ShortcutState::Pressed {
                return;
            }

            if let Some(config) = shortcut_map_handler.get(shortcut) {
                execute_shortcut_commands(config);
            }
        })
        .build()
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
fn execute_shortcut_commands(shortcut_commands: &ShortcutCommands) {
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

/// Executes a single command and returns true if successful.
///
/// # Arguments
/// * `command` - The command string to execute
/// * `description` - Description for logging
/// * `index` - 1-based index of this command in the sequence
/// * `total` - Total number of commands in the sequence
fn execute_single_command(command: &str, description: &str, index: usize, total: usize) -> bool {
    // Parse the command into parts
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        eprintln!("stache: warning: empty command for shortcut");
        return false;
    }

    let binary = parts[0];
    let args = &parts[1..];

    // Resolve the binary path
    let binary_path = match resolve_binary(binary) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("stache: warning: failed to resolve binary '{binary}': {err}");
            return false;
        }
    };

    match Command::new(&binary_path).args(args).spawn() {
        Ok(mut child) => {
            // Wait for the command to complete before proceeding to the next
            match child.wait() {
                Ok(status) => {
                    if !status.success() {
                        eprintln!(
                            "stache: command '{description}' (step {index}/{total}) exited with status: {status}"
                        );
                        return false;
                    }
                    true
                }
                Err(err) => {
                    eprintln!("stache: failed to wait for command '{description}': {err}");
                    false
                }
            }
        }
        Err(err) => {
            eprintln!(
                "stache: failed to execute command '{}': {err}",
                binary_path.display()
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

    // ========================================================================
    // ShortcutCommandMap type alias tests
    // ========================================================================

    #[test]
    fn test_shortcut_command_map_creation() {
        let map: HashMap<Shortcut, ShortcutCommands> = HashMap::new();
        let arc_map: ShortcutCommandMap = Arc::new(map);
        assert!(arc_map.is_empty());
    }

    #[test]
    fn test_shortcut_command_map_insert_and_retrieve() {
        let mut map: HashMap<Shortcut, ShortcutCommands> = HashMap::new();
        let shortcut: Shortcut = "Command+K".parse().unwrap();
        let commands = ShortcutCommands::Single("echo hello".to_string());
        map.insert(shortcut, commands);

        let arc_map: ShortcutCommandMap = Arc::new(map);
        assert_eq!(arc_map.len(), 1);
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
