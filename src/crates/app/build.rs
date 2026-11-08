fn main() {
    // A release bundle must embed its frontend. Without this feature Tauri stays
    // in dev mode and every packaged window would load the dev server URL.
    if std::env::var("PROFILE").as_deref() == Ok("release")
        && std::env::var_os("CARGO_FEATURE_CUSTOM_PROTOCOL").is_none()
    {
        eprintln!("stache-app: release builds require `--features custom-protocol`");
        std::process::exit(1);
    }
    // Only the Accessibility guidance commands are ACL-gated: the `access-window`
    // capability grants them to `stache-access-*` and no other window. Read-only
    // `app_info` and `host_status` stay available to every host window.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "request_accessibility",
            "open_accessibility_settings",
            "quit_app",
        ]),
    ))
    .expect("failed to run the Tauri build script");
}
