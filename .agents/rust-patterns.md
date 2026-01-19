# Rust Patterns

## Event Communication

Events follow the naming convention: `stache://<module>/<event-name>`

**Define in `events.rs`:**

```rust
pub mod media {
    pub const PLAYBACK_CHANGED: &str = "stache://media/playback-changed";
}
```

**Emit from Rust:**

```rust
app_handle.emit(events::media::PLAYBACK_CHANGED, &payload)?;
```

## Tauri Commands

**Define:**

```rust
#[tauri::command]
pub fn get_battery_info() -> Result<BatteryInfo, String> {
    // Implementation
}
```

**Register in `lib.rs`:**

```rust
tauri::generate_handler![
    modules::bar::components::battery::get_battery_info,
]
```

## Configuration Types

Config types use `schemars` for JSON Schema generation:

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct WeatherConfig {
    /// Path to .env file with API keys
    pub api_keys: String,
    /// Default location for weather
    pub default_location: String,
}
```

After modifying config types, regenerate the schema:

```bash
./scripts/generate-schema.sh
```

## Path Handling

Always use the centralized path utility for user-provided paths:

```rust
use crate::utils::path::{expand, expand_and_resolve};

// Tilde expansion only
let home_path = expand("~/.config/stache");

// Tilde expansion + relative path resolution
let env_path = expand_and_resolve(".env", config_dir);
```
