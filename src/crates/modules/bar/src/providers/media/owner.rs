//! The media worker: spawn, read, restart and stop one adapter child.
//!
//! [`run`] is the body of the `stache-bar-media` thread of one
//! [`ProviderMonitor`](crate::providers::ProviderMonitor). A second thread,
//! `stache-bar-media-art`, decodes artwork so a large image never delays line
//! reading, parsing or cancellation. The decode slot holds one job: the newest
//! artwork wins and an older pending decode is dropped.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use stache_sdk::CancellationToken;
use stache_sdk::commands::{
    PreparedCommand, StopOutcome, StreamEvent, StreamLimits, StreamOpenError,
};

use super::artwork::{self, ArtworkStore};
use super::parse::{ArtworkInput, Parsed, parse_line};
use super::{
    ArtPlan, GAVE_UP_REASON, GIVE_UP_AFTER_UNAVAILABLE, MediaParts, MediaPaths, MediaRunState,
    MediaStatus, PublishMedia, Publisher, STDERR_PUBLISHED_CHARS, STDERR_TAIL, UNAVAILABLE_AFTER,
};

/// How many artwork outcomes are remembered by fingerprint.
const RECENT: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A cheap fingerprint of the player's artwork, to skip repeated decodes.
fn fingerprint(input: &ArtworkInput) -> u64 {
    let mut hasher = DefaultHasher::new();
    input.mime.hash(&mut hasher);
    input.data.hash(&mut hasher);
    hasher.finish()
}

#[derive(Default)]
struct Jobs {
    pending: Option<(u64, ArtworkInput)>,
    closed: bool,
}

/// The artwork decode slot and the memory of recent outcomes.
#[derive(Default)]
struct Arts {
    jobs: Mutex<Jobs>,
    wake: Condvar,
    recent: Mutex<Vec<(u64, Option<String>)>>,
    inflight: Mutex<Option<u64>>,
}

impl Arts {
    #[allow(clippy::option_option)] // `None` unknown, `Some(None)` known to be unusable
    fn recall(&self, key: u64) -> Option<Option<String>> {
        lock(&self.recent)
            .iter()
            .find(|(held, _)| *held == key)
            .map(|(_, id)| id.clone())
    }

    fn remember(&self, key: u64, id: Option<String>) {
        let mut recent = lock(&self.recent);
        recent.retain(|(held, _)| *held != key);
        recent.push((key, id));
        if recent.len() > RECENT {
            recent.remove(0);
        }
    }

    fn submit(&self, key: u64, input: ArtworkInput) {
        *lock(&self.inflight) = Some(key);
        let mut jobs = lock(&self.jobs);
        if !jobs.closed {
            jobs.pending = Some((key, input));
        }
        drop(jobs);
        self.wake.notify_all();
    }

    fn is_inflight(&self, key: u64) -> bool { *lock(&self.inflight) == Some(key) }

    fn close(&self) {
        let mut jobs = lock(&self.jobs);
        jobs.closed = true;
        jobs.pending = None;
        drop(jobs);
        self.wake.notify_all();
    }

