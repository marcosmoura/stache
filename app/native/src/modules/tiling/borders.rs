//! Simple border management via `JankyBorders`.
//!
//! This module handles window borders by communicating with `JankyBorders`.
//! It's intentionally simple:
//!
//! 1. On init: Configure `JankyBorders` with style settings and blacklist
//! 2. On focus change: Send a single batched command with all border colors
//!
//! # Architecture
//!
//! - Uses Mach IPC for fast communication (falls back to CLI)
//! - Caches the last command to avoid duplicate sends
//! - Batches all settings into a single call

use std::ffi::CString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::config::types::tiling::LayoutType as ConfigLayoutType;
use crate::config::{
    BorderAnimationConfig, BorderColor, BorderStateConfig, GradientConfig, Rgba, get_config,
    parse_hex_color,
};
use crate::modules::tiling::effects::animation::{apply_easing, lerp};
use crate::modules::tiling::rules::{SKIP_TILING_APP_NAMES, SKIP_TILING_BUNDLE_IDS};
use crate::modules::tiling::state::LayoutType;
use crate::platform::command::resolve_binary;

// ============================================================================
// Constants
// ============================================================================

/// Mach service name for `JankyBorders`.
const JANKY_BORDERS_SERVICE: &str = "git.felix.borders";

/// Low-rate animation avoids flooding `JankyBorders`' FIFO Mach queue.
const BORDER_ANIMATION_FPS: u64 = 8;
const BORDER_ANIMATION_FRAME_DURATION_MS: u64 = 1_000 / BORDER_ANIMATION_FPS;

const fn animation_frame_duration() -> Duration {
    Duration::from_millis(BORDER_ANIMATION_FRAME_DURATION_MS)
}

// ============================================================================
// State
// ============================================================================

/// Last command sent to `JankyBorders` (for deduplication).
static LAST_COMMAND: OnceLock<Mutex<String>> = OnceLock::new();

/// Mach port for IPC communication.
static MACH_PORT: OnceLock<Mutex<Option<u32>>> = OnceLock::new();

fn get_last_command() -> &'static Mutex<String> {
    LAST_COMMAND.get_or_init(|| Mutex::new(String::new()))
}

fn get_mach_port() -> &'static Mutex<Option<u32>> { MACH_PORT.get_or_init(|| Mutex::new(None)) }

// ============================================================================
// Animation Runner (single background thread with command queue)
// ============================================================================

/// Monotonic epoch bumped on every border update so the animation runner can
/// drop frames belonging to superseded updates (their base command already
/// replaced the border state).
static ANIMATION_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Serializes the focus command with animation frames so a frame that passed
/// its epoch check cannot overwrite a newly focused border.
static BORDER_SEND_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn get_border_send_lock() -> &'static Mutex<()> { BORDER_SEND_LOCK.get_or_init(|| Mutex::new(())) }

/// Commands sent to the animation runner thread.
enum AnimationCommand {
    Update {
        /// Epoch at queue time; stale if it differs from `ANIMATION_EPOCH`.
        epoch: u64,
        animation: Option<(GradientConfig, BorderAnimationConfig)>,
    },
}

/// Sender end of the animation command channel.
static ANIMATION_TX: OnceLock<Mutex<Option<mpsc::Sender<AnimationCommand>>>> = OnceLock::new();

/// Ensures the animation runner thread is started exactly once.
static ANIMATION_RUNNER_STARTED: OnceLock<bool> = OnceLock::new();

fn get_animation_tx() -> &'static Mutex<Option<mpsc::Sender<AnimationCommand>>> {
    ANIMATION_TX.get_or_init(|| Mutex::new(None))
}

/// Spawns the animation runner thread (idempotent).
pub fn init_animation_runner() {
    let _ = ANIMATION_RUNNER_STARTED.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        *get_animation_tx().lock() = Some(tx);
        std::thread::spawn(|| animation_runner(rx));
        true
    });
}

/// Single persistent thread that processes border updates sequentially.
/// Drains stale commands — only the latest focus/update matters.
#[allow(clippy::needless_pass_by_value)]
fn animation_runner(rx: mpsc::Receiver<AnimationCommand>) {
    while let Ok(mut cmd) = rx.recv() {
        // Drain any newer commands that arrived while we were waiting;
        // keep the most recent one (last in the drain).
        while let Ok(newer) = rx.try_recv() {
            cmd = newer;
        }

        // Process the command; if run_animation consumed a newer command
        // from the channel, keep processing it without re-entering recv().
        while let Some(next) = process_update(&rx, cmd) {
            cmd = next;
        }
    }
}

