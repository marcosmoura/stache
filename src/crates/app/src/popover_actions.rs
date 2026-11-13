//! The popover Overview's module and window commands.
//!
//! Each command maps to exactly the host command the former tray row used
//! ([`PopoverRequest::send`]): Enable is configuration-backed
//! (`HostHandle::set_enabled`), Retry keeps the tray's eligibility rule on the
//! host side, and the Test Window and the Accessibility guide open through
//! `NativeWindows`, which hides the popover first. Retry and the windows return
//! at once; the result arrives through `status-changed`. Enable returns a typed
//! [`ModuleEnableOutcome`] once the runner processed it. Pause/Resume reuse the generic
//! session action ([`crate::session_action`]) with its stale-click rule and
//! return its typed outcome.
//!
//! All five are ACL-gated: only the `popover-window` capability
//! (`stache-popover`) grants them (`build.rs`).

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::host::{HostHandle, HostState};
use crate::session_action::{SessionAction, SessionActionOutcome};

/// One fire-and-forget popover request and the host command it sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PopoverRequest {
    /// The configuration-backed enabled flag (the tray's `enable:<id>`).
    SetEnabled {
        /// Module identifier.
        id: String,
        /// The requested flag.
        enabled: bool,
    },
    /// Retry an eligible failed or unavailable module (`retry:<id>`).
    Retry(String),
    /// Open or focus the development test window (`test-window`).
    TestWindow,
    /// Open or focus the Accessibility guidance window (`access-guide`).
    AccessGuide,
}

impl PopoverRequest {
    /// Queues the request's host command; never waits.
    pub fn send(self, host: &HostHandle) {
        match self {
            Self::SetEnabled { id, enabled } => host.set_enabled(id, enabled),
            Self::Retry(id) => host.retry(id),
            Self::TestWindow => host.open_test_window(),
            Self::AccessGuide => host.open_access_guide(),
        }
    }
}

/// What the host did with one popover enable request, after the runner
/// processed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ModuleEnableOutcome {
    /// The file was written and the module reloaded.
    Applied,
    /// The file already held the value; the runtime was synced to it.
    Unchanged,
    /// The host refused or failed; the same message is published as the host
    /// error.
    Refused {
        /// The host's message.
        message: String,
    },
    /// The host did not process the request (shutdown, not running, timeout).
    Unavailable {
        /// Why.
        message: String,
    },
}

impl ModuleEnableOutcome {
    pub(crate) fn unavailable(message: &str) -> Self {
        Self::Unavailable { message: message.to_owned() }
    }
}

/// Sets a module's configuration-backed `enabled` flag and returns the host's
/// typed outcome once the runner processed it (bounded; a shutdown wakes it).
/// The new state also arrives through `status-changed`.
#[tauri::command]
#[specta::specta]
pub async fn set_module_enabled(
    app: tauri::AppHandle,
    id: String,
    enabled: bool,
) -> ModuleEnableOutcome {
    use tauri::Manager as _;

    let host = app.state::<HostState>().0.clone();
    tauri::async_runtime::spawn_blocking(move || host.set_enabled_outcome(id, enabled))
        .await
        .unwrap_or_else(|_| ModuleEnableOutcome::unavailable("the host did not answer"))
}

/// Retries a module. The host applies the tray's rule: a failed module, or an
/// enabled, unpaused unavailable one. Returns at once.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)] // Tauri injects managed state by value.
pub fn retry_module(state: tauri::State<'_, HostState>, id: String) {
    PopoverRequest::Retry(id).send(&state.0);
}

/// Opens or focuses the test window (the popover hides first). Returns at once.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)] // Tauri injects managed state by value.
pub fn open_test_window(state: tauri::State<'_, HostState>) {
    PopoverRequest::TestWindow.send(&state.0);
}

/// Opens or focuses the Accessibility guidance window while the gate is not
/// open (the popover hides first). Returns at once.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)] // Tauri injects managed state by value.
pub fn open_accessibility_guide(state: tauri::State<'_, HostState>) {
    PopoverRequest::AccessGuide.send(&state.0);
}

/// Pauses or resumes one module for this session and returns the outcome.
///
/// Runs the generic session action. `action` names what the
/// caller's observed state implies (`pause` for a module it saw unpaused); a
/// request the host has moved past is `stale`.
#[tauri::command]
#[specta::specta]
pub async fn module_session_action(
    app: tauri::AppHandle,
    module: String,
    action: SessionAction,
) -> SessionActionOutcome {
    use tauri::Manager as _;

    let host = app.state::<HostState>().0.clone();
    tauri::async_runtime::spawn_blocking(move || session(&host, &module, action))
        .await
        .unwrap_or(SessionActionOutcome::Uncertain)
}

/// The session action on the host handle (blocks for a bounded time).
#[must_use]
pub fn session(host: &HostHandle, module: &str, action: SessionAction) -> SessionActionOutcome {
    host.session_action(module, action)
}
