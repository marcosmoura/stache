//! TI-09-01 to TI-09-04: borders policy and runtime over fakes only. No test
//! looks a service up, sends a Mach message or spawns a process.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_macos::borders::testing::FakeTransport;
use stache_macos::borders::{CliRunner, MACH_SEND_TIMED_OUT, Rejection, SendOutcome};

use super::fakes::{Cli, Net};
use super::policy::{
    FRAME_PERIOD, FocusInput, PingPong, base_args, blacklist, focus_command, focused_state,
    parse_color, state_settings,
};
use super::runtime::{Core, Settings, Update};
use super::{BordersControl, BordersParts, Fault, FaultKind, FaultSink};
use crate::actor::{Clock, ManualClock};
use crate::config::{
    BorderAnimationConfig, BorderStateConfig, BorderStyle, BordersConfig, EasingType,
    GradientConfig, LayoutType, TilingConfig, WindowRule,
};

fn solid(color: &str) -> BorderStateConfig {
    BorderStateConfig::SolidColor {
        width: 6,
        color: color.to_owned(),
    }
}

fn gradient(angle: f64) -> BorderStateConfig {
    BorderStateConfig::GradientColor {
        width: 5,
        gradient: GradientConfig {
            from: "#FF0000".to_owned(),
            to: "#0000FF".to_owned(),
            angle,
        },
        animation: None,
    }
}

fn animated() -> BorderStateConfig {
    BorderStateConfig::GradientColor {
        width: 5,
        gradient: GradientConfig {
            from: "#FF0000".to_owned(),
            to: "#0000FF".to_owned(),
            angle: 90.0,
        },
        animation: Some(BorderAnimationConfig {
            duration: 1000,
            easing: EasingType::Linear,
        }),
    }
}

fn focus(layout: LayoutType, floating: bool) -> FocusInput { FocusInput { layout, floating } }

fn strings(args: &[&str]) -> Vec<String> { args.iter().map(|a| (*a).to_owned()).collect() }

// ------------------------------------------------------------------ policy

#[test]
fn default_base_batch_matches_the_legacy_init_argv() {
    let config = BordersConfig::default();
    let args = base_args(&config, "BL");
    assert_eq!(
        args,
        strings(&[
            "width=4",
            "style=r",
            "hidpi=on",
            "active_color=0xFFB4BEFE",
            "inactive_color=0xFF6C7086",
            "blacklist=BL",
        ])
    );
}

#[test]
fn style_hidpi_and_width_follow_the_config() {
    let config = BordersConfig {
        style: Some(BorderStyle::Square),
        hidpi: Some(false),
        focused: solid("#00FF00"),
        ..BordersConfig::default()
    };
    let args = base_args(&config, "x");
    assert_eq!(args[0], "width=6");
    assert_eq!(args[1], "style=s");
    assert_eq!(args[2], "hidpi=off");
    assert_eq!(args[3], "active_color=0xFF00FF00");
    // A disabled focused state still sends legacy's width 4 and a transparent
    // active color.
    let off = BordersConfig {
        focused: BorderStateConfig::Disabled(false),
        ..config
    };
    let args = base_args(&off, "x");
    assert_eq!(args[0], "width=4");
    assert_eq!(args[3], "active_color=0x00000000");
}

#[test]
fn color_conversion_fixtures() {
    let janky = |color: &str| state_settings(&solid(color)).0;
    assert_eq!(janky("#F00"), "0xFFFF0000");
    assert_eq!(janky("#F008"), "0x88FF0000");
    assert_eq!(janky("#FF000080"), "0x80FF0000");
    assert_eq!(janky("0000FF"), "0xFF0000FF");
    assert_eq!(janky("  #00ff00  "), "0xFF00FF00");
    // The config validates rgb()/rgba(); they are honoured, not erased.
    assert_eq!(janky("rgb(0, 128, 255)"), "0xFF0080FF");
    assert_eq!(janky("rgba(255, 0, 0, 0.5)"), "0x80FF0000");
    // An unparsable color is transparent and keeps its width.
    assert_eq!(state_settings(&solid("zz")), ("0x00000000".to_owned(), 6));
    assert!(parse_color("#12345").is_err());
    assert!(parse_color("rgba(1, 2, 3)").is_err());
    assert!(parse_color("rgba(1, 2, 3, 2)").is_err());
    assert!(parse_color("#ééé").is_err());
}

#[test]
fn glow_and_disabled_states() {
    let glow = BorderStateConfig::GlowColor {
        width: 8,
        glow: "#B4BEFE".to_owned(),
    };
    assert_eq!(state_settings(&glow), ("glow(0xFFB4BEFE)".to_owned(), 8));
    assert_eq!(
        state_settings(&BorderStateConfig::Disabled(false)),
        ("0x00000000".to_owned(), 0)
    );
}