/// Handles a single `Update` — drives the animation for the queued command.
/// Returns `Some(command)` if `run_animation` consumed a newer command
/// from the channel without it being processed by the outer loop.
fn process_update(
    rx: &mpsc::Receiver<AnimationCommand>,
    cmd: AnimationCommand,
) -> Option<AnimationCommand> {
    if PAUSED.load(Ordering::SeqCst) {
        tracing::trace!("tiling: borders paused, dropping queued animation command");
        return None;
    }

    let AnimationCommand::Update { epoch, animation } = cmd;

    if let Some((gradient, config)) = animation {
        run_animation(rx, &gradient, &config, epoch)
    } else {
        None
    }
}

/// Drives a ping-pong gradient animation, polling the channel after each frame.
/// Returns `Some(command)` if a newer command was consumed from the channel.
fn run_animation(
    rx: &mpsc::Receiver<AnimationCommand>,
    gradient: &GradientConfig,
    animation: &BorderAnimationConfig,
    epoch: u64,
) -> Option<AnimationCommand> {
    run_animation_with_frame_sender(rx, gradient, animation, epoch, |args| {
        send_animation_frame(args)
    })
}

fn run_animation_with_frame_sender(
    rx: &mpsc::Receiver<AnimationCommand>,
    gradient: &GradientConfig,
    animation: &BorderAnimationConfig,
    epoch: u64,
    send_frame: impl FnMut(&[String]) -> bool,
) -> Option<AnimationCommand> {
    run_animation_with_frame_sender_and_wait(
        rx,
        gradient,
        animation,
        epoch,
        send_frame,
        wait_for_animation_command,
    )
}

fn run_animation_with_frame_sender_and_wait(
    rx: &mpsc::Receiver<AnimationCommand>,
    gradient: &GradientConfig,
    animation: &BorderAnimationConfig,
    epoch: u64,
    mut send_frame: impl FnMut(&[String]) -> bool,
    mut wait_for_command: impl FnMut(
        &mpsc::Receiver<AnimationCommand>,
        Duration,
    ) -> Result<Option<AnimationCommand>, mpsc::RecvTimeoutError>,
) -> Option<AnimationCommand> {
    let Ok(from) = parse_hex_color(&gradient.from) else {
        return None;
    };
    let Ok(to) = parse_hex_color(&gradient.to) else {
        return None;
    };

    let duration = Duration::from_millis(u64::from(animation.duration.max(16)));
    let easing = animation.easing;
    let frame_duration = animation_frame_duration();
    let angle = gradient.angle;

    let mut forward = true;
    let mut start = Instant::now();

    loop {
        let raw_progress = (start.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0);
        let eased = apply_easing(raw_progress, easing);
        let progress = if forward { eased } else { 1.0 - eased };

        let active_color = animated_gradient_color(&from, &to, angle, progress);
        if let Some(cmd) = take_queued_animation_command(rx) {
            return Some(cmd);
        }
        {
            let _send_lock = get_border_send_lock().lock();
            if ANIMATION_EPOCH.load(Ordering::SeqCst) != epoch {
                tracing::trace!("tiling: dropping stale border animation frames (epoch {epoch})");
                return None;
            }
            let _ = send_frame(&[format!("active_color={active_color}")]);
        }

        if raw_progress >= 1.0 {
            forward = !forward;
            start = Instant::now();
        }

        match wait_for_command(rx, frame_duration) {
            Ok(Some(cmd)) => return Some(cmd),
            Ok(None) => {}
            Err(_) => return None,
        }
    }
}

fn wait_for_animation_command(
    rx: &mpsc::Receiver<AnimationCommand>,
    frame_duration: Duration,
) -> Result<Option<AnimationCommand>, mpsc::RecvTimeoutError> {
    match rx.recv_timeout(frame_duration) {
        Ok(cmd) => Ok(Some(cmd)),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        Err(err @ mpsc::RecvTimeoutError::Disconnected) => Err(err),
    }
}

fn take_queued_animation_command(
    rx: &mpsc::Receiver<AnimationCommand>,
) -> Option<AnimationCommand> {
    rx.try_recv().ok()
}

// ============================================================================
// Mach IPC
// ============================================================================

fn command_key(args: &[String]) -> String { args.join("\0") }

fn encode_mach_args(args: &[String]) -> Vec<u8> {
    let mut payload = Vec::new();
    for arg in args {
        payload.extend_from_slice(arg.as_bytes());
        payload.push(0);
    }
    payload.push(0);
    payload
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn bootstrap_look_up(bp: u32, service_name: *const i8, sp: *mut u32) -> i32;
    fn task_get_special_port(task: u32, which_port: i32, special_port: *mut u32) -> i32;
    fn mach_msg(
        msg: *mut MachMessage,
        option: i32,
        send_size: u32,
        rcv_size: u32,
        rcv_name: u32,
        timeout: u32,
        notify: u32,
    ) -> i32;
    static mach_task_self_: u32;
}

