//! Environment file parsing utilities.
//!
//! This module provides functionality to load API keys and other secrets
//! from environment files (`.env` format) instead of storing them directly
//! in the configuration file.
//!
//! Uses the `dotenvy` crate for robust `.env` file parsing.

use std::collections::HashMap;
use std::path::Path;

use crate::utils::path::expand_and_resolve;

/// Environment variable names for API keys.
pub mod keys {
    /// Visual Crossing Weather API key.
    pub const VISUAL_CROSSING_API_KEY: &str = "VISUAL_CROSSING_API_KEY";
}

/// Parses an environment file and returns a map of key-value pairs.
///
/// Uses the `dotenvy` crate for robust parsing that handles:
/// - Comments (lines starting with `#`)
/// - Empty lines
/// - Quoted values (single and double quotes)
/// - Escaped characters
/// - Multiline values
/// - Variable expansion (disabled for security)
///
/// # Arguments
///
/// * `path` - Path to the environment file
///
/// # Returns
///
/// A `HashMap` of environment variable names to their values.
/// Returns an empty map if the file doesn't exist or can't be read.
#[must_use]
pub fn parse_env_file(path: &Path) -> HashMap<String, String> {
    match dotenvy::from_path_iter(path) {
        Ok(iter) => iter.filter_map(Result::ok).collect(),
        Err(err) => {
            if path.exists() {
                eprintln!(
                    "stache: warning: failed to read env file {}: {err}",
                    path.display()
                );
            }
            HashMap::new()
        }
    }
}

/// Loads API keys from an environment file.
///
/// # Arguments
///
/// * `api_keys_path` - Path to the env file (can be relative or absolute)
/// * `config_dir` - Directory containing the config file (for resolving relative paths)
///
/// # Returns
///
/// A struct containing the loaded API keys.
#[must_use]
pub fn load_api_keys(api_keys_path: &str, config_dir: &Path) -> ApiKeys {
    if api_keys_path.is_empty() {
        return ApiKeys::default();
    }

    let resolved_path = expand_and_resolve(api_keys_path, config_dir);
    let env_vars = parse_env_file(&resolved_path);

    ApiKeys {
        visual_crossing_api_key: env_vars.get(keys::VISUAL_CROSSING_API_KEY).cloned(),
    }
}

/// Container for API keys loaded from an environment file.
#[derive(Debug, Clone, Default)]
pub struct ApiKeys {
    /// Visual Crossing Weather API key.
    pub visual_crossing_api_key: Option<String>,
}

impl ApiKeys {
    /// Returns the Visual Crossing API key, or an empty string if not set.
    #[must_use]
    pub fn visual_crossing_api_key(&self) -> &str {
        self.visual_crossing_api_key.as_deref().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_parse_env_file_basic() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "KEY1=value1").unwrap();
        writeln!(file, "KEY2=value2").unwrap();

        let result = parse_env_file(&env_path);
        assert_eq!(result.get("KEY1"), Some(&"value1".to_string()));
        assert_eq!(result.get("KEY2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_parse_env_file_with_comments() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "# This is a comment").unwrap();
        writeln!(file, "KEY1=value1").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "# Another comment").unwrap();
        writeln!(file, "KEY2=value2").unwrap();

        let result = parse_env_file(&env_path);
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("KEY1"), Some(&"value1".to_string()));
        assert_eq!(result.get("KEY2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_parse_env_file_with_quotes() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "KEY1=\"quoted value\"").unwrap();
        writeln!(file, "KEY2='single quoted'").unwrap();
        writeln!(file, "KEY3=unquoted").unwrap();

        let result = parse_env_file(&env_path);
        assert_eq!(result.get("KEY1"), Some(&"quoted value".to_string()));
        assert_eq!(result.get("KEY2"), Some(&"single quoted".to_string()));
        assert_eq!(result.get("KEY3"), Some(&"unquoted".to_string()));
    }

    #[test]
    fn test_parse_env_file_nonexistent() {
        let result = parse_env_file(Path::new("/nonexistent/path/.env"));
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_api_keys_empty_path() {
        let config_dir = PathBuf::from("/config");
        let keys = load_api_keys("", &config_dir);
        assert!(keys.visual_crossing_api_key.is_none());
    }

    #[test]
    fn test_load_api_keys_with_file() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "VISUAL_CROSSING_API_KEY=test_api_key_123").unwrap();

        let keys = load_api_keys(".env", temp_dir.path());
        assert_eq!(keys.visual_crossing_api_key(), "test_api_key_123");
    }

    #[test]
    fn test_api_keys_default() {
        let keys = ApiKeys::default();
        assert!(keys.visual_crossing_api_key.is_none());
        assert_eq!(keys.visual_crossing_api_key(), "");
    }

    #[test]
    fn test_parse_env_file_value_with_equals() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "KEY=value=with=equals").unwrap();

        let result = parse_env_file(&env_path);
        assert_eq!(result.get("KEY"), Some(&"value=with=equals".to_string()));
    }

    #[test]
    fn test_parse_env_file_multiline_value() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, "KEY=\"line1\nline2\"").unwrap();

        let result = parse_env_file(&env_path);
        assert_eq!(result.get("KEY"), Some(&"line1\nline2".to_string()));
    }

    #[test]
    fn test_parse_env_file_escaped_characters() {
        let temp_dir = TempDir::new().unwrap();
        let env_path = temp_dir.path().join(".env");

        let mut file = fs::File::create(&env_path).unwrap();
        writeln!(file, r#"KEY="value with \"quotes\"""#).unwrap();

        let result = parse_env_file(&env_path);
        assert_eq!(result.get("KEY"), Some(&"value with \"quotes\"".to_string()));
    }
}
