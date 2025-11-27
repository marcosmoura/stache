mod bar;
mod utils;

/// Runs the Tauri application.
///
/// # Panics
pub fn run() {
    tauri::Builder::default()
        .setup(|app: &mut tauri::App| {
            bar::init(&*app);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
