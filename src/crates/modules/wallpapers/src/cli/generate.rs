//! `stache wallpaper list` and `stache wallpaper generate-all`.
//!
//! Both commands are local read/cache work using the current configuration
//! file, irrespective of `enabled`. They never touch the control socket and
//! never call a native setter. `list` reads metadata only; `generate-all`
//! takes one guarded native geometry snapshot and prepares every
//! `display × image` output through the bounded batch pipeline.
//!
//! The result layout, wording and exit contract follow the legacy
//! `print_generation_header`, `print_generation_results`, `print_single_result`
//! and `spawn_progress_reporter`: per-screen groups with zero-based headings,
//! `●` cache hits, `✓` fresh outputs and `✗` failures, then a counted summary.
//! A completed batch with per-image failures still exits 0. Unlike legacy,
//! ANSI sequences, the spinner and cursor control are written only when stdout
//! is a terminal.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use clap::ArgMatches;
use stache_macos::{DesktopOperation, DisplayId, SystemDesktopClock};
use stache_sdk::{AppPaths, CancellationToken};

use super::{WallpapersCliEnv, config_paths, load_config, wallpaper_error};
use crate::apply::PrepareError;
use crate::batch::{BatchDeps, BatchItem, BatchObserver, BatchOutput, prepare_batch_with};
use crate::limits::check_blur;
use crate::processing::Effects;
use crate::selection::{ScreenTarget, SelectedTarget, resolve_targets};
use crate::sources::{SourceContext, SourcePool, resolve_pool};

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const CLEAR_LINE: &str = "\x1b[2K\r";
const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Terminal-only styling: every code is empty unless stdout is a terminal.
#[derive(Clone, Copy)]
struct Style {
    tty: bool,
}

impl Style {
    const fn code(self, code: &'static str) -> &'static str { if self.tty { code } else { "" } }
}

fn source_context(
    env: &WallpapersCliEnv,
    paths: &stache_sdk::ConfigPaths,
) -> Result<SourceContext, String> {
    let directory = paths.directory();
    let directory = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        (env.current_dir)()
            .map_err(|error| {
                format!(
                    "cannot determine the current directory: {}",
                    super::adapters::quoted(&error.to_string())
                )
            })?
            .join(directory)
    };
    Ok(SourceContext::new(directory, env.config.home.clone()))
}

fn pool_for(env: &WallpapersCliEnv, matches: &ArgMatches) -> Result<(SourcePool, Effects), String> {
    let paths = config_paths(env, matches)?;
    let config = load_config(&paths)?;
    let context = source_context(env, &paths)?;
    let pool = resolve_pool(&config, &context).map_err(|error| error.to_string())?;
    Ok((pool, Effects {
        radius: config.radius,
        blur: config.blur,
    }))
}

pub(super) fn list(
    env: &WallpapersCliEnv,
    matches: &ArgMatches,
    ansi: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (pool, _) = match pool_for(env, matches) {
        Ok(value) => value,
        Err(message) => return wallpaper_error(err, &message),
    };
    if pool.is_empty() {
        let _ = writeln!(out, "No wallpapers found.");
        return ExitCode::SUCCESS;
    }
    let paths: Vec<String> = pool.images().iter().map(|path| path.display().to_string()).collect();
    match serde_json::to_string_pretty(&paths) {
        Ok(json) => {
            let _ = writeln!(out, "{}", (env.highlight)(&json, ansi));
            ExitCode::SUCCESS
        }
        Err(error) => wallpaper_error(err, &format!("JSON serialization error: {error}")),
    }
}

/// The recorded result for one `display × image` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowOutcome {
    /// A validated cache entry already existed.
    Cached,
    /// A fresh output was published under this cache file name.
    Generated(String),
    /// Processing failed with this message.
    Failed(String),
}

/// One printed result line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub screen: usize,
    pub name: String,
    pub outcome: RowOutcome,
}

/// Counts and timing for the summary line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub screens: usize,
    pub generated: usize,
    pub cached: usize,
    pub errors: usize,
    pub elapsed: Duration,
}

