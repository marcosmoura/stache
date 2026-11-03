//! Service infrastructure for Stache.
//!
//! This module provides traits and utilities for managing application modules
//! and background services.
//!
//! - [`traits`] - Module and service trait definitions

pub mod traits;

pub use traits::{BackgroundService, Module, ModuleError};
