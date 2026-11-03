//! Opt-in native smoke harness for the real Tauri run loop.
//!
//! Built only with `--features smoke` and launched through the `stache-smoke`
//! binary. It is not part of the shipped application and adds no production IPC.
//! The harness proves:
//!
//! - `stache-macos::run_on_main` runs a callback on the actual `AppKit` main
//!   thread and an activation observer delivers there and removes itself;
//! - launch creates zero webviews;
//! - the explicit test window opens, reuses and closes, and its real React DOM
//!   proves generated invoke plus `statusChanged` inside `WebKit`;
//! - closing that last window leaves the host and its socket running;
//! - an isolated, valid, still disabled config edit is accepted through the
//!   real control socket and its revision reaches the React DOM;
//! - a second process is rejected by the next-identity instance lock before
//!   starting Tauri;
//! - SIGTERM to this owned PID converges on the normal idempotent shutdown.
//!
//! The harness never enables `proxyAudio`: a generic host/WebKit run must not
//! select or change the user's audio devices. Both the pre-start file and the
//! reloaded file keep `enabled: false`; only another valid field changes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, process, thread};

use stache_macos::{ActivationObserver, is_main_thread, run_on_main};
use stache_sdk::ipc::IpcClient;
use stache_sdk::{
    AppEnvironment, AppPaths, ConfigFileId, ConfigPaths, InstanceError, InstanceLock, ModuleId,
    WindowBackend,
};
use tauri::{AppHandle, Manager};

use crate::bindings::{ConfigFileState, ModuleState};
use crate::host::HostHandle;
use crate::windows::NativeWindows;

/// Environment variable used by the first process to spawn the second launch.
pub const SECOND_ENV: &str = "STACHE_SMOKE_SECOND";

const PROXY_AUDIO: &str = "proxyAudio";

/// Baseline file written before the host starts: valid and explicitly disabled.
const INITIAL_CONFIG: &str = r#"{
  "enabled": false,
  "output": [{ "name": "stache-smoke disabled baseline", "strategy": "exact" }]
}
"#;

/// Changed file the reload must accept: still valid, still disabled.
const RELOAD_CONFIG: &str = r#"{
  "enabled": false,
  "output": [{ "name": "stache-smoke disabled reload", "strategy": "exact" }]
}
"#;

/// Writes the disabled `proxyAudio` baseline before the host starts.
///
/// # Errors
/// Returns a path or I/O failure.
pub fn prepare_disabled_config(config: &ConfigPaths) -> Result<(), String> {
    write_module_config(config, INITIAL_CONFIG)
}

fn write_module_config(config: &ConfigPaths, text: &str) -> Result<(), String> {
    let path = config.file(ConfigFileId::module(ModuleId::new(PROXY_AUDIO)));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create config dir: {error}"))?;
    }
    std::fs::write(&path, text).map_err(|error| format!("cannot write smoke config: {error}"))
}

/// The handles the harness needs after setup.
pub struct SmokeContext {
    /// Running application.
    pub app: AppHandle,
    /// Host command handle.
    pub host: HostHandle,
    /// Window bookkeeping used by the tray action.
    pub windows: Arc<NativeWindows>,
    /// Isolated configuration directory used to prove reload and events.
    pub config: ConfigPaths,
    /// Set only after every check reached the shutdown request.
    pub completed: Arc<AtomicBool>,
}

/// Starts the smoke checks on a worker thread.
pub fn start(context: SmokeContext) {
    let _ = thread::Builder::new()
        .name("stache-smoke".to_owned())
        .spawn(move || run_checks(&context));
}