#[test]
fn gradient_angle_boundaries() {
    let top_right = "gradient(top_right=0xFFFF0000,bottom_left=0xFF0000FF)";
    let top_left = "gradient(top_left=0xFFFF0000,bottom_right=0xFF0000FF)";
    for (angle, expected) in [
        (0.0, top_right),
        (89.9, top_right),
        (90.0, top_left),
        (179.9, top_left),
        (180.0, top_right),
        (269.9, top_right),
        (270.0, top_left),
        (359.9, top_left),
        (360.0, top_right),
        (-90.0, top_left),
        (-1.0, top_left),
        (450.0, top_left),
        (720.0, top_right),
    ] {
        assert_eq!(
            state_settings(&gradient(angle)),
            (expected.to_owned(), 5),
            "angle {angle}"
        );
    }
}

#[test]
fn state_precedence_matches_focused_state_config() {
    let mut config = BordersConfig {
        focused: solid("#111111"),
        floating: solid("#222222"),
        monocle: solid("#333333"),
        dwindle: Some(solid("#444444")),
        master: Some(BorderStateConfig::Disabled(false)),
        ..BordersConfig::default()
    };
    let pick = |config: &BordersConfig, layout, floating| {
        state_settings(focused_state(config, focus(layout, floating))).0
    };
    // Floating layout or floating window wins while its state is enabled.
    assert_eq!(pick(&config, LayoutType::Floating, false), "0xFF222222");
    assert_eq!(pick(&config, LayoutType::Dwindle, true), "0xFF222222");
    // Monocle always has a state; dwindle overrides; master is disabled so it
    // falls back to focused; grid has none.
    assert_eq!(pick(&config, LayoutType::Monocle, false), "0xFF333333");
    assert_eq!(pick(&config, LayoutType::Dwindle, false), "0xFF444444");
    assert_eq!(pick(&config, LayoutType::Master, false), "0xFF111111");
    assert_eq!(pick(&config, LayoutType::Grid, false), "0xFF111111");
    assert_eq!(pick(&config, LayoutType::Split, false), "0xFF111111");
    // A disabled floating state falls back through the layout, then focused.
    config.floating = BorderStateConfig::Disabled(false);
    assert_eq!(pick(&config, LayoutType::Dwindle, true), "0xFF444444");
    assert_eq!(pick(&config, LayoutType::Floating, false), "0xFF111111");
    assert_eq!(pick(&config, LayoutType::Grid, true), "0xFF111111");
}

#[test]
fn focus_command_carries_width_active_inactive_and_the_animation() {
    let config = BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    };
    let plain = focus_command(&config, focus(LayoutType::Grid, false));
    assert_eq!(
        plain.args,
        strings(&[
            "width=4",
            "active_color=0xFFB4BEFE",
            "inactive_color=0xFF6C7086",
        ])
    );
    assert!(plain.animation.is_none());
    let moving = focus_command(&config, focus(LayoutType::Monocle, false));
    assert_eq!(moving.args[0], "width=5");
    assert!(moving.animation.is_some());
    // An unparsable animated gradient does not animate.
    let broken = BordersConfig {
        monocle: BorderStateConfig::GradientColor {
            width: 5,
            gradient: GradientConfig {
                from: "zz".to_owned(),
                to: "#00F".to_owned(),
                angle: 0.0,
            },
            animation: Some(BorderAnimationConfig {
                duration: 100,
                easing: EasingType::Linear,
            }),
        },
        ..BordersConfig::default()
    };
    assert!(focus_command(&broken, focus(LayoutType::Monocle, false)).animation.is_none());
}

#[test]
fn ping_pong_frames_reverse_at_each_leg_end() {
    let config = BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    };
    let plan = focus_command(&config, focus(LayoutType::Monocle, false))
        .animation
        .expect("plan");
    let mut cursor = PingPong::new(plan);
    let ms = Duration::from_millis;
    assert_eq!(
        cursor.frame(ms(0)),
        "active_color=gradient(top_left=0xFFFF0000,bottom_right=0xFF0000FF)"
    );
    let mid = "active_color=gradient(top_left=0xFF800080,bottom_right=0xFF800080)";
    assert_eq!(cursor.frame(ms(500)), mid);
    // The leg ends: fully swapped, then the direction flips.
    assert_eq!(
        cursor.frame(ms(1000)),
        "active_color=gradient(top_left=0xFF0000FF,bottom_right=0xFFFF0000)"
    );
    assert_eq!(cursor.frame(ms(1500)), mid, "the way back passes the midpoint");
    assert_eq!(
        cursor.frame(ms(2000)),
        "active_color=gradient(top_left=0xFFFF0000,bottom_right=0xFF0000FF)"
    );
}

