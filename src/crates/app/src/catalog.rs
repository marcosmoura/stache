//! Composition root for configuration types.
//!
//! The host and the local config commands share this catalog so schema,
//! template, migration and runtime can never disagree about a type.

use stache_bar::BarConfig;
use stache_hold_to_quit::CommandQuitConfig;
use stache_keep_awake::KeepAwakeConfig;
use stache_keybindings::KeybindingsConfig;
use stache_menu_anywhere::MenuAnywhereConfig;
use stache_notunes::NoTunesConfig;
use stache_proxy_audio::ProxyAudioConfig;
use stache_sdk::{CatalogError, ConfigCatalog, GlobalConfig};
use stache_tiling::TilingConfig;
use stache_wallpapers::WallpapersConfig;

/// Registers every configuration type owned by Stache.
///
/// # Errors
/// Returns a catalog error if an identifier is duplicated or a default cannot
/// be serialized.
pub fn catalog() -> Result<ConfigCatalog, CatalogError> {
    let mut catalog = ConfigCatalog::new();
    catalog.register::<GlobalConfig>(GlobalConfig::ID)?;
    catalog.register::<BarConfig>(BarConfig::ID)?;
    catalog.register::<CommandQuitConfig>(CommandQuitConfig::ID)?;
    catalog.register::<KeepAwakeConfig>(KeepAwakeConfig::ID)?;
    catalog.register::<KeybindingsConfig>(KeybindingsConfig::ID)?;
    catalog.register::<MenuAnywhereConfig>(MenuAnywhereConfig::ID)?;
    catalog.register::<NoTunesConfig>(NoTunesConfig::ID)?;
    catalog.register::<ProxyAudioConfig>(ProxyAudioConfig::ID)?;
    catalog.register::<TilingConfig>(TilingConfig::ID)?;
    catalog.register::<WallpapersConfig>(WallpapersConfig::ID)?;
    Ok(catalog)
}