/// Claims the instance lock and reports whether another process owns it.
///
/// # Errors
/// Returns a path or I/O failure, or `Ok(false)` when the lock was acquired.
pub fn second_instance_probe() -> Result<bool, String> {
    let paths =
        AppPaths::resolve(&AppEnvironment::from_env()).map_err(|error| error.to_string())?;
    match InstanceLock::acquire(&paths) {
        Err(InstanceError::AlreadyRunning { .. }) => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

#[allow(clippy::too_many_lines)] // One ordered end-to-end acceptance sequence.
fn run_checks(context: &SmokeContext) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !context.host.status().ready {
        if Instant::now() >= deadline {
            fail("host did not become ready");
        }
        thread::sleep(Duration::from_millis(20));
    }
    // Give the event loop a moment to settle before asserting no window exists.
    thread::sleep(Duration::from_millis(200));
    if !context.app.webview_windows().is_empty() {
        fail("launch created a webview");
    }
    pass("zero webviews at launch");

    let (sender, receiver) = std::sync::mpsc::channel();
    if run_on_main(Box::new(move || {
        let _ = sender.send(is_main_thread());
    }))
    .is_err()
    {
        fail("run_on_main could not enqueue a job");
    }
    match receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(true) => pass("stache-macos::run_on_main ran on the AppKit main thread"),
        Ok(false) => fail("run_on_main ran on a worker thread"),
        Err(error) => fail(&format!("main-thread callback did not run: {error}")),
    }

    let (activation_sender, activation_receiver) = std::sync::mpsc::channel();
    if run_on_main(Box::new(move || {
        let (called, callbacks) = std::sync::mpsc::channel();
        let observer = ActivationObserver::install(move || {
            let _ = called.send(());
        })
        .expect("activation observer requires main");
        stache_macos::activation::post_activation_for_test();
        let delivered = callbacks.recv_timeout(Duration::from_secs(1)).is_ok();
        drop(observer);
        stache_macos::activation::post_activation_for_test();
        let removed = callbacks.recv_timeout(Duration::from_millis(100)).is_err();
        let _ = activation_sender.send(delivered && removed);
    }))
    .is_err()
    {
        fail("activation check could not run on main");
    }
    match activation_receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(true) => pass("activation observer delivered on main and removed on drop"),
        Ok(false) => fail("activation observer delivery or removal failed"),
        Err(error) => fail(&format!("activation check did not report: {error}")),
    }

    let first = match context.windows.open_test_window() {
        Ok(id) => id,
        Err(error) => fail(&format!("test window request failed: {error}")),
    };
    let label = format!("stache-test-{}", first.get());
    wait_for_window(&context.app, &label, true);
    pass("explicit test window created");

    let window = context
        .app
        .get_webview_window(&label)
        .unwrap_or_else(|| fail("test window disappeared"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !context.windows.is_loaded(&label) {
        if Instant::now() >= deadline {
            fail("test window page did not finish loading");
        }
        thread::sleep(Duration::from_millis(25));
    }
    eval_script(&window, READY_SCRIPT);
    wait_for_fragment(&context.app, &label, "smoke-ready");
    pass("WebKit rendered Host ready/config and generated invoke succeeded");

    let initial = context.host.status();
    let initial_module = initial
        .modules
        .iter()
        .find(|module| module.id == PROXY_AUDIO)
        .unwrap_or_else(|| fail("proxyAudio is missing from the host status"));
    if initial_module.enabled || initial_module.state != ModuleState::Disabled {
        fail("the pre-start config did not leave proxyAudio disabled");
    }
    let initial_file = initial
        .config
        .iter()
        .find(|file| file.file == "proxyAudio.jsonc")
        .unwrap_or_else(|| fail("proxyAudio.jsonc is missing from the host status"));
    if initial_file.state != ConfigFileState::Valid || !initial_file.active {
        fail("the pre-start proxyAudio config was not loaded as valid");
    }
    let initial_revision = initial_module.revision.clone();

    write_module_config(&context.config, RELOAD_CONFIG).unwrap_or_else(|error| fail(&error));
    let paths = AppPaths::resolve(&AppEnvironment::from_env())
        .unwrap_or_else(|error| fail(&format!("cannot resolve app paths: {error}")));
    let socket = paths
        .socket_path()
        .unwrap_or_else(|error| fail(&format!("cannot resolve socket: {error}")));
    let report = IpcClient::new(&socket)
        .reload()
        .unwrap_or_else(|error| fail(&format!("real IPC reload failed: {error}")));
    if report.has_failures() {
        fail(&format!("IPC reload reported a failure: {report:?}"));
    }
    pass("real runtime IPC reload accepted the changed disabled config");

    let reloaded_revision = wait_for_disabled_revision(context, &initial_revision);
    eval_script(&window, &event_script(&initial_revision, &reloaded_revision));
    wait_for_fragment(&context.app, &label, "smoke-event");
    pass(&format!(
        "StatusChanged reached the React DOM and the snapshot at disabled revision {reloaded_revision}"
    ));

    let second = match context.windows.open_test_window() {
        Ok(id) => id,
        Err(error) => fail(&format!("test window reuse failed: {error}")),
    };
    if first != second {
        fail("the same owner/key created a second window");
    }
    pass("same owner/key reused the test window");

    context.windows.close(first);
    wait_for_window(&context.app, &label, false);
    if !context.windows.is_empty() {
        fail("closed window stayed in the bookkeeping map");
    }
    pass("test window closed and removed");

    // Closing the last window must only release its bookkeeping. Give a wrong
    // teardown time to surface, then prove the host still answers on its socket.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if context.host.shutdown_requested() {
            fail("closing the last window started the host shutdown");
        }
        thread::sleep(Duration::from_millis(25));
    }
    if context.host.shutdown_requested() {
        fail("closing the last window started the host shutdown");
    }
    IpcClient::new(&socket).status().unwrap_or_else(|error| {
        fail(&format!(
            "the host stopped answering after a window close: {error}"
        ));
    });
    pass("closing the last window keeps the host running");

    let executable = match env::current_exe() {
        Ok(executable) => executable,
        Err(error) => fail(&format!("cannot resolve the smoke binary: {error}")),
    };
    let second_launch = process::Command::new(executable).env(SECOND_ENV, "1").status();
    match second_launch {
        Ok(status) if status.success() => pass("second launch was refused before host startup"),
        Ok(status) => fail(&format!("second launch exited with {status}")),
        Err(error) => fail(&format!("cannot spawn the second launch: {error}")),
    }

    // Deliver a real SIGTERM to this owned process only (never a shared
    // executable name). The signal bridge must converge on the same teardown
    // as tray Quit and exit normally.
    context.completed.store(true, Ordering::SeqCst);
    pass("requesting the owned process SIGTERM");
    let pid = process::id().to_string();
    let status = process::Command::new("/bin/kill").args(["-TERM", &pid]).status();
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => fail(&format!("cannot signal the smoke process: {status}")),
        Err(error) => fail(&format!("cannot run /bin/kill: {error}")),
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    fail("SIGTERM shutdown did not finish");
}

