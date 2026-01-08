fn main() {
    // SkyLight is a private framework; link path is typically /System/Library/PrivateFrameworks
    // Use framework search mode so the linker can resolve the framework
    println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
    println!("cargo:rustc-link-lib=framework=SkyLight");
    println!("cargo:rustc-link-lib=framework=CoreLocation");

    tauri_build::build();
}
