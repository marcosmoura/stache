//! Cache CLI commands.
//!
//! This module contains the cache subcommands for managing the application's cache.

use clap::Subcommand;

use crate::cache;
use crate::error::StacheError;

/// Cache subcommands for managing the application's cache.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum CacheCommands {
    /// Clear the application's cache directory.
    ///
    /// Removes all cached files including processed wallpapers and media artwork.
    /// This can help resolve issues with stale data or free up disk space.
    #[command(after_long_help = r#"Examples:
  stache cache clear   # Clear all cached data"#)]
    Clear,

    /// Show the cache directory location.
    ///
    /// Displays the path to the application's cache directory.
    #[command(after_long_help = r#"Examples:
  stache cache path    # Print the cache directory path"#)]
    Path,
}

/// Execute cache subcommands.
pub fn execute(cmd: &CacheCommands) -> Result<(), StacheError> {
    match cmd {
        CacheCommands::Clear => {
            let cache_dir = cache::get_cache_dir();
            if !cache_dir.exists() {
                println!("Cache directory does not exist. Nothing to clear.");
                return Ok(());
            }

            match cache::clear_cache() {
                Ok(bytes_freed) => {
                    let formatted = cache::format_bytes(bytes_freed);
                    println!("Cache cleared successfully. Freed {formatted}.");
                }
                Err(err) => {
                    return Err(StacheError::CacheError(format!("Failed to clear cache: {err}")));
                }
            }
        }
        CacheCommands::Path => {
            let cache_dir = cache::get_cache_dir();
            let cache_dir_display = cache_dir.display();
            println!("{cache_dir_display}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: CacheCommands,
    }

    #[test]
    fn test_cache_clear_parse() {
        let cli = TestCli::try_parse_from(["test", "clear"]).unwrap();
        assert!(matches!(cli.command, CacheCommands::Clear));
    }

    #[test]
    fn test_cache_path_parse() {
        let cli = TestCli::try_parse_from(["test", "path"]).unwrap();
        assert!(matches!(cli.command, CacheCommands::Path));
    }
}
