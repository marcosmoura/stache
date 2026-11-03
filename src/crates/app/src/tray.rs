//! Native tray menu and actions.
//!
//! The tray is created on the `AppKit` main thread before the host starts its slow
//! services. Menu callbacks only enqueue work on the host worker; they never
//! join a module, wait for the socket or block on configuration I/O.
//!
//! Enable is configuration-backed and executes through the host's validated
//! JSONC edit. Pause/resume are session-only and never touch a file. The menu is
//! built from a pure row description so wording and action eligibility are
//! testable without `AppKit`.

use tauri::AppHandle;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;

use crate::bindings::{ConfigFileView, HostStatus, ModuleState, ModuleStatusView};
use crate::host::HostHandle;

/// The tray icon identity shared with status updates.
pub const TRAY_ID: &str = "stache-next";

/// One actionable tray command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Toggle the configuration-backed enabled flag.
    Enable {
        /// Module identifier.
        module: String,
        /// The requested flag.
        enabled: bool,
    },
    /// Pause a module for this session.
    Pause(String),
    /// Resume a paused module.
    Resume(String),
    /// Retry an eligible failed or unavailable module.
    Retry(String),
    /// Prompt for a needed permission.
    Permission,
    /// Open the explicit test window.
    TestWindow,
    /// Quit through the shared shutdown coordinator.
    Quit,
}

/// A non-action status or diagnostic row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    /// Stable callback identifier, or an empty string for a disabled row.
    pub id: String,
    /// Visible text.
    pub label: String,
    /// The action, or `None` for a status/diagnostic row.
    pub action: Option<TrayAction>,
    /// Check state for a check item.
    pub checked: Option<bool>,
}

/// One rendered menu row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayRow {
    /// A menu item.
    Item(TrayItem),
    /// A separator.
    Separator,
}

/// Creates the tray and its initial menu.
///
/// # Errors
/// Returns a Tauri menu or tray construction error.
pub fn create(app: &AppHandle, host: HostHandle) -> tauri::Result<()> {
    let menu = menu(app, &host.status())?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Stache Next")
        .menu(&menu)
        .on_menu_event(move |_app, event| handle_event(&host, event.id.as_ref()));
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Rebuilds the menu after a status change. Must run on the main thread.
///
/// # Errors
/// Returns a Tauri menu error; the existing menu stays in place.
pub fn update(app: &AppHandle, status: &HostStatus) -> tauri::Result<()> {
    let menu = menu(app, status)?;
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_menu(Some(menu))?;
    }
    Ok(())
}

fn menu(app: &AppHandle, status: &HostStatus) -> tauri::Result<Menu<tauri::Wry>> {
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    for row in rows(status) {
        match row {
            TrayRow::Separator => items.push(Box::new(PredefinedMenuItem::separator(app)?)),
            TrayRow::Item(item) => {
                let enabled = item.action.is_some();
                if let Some(checked) = item.checked {
                    items.push(Box::new(CheckMenuItem::with_id(
                        app,
                        item.id,
                        item.label,
                        enabled,
                        checked,
                        None::<&str>,
                    )?));
                } else {
                    items.push(Box::new(MenuItem::with_id(
                        app,
                        item.id,
                        item.label,
                        enabled,
                        None::<&str>,
                    )?));
                }
            }
        }
    }
    let references: Vec<&dyn IsMenuItem<tauri::Wry>> =
        items.iter().map(std::convert::AsRef::as_ref).collect();
    Menu::with_items(app, &references)
}

/// Builds the pure menu description for one status snapshot.
#[must_use]
pub fn rows(status: &HostStatus) -> Vec<TrayRow> {
    let mut rows = Vec::new();
    let header = if status.ready {
        "Stache Next".to_owned()
    } else {
        "Stache Next — starting…".to_owned()
    };
    rows.push(item("status", header, None, None));
    if let Some(error) = &status.error {
        rows.push(item("error", truncate(error, 80), None, None));
    }
    for module in &status.modules {
        module_rows(&mut rows, module);
    }
    if status
        .modules
        .iter()
        .any(|module| module.state == ModuleState::WaitingForPermission)
    {
        rows.push(item(
            "permission",
            "Request Accessibility…",
            Some(TrayAction::Permission),
            None,
        ));
    }
    for file in &status.config {
        config_rows(&mut rows, file);
    }
    rows.push(TrayRow::Separator);
    rows.push(item(
        "test-window",
        "Open Test Window",
        Some(TrayAction::TestWindow),
        None,
    ));
    rows.push(TrayRow::Separator);
    rows.push(item("quit", "Quit Stache Next", Some(TrayAction::Quit), None));
    rows
}

