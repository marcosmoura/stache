//! Bundle detection for mode selection after clap validation.
//!
//! The host selects desktop for a valid bundled invocation, `--desktop`, or
//! no parsed subcommand. Argument parsing belongs solely to `commands::compose`.

use std::path::Path;

/// Whether the executable is inside an application bundle (desktop precedence).
#[must_use]
pub fn is_app_bundle(current_exe: &Path) -> bool {
    current_exe.to_string_lossy().contains(".app/Contents/MacOS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_detection_preserves_legacy_executable_precedence() {
        assert!(is_app_bundle(Path::new(
            "/Applications/Stache Next.app/Contents/MacOS/stache"
        )));
        assert!(!is_app_bundle(Path::new("/usr/local/bin/stache")));
    }
}
