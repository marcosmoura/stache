//! `generate-schema`: writes or checks the committed per-file JSON Schemas.
//!
//! The repository root is baked in at compile time because this is a developer
//! command, not a distributed executable. `--check` never writes.

#![forbid(unsafe_code)]

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut check = false;
    for argument in std::env::args_os().skip(1) {
        if argument == "--check" {
            check = true;
        } else {
            eprintln!(
                "stache: unknown argument `{}` (expected --check)",
                argument.to_string_lossy()
            );
            return ExitCode::FAILURE;
        }
    }
    let catalog = match stache_app::catalog::catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("stache: {error}");
            return ExitCode::FAILURE;
        }
    };
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").join("schemas");
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    stache_cli::run_generate(&catalog, &directory, check, &mut out, &mut err)
}
