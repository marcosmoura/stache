#![allow(clippy::multiple_crate_versions)]

// Emit a clear compile-time error if attempted to compile on unsupported platforms
#[cfg(not(target_os = "macos"))]
compile_error!("This application only supports macOS.");

fn main() { barba_lib::run(); }
