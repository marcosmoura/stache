//! FFI wrappers for macOS APIs used by the tiling window manager.
//!
//! This module provides safe Rust wrappers around macOS Accessibility API
//! and other system APIs. The goal is to encapsulate unsafe FFI code and
//! provide a safe, ergonomic interface for the rest of the tiling system.
//!
//! # Modules
//!
//! - [`accessibility`] - Safe wrappers for `AXUIElement` and related APIs

pub mod accessibility;

pub use accessibility::AXElement;
