fn main() {
    // SkyLight is a private framework; link path is typically /System/Library/PrivateFrameworks
    // Use framework search mode so the linker can resolve the framework
    println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
    println!("cargo:rustc-link-lib=framework=SkyLight");

    // Read tauri.conf.json to extract the app identifier at compile time
    let config_path = std::path::Path::new("tauri.conf.json");
    if let Ok(config_contents) = std::fs::read_to_string(config_path)
        && let Ok(config) = serde_json::from_str::<serde_json::Value>(&config_contents)
        && let Some(identifier) = config.get("identifier").and_then(|v| v.as_str())
    {
        println!("cargo:rustc-env=TAURI_APP_IDENTIFIER={identifier}");
    }

    tauri_build::build();
}
