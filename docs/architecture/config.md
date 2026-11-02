# Configuration

Read when you touch config types, loading, hot reload, `.env` handling, the config
template, or `stache.schema.json`.

## Where the config lives

`config/types/root.rs::config_paths` returns candidates in priority order:

1. `$XDG_CONFIG_HOME/stache/config.jsonc`, then `config.json`
2. `~/.config/stache/config.jsonc`, then `config.json`
3. `~/Library/Application Support/stache/config.jsonc`, then `config.json`
4. `~/.stache.jsonc`, then `~/.stache.json` (legacy)

`load_config` uses the first file that exists and logs a warning when several do.
`--config <path>` (`set_custom_config_path`) overrides the search but must run before
`config::init` or the first `get_config` call.

When nothing exists, `create_default_config_file` writes the template at the first
candidate and the app continues with `StacheConfig::default()`.

Read it through `config::get_config()`, which is a `OnceLock`. `lib.rs` clones the
sections it needs at startup (tiling, command-quit, `execOnStartup`), so those do not
observe later changes.

Files are JSONC: `json_comments::StripComments` runs before serde, so `//` and `/* */`
both work. A parse error does not abort startup — the app logs a warning and continues with
`StacheConfig::default()`.

## Types

- One module per domain under `config/types/`, re-exported by `config/types/mod.rs` and
  again by `config/mod.rs`.
- Root `StacheConfig` is `#[serde(default)]`. Keys that differ from the field name carry an
  explicit `#[serde(rename = "…")]`: `commandQuit`, `menuAnywhere`, `proxyAudio`,
  `execOnStartup`. There is **no** struct-level `rename_all` on the root, so `bar`,
  `wallpapers`, `keybindings`, `notunes` and `tiling` keep their field names.
- Nested structs use `#[serde(default, rename_all = "camelCase")]` (for example
  `BarConfig`, `WeatherConfig`). Enums use kebab-case or lowercase (`LayoutType`,
  `EasingType`, `FloatingPosition`, `WeatherProvider`).
- Every type derives `JsonSchema`. Doc comments become the schema descriptions, so write
  them for users, not for yourself.
- `StacheConfig::prepare()` precomputes lowercase match strings for tiling window rules and
  border ignore rules; call the equivalent if you add another rule collection.

## Env files

`config/env.rs` parses a `.env` with `dotenvy` (variable expansion off).
`load_api_keys(path, config_dir)` resolves the path with
`platform::path::expand_and_resolve` — relative paths are relative to the config file's
directory, `~` is expanded — and reads `VISUAL_CROSSING_API_KEY`. Used by
`bar.weather.apiKeys`. `docs/samples/.env.example` is the user-facing example.

## Template

`config/template.rs::generate_config_template` is a hardcoded JSONC string with every
option commented out. It is what `stache config init` and first-run creation write, and it
is not derived from the types. When you add, rename or remove an option, edit it there too
or `stache config init` will not show it.

## Hot reload

`config/watcher.rs` watches the config file's parent directory (to catch temp-file plus
rename saves), filters events by file name, debounces 200 ms, and then:

- debug builds: logs `config file changed - restart the app to apply new settings` and does
  nothing else.
- release builds: `app_shutdown::restart`.

There is no in-place reload. No module implements an `on_config_reload` hook, and
`stache reload` behaves the same way. Anything captured at startup — hotkeys, tiling
runtime, bar geometry — is only refreshed by the restart.

## Schema

- `schema.rs::generate_schema` builds the schemars schema for `StacheConfig` and injects
  `$id` pointing at
  `https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json`.
- `./scripts/generate-schema.sh` builds `target/release/stache` if it is missing, then runs
  `stache schema > stache.schema.json`. Commit the regenerated file.
- `stache.schema.json` is the editor-facing contract used for autocomplete against the
  `$schema` key; `beforeBuildCommand` regenerates it during `pnpm tauri:build`.

## Samples

`docs/samples/config.jsonc` is the documented user-facing example. It is neither generated
nor tested, so update it in the same change as any option you add, rename or remove.
