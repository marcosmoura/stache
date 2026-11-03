<!-- markdownlint-disable MD013 MD029 MD033 MD041 MD024 -->
<p align="center">
  <img src="app/native/icons/icon.png" alt="Stache Logo" width="320" height="320">
</p>

<h1 align="center">Stache</h1>

<p align="center">
  <strong>A macOS desktop enhancement suite built with Tauri 2.x, featuring a custom status bar, dynamic wallpapers, audio automation, and more.</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-macOS-blue?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/tauri-2.x-purple?style=flat-square" alt="Tauri">
</p>

## Features

| Feature              | Description                                                                     |
| -------------------- | ------------------------------------------------------------------------------- |
| **Status Bar**       | Custom menubar with workspaces, media, weather, CPU, battery, and clock widgets |
| **Wallpapers**       | Dynamic wallpaper rotation with blur and rounded corners effects                |
| **Audio Management** | Automatic audio device switching based on priority rules                        |
| **Keybindings**      | Global keyboard shortcuts for commands and automation                           |
| **MenuAnywhere**     | Summon any app's menu at your cursor position                                   |
| **noTunes**          | Prevent Apple Music from auto-launching                                         |
| **Hold-to-Quit**     | Require holding Cmd+Q to quit applications                                      |
| **Keep Awake**       | Prevent system sleep from the status bar                                        |

## Independent archive build

This archive keeps the original Stache identity and runtime. New root tooling does not build it.
Run all commands below from `_legacy/`, never from the new workspace root. No build command launches
the desktop runtime. Do not run legacy automation alongside new device-changing modules.

Prerequisites: macOS 14+ (the bundle minimum), Xcode command-line tools, this directory's Rust nightly,
Node 26, pnpm 11, and Homebrew `media-control`. `pnpm install` runs only sidecar preparation, **not**
Husky; it does not replace root hooks. The original `Stache App` signing certificate is required for
the configured bundle; an explicit local ad-hoc override is possible if it is unavailable.

```sh
cd _legacy
pnpm install --frozen-lockfile
pnpm build
cargo build --workspace --locked
pnpm tauri:build
# Optional local-only signing override:
# pnpm exec tauri build --config app/native/tauri.conf.json --config '{"bundle":{"macOS":{"signingIdentity":"-"}}}'
```

Outputs stay under `_legacy/app/ui/dist` and `_legacy/target`; the bundle is
`target/release/bundle/macos/Stache.app` relative to this directory. Inputs include icons, Info.plist,
media-control binaries and adapter libraries/frameworks. `scripts/prepare-sidecar.sh` refreshes them
from Homebrew, including development copies inside this archive's target directory. Build hooks run
from the archive root (Tauri frontend cwd), not relative to the native config's directory. They do
not call `killall` or format source. Schema generation invokes only the legacy schema CLI command.

Historical instructions below describe the old product; its docs are in [docs/](docs/index.md).
The root [LICENSE](../LICENSE) continues to apply.

Distribution caveat: the retained Homebrew `MediaRemoteAdapter.framework` fails strict deep signature
verification with “code has no resources but signature indicates they must be present”, both as an
input and inside the otherwise successfully built/signed bundle. Its resource signing must be repaired
before distribution. Build success is not a notarization or complete nested-signature validation.

## Requirements

- macOS 14 or later

## Installation

Download the latest release from the [Releases](https://github.com/marcosmoura/stache/releases) page, or build from source:

```bash
git clone https://github.com/marcosmoura/stache.git
cd stache/_legacy
pnpm install
pnpm release
```

## Quick Start

1. Create a configuration file at `~/.config/stache/config.jsonc`:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/_legacy/stache.schema.json",

  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "interval": 1800,
    "mode": "random",
  },

  "keybindings": {
    "Command+Control+R": "stache reload",
    "Command+Control+W": "stache wallpaper set --random",
  },

  "notunes": {
    "enabled": true,
    "targetApp": "spotify",
  },
}
```

2. Launch Stache from Applications or run `stache` in the terminal

3. Grant Accessibility permissions when prompted

## CLI

Stache includes a CLI for automation and integration:

```bash
stache reload                    # Reload configuration
stache wallpaper set --random    # Set random wallpaper
stache audio list                # List audio devices
stache event workspace-changed 1 # Send workspace event
```

## Development

```bash
# Install dependencies
pnpm install

# Development mode with hot reload
pnpm tauri:dev

# Run tests
pnpm test

# Lint and format
pnpm lint && pnpm format

# Build for production
pnpm tauri:build
```

## License

MIT