#[test]
fn a_short_animation_leg_is_clamped_to_16_ms() {
    let config = BordersConfig {
        monocle: BorderStateConfig::GradientColor {
            width: 5,
            gradient: GradientConfig {
                from: "#FF0000".to_owned(),
                to: "#0000FF".to_owned(),
                angle: 90.0,
            },
            animation: Some(BorderAnimationConfig {
                duration: 1,
                easing: EasingType::Linear,
            }),
        },
        ..BordersConfig::default()
    };
    let plan = focus_command(&config, focus(LayoutType::Monocle, false))
        .animation
        .expect("plan");
    let mut cursor = PingPong::new(plan);
    // Half of a 16 ms leg is the midpoint, not an instant swap.
    assert_eq!(
        cursor.frame(Duration::from_millis(8)),
        "active_color=gradient(top_left=0xFF800080,bottom_right=0xFF800080)"
    );
}

#[test]
fn blacklist_composes_system_tiling_and_border_rules_in_legacy_order() {
    let tiling = [
        WindowRule {
            app_id: Some("com.tile".to_owned()),
            app_name: Some("Tile".to_owned()),
            title: None,
        },
        WindowRule {
            title: Some("only a title".to_owned()),
            ..WindowRule::default()
        },
    ];
    let borders = [WindowRule {
        app_name: Some("Border".to_owned()),
        ..WindowRule::default()
    }];
    let list = blacklist(&tiling, &borders);
    assert!(
        list.starts_with("com.apple.dock,com.apple.SystemUIServer,"),
        "{list}"
    );
    assert!(
        list.ends_with(",loginwindow,borders,com.tile,Tile,Border"),
        "{list}"
    );
    let bundles = crate::rules::SKIP_TILING_BUNDLE_IDS.join(",");
    let names = crate::rules::SKIP_TILING_APP_NAMES.join(",");
    assert_eq!(list, format!("{bundles},{names},com.tile,Tile,Border"));
}

#[test]
fn argument_boundaries_not_concatenation_decide_equality() {
    let left = strings(&["a b", "c"]);
    let right = strings(&["a", "b c"]);
    assert_ne!(left, right, "same words, different argument boundaries");
    let (first, second) = (strings(&["ab", "c"]), strings(&["a", "bc"]));
    assert_eq!(first.concat(), second.concat(), "the same concatenation");
    assert_ne!(first, second, "but a different command");
}

// -------------------------------------------------------------------- core

struct Rig {
    core: Core,
    fake: FakeTransport,
    cli: Arc<Cli>,
    clock: Arc<ManualClock>,
    faults: Arc<Mutex<Vec<Option<FaultKind>>>>,
    delivered: Arc<AtomicBool>,
}

fn rig_with(fake: FakeTransport, virtual_time: bool) -> Rig {
    let cli = Cli::new();
    let clock = Arc::new(ManualClock::default());
    let faults = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&faults);
    let sink: FaultSink = Arc::new(move |fault| {
        log.lock().expect("log").push(fault.map(|fault| fault.kind));
    });
    let delivered = Arc::new(AtomicBool::new(false));
    let core = Core::new(
        Box::new(fake.clone()),
        CliRunner::new(cli.executor(virtual_time)),
        stache_sdk::CancellationToken::new(),
        Arc::clone(&clock) as Arc<dyn Clock>,
        FRAME_PERIOD,
        sink,
        Arc::clone(&delivered),
    );
    Rig {
        core,
        fake,
        cli,
        clock,
        faults,
        delivered,
    }
}

fn rig() -> Rig { rig_with(FakeTransport::present(), true) }

fn settings(config: BordersConfig) -> Arc<Settings> {
    let tiling = TilingConfig {
        borders: BordersConfig { enabled: true, ..config },
        ..TilingConfig::default()
    };
    Arc::new(Settings::from_config(&tiling).expect("enabled"))
}

fn configure(settings: &Arc<Settings>) -> Update {
    Update {
        settings: Some(Arc::clone(settings)),
        ..Update::default()
    }
}

fn focused(input: FocusInput) -> Update {
    Update {
        focus: Some(input),
        ..Update::default()
    }
}

impl Rig {
    fn last_fault(&self) -> Option<FaultKind> {
        self.faults.lock().expect("faults").last().copied().flatten()
    }
}

#[test]
fn the_base_batch_then_the_focus_command_are_sent_in_order() {
    let mut rig = rig();
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 2);
    let sends = rig.fake.sends();
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0].args, base_args(&settings.config, &settings.blacklist));
    assert_eq!(
        sends[1].args,
        focus_command(&settings.config, focus(LayoutType::Dwindle, false)).args
    );
    assert!(rig.fake.lookups() >= 2, "every attempt looks up first");
    assert!(rig.delivered.load(std::sync::atomic::Ordering::SeqCst));
    assert!(rig.cli.spawned().is_empty());
}