fn header(style: Style, screens: usize, wallpapers: usize, total: usize) -> String {
    let mut text = format!(
        "{}{}Generating wallpapers{} for {screens} screen(s), {wallpapers} wallpaper(s) each\n",
        style.code(BOLD),
        style.code(CYAN),
        style.code(RESET),
    );
    let _ = writeln!(
        text,
        "{}Total: {total} images to process{}\n",
        style.code(DIM),
        style.code(RESET)
    );
    text.push_str(style.code(HIDE_CURSOR));
    text
}

fn progress(style: Style, frame: usize, done: usize, total: usize) -> String {
    let percent = (done * 100) / total.max(1);
    format!(
        "{}{}{} Processing... {done}/{total} ({percent}%){}",
        style.code(CLEAR_LINE),
        style.code(CYAN),
        SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
        style.code(RESET),
    )
}

fn results(style: Style, rows: &[Row], summary: &Summary) -> String {
    let mut text = String::new();
    text.push_str(style.code(CLEAR_LINE));
    text.push_str(style.code(SHOW_CURSOR));
    for screen in 0..summary.screens {
        let _ = writeln!(text, "{}Screen {screen}:{}", style.code(BOLD), style.code(RESET));
        for row in rows.iter().filter(|row| row.screen == screen) {
            text.push_str(&row_line(style, row));
        }
        text.push('\n');
    }
    let _ = writeln!(
        text,
        "{}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{}",
        style.code(DIM),
        style.code(RESET)
    );
    let _ = write!(text, "{}Done!{} ", style.code(BOLD), style.code(RESET));
    if summary.generated > 0 {
        let _ = write!(
            text,
            "{}Generated: {}{} ",
            style.code(GREEN),
            summary.generated,
            style.code(RESET)
        );
    }
    if summary.cached > 0 {
        let _ = write!(
            text,
            "{}Cached: {}{} ",
            style.code(YELLOW),
            summary.cached,
            style.code(RESET)
        );
    }
    if summary.errors > 0 {
        let _ = write!(
            text,
            "{}Errors: {}{} ",
            style.code(RED),
            summary.errors,
            style.code(RESET)
        );
    }
    let _ = writeln!(
        text,
        "{}({:.2}s){}",
        style.code(DIM),
        summary.elapsed.as_secs_f64(),
        style.code(RESET)
    );
    text
}

fn row_line(style: Style, row: &Row) -> String {
    match &row.outcome {
        RowOutcome::Cached => format!(
            "  {}●{} {} {}(cached){}\n",
            style.code(DIM),
            style.code(RESET),
            row.name,
            style.code(DIM),
            style.code(RESET)
        ),
        RowOutcome::Generated(cached) => format!(
            "  {}✓{} {} -> {cached}\n",
            style.code(GREEN),
            style.code(RESET),
            row.name
        ),
        RowOutcome::Failed(message) => format!(
            "  {}✗{} {} -> {message}\n",
            style.code(RED),
            style.code(RESET),
            row.name
        ),
    }
}

/// A writer that remembers its first failure and ignores later writes.
struct Sink<'a> {
    out: &'a mut dyn Write,
    failure: Option<io::Error>,
}

impl Sink<'_> {
    fn put(&mut self, text: &str) {
        if self.failure.is_none() {
            let written = self.out.write_all(text.as_bytes()).and_then(|()| self.out.flush());
            if let Err(error) = written {
                self.failure = Some(error);
            }
        }
    }

    fn restore(&mut self, style: Style) {
        // Best effort, even after a failed write: leave the terminal usable.
        let _ = self
            .out
            .write_all(format!("{}{}", style.code(CLEAR_LINE), style.code(SHOW_CURSOR)).as_bytes());
        let _ = self.out.flush();
    }
}

struct Counter(Arc<AtomicUsize>, Arc<dyn BatchObserver + Send + Sync>);

impl BatchObserver for Counter {
    fn item_started(&self) { self.1.item_started(); }

    fn item_finished(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
        self.1.item_finished();
    }
}

enum Done {
    Output(BatchOutput),
    Failed(String),
}

type Resolved = HashMap<(DisplayId, PathBuf), Done>;

struct Batched {
    resolved: Resolved,
    cancelled: bool,
}