const READY_SCRIPT: &str = r"
(() => {
  const deadline = Date.now() + 20000;
  const finish = (value) => { location.hash = '#smoke-' + value; };
  const tick = async () => {
    try {
      const status = await window.__TAURI_INTERNALS__.invoke('host_status');
      const info = await window.__TAURI_INTERNALS__.invoke('app_info');
      const text = document.body.innerText;
      const proxy = status && status.config &&
        status.config.find((entry) => entry.file === 'proxyAudio.jsonc');
      const module = status && status.modules &&
        status.modules.find((entry) => entry.id === 'proxyAudio');
      if (status && status.ready && info && info.identifier &&
          module && module.enabled === false && module.state === 'disabled' &&
          proxy && proxy.state === 'valid' && proxy.active === true &&
          text.includes('Host ready') && text.includes('global.jsonc') &&
          text.includes('proxyAudio.jsonc')) {
        finish('ready');
        return;
      }
      if (Date.now() >= deadline) {
        if (!status || !status.ready) finish('fail-notready');
        else if (!info || !info.identifier) finish('fail-info');
        else if (!module || module.enabled !== false || module.state !== 'disabled') finish('fail-proxy-state');
        else if (!proxy || proxy.state !== 'valid' || proxy.active !== true) finish('fail-proxy-file');
        else if (!text.includes('Host test surface')) finish('fail-surface-' + encodeURIComponent(location.search + '|' + text.slice(0, 80)));
        else if (!text.includes('global.jsonc')) finish('fail-global');
        else if (!text.includes('proxyAudio.jsonc')) finish('fail-proxy');
        else finish('fail-dom');
        return;
      }
    } catch (_) {
      finish('fail-invoke');
      return;
    }
    setTimeout(tick, 50);
  };
  tick();
})();
";

const EVENT_SCRIPT: &str = r"
(() => {
  const previous = '__PREVIOUS__';
  const expected = '__EXPECTED__';
  const deadline = Date.now() + 20000;
  const finish = (value) => { location.hash = '#smoke-' + value; };
  const findCard = (name) => {
    const cards = Array.from(document.querySelectorAll('li'));
    for (const card of cards) {
      const heading = card.querySelector('h3');
      if (heading && heading.textContent === name) return card;
    }
    return null;
  };
  const rowValue = (card, label) => {
    const rows = Array.from(card.querySelectorAll('div'));
    for (const row of rows) {
      const term = Array.from(row.children).find((child) => child.tagName === 'DT');
      if (term && term.textContent === label) {
        const value = Array.from(row.children).find((child) => child.tagName === 'DD');
        return value ? value.textContent : null;
      }
    }
    return null;
  };
  const inspect = (status) => {
    const module = status && status.modules &&
      status.modules.find((entry) => entry.id === 'proxyAudio');
    const file = status && status.config &&
      status.config.find((entry) => entry.file === 'proxyAudio.jsonc');
    if (!module) return 'fail-no-module';
    if (module.revision === previous) return 'fail-stale-revision';
    if (module.revision !== expected) return 'fail-snapshot-revision';
    if (module.enabled !== false || module.state !== 'disabled') return 'fail-snapshot-state';
    if (!file || file.state !== 'valid' || file.active !== true) return 'fail-snapshot-file';
    const card = findCard('proxyAudio');
    const configCard = findCard('proxyAudio.jsonc');
    if (!card || !configCard) return 'fail-dom-card';
    if (rowValue(card, 'Revision') !== expected || rowValue(card, 'Enabled') !== 'No') {
      return 'fail-dom-revision';
    }
    const badge = card.querySelector('[data-state]');
    if (!badge || badge.textContent !== 'Disabled') return 'fail-dom-state';
    const configBadge = configCard.querySelector('[data-state]');
    if (!configBadge || configBadge.textContent !== 'Valid' ||
        configCard.textContent.indexOf('Active configuration') === -1) {
      return 'fail-dom-file';
    }
    return 'event';
  };
  const tick = async () => {
    try {
      const status = await window.__TAURI_INTERNALS__.invoke('host_status');
      const verdict = inspect(status);
      if (verdict === 'event') {
        finish('event');
        return;
      }
      if (Date.now() >= deadline) {
        finish(verdict);
        return;
      }
    } catch (_) {
      finish('fail-invoke');
      return;
    }
    setTimeout(tick, 50);
  };
  tick();
})();
";

