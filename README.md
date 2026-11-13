<p align="center">
  <img src="src/crates/app/icons/icon.png" alt="Stache Logo" width="320" height="320">
</p>

<h1 align="center">Stache Next</h1>

<p align="center">
  <strong>A macOS enhancement suite rebuilt on Tauri 2 with a typed module SDK: audio automation, an Apple Music blocker and Keep Awake today, the rest of Stache on the way.</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-macOS-blue?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/tauri-2.x-purple?style=flat-square" alt="Tauri">
</p>

## Features

The `proxyAudio`, `notunes` and `keepAwake` modules ship on `main`.
Every other row is a specified rewrite on
the [roadmap](docs/roadmap.md), not shipped behavior.

| Feature              | Description                                                                                                                                                                            |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Status Bar**       | Planned. Custom menubar with workspaces, media, weather, CPU, battery and clock widgets on the primary display.                                                                        |
| **Tiling**           | Planned. Tiling window manager with virtual workspaces per screen, layout algorithms, gaps and borders; last on the roadmap.                                                           |
| **Wallpapers**       | Planned. Dynamic wallpaper rotation with blur and rounded corners, applied per display without System Events.                                                                          |
| **Audio Management** | Keeps the default input and output on the best available device using ordered name-pattern rules, with AirPlay and HDMI handling.                                                      |
| **Keybindings**      | Planned. Global keyboard shortcuts on a shared input-tap service with fixed priority ordering.                                                                                         |
| **MenuAnywhere**     | Planned. Summon any app's menu at the cursor position.                                                                                                                                 |
| **noTunes**          | Terminates Apple Music and iTunes on launch and optionally opens one replacement player.                                                                                               |
| **Hold-to-Quit**     | Planned. Require holding Cmd+Q before an application quits, with a native HUD and Finder/Stache pass-through.                                                                          |
| **Keep Awake**       | Default-disabled. Prevents display, idle and system sleep while unlocked; releases on lock and restores on unlock, with read-only status. Real power/lock behavior remains unverified. |

## Requirements

- macOS 26 or later.
- Bun 1.4.4, Node 26 and the Xcode command-line tools.
- The Rust toolchain pinned in `rust-toolchain.toml`, plus `cargo-sort` and `cargo-nextest` for the
  quality gates.

## Installation

There is no packaged release yet; the [install guide](docs/guides/install.md) describes the
planned DMG, zip and Homebrew install. Build from source:

```bash
git clone https://github.com/marcosmoura/stache.git
cd stache
bun install --frozen-lockfile
node_modules/.bin/playwright install webkit
bun run tauri:build
```

The bundle is `target/release/bundle/macos/Stache Next.app`, ad-hoc signed by default. It uses
`com.marcosmoura.stache.next`, so it does not install over or terminate Stache. It needs no
media-control resources or device permissions; distribution signing and notarization are not
configured.

## Quick Start

1. Create the configuration files. Each module owns its own document under `~/.config/stache/`:

```bash
cargo build -p stache-app --bin stache
./target/debug/stache config init
```

`config init` writes `global.toml`, `commandQuit.toml`, `keepAwake.toml`,
`keybindings.toml`, `notunes.toml` and `proxyAudio.toml`; each
file is validated separately, and runtime-backed modules reload independently. `keepAwake.toml`
starts disabled: setting `enabled = true` (or tray Enable) starts the lock-aware power assertion,
and an invalid file keeps the last accepted lease and reports a diagnostic.

2. Enable what you need. `proxyAudio.toml`:

```toml
#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/proxyAudio.schema.json
enabled = true

[[input]]
name = "MacBook Pro"
strategy = "contains"

[[output]]
name = "AirPods"
strategy = "contains"

[[output]]
name = "MacBook Pro"
strategy = "contains"
```

`notunes.toml`:

```toml
#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/notunes.schema.json
enabled = true
target_app = "com.spotify.client"
```

`target_app` is optional: leave it out to block without launching anything.

3. Launch Stache Next from the bundle, or run `bun run tauri:dev`. It lives in the menu bar; the tray
   exposes Enable/Disable, Pause/Resume and Retry for each module, plus Quit.

### Upgrading

Configuration files are now `<id>.toml` with snake_case keys, and Stache no longer reads `.jsonc`.
After updating, run `stache config migrate` once: it converts your existing per-module `.jsonc`
files (or a legacy single `config.jsonc`) to `.toml` and leaves the originals untouched. Until
then `stache config path` and the startup log tell you which `.jsonc` files are being ignored.

## CLI

One `stache` binary is both the desktop host and the command line:

```bash
stache config init              # write every registered config template
stache config path              # print the directory and each file's state
stache config migrate           # convert a legacy single file or per-module .jsonc files to TOML
stache reload                   # ask the running app to reread every file
stache schema proxyAudio        # print one JSON Schema
stache cache path               # print the cache directory
stache completions --shell zsh  # generate shell completions
stache audio list --json        # list audio devices
```

The packaged `.app` always launches the desktop, so command-line use needs a standalone binary
(`./target/debug/stache` from a checkout, or a built `stache-app` binary).

## Development

```bash
bun run dev          # browser shell on http://localhost:1420
bun run tauri:dev    # tray host plus Vite
bun run test         # Vitest/Playwright UI tests and cargo nextest
bun run test:ui:coverage
bun run lint
bun run format:check
bun run build
bun run tauri:build
```

`bun run format` is the explicit formatting operation. Lint, checks and hooks never fix source.

## Documentation

- [Docs index](docs/index.md), [architecture](docs/architecture/overview.md),
  [validation](docs/guides/testing.md), [agent workflow](docs/workflow.md),
  [roadmap](docs/roadmap.md). Shipped specs stay under [docs/specs](docs/specs/README.md).

## License

MIT
