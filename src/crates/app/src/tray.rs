//! Native tray menu and actions.
//!
//! The tray is created on the `AppKit` main thread before the host starts its slow
//! services. Menu callbacks only enqueue work on the host worker; they never
//! join a module, wait for the socket or block on configuration I/O.
//!
//! Enable is configuration-backed and executes through the host's validated
//! TOML edit. Pause/resume are session-only and never touch a file. The menu is
//! built from a pure row description so wording and action eligibility are
//! testable without `AppKit`.

use tauri::AppHandle;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;

use crate::bindings::{AccessState, ConfigFileView, HostStatus, ModuleState, ModuleStatusView};
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
    /// Open or focus the Accessibility guidance window.
    AccessGuide,
    /// Explicitly request Accessibility (may show the system prompt).
    AccessRequest,
    /// Open the Accessibility pane of System Settings.
    AccessSettings,
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
        match (status.access.state, status.access.revoked) {
            (AccessState::Checking, _) => "Stache Next — checking Accessibility…",
            (AccessState::Blocked, true) => "Stache Next — Accessibility was turned off",
            (AccessState::Blocked, false) => "Stache Next — Accessibility required",
            (AccessState::Open, _) => "Stache Next",
        }
    } else {
        "Stache Next — starting…"
    };
    rows.push(item("status", header, None, None));
    if status.access.state == AccessState::Blocked {
        rows.push(item(
            "access-guide",
            "Show Accessibility Help…",
            Some(TrayAction::AccessGuide),
            None,
        ));
        rows.push(item(
            "access-request",
            "Request Accessibility…",
            Some(TrayAction::AccessRequest),
            None,
        ));
        rows.push(item(
            "access-settings",
            "Open Accessibility Settings…",
            Some(TrayAction::AccessSettings),
            None,
        ));
    }
    if let Some(error) = &status.error {
        rows.push(item("error", truncate(error, 80), None, None));
    }
    for module in &status.modules {
        module_rows(&mut rows, module);
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
    // A failed disabled or paused module has a failed stop to retry.
    let retryable = module.state == ModuleState::Failed
        || (module.state == ModuleState::Unavailable && module.enabled && !module.paused);
    if retryable {
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
    } else if id == "access-guide" {
        host.open_access_guide();
    } else if id == "access-request" {
        host.request_permission();
    } else if id == "access-settings" {
        host.open_accessibility_settings();
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
    use crate::bindings::{AccessView, ConfigFileState, ModuleStatusView};

    fn module(state: ModuleState, enabled: bool, paused: bool) -> ModuleStatusView {
        ModuleStatusView {
            id: "testModule".to_owned(),
            state,
            enabled,
            paused,
            revision: "1".to_owned(),
            detail: None,
            diagnostic: None,
            keybindings: None,
            tiling: None,
        }
    }

    fn status() -> HostStatus {
        HostStatus {
            ready: true,
            error: None,
            access: AccessView {
                state: AccessState::Open,
                revoked: false,
                app_path: None,
            },
            modules: Vec::new(),
            config: Vec::new(),
        }
    }

    fn gated(state: AccessState, revoked: bool) -> HostStatus {
        let mut snapshot = status();
        snapshot.access.state = state;
        snapshot.access.revoked = revoked;
        snapshot
    }

    fn labels(rows: &[TrayRow]) -> Vec<(String, String)> {
        rows.iter()
            .filter_map(|row| match row {
                TrayRow::Item(item) => Some((item.id.clone(), item.label.clone())),
                TrayRow::Separator => None,
            })
            .collect()
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
    fn retry_is_offered_for_every_failed_module_and_only_an_active_unavailable_one() {
        let mut snapshot = status();
        snapshot.modules = vec![module(ModuleState::Failed, false, false)];
        assert!(actions(&rows(&snapshot)).iter().any(|a| matches!(a, TrayAction::Retry(_))));
        snapshot.modules = vec![module(ModuleState::Failed, true, true)];
        assert!(actions(&rows(&snapshot)).iter().any(|a| matches!(a, TrayAction::Retry(_))));
        snapshot.modules = vec![module(ModuleState::Unavailable, false, false)];
        assert!(!actions(&rows(&snapshot)).iter().any(|a| matches!(a, TrayAction::Retry(_))));
        snapshot.modules = vec![module(ModuleState::Unavailable, true, true)];
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
            file: "proxyAudio.toml".to_owned(),
            path: "/tmp/proxyAudio.toml".to_owned(),
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

    fn access_rows(snapshot: &HostStatus) -> Vec<(String, String)> {
        labels(&rows(snapshot))
            .into_iter()
            .filter(|(id, _)| id == "status" || id.starts_with("access-"))
            .collect()
    }

    fn pair(id: &str, label: &str) -> (String, String) { (id.to_owned(), label.to_owned()) }

    #[test]
    fn checking_shows_only_the_checking_status_row() {
        let snapshot = gated(AccessState::Checking, false);
        assert_eq!(access_rows(&snapshot), vec![pair(
            "status",
            "Stache Next — checking Accessibility…"
        )]);
    }

    #[test]
    fn blocked_shows_the_required_status_and_both_gate_actions() {
        let snapshot = gated(AccessState::Blocked, false);
        assert_eq!(access_rows(&snapshot), vec![
            pair("status", "Stache Next — Accessibility required"),
            pair("access-guide", "Show Accessibility Help…"),
            pair("access-request", "Request Accessibility…"),
            pair("access-settings", "Open Accessibility Settings…"),
        ]);
        let actions = actions(&rows(&snapshot));
        assert!(actions.contains(&TrayAction::AccessRequest));
        assert!(actions.contains(&TrayAction::AccessSettings));
    }

    #[test]
    fn the_guide_row_is_the_first_gate_row_and_only_while_blocked() {
        for revoked in [false, true] {
            let rows = rows(&gated(AccessState::Blocked, revoked));
            let ids: Vec<String> = labels(&rows).into_iter().map(|(id, _)| id).collect();
            assert_eq!(ids[0], "status");
            assert_eq!(ids[1], "access-guide", "{ids:?}");
            assert_eq!(ids[2], "access-request");
            assert_eq!(ids[3], "access-settings");
            assert!(actions(&rows).contains(&TrayAction::AccessGuide));
        }
        for state in [AccessState::Checking, AccessState::Open] {
            let rows = rows(&gated(state, false));
            assert!(!labels(&rows).iter().any(|(id, _)| id == "access-guide"));
            assert!(!actions(&rows).contains(&TrayAction::AccessGuide));
        }
    }

    #[test]
    fn revoked_changes_only_the_status_wording() {
        let snapshot = gated(AccessState::Blocked, true);
        assert_eq!(access_rows(&snapshot), vec![
            pair("status", "Stache Next — Accessibility was turned off"),
            pair("access-guide", "Show Accessibility Help…"),
            pair("access-request", "Request Accessibility…"),
            pair("access-settings", "Open Accessibility Settings…"),
        ]);
    }

    #[test]
    fn open_has_no_gate_row_and_no_snapshot_has_a_permission_row() {
        assert_eq!(access_rows(&gated(AccessState::Open, false)), vec![pair(
            "status",
            "Stache Next"
        )]);
        for (state, revoked) in [
            (AccessState::Checking, false),
            (AccessState::Blocked, false),
            (AccessState::Blocked, true),
            (AccessState::Open, false),
        ] {
            let mut snapshot = gated(state, revoked);
            snapshot.modules = vec![module(ModuleState::WaitingForPermission, true, false)];
            assert!(
                !labels(&rows(&snapshot)).iter().any(|(id, _)| id == "permission"),
                "the per-module permission row is gone"
            );
        }
    }

    #[test]
    fn a_blocked_menu_keeps_modules_and_the_always_available_actions() {
        let mut snapshot = gated(AccessState::Blocked, false);
        snapshot.modules = vec![module(ModuleState::WaitingForPermission, true, false)];
        let rows = rows(&snapshot);
        assert!(labels(&rows).iter().any(|(_, label)| label.contains("waiting for permission")));
        let actions = actions(&rows);
        for expected in [
            TrayAction::Enable {
                module: "testModule".to_owned(),
                enabled: false,
            },
            TrayAction::Pause("testModule".to_owned()),
            TrayAction::TestWindow,
            TrayAction::Quit,
        ] {
            assert!(actions.contains(&expected), "missing {expected:?}");
        }
    }

    #[test]
    fn a_host_that_is_not_ready_reads_starting() {
        let mut snapshot = gated(AccessState::Checking, false);
        snapshot.ready = false;
        assert_eq!(access_rows(&snapshot), vec![pair(
            "status",
            "Stache Next — starting…"
        )]);
    }
}
