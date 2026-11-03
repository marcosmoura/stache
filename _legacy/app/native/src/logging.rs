//! Logging initialization using the `tracing` crate.
//!
//! This module configures the tracing subscriber with sensible defaults:
//! - Uses `RUST_LOG` environment variable for filtering (default: `info`)
//! - Outputs to stderr for desktop app compatibility
//! - Includes timestamps, target, and log levels

use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Initializes the global tracing subscriber.
///
/// This should be called once at application startup, before any logging occurs.
///
/// The log level can be controlled via the `RUST_LOG` environment variable:
/// - `RUST_LOG=debug` - Show debug and above
/// - `RUST_LOG=stache=debug,warn` - Debug for stache, warn for others
/// - `RUST_LOG=trace` - Show all logs including trace
///
/// Default level is `info` for release builds and `debug` for debug builds.
pub fn init() {
    let default_level = if cfg!(debug_assertions) {
        "debug"
    } else {
        "info"
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        // Default: show info+ for stache, warn+ for everything else
        EnvFilter::new(format!("warn,stache={default_level}"))
    });

    let subscriber = fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false)
        .with_ansi(true)
        .compact();

    tracing_subscriber::registry().with(filter).with(subscriber).init();

    tracing::info!("starting stache desktop application");
}