fn module_rows(rows: &mut Vec<TrayRow>, module: &ModuleStatusView) {
    rows.push(item(
        format!("enable:{}", module.id),
        format!("{} enabled", module.id),
        Some(TrayAction::Enable {
            module: module.id.clone(),
            enabled: !module.enabled,
        }),
        Some(module.enabled),
    ));
    rows.push(item(
        format!("state:{}", module.id),
        format!("  {}: {}", module.id, state_label(module.state)),
        None,
        None,
    ));
    if let Some(detail) = &module.detail {
        rows.push(item(
            format!("detail:{}", module.id),
            format!("  {}", truncate(detail, 80)),
            None,
            None,
        ));
    }
    if let Some(diagnostic) = &module.diagnostic {
        rows.push(item(
            format!("diagnostic:{}", module.id),
            format!("  configuration: {}", truncate(diagnostic, 80)),
            None,
            None,
        ));
    }
    if module.enabled {
        let (id, label, action) = if module.paused {
            (
                format!("resume:{}", module.id),
                format!("Resume {}", module.id),
                TrayAction::Resume(module.id.clone()),
            )
        } else {
            (
                format!("pause:{}", module.id),
                format!("Pause {}", module.id),
                TrayAction::Pause(module.id.clone()),
            )
        };
        rows.push(item(id, label, Some(action), None));
    }
    if module.enabled
        && !module.paused
        && matches!(module.state, ModuleState::Failed | ModuleState::Unavailable)
    {
        rows.push(item(
            format!("retry:{}", module.id),
            format!("Retry {}", module.id),
            Some(TrayAction::Retry(module.id.clone())),
            None,
        ));
    }
}

fn config_rows(rows: &mut Vec<TrayRow>, file: &ConfigFileView) {
    if let Some(diagnostic) = &file.diagnostic {
        rows.push(item(
            format!("config:{}", file.file),
            format!("{}: {}", file.file, truncate(diagnostic, 80)),
            None,
            None,
        ));
    }
    for (index, warning) in file.warnings.iter().enumerate() {
        rows.push(item(
            format!("warning:{}:{index}", file.file),
            format!("{}: {}", file.file, truncate(warning, 80)),
            None,
            None,
        ));
    }
}

fn item(
    id: impl Into<String>,
    label: impl Into<String>,
    action: Option<TrayAction>,
    checked: Option<bool>,
) -> TrayRow {
    TrayRow::Item(TrayItem {
        id: id.into(),
        label: label.into(),
        action,
        checked,
    })
}

const fn state_label(state: ModuleState) -> &'static str {
    match state {
        ModuleState::Stopped => "stopped",
        ModuleState::Disabled => "disabled",
        ModuleState::Running => "running",
        ModuleState::Paused => "paused",
        ModuleState::WaitingForPermission => "waiting for permission",
        ModuleState::Unavailable => "unavailable",
        ModuleState::Failed => "failed",
    }
}

fn handle_event(host: &HostHandle, id: &str) {
    if let Some(module) = id.strip_prefix("enable:") {
        if let Some(current) = module_status(host, module) {
            host.set_enabled(module, !current.enabled);
        }
    } else if let Some(module) = id.strip_prefix("pause:") {
        host.pause(module);
    } else if let Some(module) = id.strip_prefix("resume:") {
        host.resume(module);
    } else if let Some(module) = id.strip_prefix("retry:") {
        host.retry(module);
    } else if id == "permission" {
        host.request_permission();
    } else if id == "test-window" {
        host.open_test_window();
    } else if id == "quit" {
        host.request_shutdown();
    }
}