#[test]
fn identical_commands_are_deduplicated_by_argument_vectors() {
    let mut rig = rig();
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 2);
    // Another layout with no override sends the very same argv: nothing is sent.
    rig.core.apply(focused(focus(LayoutType::Grid, false)), 3);
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 4);
    assert_eq!(rig.fake.sends().len(), 2, "base + one focus command");
    // A different state sends a different command.
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 5);
    assert_eq!(rig.fake.sends().len(), 3);
}

#[test]
fn a_stale_frame_cannot_overwrite_a_newer_focus() {
    let mut rig = rig();
    let config = BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    };
    let settings = settings(config);
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 2);
    assert!(rig.core.animating());
    let before = rig.fake.sends().len();
    // A newer update was deposited (epoch 3) but not applied yet: the frame of
    // epoch 2 is dropped, never sent.
    rig.clock.advance(Duration::from_secs(1));
    rig.core.frame(3);
    assert_eq!(rig.fake.sends().len(), before, "no stale frame");
    // The current epoch may send.
    rig.core.frame(2);
    assert_eq!(rig.fake.sends().len(), before + 1);
    assert!(rig.fake.sends().last().expect("frame").args[0].starts_with("active_color=gradient("));
    // Applying a different focus replaces the animation: frames follow it only.
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 3);
    assert!(!rig.core.animating(), "the plain state has no animation");
    let sent = rig.fake.sends().len();
    rig.clock.advance(Duration::from_secs(1));
    rig.core.frame(3);
    assert_eq!(rig.fake.sends().len(), sent);
}

#[test]
fn an_unchanged_command_keeps_the_running_animation_under_the_new_epoch() {
    let mut rig = rig();
    let config = BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    };
    let settings = settings(config);
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 2);
    // Same argv again (a deposit with no visible change): the animation lives on.
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 3);
    assert!(rig.core.animating());
    rig.clock.advance(Duration::from_secs(1));
    let before = rig.fake.sends().len();
    rig.core.frame(2);
    assert_eq!(rig.fake.sends().len(), before, "epoch 2 is stale now");
    rig.core.frame(3);
    assert_eq!(rig.fake.sends().len(), before + 1);
}

#[test]
fn animation_frames_never_exceed_eight_per_second() {
    let mut rig = rig();
    let config = BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    };
    let settings = settings(config);
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 2);
    let base = rig.fake.sends().len();
    let started = rig.clock.now();
    let mut stamps = Vec::new();
    let mut seen = base;
    // 10 ms ticks over 4 seconds of fake time, polled far more often than 8 Hz.
    for _ in 0..400 {
        rig.core.frame(2);
        let now = rig.fake.sends().len();
        if now > seen {
            stamps.push(rig.clock.now().saturating_duration_since(started));
            seen = now;
        }
        rig.clock.advance(Duration::from_millis(10));
    }
    let frames = seen - base;
    assert!(
        (30..=33).contains(&frames),
        "8 fps over 4 s is about 32 frames, got {frames}"
    );
    for pair in stamps.windows(2) {
        let spacing = pair[1].saturating_sub(pair[0]);
        assert!(spacing >= FRAME_PERIOD, "spacing {spacing:?}");
    }
}

#[test]
fn a_burst_of_new_animations_still_respects_the_frame_period() {
    let mut rig = rig();
    let config = BordersConfig {
        monocle: animated(),
        dwindle: Some(BorderStateConfig::GradientColor {
            width: 7,
            gradient: GradientConfig {
                from: "#00FF00".to_owned(),
                to: "#0000FF".to_owned(),
                angle: 0.0,
            },
            animation: Some(BorderAnimationConfig {
                duration: 500,
                easing: EasingType::Linear,
            }),
        }),
        ..BordersConfig::default()
    };
    let settings = settings(config);
    rig.core.apply(configure(&settings), 1);
    let mut epoch = 1;
    let mut frames = 0usize;
    // Alternate two animated states every 10 ms for one second.
    for step in 0..100 {
        epoch += 1;
        let layout = if step % 2 == 0 {
            LayoutType::Monocle
        } else {
            LayoutType::Dwindle
        };
        let before = rig.fake.sends().len();
        rig.core.apply(focused(focus(layout, false)), epoch);
        rig.core.frame(epoch);
        frames += rig.fake.sends().len() - before - 1; // minus the focus command
        rig.clock.advance(Duration::from_millis(10));
    }
    assert!(
        frames <= 9,
        "at most one frame per period over one second, got {frames}"
    );
}

