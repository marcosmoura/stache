fn main() {
    // A release bundle must embed its frontend. Without this feature Tauri stays
    // in dev mode and every packaged window would load the dev server URL.
    if std::env::var("PROFILE").as_deref() == Ok("release")
        && std::env::var_os("CARGO_FEATURE_CUSTOM_PROTOCOL").is_none()
    {
        eprintln!("stache-app: release builds require `--features custom-protocol`");
        std::process::exit(1);
    }
    // Every app command is listed in the app manifest. Once the manifest lists
    // commands, a command missing from it is refused for every window ("not
    // allowed. Command not found"), so read-only commands are listed too and
    // each one is granted only to the capabilities whose UI calls it:
    // - `app_info`, `host_status`: Accessibility guide; the popover gets
    //   `host_status`.
    // - `request_accessibility`, `open_accessibility_settings`, `quit_app`:
    //   Accessibility guide and popover.
    // - `bar_*`: only the `bar-window` capability (`stache-bar-*`), on top of
    //   their own caller-label check.
    // - `hide_popover`, the Overview's `set_module_enabled`,
    //   `module_session_action`, `retry_module`,
    //   `open_accessibility_guide`, and the settings editor's `config_files`,
    //   `config_document`, `config_edit` and `open_config_file`: only the
    //   `popover-window` capability (`stache-popover`).
    // `tests/host_policy.rs` checks each surface's UI calls against its grants.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "app_info",
            "host_status",
            "request_accessibility",
            "open_accessibility_settings",
            "quit_app",
            "bar_snapshot",
            "bar_action",
            "bar_weather",
            "bar_media_artwork",
            "bar_session_action",
            "bar_tiling_action",
            "hide_popover",
            "set_module_enabled",
            "module_session_action",
            "retry_module",
            "open_accessibility_guide",
            "config_files",
            "config_document",
            "config_edit",
            "open_config_file",
        ]),
    ))
    .expect("failed to run the Tauri build script");
}
