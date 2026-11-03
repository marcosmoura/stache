use std::fmt::Debug;

/// Runtime lifecycle state for a Stache module, surfaced in the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleStatus {
    /// Disabled in persisted config. Shown in tray but locked (cannot be toggled at runtime).
    ConfiguredOff,
    /// Configured on and currently active.
    Running,
    /// Configured on but paused by the user via the tray. Resumes on restart/reload.
    Paused,
    /// Configured on but failed to start. `reason` explains why (e.g. missing permission).
    Unavailable(String),
}

/// Uniform contract so every module can be paused/resumed from the tray without
/// per-module special-casing. Implemented by each of the 6 toggleable modules.
pub trait LifecycleModule: Send + Sync {
    /// Human-readable module name (also used as the tray item label).
    fn name(&self) -> &'static str;
    /// Stable menu item id used to route `on_menu_event`.
    fn id(&self) -> &'static str;
    /// Start the module (called once at startup if configured on).
    ///
    /// # Errors
    ///
    /// Returns a human-readable error when the module cannot start.
    fn start(&self) -> Result<(), String>;
    /// Pause the module, releasing/disabling its OS resources.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error when the module cannot pause.
    fn pause(&self) -> Result<(), String>;
    /// Resume the module, re-acquiring OS resources.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error when the module cannot resume.
    fn resume(&self) -> Result<(), String>;
    /// Current lifecycle status.
    fn status(&self) -> ModuleStatus;
}