    fn next_job(&self) -> Option<(u64, ArtworkInput)> {
        let mut jobs = lock(&self.jobs);
        loop {
            if jobs.closed {
                return None;
            }
            if let Some(job) = jobs.pending.take() {
                return Some(job);
            }
            jobs = self.wake.wait(jobs).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn decode_loop(arts: &Arts, publisher: &Publisher, store: &ArtworkStore, stop: &CancellationToken) {
    while let Some((key, input)) = arts.next_job() {
        if stop.is_cancelled() {
            return;
        }
        let outcome = artwork::process(&input.data, &input.mime);
        drop(input);
        if stop.is_cancelled() {
            return;
        }
        let id = outcome.ok().map(|png| store.put(&png).0);
        arts.remember(key, id.clone());
        {
            let mut inflight = lock(&arts.inflight);
            if *inflight == Some(key) {
                *inflight = None;
            }
        }
        if let Some(id) = id {
            publisher.attach(key, &id);
        }
    }
}

/// How one adapter run ended.
enum RunEnd {
    /// The worker was asked to stop; the media was invalidated first.
    Cancelled { reaped: bool },
    /// The child could not be started.
    OpenFailed(String),
    /// The child ended or its output failed.
    Ended {
        reason: String,
        stderr: Option<String>,
        reaped: bool,
    },
}

/// Runs the media worker until `stop` is cancelled.
pub fn run(parts: &MediaParts, stop: &CancellationToken, publish: PublishMedia) {
    let publisher = Arc::new(Publisher::new(publish));
    supervise(parts, stop, &publisher);
    publisher.close();
}

fn supervise(parts: &MediaParts, stop: &CancellationToken, publisher: &Arc<Publisher>) {
    let unavailable = |reason: String| MediaStatus {
        state: MediaRunState::Unavailable,
        reason: Some(reason),
        stderr_tail: None,
    };
    let paths = match (parts.resolver)() {
        Ok(paths) => paths,
        Err(reason) => {
            publisher.hide(unavailable(reason));
            return;
        }
    };
    let Some(prepared) = prepared_command(parts, &paths) else {
        publisher.hide(unavailable(
            "the media adapter paths are not absolute UTF-8 paths".to_owned(),
        ));
        return;
    };
    let limits = StreamLimits::new(parts.timing.line_cap, STDERR_TAIL, parts.timing.term_grace)
        .unwrap_or_default();

    let arts = Arc::new(Arts::default());
    let decoder = {
        let arts = Arc::clone(&arts);
        let publisher = Arc::clone(publisher);
        let store = Arc::clone(&parts.store);
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("stache-bar-media-art".to_owned())
            .spawn(move || decode_loop(&arts, &publisher, &store, &stop))
            .ok()
    };

    restart_loop(parts, stop, publisher, &arts, &prepared, limits);

    publisher.close();
    arts.close();
    if let Some(handle) = decoder {
        join(handle);
    }
}

fn join(handle: JoinHandle<()>) { let _ = handle.join(); }

fn prepared_command(parts: &MediaParts, paths: &MediaPaths) -> Option<PreparedCommand> {
    let mut arguments = Vec::with_capacity(5);
    for path in [&paths.script, &paths.framework, &paths.test_client] {
        if !path.is_absolute() {
            return None;
        }
        arguments.push(path.to_str()?.to_owned());
    }
    arguments.push("stream".to_owned());
    arguments.push("--no-diff".to_owned());
    Some(PreparedCommand {
        program: parts.perl.clone(),
        arguments,
    })
}

fn restart_loop(
    parts: &MediaParts,
    stop: &CancellationToken,
    publisher: &Arc<Publisher>,
    arts: &Arc<Arts>,
    prepared: &PreparedCommand,
    limits: StreamLimits,
) {
    let timing = parts.timing;
    let mut failures: u32 = 0;
    loop {
        if stop.is_cancelled() {
            return;
        }
        let started = parts.clock.now();
        let end = run_once(parts, stop, publisher, arts, prepared, limits, failures);
        let (reason, stderr) = match end {
            RunEnd::Cancelled { reaped } => {
                if !reaped {
                    settle_unreaped(parts);
                }
                return;
            }
            RunEnd::OpenFailed(reason) => (reason, None),
            RunEnd::Ended { reason, stderr, reaped } => {
                if !reaped && !settle_unreaped(parts) {
                    // A child that cannot be confirmed gone is never joined by a
                    // second one. The executor keeps it; the monitor stays
                    // unsettled until it is reaped.
                    publisher.hide(MediaStatus {
                        state: MediaRunState::Unavailable,
                        reason: Some(
                            "the media adapter did not exit; cleanup is pending".to_owned(),
                        ),
                        stderr_tail: stderr,
                    });
                    return;
                }
                (reason, stderr)
            }
        };
        if parts.clock.now().saturating_sub(started) >= timing.reset_after {
            failures = 0;
        }
        failures = failures.saturating_add(1);
        let last_stderr = stderr.clone();
        publisher.hide(MediaStatus {
            state: if failures >= UNAVAILABLE_AFTER {
                MediaRunState::Unavailable
            } else {
                MediaRunState::Restarting
            },
            reason: Some(reason.clone()),
            stderr_tail: stderr,
        });
        if failures >= UNAVAILABLE_AFTER + GIVE_UP_AFTER_UNAVAILABLE {
            // Stay unavailable for good: the provider is recreated only by a
            // new Bar run (start, or resume after a pause). The published
            // reason says why nothing is respawned and keeps the last failure.
            publisher.hide(MediaStatus {
                state: MediaRunState::Unavailable,
                reason: Some(gave_up_reason(&reason)),
                stderr_tail: last_stderr,
            });
            return;
        }
        let index = usize::try_from(failures - 1)
            .unwrap_or(usize::MAX)
            .min(timing.backoff.len() - 1);
        if parts.clock.wait(Some(stop), timing.backoff[index]) || stop.is_cancelled() {
            return;
        }
    }
}

/// Rechecks an unreaped child a bounded number of times. `true` once none is
/// retained.
fn settle_unreaped(parts: &MediaParts) -> bool {
    for _ in 0..parts.timing.reap_retries {
        if parts.executor.sweep().is_empty() {
            return true;
        }
        parts.clock.wait(None, parts.timing.reap_retry_wait);
    }
    parts.executor.sweep().is_empty()
}

fn run_once(
    parts: &MediaParts,
    stop: &CancellationToken,
    publisher: &Arc<Publisher>,
    arts: &Arc<Arts>,
    prepared: &PreparedCommand,
    limits: StreamLimits,
    failures: u32,
) -> RunEnd {
    let mut stream = match parts.executor.open_line_stream(prepared, limits, stop, stop) {
        Ok(stream) => stream,
        Err(StreamOpenError::Cancelled) => return RunEnd::Cancelled { reaped: true },
        Err(StreamOpenError::Failed(failure)) => {
            return RunEnd::OpenFailed(clip(&format!(
                "the media adapter could not be started ({failure:?})"
            )));
        }
    };
    let mut confirmed = failures < UNAVAILABLE_AFTER;
    if confirmed {
        publisher.set_status(MediaStatus::running());
    }
    let reason = loop {
        match stream.next_event(stop) {
            StreamEvent::Line(line) => {
                if apply(&line, publisher, arts) && !confirmed {
                    confirmed = true;
                    publisher.set_status(MediaStatus::running());
                }
            }
            // A dropped frame never changes the state; the next line replaces it.
            StreamEvent::InvalidUtf8 { .. }
            | StreamEvent::LineTooLong { .. }
            | StreamEvent::Unterminated { .. } => {}
            StreamEvent::Closed { status } => {
                break status.map_or_else(
                    || "the media adapter closed its output".to_owned(),
                    |status| {
                        status.code().map_or_else(
                            || "the media adapter was ended by a signal".to_owned(),
                            |code| format!("the media adapter exited with code {code}"),
                        )
                    },
                );
            }
            StreamEvent::ReadFailed(_) => {
                break "reading the media adapter output failed".to_owned();
            }
            StreamEvent::Cancelled => {
                // Invalidate first: nothing the child or a decode finishes later
                // can reach the Bar after this point.
                publisher.close();
                let outcome = stream.stop();
                return RunEnd::Cancelled {
                    reaped: !matches!(outcome, StopOutcome::ReapTimeout { .. }),
                };
            }
        }
    };
    publisher.hide(MediaStatus {
        state: MediaRunState::Restarting,
        reason: Some(reason.clone()),
        stderr_tail: None,
    });
    let stderr = sanitize_stderr(&stream.stderr_tail());
    let outcome = stream.stop();
    RunEnd::Ended {
        reason,
        stderr,
        reaped: !matches!(outcome, StopOutcome::ReapTimeout { .. }),
    }
}

/// Applies one line; `true` when it was a usable data line.
fn apply(line: &str, publisher: &Arc<Publisher>, arts: &Arc<Arts>) -> bool {
    match parse_line(line) {
        Parsed::Ignore => false,
        Parsed::Absent => {
            publisher.set_absent();
            true
        }
        Parsed::State { reading, artwork } => {
            let plan = artwork.map_or(ArtPlan::None, |input| plan_artwork(input, publisher, arts));
            publisher.set_state(reading, plan);
            true
        }
    }
}

fn plan_artwork(input: ArtworkInput, publisher: &Publisher, arts: &Arts) -> ArtPlan {
    let key = fingerprint(&input);
    match arts.recall(key) {
        Some(Some(id)) => ArtPlan::Known(id),
        // This artwork already failed once: the state carries none.
        Some(None) => ArtPlan::None,
        None => {
            if !(arts.is_inflight(key) && publisher.expects(key)) {
                arts.submit(key, input);
            }
            ArtPlan::Pending(key)
        }
    }
}

fn clip(text: &str) -> String { text.chars().take(200).collect() }

/// Makes the adapter's stderr safe to show: control characters become spaces,
/// long base64-looking runs are dropped and only the last
/// [`STDERR_PUBLISHED_CHARS`] characters are kept.
#[must_use]
pub fn sanitize_stderr(tail: &str) -> Option<String> {
    let joined = tail
        .split_whitespace()
        .map(|word| {
            let encoded = word.len() >= 64
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='));
            if encoded { "\u{2026}" } else { word }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned: String = joined.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let count = cleaned.chars().count();
    let kept: String = cleaned.chars().skip(count.saturating_sub(STDERR_PUBLISHED_CHARS)).collect();
    let kept = kept.trim().to_owned();
    (!kept.is_empty()).then_some(kept)
}

/// The give-up reason with the last failure appended (that failure text is
/// already sanitized and bounded where it is produced).
fn gave_up_reason(last: &str) -> String { format!("{GAVE_UP_REASON} (last failure: {last})") }
