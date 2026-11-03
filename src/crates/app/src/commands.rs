//! Contributed CLI commands.
//!
//! The composition root owns both the native backend and the CLI styling used
//! by the pilot command. The CLI crate only sees the generic `CommandCatalog`.

use std::sync::Arc;

use stache_macos::audio::{AudioBackend, CoreAudioBackend};
use stache_sdk::CommandCatalog;

/// The application's contributed commands.
///
/// The audio command enumerates locally and never starts the desktop host or a
/// listener.
///
/// # Panics
/// Panics only if the `audio` name already exists in the catalog, which would
/// be a composition bug.
#[must_use]
pub fn catalog() -> CommandCatalog {
    let mut catalog = CommandCatalog::new();
    let backend: Arc<dyn AudioBackend> = Arc::new(CoreAudioBackend);
    catalog
        .add(stache_proxy_audio::cli::contribution(
            backend,
            Arc::new(stache_cli::output::highlight_json),
        ))
        .expect("the audio command is contributed exactly once");
    catalog
}
