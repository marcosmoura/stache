//! Composition root for configuration types.
//!
//! The host and the local config commands share this catalog so schema,
//! template, migration and runtime can never disagree about a type.

use stache_notunes::NoTunesConfig;
use stache_proxy_audio::ProxyAudioConfig;
use stache_sdk::{CatalogError, ConfigCatalog, GlobalConfig};

/// Registers every configuration type owned by Stache Next.
///
/// # Errors
/// Returns a catalog error if an identifier is duplicated or a default cannot
/// be serialized.
pub fn catalog() -> Result<ConfigCatalog, CatalogError> {
    let mut catalog = ConfigCatalog::new();
    catalog.register::<GlobalConfig>(GlobalConfig::ID)?;
    catalog.register::<NoTunesConfig>(NoTunesConfig::ID)?;
    catalog.register::<ProxyAudioConfig>(ProxyAudioConfig::ID)?;
    Ok(catalog)
}