/// Runs the batch in rounds so one failed image never hides the others.
///
/// `prepare_batch` stops pulling work at its first failure. Each round records
/// that failure and every output it finished, then the unresolved items run
/// again, so every `display × image` ends as an output or a typed failure
/// unless cancellation stops the run.
fn run_rounds(
    tasks: &[(SelectedTarget, PathBuf)],
    effects: Effects,
    app: &AppPaths,
    cancel: &CancellationToken,
    counter: &Counter,
) -> Result<Batched, PrepareError> {
    let mut resolved: Resolved = HashMap::new();
    let mut pending: Vec<usize> = (0..tasks.len()).collect();
    let mut cancelled = false;
    while !pending.is_empty() {
        let items = pending.iter().map(|index| {
            let (target, source) = &tasks[*index];
            BatchItem {
                source: source.clone(),
                targets: vec![*target],
                effects,
            }
        });
        let report = prepare_batch_with(items, &BatchDeps { paths: app }, cancel, counter)?;
        for output in report.outputs {
            resolved.insert((output.display, output.source.clone()), Done::Output(output));
        }
        for failure in report.failures {
            let message = failure.error.to_string();
            match failure.display {
                Some(display) => {
                    resolved.insert((display, failure.source), Done::Failed(message));
                }
                None => {
                    // The source itself failed before any target.
                    for (target, source) in tasks.iter().filter(|(_, s)| *s == failure.source) {
                        resolved
                            .entry((target.id(), source.clone()))
                            .or_insert_with(|| Done::Failed(message.clone()));
                    }
                }
            }
        }
        if report.cancelled || cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        let before = pending.len();
        pending.retain(|index| {
            let (target, source) = &tasks[*index];
            !resolved.contains_key(&(target.id(), source.clone()))
        });
        if pending.len() == before {
            break;
        }
    }
    Ok(Batched { resolved, cancelled })
}

/// Marks the worker finished and wakes the progress loop when dropped.
struct FinishGuard<'a>(&'a (Mutex<bool>, Condvar));

