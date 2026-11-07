//! Headless binding export must not start the host.

use specta_typescript::Typescript;

#[test]
fn the_shared_builder_exports_commands_events_and_config_types_without_a_host() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("bindings.gen.ts");
    stache_app::bindings::builder()
        .export(Typescript::default(), &path)
        .expect("export bindings");
    let bindings = std::fs::read_to_string(&path).expect("read bindings");
    for expected in [
        "appInfo",
        "hostStatus",
        "statusChanged",
        "export type AppInfo",
        "export type HostStatus",
        "export type ConfigFileView",
        "export type GlobalConfig",
        "export type CommandQuitConfig",
        "export type KeepAwakeConfig",
        "export type KeepAwakeStatus",
        "export type KeepAwakeRuntimeStatus",
        "export type AssertionState",
        "export type LockState",
        "export type LockObservation",
        "export type KeepAwakeCondition",
        "export type KeepAwakeConditionKind",
        "export type ProxyAudioConfig",
        "export type MatchStrategy",
        "export type StartupCommands",
        "export type AudioDeviceInfo",
        "export type AudioListKind",
        "export type AudioListParams",
        "export type WallpapersConfig",
        "export type WallpaperMode",
        "export type WallpapersSetParams",
        "export type WallpaperSelection",
        "export type WallpaperScreen",
        "export type WallpapersSetOutcome",
        "export type WallpapersOutcomeStatus",
        "export type WallpapersOutcomeCause",
        "export type WallpapersTargetFailure",
        "export type WallpapersTargetOutcome",
        "export type WallpapersTargetResult",
    ] {
        assert!(bindings.contains(expected), "missing {expected}");
    }
    assert!(
        bindings.contains("revision: string"),
        "revision must be text on the wire"
    );
    for field in ["assertionState", "lockState", "conditions"] {
        assert!(
            bindings.contains(field),
            "Keep Awake runtime uses the camelCase field `{field}`"
        );
    }
    assert!(
        bindings.contains("\"releasePending\""),
        "the four-state assertion enum is exported from the shared collection"
    );
    assert!(
        !bindings.contains("audioList"),
        "the pilot has no Tauri command, only exported DTOs"
    );
}

#[test]
fn command_quit_exports_no_lossy_hold_duration() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("bindings.gen.ts");
    stache_app::bindings::builder()
        .export(Typescript::default(), &path)
        .expect("export bindings");
    let bindings = std::fs::read_to_string(&path).expect("read bindings");
    assert!(
        bindings.contains("export type CommandQuitConfig_Serialize = {"),
        "the config type stays exported"
    );
    assert!(
        !bindings.contains("holdDuration") && !bindings.contains("hold_duration"),
        "holdDuration must not be claimed as a lossy TypeScript number"
    );
    assert!(
        !bindings.contains("dismissDelay") && !bindings.contains("dismiss_delay"),
        "dismissDelay must not be claimed as a lossy TypeScript number"
    );
    for exported in [
        "position?: HudPosition",
        "animation?: HudAnimation",
        "export type HudPosition =",
        "export type HudAnimation =",
        "\"bottom\" |",
        "\"top\" |",
        "\"left\" |",
        "\"right\" |",
        "\"center\"",
        "\"fade\" |",
        "\"slide\"",
    ] {
        assert!(
            bindings.contains(exported),
            "{exported} is exported as a string enum"
        );
    }
    let again = directory.path().join("again.gen.ts");
    stache_app::bindings::builder()
        .export(Typescript::default(), &again)
        .expect("export bindings again");
    assert_eq!(bindings, std::fs::read_to_string(&again).expect("read again"));
}

#[test]
fn the_event_name_comes_from_the_derive_not_a_hand_mirror() {
    use tauri_specta::Event as _;
    assert_eq!(stache_app::bindings::StatusChanged::NAME, "status-changed");
}

#[test]
fn status_revision_preserves_values_above_u32() {
    let revision = u64::from(u32::MAX) + 1;
    let snapshot = stache_sdk::ModuleSnapshot {
        id: stache_sdk::ModuleId::new("test"),
        status: stache_sdk::ModuleStatus::Running,
        enabled: true,
        paused: false,
        revision,
        diagnostic: None,
    };
    let view = stache_app::bindings::ModuleStatusView::from_snapshot(&snapshot);
    assert_eq!(view.revision, "4294967296");
    assert_eq!(view.state, stache_app::bindings::ModuleState::Running);
    assert!(view.detail.is_none());
}

#[test]
fn app_info_is_static_identity_data() {
    let info = stache_app::bindings::app_info();
    assert_eq!(info.name, "Stache Next");
    assert_eq!(info.identifier, "com.marcosmoura.stache.next");
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
}
