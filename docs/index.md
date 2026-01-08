# Stache Documentation

Stache is a macOS desktop enhancement suite built with Tauri 2.x that provides a custom status bar, dynamic wallpaper management, audio device automation, global hotkeys, and more.

## Quick Links

- [Getting Started](./getting-started.md) - Installation and initial setup
- [Configuration](./configuration.md) - Complete configuration reference
- [CLI Reference](./cli.md) - Command-line interface documentation
- [Architecture](./architecture.md) - Technical architecture overview
- [Development](./development.md) - Contributing and development guide

## Features

| Feature              | Description                                                                    | Documentation                               |
| -------------------- | ------------------------------------------------------------------------------ | ------------------------------------------- |
| **Status Bar**       | Custom menubar with workspace, media, weather, CPU, battery, and clock widgets | [Status Bar](./features/status-bar.md)      |
| **Wallpapers**       | Dynamic wallpaper rotation with blur and rounded corners effects               | [Wallpapers](./features/wallpapers.md)      |
| **Audio Management** | Automatic audio device switching based on priority rules                       | [Audio](./features/audio.md)                |
| **Keybindings**      | Global keyboard shortcuts for commands and automation                          | [Keybindings](./features/keybindings.md)    |
| **MenuAnywhere**     | Summon any app's menu at cursor position                                       | [MenuAnywhere](./features/menu-anywhere.md) |
| **noTunes**          | Prevent Apple Music from auto-launching                                        | [noTunes](./features/notunes.md)            |
| **Hold-to-Quit**     | Require holding Cmd+Q to quit applications                                     | Built-in                                    |
| **Keep Awake**       | Prevent system sleep from status bar                                           | Built-in                                    |

## System Requirements

- **macOS 10.15** (Catalina) or later
- **Accessibility permissions** (for MenuAnywhere and Hold-to-Quit features)

## Architecture Overview

Stache uses a single binary architecture that serves both as a desktop application and a CLI tool:

```text
┌─────────────────────────────────────────────────────────────┐
│                       Stache Binary                          │
│  ┌─────────────────────┐    ┌─────────────────────────────┐ │
│  │   CLI Mode          │    │      Desktop App Mode       │ │
│  │   (with args)       │    │      (no args)              │ │
│  │                     │    │                             │ │
│  │  stache reload      │───►│  IPC Listener               │ │
│  │  stache wallpaper   │    │  (NSDistributedNotification)│ │
│  │  stache audio       │    │                             │ │
│  └─────────────────────┘    └─────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

The CLI communicates with the running desktop app via macOS `NSDistributedNotificationCenter`, allowing external tools like window managers (yabai, aerospace, skhd) to trigger Stache actions.

## Quick Start

1. Download from [Releases](https://github.com/marcosmoura/stache/releases) or build from source
2. Create a configuration file at `~/.config/stache/config.jsonc`
3. Launch Stache from Applications or run `stache` in terminal
4. Grant necessary permissions when prompted

See the [Getting Started](./getting-started.md) guide for detailed instructions.

## Configuration

Stache uses JSONC (JSON with comments) for configuration. The recommended location is:

```text
~/.config/stache/config.jsonc
```

A JSON Schema is provided for editor autocompletion:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json",
  // Your configuration here
}
```

See the [Configuration Reference](./configuration.md) for all available options.

## Getting Help

- Check the [sample configuration](./sample-config.jsonc) for examples
- Review feature-specific documentation in the [Features](./features/) directory
- Report issues at [GitHub Issues](https://github.com/marcosmoura/stache/issues)