fn module_status(host: &HostHandle, id: &str) -> Option<ModuleStatusView> {
    host.status().modules.into_iter().find(|module| module.id == id)
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut shortened: String = text.chars().take(limit.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{ConfigFileState, ModuleStatusView};

    fn module(state: ModuleState, enabled: bool, paused: bool) -> ModuleStatusView {
        ModuleStatusView {
            id: "testModule".to_owned(),
            state,
            enabled,
            paused,
            revision: "1".to_owned(),
            detail: None,
            diagnostic: None,
        }
    }

    fn status() -> HostStatus {
        HostStatus {
            ready: true,
            error: None,
            modules: Vec::new(),
            config: Vec::new(),
        }
    }

    fn actions(rows: &[TrayRow]) -> Vec<TrayAction> {
        rows.iter()
            .filter_map(|row| match row {
                TrayRow::Item(item) => item.action.clone(),
                TrayRow::Separator => None,
            })
            .collect()
    }

    #[test]
    fn long_details_are_truncated_on_a_character_boundary() {
        let text = "é".repeat(90);
        let shortened = truncate(&text, 10);
        assert_eq!(shortened.chars().count(), 10);
        assert!(shortened.ends_with('…'));
    }

    #[test]
    fn a_running_module_shows_plain_state_and_no_retry() {
        let mut snapshot = status();
        snapshot.modules = vec![module(ModuleState::Running, true, false)];
        let rows = rows(&snapshot);
        assert!(
            rows.iter().any(|row| matches!(
                row,
                TrayRow::Item(item) if item.action.is_none() && item.label.contains("running")
            )),
            "a plain running status row is required"
        );
        assert!(!actions(&rows).iter().any(|action| matches!(action, TrayAction::Retry(_))));
    }

    #[test]
    fn retry_is_only_offered_for_an_enabled_unpaused_failed_module() {
        let mut snapshot = status();
        snapshot.modules = vec![module(ModuleState::Failed, false, false)];
        assert!(!actions(&rows(&snapshot)).iter().any(|a| matches!(a, TrayAction::Retry(_))));
        snapshot.modules = vec![module(ModuleState::Failed, true, true)];
        assert!(!actions(&rows(&snapshot)).iter().any(|a| matches!(a, TrayAction::Retry(_))));
        snapshot.modules = vec![module(ModuleState::Failed, true, false)];
        assert_eq!(actions(&rows(&snapshot)), vec![
            TrayAction::Enable {
                module: "testModule".to_owned(),
                enabled: false
            },
            TrayAction::Pause("testModule".to_owned()),
            TrayAction::Retry("testModule".to_owned()),
            TrayAction::TestWindow,
            TrayAction::Quit,
        ]);
    }

    #[test]
    fn module_and_file_diagnostics_are_separate_non_action_rows() {
        let mut snapshot = status();
        let mut failed = module(ModuleState::Failed, true, false);
        failed.detail = Some("start failed".to_owned());
        failed.diagnostic = Some("bad field".to_owned());
        snapshot.modules = vec![failed];
        snapshot.config = vec![ConfigFileView {
            file: "proxyAudio.jsonc".to_owned(),
            path: "/tmp/proxyAudio.jsonc".to_owned(),
            state: ConfigFileState::Invalid,
            active: false,
            diagnostic: Some("parse error".to_owned()),
            warnings: vec!["unknown key".to_owned()],
        }];
        let rows = rows(&snapshot);
        for expected in ["start failed", "bad field", "parse error", "unknown key"] {
            assert!(
                rows.iter().any(|row| matches!(
                    row,
                    TrayRow::Item(item) if item.action.is_none() && item.label.contains(expected)
                )),
                "missing non-action row for {expected}"
            );
        }
    }

    #[test]
    fn paused_and_disabled_rows_do_not_offer_retry() {
        let mut snapshot = status();
        snapshot.modules = vec![
            module(ModuleState::Paused, true, true),
            module(ModuleState::Disabled, false, false),
        ];
        let actions = actions(&rows(&snapshot));
        assert!(!actions.iter().any(|action| matches!(action, TrayAction::Retry(_))));
        assert!(actions.iter().any(|action| matches!(action, TrayAction::Resume(_))));
    }
}
