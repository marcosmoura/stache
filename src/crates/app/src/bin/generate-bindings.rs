//! `generate-bindings`: writes or checks the committed TypeScript bindings.
//!
//! The repository root is baked in at compile time because this is a developer
//! command, not a distributed executable. `--check` exports to a temporary file
//! and compares bytes; it never writes the committed file.

#![forbid(unsafe_code)]

use std::path::Path;
use std::process::ExitCode;

const COMMITTED: &str = "src/ui/src/sdk/bindings.gen.ts";

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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let root = root.canonicalize().unwrap_or(root);
    let committed = root.join(COMMITTED);
    if check {
        check_bindings(&committed)
    } else {
        write_bindings(&committed)
    }
}

fn write_bindings(committed: &Path) -> ExitCode {
    match export(committed) {
        Ok(()) => {
            println!("wrote {}", committed.display());
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error),
    }
}

fn check_bindings(committed: &Path) -> ExitCode {
    let temporary = match tempfile::NamedTempFile::new() {
        Ok(file) => file,
        Err(error) => return fail(&format!("cannot create a temporary export: {error}")),
    };
    if let Err(error) = export(temporary.path()) {
        return fail(&error);
    }
    let generated = match std::fs::read(temporary.path()) {
        Ok(contents) => contents,
        Err(error) => {
            return fail(&format!("cannot read {}: {error}", temporary.path().display()));
        }
    };
    let current = match std::fs::read(committed) {
        Ok(contents) => contents,
        Err(error) => {
            return fail(&format!("cannot read {}: {error}", committed.display()));
        }
    };
    if generated == current {
        println!("bindings are up to date");
        ExitCode::SUCCESS
    } else {
        eprintln!("stache: {} is out of date", committed.display());
        eprintln!("stache: run `bun run generate-bindings` and commit the result");
        ExitCode::FAILURE
    }
}

fn export(path: &Path) -> Result<(), String> {
    stache_app::bindings::builder()
        .export(specta_typescript::Typescript::default(), path)
        .map_err(|error| format!("failed to export bindings: {error}"))?;
    // Specta emits trailing spaces after union separators and doc-comment lines,
    // and may end the file with more than one newline. Trim trailing whitespace
    // per line and close the file with exactly one newline; the generated
    // indentation and layout stay untouched.
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut normalized = String::with_capacity(text.len() + 1);
    for line in text.trim_end().lines() {
        normalized.push_str(line.trim_end());
        normalized.push('\n');
    }
    std::fs::write(path, normalized)
        .map_err(|error| format!("cannot normalize {}: {error}", path.display()))
}

fn fail(error: &str) -> ExitCode {
    eprintln!("stache: {error}");
    ExitCode::FAILURE
}