#[test]
fn a_missing_service_sends_nothing_and_spawns_no_cli() {
    let mut rig = rig_with(FakeTransport::missing(), true);
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 2);
    assert!(rig.fake.sends().is_empty(), "nothing is sent without a service");
    assert!(rig.cli.spawned().is_empty(), "no CLI fallback without a service");
    assert!(rig.fake.lookups() >= 1);
    assert_eq!(rig.last_fault(), Some(FaultKind::MissingService));
    assert!(!rig.delivered.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn the_service_appearing_later_is_used_only_on_a_new_update() {
    let mut rig = rig_with(FakeTransport::missing(), true);
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    assert!(rig.fake.sends().is_empty());
    rig.fake.set_present(true);
    // No autonomous retry: still nothing until a new update arrives.
    rig.core.frame(1);
    assert!(rig.fake.sends().is_empty());
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 2);
    assert_eq!(rig.fake.sends().len(), 2, "base first, then the focus command");
    assert_eq!(rig.last_fault(), None);
    assert_eq!(rig.faults.lock().expect("faults").last(), Some(&None), "cleared");
}

#[test]
fn a_mach_rejection_after_a_lookup_falls_back_to_the_cli_once() {
    let mut rig = rig();
    // A message-validation rejection (MACH_SEND_TOO_LARGE), not a dead destination.
    rig.fake.script(SendOutcome::Rejected(Rejection::Kernel(0x1000_000e)));
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    let args = base_args(&settings.config, &settings.blacklist);
    assert_eq!(rig.fake.sends().len(), 1);
    assert_eq!(rig.cli.spawned(), vec![args], "the exact argv, once");
    assert_eq!(rig.last_fault(), None, "the fallback delivered it");
    assert!(rig.delivered.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn a_rejection_relooks_up_and_runs_the_cli_once_while_the_service_is_present() {
    let mut rig = rig();
    rig.fake.script(SendOutcome::Rejected(Rejection::Kernel(0x1000_000e)));
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    assert_eq!(
        rig.fake.lookups(),
        2,
        "the attempt lookup and the pre-CLI lookup"
    );
    assert_eq!(rig.cli.spawned().len(), 1);
    assert_eq!(rig.last_fault(), None);
}

#[test]
fn a_service_that_vanishes_after_a_rejection_never_gets_the_cli() {
    let mut rig = rig();
    rig.fake.script(SendOutcome::Rejected(Rejection::Kernel(0x1000_000e)));
    rig.fake.vanish_on_send();
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    assert_eq!(rig.fake.sends().len(), 1);
    assert!(rig.cli.spawned().is_empty(), "the re-lookup failed, so no CLI");
    assert_eq!(rig.last_fault(), Some(FaultKind::MissingService));
    assert!(!rig.delivered.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn a_dead_destination_is_a_missing_service_and_never_runs_the_cli() {
    let mut rig = rig();
    rig.fake.script(SendOutcome::MissingService);
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    assert_eq!(rig.fake.sends().len(), 1);
    assert!(
        rig.cli.spawned().is_empty(),
        "no CLI after a proven-dead service"
    );
    assert_eq!(rig.last_fault(), Some(FaultKind::MissingService));
    assert!(!rig.delivered.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn a_failing_cli_fallback_is_a_protocol_fault_and_not_cached() {
    let mut rig = rig();
    rig.cli.set_exit(3);
    rig.fake.script(SendOutcome::Rejected(Rejection::Kernel(0x1000_000e)));
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    assert_eq!(rig.last_fault(), Some(FaultKind::Protocol));
    assert_eq!(rig.cli.spawned().len(), 1);
    // A new update tries the base command again over Mach.
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 2);
    assert_eq!(rig.fake.sends().len(), 3, "failed base, resent base, focus");
    assert_eq!(rig.last_fault(), None);
}

#[test]
fn an_uncertain_send_never_falls_back_or_retries() {
    for (code, kind) in [
        (MACH_SEND_TIMED_OUT, FaultKind::Timeout),
        (0x1000_0007, FaultKind::Protocol),
    ] {
        let mut rig = rig();
        rig.fake.script(SendOutcome::Uncertain(code));
        let settings = settings(BordersConfig::default());
        rig.core.apply(configure(&settings), 1);
        assert_eq!(rig.fake.sends().len(), 1, "one send, no retry");
        assert!(rig.cli.spawned().is_empty(), "no CLI after an ambiguous send");
        assert_eq!(rig.last_fault(), Some(kind));
        // No timer-driven retry either.
        rig.clock.advance(Duration::from_secs(10));
        rig.core.frame(1);
        assert_eq!(rig.fake.sends().len(), 1);
    }
}

#[test]
fn invalid_arguments_are_a_protocol_fault_without_a_fallback() {
    let mut rig = rig();
    rig.fake.script(SendOutcome::Rejected(Rejection::InvalidArguments(
        stache_macos::borders::EncodeError::InteriorNul { index: 5 },
    )));
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    assert_eq!(rig.last_fault(), Some(FaultKind::Protocol));
    assert!(rig.cli.spawned().is_empty());
}

#[test]
fn the_next_new_update_retries_a_failed_command_but_nothing_else_does() {
    let mut rig = rig();
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 2);
    assert_eq!(rig.fake.sends().len(), 2);
    // The focus command fails ambiguously.
    rig.fake.script(SendOutcome::Uncertain(MACH_SEND_TIMED_OUT));
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 3);
    assert_eq!(rig.fake.sends().len(), 3);
    assert_eq!(rig.last_fault(), Some(FaultKind::Timeout));
    // Time passing retries nothing (no retry loop).
    rig.clock.advance(Duration::from_secs(30));
    rig.core.frame(3);
    assert_eq!(rig.fake.sends().len(), 3);
    // A new update, even for the same input, tries the failed command again...
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 4);
    assert_eq!(rig.fake.sends().len(), 4);
    assert_eq!(rig.last_fault(), None);
    // ...and a delivered command is deduplicated again.
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 5);
    assert_eq!(rig.fake.sends().len(), 4);
}

#[test]
fn an_explicit_retry_resends_the_base_and_focus() {
    let mut rig = rig();
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Dwindle, false)), 2);
    rig.core.apply(
        Update {
            retry: true,
            ..Update::default()
        },
        3,
    );
    assert_eq!(rig.fake.sends().len(), 4);
}

