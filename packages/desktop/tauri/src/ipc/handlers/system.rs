//! System command handlers.
//!
//! This module handles system-level IPC commands such as
//! configuration schema generation.

use std::io::Write;
use std::os::unix::net::UnixStream;

use crate::config;

/// Handles the generate-schema command.
///
/// Generates and prints the JSON schema for the configuration file.
pub fn handle_generate_schema(stream: &mut UnixStream) {
    let schema = config::print_schema();
    println!("{schema}");
    stream.write_all(b"1").ok();
}
