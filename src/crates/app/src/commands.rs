//! Contributed CLI commands.
//!
//! The composition root owns the native backends, the CLI styling and the
//! process environment used by the contributed commands. The CLI crate only
//! sees the generic `CommandCatalog`.

use std::sync::Arc;

use stache_cli::CliEnvironment;
use stache_macos::audio::{AudioBackend, CoreAudioBackend};
use stache_macos::{GuardedDesktopBackend, MainThread, NativeDesktop};
use stache_sdk::{CancellationToken, CommandCatalog};
use stache_wallpapers::cli::{CancelGuard, CancelScope, WallpapersCliEnv};

use crate::shutdown::SignalBridge;

/// The application's contributed commands for the process environment.
///
/// # Panics
/// Panics only on a composition bug, such as a duplicate command name.
#[must_use]
pub fn catalog() -> CommandCatalog { catalog_for(&CliEnvironment::from_env()) }

/// The application's contributed commands for an injected environment.
///
/// The audio command enumerates locally and never starts the desktop host or a
/// listener. The wallpaper command routes `set` to a running same-config host
/// and otherwise works locally: `list` and `generate-all` are always local, and
/// a standalone `set` runs on the process main thread, where the native adapter
/// executes inline, so no tray, window, registry, watcher, timer or listener is
/// started.
///
/// # Panics
/// Panics only if a command name is contributed twice, which would be a
/// composition bug.
#[must_use]
pub fn catalog_for(environment: &CliEnvironment) -> CommandCatalog {
    let mut catalog = CommandCatalog::new();
    let backend: Arc<dyn AudioBackend> = Arc::new(CoreAudioBackend);
    catalog
        .add(stache_proxy_audio::cli::contribution(
            backend,
            Arc::new(stache_cli::output::highlight_json),
        ))
        .expect("the audio command is contributed exactly once");
    let desktop: Arc<dyn GuardedDesktopBackend> =
        Arc::new(NativeDesktop::new(Arc::new(MainThread)));
    catalog
        .add(stache_wallpapers::cli::contribution(WallpapersCliEnv::new(
            environment.config.clone(),
            environment.app.clone(),
            desktop,
            Arc::new(stache_cli::output::highlight_json),
            interrupt_scope(),
        )))
        .expect("the wallpaper command is contributed exactly once");
    catalog
}

/// Counts termination signals: the first requests a cooperative stop and any
/// later one force-exits.
struct SignalCounter<C, E> {
    seen: std::sync::atomic::AtomicUsize,
    cancel: C,
    exit: E,
}

impl<C: Fn(), E: Fn(i32)> SignalCounter<C, E> {
    const fn new(cancel: C, exit: E) -> Self {
        Self {
            seen: std::sync::atomic::AtomicUsize::new(0),
            cancel,
            exit,
        }
    }

    fn signal(&self) {
        if self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            (self.cancel)();
        } else {
            (self.exit)(FORCED_EXIT_STATUS);
        }
    }
}

/// Exit status of a forced exit after a second signal.
const FORCED_EXIT_STATUS: i32 = 130;

/// Restores a hidden cursor (best effort, terminals only) and exits.
fn force_exit(status: i32) {
    use std::io::{IsTerminal, Write};
    let mut stderr = std::io::stderr();
    if stderr.is_terminal() {
        let _ = stderr.write_all(b"\x1b[?25h");
        let _ = stderr.flush();
    }
    std::process::exit(status);
}

/// A cancellation scope that SIGINT/SIGTERM/SIGHUP cancel while a wallpaper
/// command runs.
///
/// The existing [`SignalBridge`] is installed only when a command starts and
/// released when it finishes. The first signal cancels cooperatively: an
/// interrupted `generate-all` stops admission, joins its workers and restores
/// the terminal, and an interrupted standalone `set` stops before the next
/// native step. A second signal force-exits with status 130 after restoring a
/// hidden cursor. Releasing the bridge does not restore default handlers; the
/// process exits right after the command. If the handler cannot be installed
/// the command still runs uncancellable.
fn interrupt_scope() -> CancelScope {
    Arc::new(|| {
        let token = CancellationToken::new();
        let notify = token.clone();
        let counter = SignalCounter::new(move || notify.cancel(), force_exit);
        match SignalBridge::install(move || counter.signal()) {
            Ok(bridge) => CancelGuard::with_release(token, Box::new(bridge)),
            Err(_) => CancelGuard::new(token),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[test]
    fn first_signal_cancels_and_later_signals_force_exit_with_130() {
        let cancels = Cell::new(0);
        let exits = RefCell::new(Vec::new());
        let counter =
            SignalCounter::new(|| cancels.set(cancels.get() + 1), |s| exits.borrow_mut().push(s));
        counter.signal();
        assert_eq!(cancels.get(), 1);
        assert!(exits.borrow().is_empty());
        counter.signal();
        counter.signal();
        assert_eq!(cancels.get(), 1);
        assert_eq!(*exits.borrow(), vec![130, 130]);
    }
}