impl Drop for FinishGuard<'_> {
    fn drop(&mut self) {
        *lock(&self.0.0) = true;
        self.0.1.notify_all();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[allow(clippy::too_many_lines)]
pub(super) fn generate_all(
    env: &WallpapersCliEnv,
    matches: &ArgMatches,
    ansi: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let (pool, effects) = match pool_for(env, matches) {
        Ok(value) => value,
        Err(message) => return wallpaper_error(err, &message),
    };
    if pool.is_empty() {
        let _ = writeln!(out, "No wallpapers to generate.");
        return ExitCode::SUCCESS;
    }
    if let Err(error) = check_blur(effects.blur) {
        return wallpaper_error(err, &error.to_string());
    }
    let app = match AppPaths::resolve(&env.app) {
        Ok(app) => app,
        Err(error) => return super::fail(err, &error.to_string()),
    };

    let guard = (env.cancel)();
    let cancel = guard.token().clone();
    let current: Arc<dyn Fn() -> bool + Send + Sync> = {
        let cancel = cancel.clone();
        Arc::new(move || !cancel.is_cancelled())
    };
    let operation =
        DesktopOperation::new(cancel.clone(), current).with_clock(Arc::new(SystemDesktopClock));
    let snapshot = match env.desktop.snapshot_guarded(&operation) {
        Ok(snapshot) => snapshot,
        Err(error) => return wallpaper_error(err, &error.to_string()),
    };
    let selected = match resolve_targets(&snapshot, &ScreenTarget::All) {
        Ok(selected) => selected,
        Err(error) => return wallpaper_error(err, &error.to_string()),
    };

    let style = Style { tty: ansi };
    let screens = selected.len();
    let wallpapers = pool.len();
    let total = screens * wallpapers;
    let mut sink = Sink { out, failure: None };
    sink.put(&header(style, screens, wallpapers, total));
    if sink.failure.is_some() {
        return write_failure(&mut sink, style, err);
    }

    // One unit of work per distinct `display × source`; a duplicate pool entry
    // reuses the first result.
    let mut tasks: Vec<(SelectedTarget, PathBuf)> = Vec::new();
    for target in &selected {
        for image in pool.images() {
            if !tasks.iter().any(|(t, s)| t.id() == target.id() && s == image) {
                tasks.push((*target, image.clone()));
            }
        }
    }

    let start = env.clock.now();
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = Counter(Arc::clone(&completed), Arc::clone(&env.observer));
    let finished = (Mutex::new(false), Condvar::new());
    let batch = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            // Fires on unwind too, so a panicking worker never leaves the TTY
            // loop spinning. The join below turns the panic into a top-level
            // failure (exit 1) after the cursor is restored; it is not resumed. Unwind-
            // enabled builds only: a release build aborts.
            let _done = FinishGuard(&finished);
            run_rounds(&tasks, effects, &app, &cancel, &counter)
        });
        if style.tty {
            let mut frame = 0;
            loop {
                if *lock(&finished.0) {
                    break;
                }
                sink.put(&progress(style, frame, completed.load(Ordering::Relaxed), total));
                if sink.failure.is_some() {
                    // Stop admission; the worker joins below.
                    cancel.cancel();
                }
                frame += 1;
                let _ = finished.1.wait_timeout_while(
                    lock(&finished.0),
                    env.progress_interval,
                    |done| !*done,
                );
            }
        }
        worker.join()
    });
    let end = env.clock.now();

    if sink.failure.is_some() {
        return write_failure(&mut sink, style, err);
    }
    let batched = match batch {
        Ok(Ok(batched)) => batched,
        Ok(Err(error)) => {
            sink.restore(style);
            return wallpaper_error(err, &error.to_string());
        }
        Err(_) => {
            sink.restore(style);
            return wallpaper_error(err, "wallpaper generation stopped unexpectedly");
        }
    };
    if batched.cancelled {
        sink.restore(style);
        return wallpaper_error(err, "wallpaper generation was cancelled");
    }

    let mut rows = Vec::with_capacity(total);
    for (screen, target) in selected.iter().enumerate() {
        for image in pool.images() {
            let name = image
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("unknown")
                .to_owned();
            let outcome = match batched.resolved.get(&(target.id(), image.clone())) {
                Some(Done::Output(output)) if output.cache_hit => RowOutcome::Cached,
                Some(Done::Output(output)) => RowOutcome::Generated(
                    output
                        .image
                        .file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or("cached")
                        .to_owned(),
                ),
                Some(Done::Failed(message)) => RowOutcome::Failed(message.clone()),
                None => RowOutcome::Failed("the image was not processed".to_owned()),
            };
            rows.push(Row { screen, name, outcome });
        }
    }
    let cached = rows.iter().filter(|row| row.outcome == RowOutcome::Cached).count();
    let errors = rows.iter().filter(|row| matches!(row.outcome, RowOutcome::Failed(_))).count();
    let summary = Summary {
        screens,
        generated: total.saturating_sub(cached + errors),
        cached,
        errors,
        elapsed: end.saturating_duration_since(start),
    };
    sink.put(&results(style, &rows, &summary));
    if sink.failure.is_some() {
        return write_failure(&mut sink, style, err);
    }
    ExitCode::SUCCESS
}

fn write_failure(sink: &mut Sink<'_>, style: Style, err: &mut dyn Write) -> ExitCode {
    let message = sink
        .failure
        .take()
        .map_or_else(|| "unknown error".to_owned(), |error| error.to_string());
    sink.restore(style);
    wallpaper_error(err, &format!("cannot write the generation output: {message}"))
}

#[cfg(test)]
pub mod render {
    //! Pure renderers, exposed to the golden tests.
    use super::{Row, Style, Summary, header, progress, results};

    pub fn header_text(tty: bool, screens: usize, wallpapers: usize) -> String {
        header(Style { tty }, screens, wallpapers, screens * wallpapers)
    }

    pub fn progress_text(tty: bool, frame: usize, done: usize, total: usize) -> String {
        progress(Style { tty }, frame, done, total)
    }

    pub fn results_text(tty: bool, rows: &[Row], summary: &Summary) -> String {
        results(Style { tty }, rows, summary)
    }
}