#[test]
fn a_failed_frame_stops_the_animation_with_no_retry_loop() {
    let mut rig = rig();
    let config = BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    };
    let settings = settings(config);
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 2);
    rig.fake.script(SendOutcome::Uncertain(MACH_SEND_TIMED_OUT));
    rig.core.frame(2);
    let sent = rig.fake.sends().len();
    assert!(!rig.core.animating());
    assert_eq!(rig.last_fault(), Some(FaultKind::Timeout));
    for _ in 0..10 {
        rig.clock.advance(Duration::from_secs(1));
        rig.core.frame(2);
    }
    assert_eq!(rig.fake.sends().len(), sent);
    assert!(rig.cli.spawned().is_empty(), "frames never use the CLI");
}

#[test]
fn frames_look_the_service_up_and_stop_when_it_disappears() {
    let mut rig = rig();
    let config = BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    };
    let settings = settings(config);
    rig.core.apply(configure(&settings), 1);
    rig.core.apply(focused(focus(LayoutType::Monocle, false)), 2);
    rig.fake.set_present(false);
    let sent = rig.fake.sends().len();
    rig.core.frame(2);
    assert_eq!(rig.fake.sends().len(), sent);
    assert_eq!(rig.last_fault(), Some(FaultKind::MissingService));
    assert!(!rig.core.animating());
}

#[test]
fn a_hung_helper_child_that_cannot_be_reaped_is_retained_not_dropped() {
    let mut rig = rig();
    rig.cli.set_hang(true);
    rig.fake.script(SendOutcome::Rejected(Rejection::Kernel(0x1000_000e)));
    let settings = settings(BordersConfig::default());
    rig.core.apply(configure(&settings), 1);
    assert_eq!(rig.cli.spawned().len(), 1);
    assert!(matches!(
        rig.last_fault(),
        Some(FaultKind::Cleanup | FaultKind::Timeout)
    ));
    let parked = rig.core.with_reap_bound(Duration::from_millis(30)).finish().expect("retained");
    assert_eq!(parked.left.len(), 1, "the exact helper child is retained");
    assert_eq!(parked.cli.sweep().len(), 1);
}

#[test]
fn a_clean_finish_releases_the_connection_and_retains_nothing() {
    let rig = rig();
    assert!(rig.core.finish().is_none());
}

// -------------------------------------------------------------- the runner

const WAIT: Duration = Duration::from_secs(10);

fn parts(net: &Arc<Net>, cli: &Arc<Cli>) -> BordersParts {
    BordersParts::new(net.factory(), cli.factory())
}

fn enabled_config(config: BordersConfig) -> TilingConfig {
    TilingConfig {
        borders: BordersConfig { enabled: true, ..config },
        ..TilingConfig::default()
    }
}

fn control(parts: Option<BordersParts>) -> (BordersControl, Arc<Mutex<Vec<Option<Fault>>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    let sink: FaultSink = Arc::new(move |fault| sink_log.lock().expect("log").push(fault.cloned()));
    (
        BordersControl::new(parts, Arc::new(crate::actor::SystemClock), sink),
        log,
    )
}

