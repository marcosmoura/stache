//! `panel-native-check`: the opt-in live gate for the non-activating panel.
//!
//! Never built or run by default (`required-features = ["panel-native-check"]`)
//! and never invoked by the automated suite. See `docs/guides/testing.md`.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use stache_app::panel_check::{USAGE, parse};
use stache_sdk::ConfigPaths;
use stache_sdk::config::ConfigEnvironment;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&arguments) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("panel-native-check: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let config = match ConfigPaths::resolve(None, &ConfigEnvironment::from_env()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("panel-native-check: {error}");
            return ExitCode::FAILURE;
        }
    };
    match stache_app::run_panel_check(config, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("panel-native-check: {error}");
            ExitCode::FAILURE
        }
    }
}