const KERN_SUCCESS: i32 = 0;
const TASK_BOOTSTRAP_PORT: i32 = 4;
const MACH_SEND_MSG: i32 = 0x0000_0001;
const MACH_SEND_TIMEOUT: i32 = 0x0000_0010;
const MACH_SEND_OPTIONS: i32 = MACH_SEND_MSG | MACH_SEND_TIMEOUT;
const MACH_PORT_NULL: u32 = 0;

const MACH_COMMAND_SEND_TIMEOUT_MS: u32 = 100;
const MACH_FRAME_SEND_TIMEOUT_MS: u32 = 0;

#[repr(C, packed)]
struct MachMessage {
    header: MachMsgHeader,
    body: MachMsgBody,
    descriptor: MachMsgOolDescriptor,
}

#[repr(C)]
struct MachMsgHeader {
    bits: u32,
    size: u32,
    remote_port: u32,
    local_port: u32,
    voucher_port: u32,
    id: i32,
}

#[repr(C)]
struct MachMsgBody {
    descriptor_count: u32,
}

#[repr(C)]
struct MachMsgOolDescriptor {
    address: *const u8,
    deallocate: u8,
    copy: u8,
    pad1: u8,
    type_: u8,
    size: u32,
}

fn get_bootstrap_port() -> Option<u32> {
    let mut bootstrap_port = 0;
    let task = unsafe { mach_task_self_ };
    let result =
        unsafe { task_get_special_port(task, TASK_BOOTSTRAP_PORT, &raw mut bootstrap_port) };

    if result == KERN_SUCCESS && bootstrap_port != 0 {
        Some(bootstrap_port)
    } else {
        None
    }
}

/// Connects to `JankyBorders` via Mach IPC.
fn connect_mach() -> bool {
    let Ok(service) = CString::new(JANKY_BORDERS_SERVICE) else {
        return false;
    };
    let Some(bootstrap_port) = get_bootstrap_port() else {
        return false;
    };

    let mut port: u32 = 0;
    let result = unsafe { bootstrap_look_up(bootstrap_port, service.as_ptr(), &raw mut port) };

    if result == 0 && port != 0 {
        *get_mach_port().lock() = Some(port);
        true
    } else {
        false
    }
}

/// Sends arguments via Mach IPC.
fn send_mach(args: &[String]) -> bool { send_mach_with_timeout(args, MACH_COMMAND_SEND_TIMEOUT_MS) }

fn send_mach_with_timeout(args: &[String], timeout_ms: u32) -> bool {
    const MACH_MSGH_BITS_COMPLEX: u32 = 0x8000_0000;
    const MACH_MSGH_BITS_COPY_SEND: u32 = 19;
    const MACH_MSG_OOL_DESCRIPTOR: u8 = 1;
    const MACH_MSG_VIRTUAL_COPY: u8 = 1;

    let Some(port) = *get_mach_port().lock() else {
        return false;
    };

    let data = encode_mach_args(args);

    let mut msg = MachMessage {
        header: MachMsgHeader {
            bits: MACH_MSGH_BITS_COMPLEX | MACH_MSGH_BITS_COPY_SEND,
            #[allow(clippy::cast_possible_truncation)]
            size: std::mem::size_of::<MachMessage>() as u32,
            remote_port: port,
            local_port: 0,
            voucher_port: 0,
            id: 0,
        },
        body: MachMsgBody { descriptor_count: 1 },
        descriptor: MachMsgOolDescriptor {
            address: data.as_ptr(),
            deallocate: 0,
            copy: MACH_MSG_VIRTUAL_COPY,
            pad1: 0,
            type_: MACH_MSG_OOL_DESCRIPTOR,
            #[allow(clippy::cast_possible_truncation)]
            size: data.len() as u32,
        },
    };

    let result = unsafe {
        mach_msg(
            &raw mut msg,
            MACH_SEND_OPTIONS,
            msg.header.size,
            0,
            MACH_PORT_NULL,
            timeout_ms,
            MACH_PORT_NULL,
        )
    };

    // The timeout only applies because MACH_SEND_TIMEOUT is set in options.
    result == 0
}

// ============================================================================
// Color Conversion
// ============================================================================

/// Converts RGBA to `JankyBorders` hex format (`0xAARRGGBB`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rgba_to_hex(rgba: &Rgba) -> String {
    let a = (rgba.a * 255.0).round() as u8;
    let r = (rgba.r * 255.0).round() as u8;
    let g = (rgba.g * 255.0).round() as u8;
    let b = (rgba.b * 255.0).round() as u8;
    format!("0x{a:02X}{r:02X}{g:02X}{b:02X}")
}

fn lerp_rgba(from: &Rgba, to: &Rgba, progress: f64) -> Rgba {
    Rgba {
        r: lerp(from.r, to.r, progress),
        g: lerp(from.g, to.g, progress),
        b: lerp(from.b, to.b, progress),
        a: lerp(from.a, to.a, progress),
    }
}

