//! `stache wallpaper set`: argument validation, routing and the standalone
//! application.
//!
//! Order of work: (1) both/neither validation, with no configuration, source,
//! manager or socket access; (2) client-side PATH resolution; (3) one
//! `wallpapers.set` request; (4) one standalone application only after proven
//! absence of the host.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::ArgMatches;
use stache_macos::{DesktopOperation, SystemDesktopClock};
use stache_sdk::ipc::{ClientError, Response};
use stache_sdk::{AppPaths, ConfigPaths};

use super::adapters::{SUCCESS_LINE, render_outcome, response_uncertainty_body, wire_error_body};
use super::token::{TokenResolution, resolve_token};
use super::{WallpapersCliEnv, config_paths, fail, load_config, set_request, wallpaper_error};
use crate::WallpapersConfig;
use crate::apply::PlannedTarget;
use crate::limits::check_blur;
use crate::processing::Effects;
use crate::runtime::{
    WallpaperScreen, WallpaperSelection, WallpapersOutcomeCause, WallpapersOutcomeStatus,
    WallpapersSetOutcome, WallpapersSetParams, prepare_failure,
};
use crate::selection::{SelectionError, resolve_targets};
use crate::sources::{
    SourceContext, SourceError, SourcePool, find_pool_entry, resolve_explicit_file, resolve_pool,
};

const BOTH: &str = "Cannot specify both <path> and --random. Use one or the other.";
const NEITHER: &str = "Either <path> or --random must be specified.";

pub(super) fn run(
    env: &WallpapersCliEnv,
    matches: &ArgMatches,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let path = matches.get_one::<String>("path");
    let random = matches.get_flag("random");
    match (path, random) {
        (Some(_), true) => return fail(err, BOTH),
        (None, false) => return fail(err, NEITHER),
        _ => {}
    }
    let screen = matches
        .get_one::<WallpaperScreen>("screen")
        .cloned()
        .unwrap_or(WallpaperScreen::All);

    let config = match config_paths(env, matches) {
        Ok(config) => config,
        Err(message) => return fail(err, &message),
    };
    let app = match AppPaths::resolve(&env.app) {
        Ok(app) => app,
        Err(error) => return fail(err, &error.to_string()),
    };
    let socket = match app.socket_path() {
        Ok(socket) => socket,
        Err(error) => return fail(err, &error.to_string()),
    };
    let cwd = match (env.current_dir)() {
        Ok(cwd) => cwd,
        Err(error) => {
            return wallpaper_error(
                err,
                &format!(
                    "cannot determine the current directory: {}",
                    super::adapters::quoted(&error.to_string())
                ),
            );
        }
    };

    let selection = match path {
        None => WallpaperSelection::Random,
        Some(token) => match resolve_token(token, &cwd, env.config.home.as_deref()) {
            Ok(TokenResolution::Name(name)) => WallpaperSelection::Name { name },
            Ok(TokenResolution::File(file)) => match file.to_str() {
                Some(file) => WallpaperSelection::File { path: file.to_owned() },
                None => {
                    return wallpaper_error(
                        err,
                        &format!(
                            "Invalid wallpaper path {}: the path is not valid UTF-8",
                            super::adapters::quoted(&file.to_string_lossy())
                        ),
                    );
                }
            },
            Err(error) => return wallpaper_error(err, &error.to_string()),
        },
    };
    let directory = absolute(config.directory(), &cwd);
    let Some(config_directory) = directory.to_str().map(str::to_owned) else {
        return wallpaper_error(
            err,
            &format!(
                "the configuration directory {} is not valid UTF-8",
                super::adapters::quoted(&directory.to_string_lossy())
            ),
        );
    };
    let params = WallpapersSetParams {
        config_directory,
        selection,
        screen,
    };
    let request = match set_request(&params) {
        Ok(request) => request,
        Err(error) => return fail(err, &format!("cannot serialize the request: {error}")),
    };

    // Exactly one mutation attempt reaches the transport.
    match env.transport.send(&socket, &request) {
        Ok(Response::Ok { data }) => match serde_json::from_value::<WallpapersSetOutcome>(data) {
            Ok(outcome) => finish(&outcome, out, err),
            Err(_) => wallpaper_error(err, &response_uncertainty_body()),
        },
        Ok(Response::Error { error }) => match wire_error_body(&error) {
            Some(body) => wallpaper_error(err, &body),
            None => wallpaper_error(err, &response_uncertainty_body()),
        },
        Err(ClientError::NotRunning) => match standalone(env, &params, &config, &directory, &app) {
            Ok(outcome) => finish(&outcome, out, err),
            Err(message) => wallpaper_error(err, &message),
        },
        Err(_) => wallpaper_error(err, &response_uncertainty_body()),
    }
}

fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn finish(outcome: &WallpapersSetOutcome, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match render_outcome(outcome) {
        Ok(()) => {
            let _ = writeln!(out, "{SUCCESS_LINE}");
            ExitCode::SUCCESS
        }
        Err(body) => wallpaper_error(err, &body),
    }
}

/// Applies once on the process main thread, without a host.
///
/// Reads the current configuration file (a missing file means defaults; a
/// malformed one is an error). Starts no registry, module, tray, window,
/// watcher, timer, listener or startup command.
fn standalone(
    env: &WallpapersCliEnv,
    params: &WallpapersSetParams,
    config_paths: &ConfigPaths,
    directory: &Path,
    app: &AppPaths,
) -> Result<WallpapersSetOutcome, String> {
    let config = load_config(config_paths)?;
    let context = SourceContext::new(directory, env.config.home.clone());
    let guard = (env.cancel)();
    Ok(apply_local(env, params, &config, &context, app, guard.token()))
}

