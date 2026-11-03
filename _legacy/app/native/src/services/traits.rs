//! Module and service trait definitions.
//!
//! This module defines the core traits for application modules that can be
//! initialized, configured, and shut down in a uniform way.

use tauri::AppHandle;
use thiserror::Error;

/// Errors that can occur during module operations.
#[derive(Debug, Error)]
pub enum ModuleError {
    /// Module initialization failed.
    #[error("Failed to initialize module '{name}': {reason}")]
    InitializationFailed { name: &'static str, reason: String },

    /// Module is not enabled in configuration.
    #[error("Module '{0}' is not enabled")]
    NotEnabled(&'static str),

    /// Required permission is missing.
    #[error("Module '{name}' requires permission: {permission}")]
    PermissionDenied {
        name: &'static str,
        permission: String,
    },

    /// Module operation failed.
    #[error("Module '{name}' operation failed: {reason}")]
    OperationFailed { name: &'static str, reason: String },
}

impl ModuleError {
    /// Creates an initialization failed error.
    pub fn init_failed(name: &'static str, reason: impl Into<String>) -> Self {
        Self::InitializationFailed { name, reason: reason.into() }
    }

    /// Creates a permission denied error.
    pub fn permission_denied(name: &'static str, permission: impl Into<String>) -> Self {
        Self::PermissionDenied {
            name,
            permission: permission.into(),
        }
    }

    /// Creates an operation failed error.
    pub fn operation_failed(name: &'static str, reason: impl Into<String>) -> Self {
        Self::OperationFailed { name, reason: reason.into() }
    }
}

/// Result type for module operations.
pub type ModuleResult<T> = std::result::Result<T, ModuleError>;

/// Trait for application modules that can be initialized and shut down.
///
/// Modules are self-contained features of the application that:
/// - Have a configuration that determines if they're enabled
/// - Can be initialized during app startup
/// - Can be gracefully shut down during app exit
///
/// # Example
///
/// ```ignore
/// struct AudioModule {
///     enabled: bool,
///     watcher: Option<AudioWatcher>,
/// }
///
/// impl Module for AudioModule {
///     fn name(&self) -> &'static str { "audio" }
///
///     fn is_enabled(&self) -> bool { self.enabled }
///
///     fn init(&mut self, app: AppHandle) -> ModuleResult<()> {
///         self.watcher = Some(AudioWatcher::new(app));
///         Ok(())
///     }
/// }
/// ```
pub trait Module: Send + Sync {
    /// Returns the module name for logging and identification.
    fn name(&self) -> &'static str;

    /// Checks if the module is enabled in configuration.
    ///
    /// Modules that return `false` will not be initialized.
    fn is_enabled(&self) -> bool;

    /// Initializes the module.
    ///
    /// Called during app startup for all enabled modules.
    /// The app handle can be stored for later use (e.g., emitting events).
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails.
    fn init(&mut self, app: AppHandle) -> ModuleResult<()>;

    /// Shuts down the module gracefully.
    ///
    /// Called during app exit. Default implementation does nothing.
    fn shutdown(&mut self) {}

    /// Called when configuration is reloaded.
    ///
    /// Modules can override this to handle hot-reload of settings.
    /// Default implementation does nothing.
    fn on_config_reload(&mut self) {}
}

/// Trait for modules that run background services.
///
/// Background services are long-running tasks that need to be started
/// and stopped independently of module initialization.
///
/// # Example
///
/// ```ignore
/// impl BackgroundService for WallpaperModule {
///     fn start(&mut self) -> ModuleResult<()> {
///         self.timer = Some(start_wallpaper_timer());
///         Ok(())
///     }
///
///     fn stop(&mut self) {
///         if let Some(timer) = self.timer.take() {
///             timer.cancel();
///         }
///     }
///
///     fn is_running(&self) -> bool {
///         self.timer.is_some()
///     }
/// }
/// ```
pub trait BackgroundService: Module {
    /// Starts the background service.
    ///
    /// # Errors
    ///
    /// Returns an error if the service fails to start.
    fn start(&mut self) -> ModuleResult<()>;

    /// Stops the background service.
    fn stop(&mut self);

    /// Checks if the service is currently running.
    fn is_running(&self) -> bool;

    /// Restarts the background service.
    ///
    /// Default implementation stops and starts the service.
    ///
    /// # Errors
    ///
    /// Returns an error if the service fails to restart.
    fn restart(&mut self) -> ModuleResult<()> {
        self.stop();
        self.start()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_error_display() {
        let err = ModuleError::init_failed("test", "Something went wrong");
        let msg = err.to_string();
        assert!(msg.contains("test"));
        assert!(msg.contains("Something went wrong"));
    }

    #[test]
    fn test_module_error_permission_denied() {
        let err = ModuleError::permission_denied("tiling", "Accessibility");
        let msg = err.to_string();
        assert!(msg.contains("tiling"));
        assert!(msg.contains("Accessibility"));
    }
}