/// Converts a hex color string to `JankyBorders` format.
fn hex_to_janky(hex: &str) -> Option<String> {
    let rgba = parse_hex_color(hex).ok()?;
    Some(rgba_to_hex(&rgba))
}

fn gradient_to_janky(from_hex: &str, to_hex: &str, angle: f64) -> String {
    let angle = ((angle % 360.0) + 360.0) % 360.0;
    if (0.0..90.0).contains(&angle) || (180.0..270.0).contains(&angle) {
        format!("gradient(top_right={from_hex},bottom_left={to_hex})")
    } else {
        format!("gradient(top_left={from_hex},bottom_right={to_hex})")
    }
}

fn animated_gradient_color(from: &Rgba, to: &Rgba, angle: f64, progress: f64) -> String {
    let animated_from = lerp_rgba(from, to, progress);
    let animated_to = lerp_rgba(to, from, progress);
    let from_hex = rgba_to_hex(&animated_from);
    let to_hex = rgba_to_hex(&animated_to);
    gradient_to_janky(&from_hex, &to_hex, angle)
}

/// Converts a `BorderColor` to `JankyBorders` color string.
fn border_color_to_janky(color: &BorderColor) -> Option<String> {
    match color {
        BorderColor::Solid(hex) => hex_to_janky(hex),
        BorderColor::Gradient { from, to, angle } => {
            let from_hex = hex_to_janky(from)?;
            let to_hex = hex_to_janky(to)?;
            let angle = angle.unwrap_or(135.0);
            Some(gradient_to_janky(&from_hex, &to_hex, angle))
        }
        BorderColor::Glow(hex) => {
            let janky_hex = hex_to_janky(hex)?;
            Some(format!("glow({janky_hex})"))
        }
    }
}

/// Gets the `JankyBorders` color string for a border state config.
/// Returns transparent (0x00000000) and width 0 if disabled.
fn get_border_settings(config: &BorderStateConfig) -> (String, u32) {
    if !config.is_enabled() {
        return ("0x00000000".to_string(), 0);
    }

    let color = BorderColor::from_state_config(config)
        .and_then(|c| border_color_to_janky(&c))
        .unwrap_or_else(|| "0x00000000".to_string());

    let width = config.width().unwrap_or(0);

    (color, width)
}

const fn animated_gradient_parts(
    config: &BorderStateConfig,
) -> Option<(&GradientConfig, &BorderAnimationConfig)> {
    match config {
        BorderStateConfig::GradientColor {
            gradient,
            animation: Some(animation),
            ..
        } => Some((gradient, animation)),
        BorderStateConfig::Disabled(_)
        | BorderStateConfig::SolidColor { .. }
        | BorderStateConfig::GradientColor { animation: None, .. }
        | BorderStateConfig::GlowColor { .. } => None,
    }
}

// ============================================================================
// JankyBorders Communication
// ============================================================================

/// Checks if `JankyBorders` is available.
fn is_available() -> bool { is_available_with_resolver(resolve_binary) }

fn is_available_with_resolver(resolve: impl FnOnce(&str) -> Result<PathBuf, String>) -> bool {
    resolve("borders").is_ok()
}

fn resolve_borders_binary() -> Option<PathBuf> { resolve_binary("borders").ok() }