const fn refused(
    status: WallpapersOutcomeStatus,
    cause: WallpapersOutcomeCause,
) -> WallpapersSetOutcome {
    WallpapersSetOutcome::refused(status, cause)
}

fn source_failure(error: &SourceError) -> WallpapersSetOutcome {
    WallpapersSetOutcome::source_failure(error)
}

const fn invalid(message: String) -> WallpapersSetOutcome {
    refused(
        WallpapersOutcomeStatus::Failure,
        WallpapersOutcomeCause::InvalidSelection { message },
    )
}

enum Chosen {
    File(PathBuf),
    Entry(SourcePool, usize),
    Random(SourcePool),
}

#[allow(clippy::result_large_err)]
fn choose(
    selection: &WallpaperSelection,
    config: &WallpapersConfig,
    context: &SourceContext,
) -> Result<Chosen, WallpapersSetOutcome> {
    match selection {
        WallpaperSelection::File { path } => {
            let path = Path::new(path);
            if !path.is_absolute() {
                return Err(invalid(
                    "the explicit wallpaper path must be absolute".to_owned(),
                ));
            }
            resolve_explicit_file(path, Path::new("/"))
                .map(Chosen::File)
                .map_err(|error| source_failure(&error))
        }
        WallpaperSelection::Name { name } => {
            let pool = resolve_pool(config, context).map_err(|error| source_failure(&error))?;
            match find_pool_entry(&pool, name) {
                Some(index) => Ok(Chosen::Entry(pool, index)),
                None => Err(invalid(format!(
                    "no wallpaper named `{name}` is in the configured pool of {} entries",
                    pool.len()
                ))),
            }
        }
        WallpaperSelection::Collection { index } => {
            let pool = resolve_pool(config, context).map_err(|error| source_failure(&error))?;
            let position = usize::try_from(*index).ok().filter(|position| *position < pool.len());
            match position {
                Some(position) => Ok(Chosen::Entry(pool, position)),
                None => Err(invalid(format!(
                    "collection index {index} is out of range for a pool of {} entries",
                    pool.len()
                ))),
            }
        }
        WallpaperSelection::Random => {
            let pool = resolve_pool(config, context).map_err(|error| source_failure(&error))?;
            pool.require_non_empty().map_err(|error| source_failure(&error))?;
            Ok(Chosen::Random(pool))
        }
    }
}

/// Snapshot, range check, prepare and apply on the calling (main) thread.
fn apply_local(
    env: &WallpapersCliEnv,
    params: &WallpapersSetParams,
    config: &WallpapersConfig,
    context: &SourceContext,
    app: &AppPaths,
    cancel: &stache_sdk::CancellationToken,
) -> WallpapersSetOutcome {
    let effects = Effects {
        radius: config.radius,
        blur: config.blur,
    };
    if let Err(error) = check_blur(effects.blur) {
        return refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::Processing { message: error.to_string() },
        );
    }
    let chosen = match choose(&params.selection, config, context) {
        Ok(chosen) => chosen,
        Err(outcome) => return outcome,
    };

    let current: Arc<dyn Fn() -> bool + Send + Sync> = {
        let cancel = cancel.clone();
        Arc::new(move || !cancel.is_cancelled())
    };
    let clock = Arc::new(SystemDesktopClock);
    let operation =
        DesktopOperation::new(cancel.clone(), Arc::clone(&current)).with_clock(clock.clone());
    let snapshot = match env.desktop.snapshot_guarded(&operation) {
        Ok(snapshot) => snapshot,
        Err(error) => return WallpapersSetOutcome::native_failure(&error),
    };
    // The range check precedes any decode or cache write.
    let selected = match resolve_targets(&snapshot, &params.screen.as_target()) {
        Ok(selected) => selected,
        Err(error) => return invalid(selection_text(&error)),
    };
    let plan: Vec<PlannedTarget> = selected
        .iter()
        .map(|target| {
            let source = match &chosen {
                Chosen::File(path) => path.clone(),
                Chosen::Entry(pool, index) => pool.images()[*index].clone(),
                Chosen::Random(pool) => pool.images()[env.random.index(pool.len())].clone(),
            };
            PlannedTarget {
                target: *target,
                source,
                effects,
            }
        })
        .collect();
    let order: Vec<_> = selected.iter().map(|target| target.entry.id).collect();

    let prepared = match env.processor.prepare(&plan, app, cancel) {
        Ok(prepared) => prepared,
        Err(error) => return prepare_failure(&error),
    };
    let applied = match prepared.apply_guarded(env.desktop.as_ref(), cancel, current, None, clock) {
        Ok(applied) => applied,
        Err(error) => {
            return refused(
                WallpapersOutcomeStatus::Failure,
                WallpapersOutcomeCause::Processing { message: error.to_string() },
            );
        }
    };
    let mut outcome = WallpapersSetOutcome::from_apply_ordered(&applied, &order, None);
    if applied.applied.is_empty()
        && applied.failed.is_empty()
        && applied.unknown.is_empty()
        && !applied.not_attempted.is_empty()
    {
        outcome.status = WallpapersOutcomeStatus::Cancelled;
        outcome.cause = Some(WallpapersOutcomeCause::Cancelled);
    }
    outcome
}

fn selection_text(error: &SelectionError) -> String { error.to_string() }