fn event_script(previous: &str, expected: &str) -> String {
    EVENT_SCRIPT.replace("__PREVIOUS__", previous).replace("__EXPECTED__", expected)
}

fn wait_for_disabled_revision(context: &SmokeContext, previous: &str) -> String {
    let previous = previous
        .parse::<u64>()
        .unwrap_or_else(|error| fail(&format!("previous revision is not a number: {error}")));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = context.host.status();
        if let Some(module) = status.modules.iter().find(|module| module.id == PROXY_AUDIO) {
            if module.enabled || module.state != ModuleState::Disabled {
                fail("the reload enabled or started proxyAudio");
            }
            let file = status.config.iter().find(|file| file.file == "proxyAudio.jsonc");
            if file.is_some_and(|file| file.state == ConfigFileState::Valid && file.active)
                && let Ok(revision) = module.revision.parse::<u64>()
                && revision > previous
            {
                return module.revision.clone();
            }
        }
        if Instant::now() >= deadline {
            fail("the reload did not advance the disabled proxyAudio revision");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn eval_script(window: &tauri::WebviewWindow, script: &str) {
    window
        .eval(script)
        .unwrap_or_else(|error| fail(&format!("cannot evaluate the WebKit probe: {error}")));
}

fn wait_for_fragment(app: &AppHandle, label: &str, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(window) = app.get_webview_window(label)
            && let Ok(url) = window.url()
            && let Some(fragment) = url.fragment()
        {
            if fragment == expected {
                return;
            }
            if fragment.starts_with("smoke-fail") {
                fail(&format!("WebKit probe reported `{fragment}`"));
            }
        }
        if Instant::now() >= deadline {
            fail(&format!("WebKit probe did not report `{expected}`"));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_window(app: &AppHandle, label: &str, present: bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found = app.get_webview_window(label).is_some();
        if found == present {
            return;
        }
        if Instant::now() >= deadline {
            fail(&format!(
                "window `{label}` did not become {}",
                if present {
                    "visible in the manager"
                } else {
                    "closed"
                }
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn pass(message: &str) {
    println!("smoke: ok: {message}");
}

fn fail(message: &str) -> ! {
    eprintln!("smoke: FAIL: {message}");
    process::exit(1);
}

#[cfg(test)]
mod tests {
    use stache_proxy_audio::ProxyAudioConfig;
    use stache_sdk::{ConfigFileId, ConfigPaths, ModuleConfig, ModuleId};

    use super::{INITIAL_CONFIG, PROXY_AUDIO, RELOAD_CONFIG, prepare_disabled_config};

    #[test]
    fn both_smoke_configs_are_valid_disabled_and_distinct() {
        let initial: ProxyAudioConfig = serde_json::from_str(INITIAL_CONFIG).expect("decode");
        let reload: ProxyAudioConfig = serde_json::from_str(RELOAD_CONFIG).expect("decode");
        for config in [&initial, &reload] {
            assert!(!config.enabled);
            config.validate().expect("valid disabled configuration");
        }
        assert_ne!(initial, reload);
    }

    #[test]
    fn the_initial_disabled_file_is_written_before_the_host_can_read_it() {
        let root = tempfile::tempdir().expect("root");
        let config = ConfigPaths::new(root.path().join("nested"));
        prepare_disabled_config(&config).expect("write");
        let path = config.file(ConfigFileId::module(ModuleId::new(PROXY_AUDIO)));
        assert_eq!(std::fs::read_to_string(path).expect("read"), INITIAL_CONFIG);
    }

    #[test]
    fn the_event_probe_carries_both_exact_revisions() {
        let script = super::event_script("18446744073709551614", "18446744073709551615");
        assert!(script.contains("const previous = '18446744073709551614';"));
        assert!(script.contains("const expected = '18446744073709551615';"));
        assert!(!script.contains("__PREVIOUS__"));
        assert!(!script.contains("__EXPECTED__"));
    }
}