fn last_fault(log: &Mutex<Vec<Option<Fault>>>) -> Option<Fault> {
    log.lock().expect("log").last().cloned().flatten()
}

#[test]
fn disabled_owns_no_runner_port_or_child() {
    let net = Net::new(true);
    let cli = Cli::new();
    let (control, log) = control(Some(parts(&net, &cli)));
    control.configure(&TilingConfig::default());
    control.observe(focus(LayoutType::Dwindle, false));
    assert!(!control.is_running());
    assert!(!control.owes_cleanup());
    assert_eq!((net.created(), net.lookups(), net.sends().len()), (0, 0, 0));
    assert!(cli.spawned().is_empty());
    assert!(log.lock().expect("log").iter().all(Option::is_none));
    assert!(!control.left_settings());
}

#[test]
fn without_composed_authority_an_enabled_config_only_reports() {
    let (control, log) = control(None);
    control.configure(&enabled_config(BordersConfig::default()));
    assert!(!control.is_running());
    let fault = last_fault(&log).expect("fault");
    assert_eq!(fault.kind, FaultKind::Authority);
    assert!(fault.describe().contains("no JankyBorders authority"));
}

#[test]
fn the_runner_sends_the_base_batch_then_the_latest_focus() {
    let net = Net::new(true);
    let cli = Cli::new();
    let (control, _) = control(Some(parts(&net, &cli)));
    let config = enabled_config(BordersConfig::default());
    control.configure(&config);
    assert!(net.wait_sends(1, WAIT));
    let settings = Settings::from_config(&config).expect("settings");
    assert_eq!(net.sends()[0], base_args(&settings.config, &settings.blacklist));
    control.observe(focus(LayoutType::Monocle, false));
    assert!(net.wait_sends(2, WAIT));
    assert_eq!(
        net.sends()[1],
        focus_command(&settings.config, focus(LayoutType::Monocle, false)).args
    );
    assert_eq!(net.created(), 1, "one transport, created on the runner thread");
    assert!(
        net.deadlines()
            .iter()
            .all(|d| *d <= stache_macos::borders::MACH_SEND_DEADLINE_MAX)
    );
    control.stop();
    assert!(control.join_within(WAIT));
}

#[test]
fn a_focus_seen_before_enabling_is_sent_when_the_runner_starts() {
    let net = Net::new(true);
    let cli = Cli::new();
    let (control, _) = control(Some(parts(&net, &cli)));
    control.observe(focus(LayoutType::Monocle, false));
    assert_eq!(net.created(), 0, "observing alone allocates no runner");
    control.configure(&enabled_config(BordersConfig::default()));
    assert!(net.wait_sends(2, WAIT));
    control.stop();
    assert!(control.join_within(WAIT));
}

#[test]
fn stop_releases_the_connection_and_sends_no_mutation() {
    let net = Net::new(true);
    let cli = Cli::new();
    let (control, _) = control(Some(parts(&net, &cli)));
    control.configure(&enabled_config(BordersConfig::default()));
    control.observe(focus(LayoutType::Dwindle, false));
    assert!(net.wait_sends(2, WAIT));
    let before = net.sends();
    control.stop();
    assert!(control.join_within(WAIT));
    assert_eq!(net.dropped(), 1, "the connection is released");
    assert_eq!(net.sends(), before, "stop sent nothing");
    assert!(
        net.sends().iter().flatten().all(|arg| arg != "width=0"),
        "no pause or hide command, ever"
    );
    assert!(cli.spawned().is_empty());
    assert!(control.left_settings(), "the service keeps what was sent");
    assert!(!control.owes_cleanup());
    // A stopped control never starts again and ignores updates.
    control.configure(&enabled_config(BordersConfig::default()));
    control.observe(focus(LayoutType::Monocle, false));
    assert_eq!(net.created(), 1);
}

#[test]
fn reload_enables_retunes_and_disables_live_without_a_mutation_on_disable() {
    let net = Net::new(true);
    let cli = Cli::new();
    let (control, _) = control(Some(parts(&net, &cli)));
    control.configure(&TilingConfig::default());
    assert!(!control.is_running());
    control.configure(&enabled_config(BordersConfig::default()));
    assert!(control.is_running());
    assert!(net.wait_sends(1, WAIT));
    // A retune reaches the one runner and re-sends the base batch.
    let retuned = BordersConfig {
        style: Some(BorderStyle::Square),
        ..BordersConfig::default()
    };
    control.configure(&enabled_config(retuned));
    assert!(net.wait_sends(2, WAIT));
    assert_eq!(net.sends()[1][1], "style=s");
    assert_eq!(net.created(), 1, "the runner was kept");
    let before = net.sends();
    control.configure(&TilingConfig::default());
    assert!(!control.is_running());
    assert!(control.join_within(WAIT));
    assert_eq!(net.dropped(), 1);
    assert_eq!(net.sends(), before, "reload-to-disabled sent nothing");
    // Enabling again starts a fresh runner and connection.
    control.configure(&enabled_config(BordersConfig::default()));
    assert!(net.wait_sends(before.len() + 1, WAIT));
    assert_eq!(net.created(), 2);
    control.stop();
    assert!(control.join_within(WAIT));
    assert_eq!(net.dropped(), 2);
}

