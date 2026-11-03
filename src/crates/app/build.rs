fn main() {
    // A release bundle must embed its frontend. Without this feature Tauri stays
    // in dev mode and every packaged window would load the dev server URL.
    if std::env::var("PROFILE").as_deref() == Ok("release")
        && std::env::var_os("CARGO_FEATURE_CUSTOM_PROTOCOL").is_none()
    {
        eprintln!("stache-app: release builds require `--features custom-protocol`");
        std::process::exit(1);
    }
    tauri_build::build();
}
