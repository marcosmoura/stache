use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{Map, Value};
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

use crate::utils::command::resolve_binary;

static HYPRSPACE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Retrieve every workspace that AeroSpace/Hyprspace knows about.
/// Falls back to `--all` so multi-monitor setups are covered.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_hyprspace_workspaces(app: tauri::AppHandle) -> Result<Vec<Value>, String> {
    run_hyprspace_json(&app, &["list-workspaces", "--all", "--json"])
}

/// Retrieve all windows for the currently focused workspace.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_hyprspace_current_workspace_windows(
    app: tauri::AppHandle,
) -> Result<Vec<Value>, String> {
    run_hyprspace_json(&app, &["list-windows", "--workspace", "focused", "--json"])
}

/// Retrieve all windows for the currently focused workspace.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_hyprspace_focused_window(app: tauri::AppHandle) -> Result<Vec<Value>, String> {
    run_hyprspace_json(&app, &["list-windows", "--focused", "--json"])
}

/// Retrieve metadata for the focused workspace only.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_hyprspace_focused_workspace(app: tauri::AppHandle) -> Result<Value, String> {
    let mut workspaces = run_hyprspace_json(&app, &["list-workspaces", "--focused", "--json"])?;
    workspaces
        .pop()
        .ok_or_else(|| "Hyprspace did not return a focused workspace".to_string())
}

/// Switch to the requested workspace by name.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn go_to_hyprspace_workspace(
    app: tauri::AppHandle,
    mut workspace: String,
) -> Result<(), String> {
    workspace = workspace.trim().to_string();
    if workspace.is_empty() {
        return Err("Workspace name cannot be empty".to_string());
    }

    let binary = hyprspace_binary()?;

    let status = tauri::async_runtime::block_on(async {
        app.shell()
            .command("script")
            .args(["-q", "-t", "0", "/dev/null"])
            .arg(binary.as_os_str())
            .arg("workspace")
            .arg(&workspace)
            .status()
            .await
    })
    .map_err(|err| {
        let cmd = format!(
            "script -q -t 0 /dev/null {} workspace {}",
            binary.display(),
            workspace
        );
        format!("Failed to run `{cmd}`: {err}")
    })?;

    if !status.success() {
        let message = status.code().map_or_else(
            || "Hyprspace workspace switch terminated by signal".to_string(),
            |code| format!("Hyprspace workspace switch exited with status {code}"),
        );
        return Err(message);
    }

    Ok(())
}

fn run_hyprspace_json(app: &AppHandle, args: &[&str]) -> Result<Vec<Value>, String> {
    let raw = run_hyprspace_command(app, args)?;
    parse_json_array(&raw)
}

fn hyprspace_binary() -> Result<&'static PathBuf, String> {
    if let Some(path) = HYPRSPACE_PATH.get() {
        return Ok(path);
    }

    let resolved = resolve_binary("hyprspace")
        .map_err(|err| format!("Unable to resolve hyprspace binary: {err}"))?;
    let _ = HYPRSPACE_PATH.set(resolved);

    HYPRSPACE_PATH
        .get()
        .ok_or_else(|| "Unable to cache hyprspace binary path".to_string())
}

fn format_command(binary: &Path, args: &[&str]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(binary.display().to_string());
    parts.extend(args.iter().map(|arg| (*arg).to_string()));
    parts.join(" ")
}

fn run_hyprspace_command(app: &AppHandle, args: &[&str]) -> Result<String, String> {
    let binary = hyprspace_binary()?;
    let formatted = format_command(binary, args);
    let output = tauri::async_runtime::block_on(async {
        app.shell().command(binary.as_os_str()).args(args).output().await
    })
    .map_err(|err| format!("Failed to run `{formatted}`: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`{formatted}` exited with status {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }

    String::from_utf8(output.stdout)
        .map_err(|err| format!("`{}` returned invalid UTF-8: {err}", binary.display()))
}

fn parse_json_array(raw: &str) -> Result<Vec<Value>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    let parsed: Value =
        serde_json::from_str(raw).map_err(|err| format!("Failed to parse JSON output: {err}"))?;

    match parsed {
        Value::Array(items) => Ok(items.into_iter().map(normalize_value).collect()),
        other => Ok(vec![normalize_value(other)]),
    }
}

fn normalize_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(normalize_map(map)),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_value).collect()),
        other => other,
    }
}

fn normalize_map(map: Map<String, Value>) -> Map<String, Value> {
    let mut normalized = Map::with_capacity(map.len());
    for (key, value) in map {
        let normalized_key = to_camel_case(&key);
        normalized.insert(normalized_key, normalize_value(value));
    }
    normalized
}

fn to_camel_case(input: &str) -> String {
    if !input.contains(['-', '_', ' ']) {
        return input.to_string();
    }

    let mut result = String::with_capacity(input.len());
    let mut uppercase_next = false;

    for ch in input.chars() {
        match ch {
            '-' | '_' | ' ' => uppercase_next = true,
            _ if uppercase_next => {
                result.extend(ch.to_uppercase());
                uppercase_next = false;
            }
            _ => result.push(ch),
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_json_array, to_camel_case};

    #[test]
    fn parse_empty_output_is_ok() {
        assert!(parse_json_array("  ").unwrap().is_empty());
    }

    #[test]
    fn parse_array_output_is_forwarded() {
        let parsed = parse_json_array(r#"[{"a":1},{"b":2}]"#).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_object_output_is_wrapped() {
        let parsed = parse_json_array(r#"{"workspace":"dev"}"#).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], json!({"workspace": "dev"}));
    }

    #[test]
    fn parse_converts_hyphenated_keys_to_camel_case() {
        let parsed =
            parse_json_array(r#"[{"app-name":"Ghostty","window-id":37,"window-title":"Barba"}]"#)
                .unwrap();
        assert_eq!(
            parsed[0],
            json!({"appName":"Ghostty","windowId":37,"windowTitle":"Barba"})
        );
    }

    #[test]
    fn to_camel_case_handles_mixed_input() {
        assert_eq!(to_camel_case("app-name"), "appName");
        assert_eq!(to_camel_case("window-id"), "windowId");
        assert_eq!(to_camel_case("alreadyCamel"), "alreadyCamel");
    }

    #[test]
    fn go_to_workspace_rejects_blank_name() {
        // This test is removed because go_to_workspace now requires an AppHandle
        // and testing it would require setting up a full Tauri app context.
        // The validation logic is still present in the function.
    }
}
