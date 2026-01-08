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

## Requirements

- macOS 10.15 (Catalina) or later

## Installation

Download the latest release from the [Releases](https://github.com/marcosmoura/stache/releases) page, or build from source:

```bash
git clone https://github.com/marcosmoura/stache.git
cd stache
pnpm install
pnpm release
```

## Quick Start

1. Create a configuration file at `~/.config/stache/config.jsonc`:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json",

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

## Documentation

- [Getting Started](docs/getting-started.md)
- [Configuration Reference](docs/configuration.md)
- [CLI Reference](docs/cli.md)
- [Architecture](docs/architecture.md)
- [Development Guide](docs/development.md)

### Features

- [Status Bar](docs/features/status-bar.md)
- [Wallpapers](docs/features/wallpapers.md)
- [Audio Management](docs/features/audio.md)
- [Keybindings](docs/features/keybindings.md)
- [MenuAnywhere](docs/features/menu-anywhere.md)
- [noTunes](docs/features/notunes.md)

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
