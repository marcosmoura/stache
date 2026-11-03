//! Developer-only native smoke harness. Built with `--features smoke`.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use stache_sdk::ConfigPaths;
use stache_sdk::config::ConfigEnvironment;

fn main() -> ExitCode {
    if std::env::var_os(stache_app::smoke::SECOND_ENV).is_some() {
        return match stache_app::smoke::second_instance_probe() {
            Ok(true) => {
                println!("smoke: ok: second instance saw the held lock");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                eprintln!("smoke: FAIL: second instance acquired the lock");
                ExitCode::FAILURE
            }
            Err(error) => {
                eprintln!("smoke: FAIL: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let config = match ConfigPaths::resolve(None, &ConfigEnvironment::from_env()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("stache: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Write the disabled proxyAudio baseline before the host starts so a
    // generic host/WebKit smoke run can never select or change an audio device.
    if let Err(error) = stache_app::smoke::prepare_disabled_config(&config) {
        eprintln!("stache: {error}");
        return ExitCode::FAILURE;
    }
    match stache_app::run_smoke(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("stache: {error}");
            ExitCode::FAILURE
        }
    }
}