#[test]
fn a_missing_service_is_a_diagnostic_with_no_send_and_no_cli() {
    let net = Net::new(false);
    let cli = Cli::new();
    let (control, log) = control(Some(parts(&net, &cli)));
    control.configure(&enabled_config(BordersConfig::default()));
    control.observe(focus(LayoutType::Dwindle, false));
    let deadline = Instant::now() + WAIT;
    while last_fault(&log).is_none() {
        assert!(Instant::now() < deadline, "no diagnostic");
        std::thread::sleep(Duration::from_millis(2));
    }
    let fault = last_fault(&log).expect("fault");
    assert_eq!(fault.kind, FaultKind::MissingService);
    assert!(fault.message.contains("git.felix.borders"));
    assert!(net.sends().is_empty());
    assert!(cli.spawned().is_empty());
    control.stop();
    assert!(control.join_within(WAIT));
    assert!(!control.left_settings(), "nothing was ever sent");
}

#[test]
fn cancellation_wakes_the_pacing_wait_promptly() {
    let net = Net::new(true);
    let cli = Cli::new();
    let mut parts = parts(&net, &cli);
    parts.frame_period = Duration::from_secs(60);
    let (control, _) = control(Some(parts));
    let config = enabled_config(BordersConfig {
        monocle: animated(),
        ..BordersConfig::default()
    });
    control.configure(&config);
    control.observe(focus(LayoutType::Monocle, false));
    // Base, focus, then the first frame; the next is a minute away.
    assert!(net.wait_sends(3, WAIT));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(net.sends().len(), 3, "no second frame within the period");
    let started = Instant::now();
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
    assert!(started.elapsed() < Duration::from_secs(5), "the sleep was woken");
    assert_eq!(net.dropped(), 1);
}

#[test]
fn a_runner_blocked_in_a_send_is_joined_late_never_detached() {
    let net = Net::new(true);
    let cli = Cli::new();
    net.hold();
    let (control, _) = control(Some(parts(&net, &cli)));
    control.configure(&enabled_config(BordersConfig::default()));
    assert!(net.wait_entered(1, WAIT));
    control.stop();
    assert!(
        !control.join_within(Duration::from_millis(60)),
        "bounded wait, not ended"
    );
    assert!(control.owes_cleanup(), "the exact handle is retained");
    assert_eq!(
        net.dropped(),
        0,
        "the connection is still owned by the blocked thread"
    );
    net.release();
    assert!(control.join_within(WAIT));
    assert!(!control.owes_cleanup());
    assert_eq!(net.dropped(), 1);
    assert_eq!(net.sends().len(), 1, "nothing was sent after the stop");
}

#[test]
fn enabling_while_a_previous_runner_is_still_ending_reports_cleanup() {
    let net = Net::new(true);
    let cli = Cli::new();
    net.hold();
    let (control, log) = control(Some(parts(&net, &cli)));
    control.configure(&enabled_config(BordersConfig::default()));
    assert!(net.wait_entered(1, WAIT));
    control.configure(&TilingConfig::default());
    control.configure(&enabled_config(BordersConfig::default()));
    assert!(!control.is_running(), "no second runner beside an unjoined one");
    assert_eq!(last_fault(&log).expect("fault").kind, FaultKind::Cleanup);
    net.release();
    control.stop();
    assert!(control.join_within(WAIT));
}

#[test]
fn a_focus_change_sends_only_the_settings_that_changed() {
    let args = |width: &str, color: &str| {
        vec![
            format!("width={width}"),
            format!("active_color={color}"),
            "inactive_color=0x00000000".to_owned(),
        ]
    };
    let first = args("6", "0xFF89DCEB");
    assert_eq!(super::runtime::changed_args(None, &first), first);
    let color_only = args("6", "0xFFF9E2AF");
    assert_eq!(super::runtime::changed_args(Some(&first), &color_only), vec![
        "active_color=0xFFF9E2AF".to_owned()
    ]);
    let width_and_color = args("4", "0xFF94E2D5");
    assert_eq!(
        super::runtime::changed_args(Some(&color_only), &width_and_color),
        vec!["width=4".to_owned(), "active_color=0xFF94E2D5".to_owned()]
    );
}
