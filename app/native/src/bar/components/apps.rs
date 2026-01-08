//! Application launcher component.
//!
//! Manages opening whitelisted macOS applications and URLs via the Tauri command interface.

#![allow(unexpected_cfgs)]

use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

use crate::error::StacheError;

#[derive(Clone, Copy)]
enum LaunchTarget {
    Application(&'static str),
    Url(&'static str),
}

#[derive(Clone, Copy)]
struct AppEntry {
    display_name: &'static str,
    target: LaunchTarget,
}

impl AppEntry {
    const fn app(name: &'static str) -> Self {
        Self {
            display_name: name,
            target: LaunchTarget::Application(name),
        }
    }

    const fn url(display_name: &'static str, url: &'static str) -> Self {
        Self {
            display_name,
            target: LaunchTarget::Url(url),
        }
    }
}

/// Allowed macOS application display names that can be opened via the Tauri command.
const ALLOWED_APPS: [AppEntry; 7] = [
    AppEntry::app("Activity Monitor"),
    AppEntry::app("Clock"),
    AppEntry::app("Microsoft Edge Dev"),
    AppEntry::app("Spotify"),
    AppEntry::app("Tidal"),
    AppEntry::app("Weather"),
    AppEntry::url(
        "Battery",
        "x-apple.systempreferences:com.apple.Battery-Settings.extension",
    ),
];

#[inline]
fn resolve_allowed_app(name: &str) -> Option<&'static AppEntry> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }

    ALLOWED_APPS
        .iter()
        .find(|entry| trimmed.eq_ignore_ascii_case(entry.display_name))
}

fn run_open_command<'a, I>(app: &AppHandle, args: I, target: &str) -> Result<(), StacheError>
where I: IntoIterator<Item = &'a str> {
    let collected_args: Vec<&str> = args.into_iter().collect();
    let status = tauri::async_runtime::block_on(async {
        app.shell().command("open").args(&collected_args).status().await
    })
    .map_err(|err| StacheError::ShellError(format!("Failed to launch '{target}': {err}")))?;

    if status.success() {
        Ok(())
    } else if let Some(code) = status.code() {
        Err(StacheError::ShellError(format!(
            "Launching '{target}' exited with status code {code}."
        )))
    } else {
        Err(StacheError::ShellError(format!(
            "Launching '{target}' failed: terminated by external signal"
        )))
    }
}

fn launch_application(app: &AppHandle, name: &str) -> Result<(), StacheError> {
    run_open_command(app, ["-a", name], name)
}

fn launch_url(app: &AppHandle, url: &str) -> Result<(), StacheError> {
    run_open_command(app, [url], url)
}

/// Opens a whitelisted macOS application by its display name.
///
/// # Errors
///
/// Returns an error if the application name is not whitelisted or if launching the
/// application fails.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn open_app(app: tauri::AppHandle, name: &str) -> Result<(), StacheError> {
    resolve_allowed_app(name).map_or_else(
        || {
            Err(StacheError::InvalidArguments(format!(
                "Application '{name}' is not allowed."
            )))
        },
        |entry| match entry.target {
            LaunchTarget::Application(app_name) => launch_application(&app, app_name),
            LaunchTarget::Url(url) => launch_url(&app, url),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_allowed_app_finds_application_case_insensitively() {
        let entry = resolve_allowed_app("  spotify  ").expect("Spotify should be allowed");
        assert_eq!(entry.display_name, "Spotify");

        match entry.target {
            LaunchTarget::Application(name) => assert_eq!(name, "Spotify"),
            LaunchTarget::Url(_) => panic!("Spotify should resolve to an application"),
        }
    }

    #[test]
    fn resolve_allowed_app_handles_url_entries() {
        let entry = resolve_allowed_app("battery").expect("Battery shortcut should exist");

        match entry.target {
            LaunchTarget::Url(url) => assert_eq!(
                url,
                "x-apple.systempreferences:com.apple.Battery-Settings.extension"
            ),
            LaunchTarget::Application(_) => panic!("Battery shortcut should resolve to a URL"),
        }
    }

    #[test]
    fn resolve_allowed_app_rejects_empty_or_unknown_names() {
        assert!(resolve_allowed_app("   ").is_none());
        assert!(resolve_allowed_app("Nonexistent App").is_none());
    }
}