fn send_cli(args: &[String]) -> bool {
    let Some(binary) = resolve_borders_binary() else {
        return false;
    };

    Command::new(binary)
        .args(args)
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Sends arguments to `JankyBorders` (with deduplication).
fn send_command(args: &[String]) -> bool {
    send_command_with(args, send_mach, connect_mach, send_cli)
}

fn send_command_with(
    args: &[String],
    mut send_mach_fn: impl FnMut(&[String]) -> bool,
    mut connect_mach_fn: impl FnMut() -> bool,
    mut send_cli_fn: impl FnMut(&[String]) -> bool,
) -> bool {
    let key = command_key(args);

    // Check if command is the same as last time
    {
        let last = get_last_command().lock();
        if *last == key {
            return true; // Already sent this exact command
        }
    }

    // Try Mach IPC first
    if send_mach_fn(args) {
        *get_last_command().lock() = key;
        return true;
    }

    if connect_mach_fn() && send_mach_fn(args) {
        *get_last_command().lock() = key;
        return true;
    }

    // Fall back to CLI
    if send_cli_fn(args) {
        *get_last_command().lock() = key;
        return true;
    }

    false
}

fn send_animation_frame(args: &[String]) -> bool {
    send_animation_frame_with(args, |args| {
        send_mach_with_timeout(args, MACH_FRAME_SEND_TIMEOUT_MS)
    })
}

fn send_animation_frame_with(
    args: &[String],
    mut send_mach_fn: impl FnMut(&[String]) -> bool,
) -> bool {
    // Animation frames must not replace the base configuration in the command
    // cache. That configuration is what lets repeated focus changes skip a
    // needless JankyBorders reconfiguration.
    send_mach_fn(args)
}

/// Builds the blacklist string for `JankyBorders`.
fn build_blacklist() -> String {
    let config = get_config();
    let mut apps: Vec<String> = Vec::new();

    // Add built-in skip lists from rules module
    apps.extend(SKIP_TILING_BUNDLE_IDS.iter().map(|s| (*s).to_string()));
    apps.extend(SKIP_TILING_APP_NAMES.iter().map(|s| (*s).to_string()));

    // Add user-configured ignore rules (app names and bundle IDs)
    for rule in &config.tiling.ignore {
        if let Some(app_id) = &rule.app_id {
            apps.push(app_id.clone());
        }
        if let Some(app_name) = &rule.app_name {
            apps.push(app_name.clone());
        }
    }

    // Add border-specific ignore rules
    for rule in &config.tiling.borders.ignore {
        if let Some(app_id) = &rule.app_id {
            apps.push(app_id.clone());
        }
        if let Some(app_name) = &rule.app_name {
            apps.push(app_name.clone());
        }
    }

    apps.join(",")
}

// ============================================================================
// Public API
// ============================================================================

/// Initializes the border system.
///
/// Sets up `JankyBorders` with:
/// - Style settings (width, style, hidpi)
/// - Blacklist of ignored apps
/// - Initial colors (unfocused always, active based on initial layout)
#[must_use]
pub fn init() -> bool {
    let config = get_config();

    if !config.tiling.borders.is_enabled() {
        tracing::debug!("tiling: borders disabled in config");
        return true;
    }

    if !is_available() {
        tracing::warn!("tiling: JankyBorders not found");
        return false;
    }

    // Connect via Mach IPC
    if connect_mach() {
        tracing::debug!("tiling: connected to JankyBorders via Mach IPC");
    }

    // Build initial command with all settings
    let borders = &config.tiling.borders;
    let blacklist = build_blacklist();

    // Get style settings
    let width = borders.focused.width().unwrap_or(4);
    let style = borders.style.as_deref().unwrap_or("round");
    let style_char = if style == "square" { 's' } else { 'r' };
    let hidpi = if borders.hidpi.unwrap_or(true) {
        "on"
    } else {
        "off"
    };

    // Get unfocused color (always needed)
    let (inactive_color, _) = get_border_settings(&borders.unfocused);

    // Get initial active color (default to focused)
    let (active_color, _) = get_border_settings(&borders.focused);

    // Build and send the initial command
    let args = vec![
        format!("width={width}"),
        format!("style={style_char}"),
        format!("hidpi={hidpi}"),
        format!("active_color={active_color}"),
        format!("inactive_color={inactive_color}"),
        format!("blacklist={blacklist}"),
    ];

    // Clear the cache so first command always sends
    *get_last_command().lock() = String::new();

    if !send_command(&args) {
        tracing::warn!("tiling: failed to initialize borders");
        return false;
    }

    tracing::debug!("tiling: borders initialized");

    // Start the single animation runner thread
    init_animation_runner();

    true
}

/// Whether the border system is paused.
///
/// The animation runner thread lives for the process lifetime; this flag
/// makes it inert during a tiling pause.
static PAUSED: AtomicBool = AtomicBool::new(false);

/// Pauses the border system: marks the runner inactive, clears the
/// last-command cache, and sends a zero-width/hidden border command so no
/// stale borders linger on screen.
pub fn pause() {
    PAUSED.store(true, Ordering::SeqCst);

    let args = vec![
        "width=0".to_string(),
        "active_color=0x00000000".to_string(),
        "inactive_color=0x00000000".to_string(),
    ];
    {
        let _send_lock = get_border_send_lock().lock();
        ANIMATION_EPOCH.fetch_add(1, Ordering::SeqCst);
        *get_last_command().lock() = String::new();
        if !send_command(&args) {
            tracing::debug!("tiling: borders pause hide command not sent (borders unavailable)");
        }
    }

    tracing::debug!("tiling: borders paused");
}

/// Resumes the border system and refreshes it against fresh tiling state.
pub fn resume() {
    PAUSED.store(false, Ordering::SeqCst);
    refresh();
    tracing::debug!("tiling: borders resumed");
}

/// Returns whether the border system is paused.
#[must_use]
pub fn is_paused() -> bool { PAUSED.load(Ordering::SeqCst) }

/// Prepares a focus-change border update under the send lock.
///
/// Returns `Some(epoch)` when the base command was sent (the animation for
/// that epoch may run). Returns `None` when the command was deduplicated
/// against the cache or the send failed; in both cases no animation should
/// be queued.
///
/// On a failed send the cache is cleared: an interrupted animation can leave
/// the active color mid-gradient, so the cached command no longer reflects
/// the borders state and the next focus change (even with identical
/// arguments) must retry the base command.
fn prepare_focus_command(args: &[String], send: impl FnOnce(&[String]) -> bool) -> Option<u64> {
    let _send_lock = get_border_send_lock().lock();
    if *get_last_command().lock() == command_key(args) {
        return None;
    }

    // Invalidate any existing animation before the base command so no stale
    // frame can overwrite the new configuration.
    let epoch = ANIMATION_EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
    if !send(args) {
        tracing::warn!("tiling: FAILED to send border command");
        *get_last_command().lock() = String::new();
        return None;
    }
    *get_last_command().lock() = command_key(args);
    Some(epoch)
}

/// Updates borders based on workspace layout.
///
/// Called when focus changes. Determines the correct active color based on:
/// - Monocle layout → monocle config (if enabled)
/// - Floating layout → floating config (if enabled)
/// - Otherwise → focused config
///
/// Always sends unfocused color as `inactive_color`.
/// All settings are batched into a single `JankyBorders` call.
pub fn on_focus_changed(layout: LayoutType, is_window_floating: bool) {
    if PAUSED.load(Ordering::SeqCst) {
        tracing::trace!("tiling: borders paused, ignoring focus change");
        return;
    }

    let config = get_config();
    let borders = &config.tiling.borders;

    if !borders.is_enabled() {
        tracing::debug!("tiling: borders not enabled, skipping focus change");
        return;
    }

    let config_layout = match layout {
        LayoutType::Floating => ConfigLayoutType::Floating,
        LayoutType::Dwindle => ConfigLayoutType::Dwindle,
        LayoutType::Monocle => ConfigLayoutType::Monocle,
        LayoutType::Master => ConfigLayoutType::Master,
        LayoutType::Split => ConfigLayoutType::Split,
        LayoutType::SplitVertical => ConfigLayoutType::SplitVertical,
        LayoutType::SplitHorizontal => ConfigLayoutType::SplitHorizontal,
        LayoutType::Grid => ConfigLayoutType::Grid,
    };

    // Determine which config to use for active color
    let active_config = borders.focused_state_config(config_layout, is_window_floating);

    // Get colors and width
    let (active_color, width) = get_border_settings(active_config);
    let (inactive_color, _) = get_border_settings(&borders.unfocused);

    tracing::debug!(
        "tiling: focus changed layout={:?} floating={is_window_floating} \
         config={:?} has_animation={}",
        config_layout,
        active_config,
        animated_gradient_parts(active_config).is_some(),
    );

    let args = vec![
        format!("width={width}"),
        format!("active_color={active_color}"),
        format!("inactive_color={inactive_color}"),
    ];

    let animation = animated_gradient_parts(active_config).map(|(g, a)| (g.clone(), a.clone()));

    // JankyBorders tracks the active window itself. Avoid reconfiguring it
    // when a focus change stays in the same border state; that work delays its
    // native focus render and is especially costly while a gradient animates.
    let Some(epoch) = prepare_focus_command(&args, send_command) else {
        return;
    };

    init_animation_runner();

    let tx = get_animation_tx().lock().as_ref().cloned();
    let command = AnimationCommand::Update { epoch, animation };

    let Some(tx) = tx else {
        tracing::warn!("tiling: border animation runner not available");
        return;
    };

    if tx.send(command).is_err() {
        tracing::warn!("tiling: FAILED to queue border command on focus change");
    }
}

/// Refreshes border configuration.
///
/// Call this when configuration is reloaded.
pub fn refresh() {
    {
        // Stop an existing animation before applying reloaded settings.
        let _send_lock = get_border_send_lock().lock();
        ANIMATION_EPOCH.fetch_add(1, Ordering::SeqCst);
        *get_last_command().lock() = String::new();
    }

    // Re-initialize
    let _ = init();
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the shared border globals
    /// (`LAST_COMMAND`, `ANIMATION_EPOCH`), which run in parallel by default.
    static BORDER_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_rgba_to_hex() {
        let rgba = Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        assert_eq!(rgba_to_hex(&rgba), "0xFFFF0000");
    }

    #[test]
    fn test_hex_to_janky() {
        assert_eq!(hex_to_janky("#FF0000"), Some("0xFFFF0000".to_string()));
        assert_eq!(hex_to_janky("#00FF00"), Some("0xFF00FF00".to_string()));
    }

    #[test]
    fn test_lerp_rgba_midpoint() {
        let from = Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        let to = Rgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };

        let color = lerp_rgba(&from, &to, 0.5);

        assert!((color.r - 0.5).abs() < f64::EPSILON);
        assert!((color.g - 0.0).abs() < f64::EPSILON);
        assert!((color.b - 0.5).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_gradient_color_string_uses_interpolated_from_and_to() {
        let from = Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 };
        let to = Rgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0 };

        let color = animated_gradient_color(&from, &to, 180.0, 0.5);

        assert_eq!(
            color,
            "gradient(top_right=0xFF800080,bottom_left=0xFF800080)".to_string()
        );
    }

    #[test]
    fn test_gradient_to_janky_preserves_180_degree_boundary() {
        let color = gradient_to_janky("0xFFFF0000", "0xFF0000FF", 180.0);

        assert_eq!(
            color,
            "gradient(top_right=0xFFFF0000,bottom_left=0xFF0000FF)".to_string()
        );
    }

    #[test]
    fn test_gradient_with_animation_is_animatable() {
        let config = BorderStateConfig::GradientColor {
            width: 6,
            gradient: GradientConfig {
                from: "#cba6f7".to_string(),
                to: "#a6e3a1".to_string(),
                angle: 180.0,
            },
            animation: Some(BorderAnimationConfig {
                duration: 350,
                easing: crate::config::EasingType::EaseOutExpo,
            }),
        };

        assert!(animated_gradient_parts(&config).is_some());
    }

    #[test]
    fn test_solid_color_is_not_animatable() {
        let config = BorderStateConfig::SolidColor {
            width: 6,
            color: "#cba6f7".to_string(),
        };

        assert!(animated_gradient_parts(&config).is_none());
    }

    #[test]
    fn test_get_border_settings_disabled() {
        let config = BorderStateConfig::Disabled(false);
        let (color, width) = get_border_settings(&config);
        assert_eq!(color, "0x00000000");
        assert_eq!(width, 0);
    }

    #[test]
    fn test_encode_mach_args_uses_nul_separated_argv() {
        let args = vec![
            "width=6".to_string(),
            "active_color=0xFFFF0000".to_string(),
            "inactive_color=0x00000000".to_string(),
        ];

        let payload = encode_mach_args(&args);

        assert_eq!(
            payload,
            b"width=6\0active_color=0xFFFF0000\0inactive_color=0x00000000\0\0".to_vec()
        );
    }

    #[test]
    fn test_command_key_distinguishes_argument_boundaries() {
        let args = vec!["width=6".to_string(), "active_color=0xFFFF0000".to_string()];

        assert_eq!(command_key(&args), "width=6\0active_color=0xFFFF0000");
    }

    #[test]
    fn test_mach_ool_descriptor_layout_matches_macos() {
        assert_eq!(std::mem::offset_of!(MachMsgOolDescriptor, address), 0);
        assert_eq!(std::mem::offset_of!(MachMsgOolDescriptor, deallocate), 8);
        assert_eq!(std::mem::offset_of!(MachMsgOolDescriptor, copy), 9);
        assert_eq!(std::mem::offset_of!(MachMsgOolDescriptor, pad1), 10);
        assert_eq!(std::mem::offset_of!(MachMsgOolDescriptor, type_), 11);
        assert_eq!(std::mem::offset_of!(MachMsgOolDescriptor, size), 12);
        assert_eq!(std::mem::size_of::<MachMsgOolDescriptor>(), 16);
    }

    #[test]
    fn test_mach_message_size_matches_kernel_expectation() {
        assert_eq!(std::mem::size_of::<MachMsgHeader>(), 24);
        assert_eq!(std::mem::size_of::<MachMsgBody>(), 4);
        // Kernel reads descriptors immediately after body (no alignment padding)
        assert_eq!(std::mem::size_of::<MachMessage>(), 44);
    }

    #[test]
    fn test_is_available_uses_binary_resolver() {
        assert!(is_available_with_resolver(|binary| {
            assert_eq!(binary, "borders");
            Ok(std::path::PathBuf::from("/opt/homebrew/bin/borders"))
        }));
        assert!(!is_available_with_resolver(|_| Err("missing".to_string())));
    }

    #[test]
    fn test_failed_commands_are_not_cached() {
        let _test_lock = BORDER_TEST_LOCK.lock();
        let args = vec!["active_color=0xFFFF0000".to_string()];
        let expected_key = command_key(&args);
        *get_last_command().lock() = String::new();

        let failed = send_command_with(&args, |_| false, || false, |_| false);

        assert!(!failed);
        assert_ne!(*get_last_command().lock(), expected_key);

        let retried = send_command_with(&args, |_| false, || false, |_| true);

        assert!(retried);
        assert_eq!(*get_last_command().lock(), expected_key);
    }

    #[test]
    fn test_focus_command_dedup_skips_send_and_keeps_cache() {
        let _test_lock = BORDER_TEST_LOCK.lock();
        let args = vec!["active_color=0xFFFF0000".to_string()];
        let key = command_key(&args);
        *get_last_command().lock() = key.clone();

        let mut send_called = false;
        let epoch = prepare_focus_command(&args, |_| {
            send_called = true;
            true
        });

        assert_eq!(epoch, None);
        assert!(!send_called, "deduplicated command must not be sent");
        assert_eq!(*get_last_command().lock(), key);
    }

    #[test]
    fn test_focus_command_send_failure_clears_cache_for_retry() {
        let _test_lock = BORDER_TEST_LOCK.lock();
        let args = vec!["active_color=0xFFFF0000".to_string()];
        let key = command_key(&args);
        *get_last_command().lock() = "active_color=0x00000000".to_string();
        let epoch_before = ANIMATION_EPOCH.load(Ordering::SeqCst);

        // Failed send: no epoch is returned, and the stale cache is cleared
        // so a later identical focus change retries the base command.
        let epoch = prepare_focus_command(&args, |_| false);
        assert_eq!(epoch, None);
        assert_eq!(*get_last_command().lock(), "");
        assert!(
            ANIMATION_EPOCH.load(Ordering::SeqCst) > epoch_before,
            "failed update must still invalidate a running animation"
        );

        // Retry with identical arguments must re-attempt the send.
        let epoch = prepare_focus_command(&args, |_| true);
        assert!(epoch.is_some(), "retry after failure must send again");
        assert_eq!(*get_last_command().lock(), key);
    }

    #[test]
    fn test_focus_command_success_caches_and_bumps_epoch() {
        let _test_lock = BORDER_TEST_LOCK.lock();
        let args = vec!["active_color=0xFFFF0000".to_string()];
        let key = command_key(&args);
        *get_last_command().lock() = String::new();
        let epoch_before = ANIMATION_EPOCH.load(Ordering::SeqCst);

        let epoch = prepare_focus_command(&args, |_| true);

        assert!(epoch.is_some_and(|e| e > epoch_before));
        assert_eq!(*get_last_command().lock(), key);
    }

    #[test]
    fn test_mach_send_options_apply_timeout() {
        assert_eq!(MACH_SEND_OPTIONS, MACH_SEND_MSG | MACH_SEND_TIMEOUT);
    }

    #[test]
    fn test_animation_frame_failure_is_dropped_without_caching() {
        let _test_lock = BORDER_TEST_LOCK.lock();
        let args = vec!["active_color=0xFFFF0000".to_string()];
        let expected_key = command_key(&args);
        *get_last_command().lock() = String::new();

        let sent = send_animation_frame_with(&args, |_| false);

        assert!(!sent);
        assert_ne!(*get_last_command().lock(), expected_key);
    }

    #[test]
    fn test_animation_wait_returns_queued_command_before_next_frame() {
        let (tx, rx) = mpsc::channel();
        tx.send(AnimationCommand::Update { epoch: 0, animation: None }).unwrap();

        let command = wait_for_animation_command(&rx, Duration::from_secs(1))
            .expect("channel should stay connected")
            .expect("queued command should be returned");

        let AnimationCommand::Update { epoch, animation } = command;
        assert_eq!(epoch, 0);
        assert!(animation.is_none());
    }

    #[test]
    fn test_animation_frame_duration_is_low_rate() {
        assert_eq!(BORDER_ANIMATION_FPS, 8);
        assert_eq!(animation_frame_duration(), Duration::from_millis(125));
    }

    #[test]
    fn test_animation_wait_does_not_hold_border_send_lock() {
        let _test_lock = BORDER_TEST_LOCK.lock();
        ANIMATION_EPOCH.store(1, Ordering::SeqCst);

        let (tx, rx) = mpsc::channel();
        let (frame_sent_tx, frame_sent_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_animation_with_frame_sender_and_wait(
                &rx,
                &GradientConfig::default(),
                &BorderAnimationConfig {
                    duration: 16,
                    easing: crate::config::types::tiling::EasingType::Linear,
                },
                1,
                |_| true,
                |rx, frame_duration| {
                    frame_sent_tx.send(()).unwrap();
                    wait_for_animation_command(rx, frame_duration)
                },
            )
        });

        frame_sent_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let send_lock = get_border_send_lock().try_lock();
        assert!(send_lock.is_some());
        drop(send_lock);

        tx.send(AnimationCommand::Update { epoch: 1, animation: None }).unwrap();
        assert!(worker.join().unwrap().is_some());
    }

    #[test]
    fn test_take_queued_animation_command_returns_pending_update() {
        let (tx, rx) = mpsc::channel();
        tx.send(AnimationCommand::Update { epoch: 0, animation: None }).unwrap();

        let command =
            take_queued_animation_command(&rx).expect("queued command should be returned");

        let AnimationCommand::Update { epoch, animation } = command;
        assert_eq!(epoch, 0);
        assert!(animation.is_none());
    }

    #[test]
    fn test_pause_and_resume_flags() {
        let _test_lock = BORDER_TEST_LOCK.lock();
        resume(); // ensure clean initial state
        assert!(!is_paused());

        pause();
        assert!(is_paused());

        resume();
        assert!(!is_paused());
    }
}
