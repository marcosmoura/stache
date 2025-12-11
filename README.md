<!-- markdownlint-disable MD033 MD041 MD024 -->
<p align="center">
  <img src="packages/desktop/tauri/icons/icon.png" alt="Barba Shell Logo" width="128" height="128">
</p>

<h1 align="center">Barba Shell</h1>

<p align="center">
  <strong>A minimal, fast, and customizable macOS desktop environment</strong>
</p>

<p align="center">
  <a href="#features">Features</a> •
  <a href="#installation">Installation</a> •
  <a href="#configuration">Configuration</a> •
  <a href="#cli-reference">CLI</a> •
  <a href="#development">Development</a> •
  <a href="#license">License</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-macOS-blue?style=flat-square" alt="Platform">
  <img src="https://img.shields.io/badge/license-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/rust-2024-orange?style=flat-square" alt="Rust">
  <img src="https://img.shields.io/badge/tauri-2.x-purple?style=flat-square" alt="Tauri">
</p>

---

## Overview

Barba Shell is a **macOS-only** Tauri 2.x desktop application that provides a complete desktop environment experience with:

- 🪟 **Tiling Window Manager** — Automatic window tiling with multiple layout modes
- 📊 **Status Bar** — Customizable menubar with system information widgets
- ⌨️ **Global Keybindings** — Configurable keyboard shortcuts for all actions
- 🎨 **Dynamic Wallpapers** — Automatic wallpaper rotation with blur and rounded corners
- 🎵 **Media Controls** — Now playing widget with playback controls

Built with **Rust** for the backend and **React 19** for the frontend, Barba Shell combines native performance with a modern, reactive UI.

---

## Features

### 🪟 Tiling Window Manager

Barba Shell includes a powerful tiling window manager with multiple layout modes:

| Layout             | Description                                      |
| ------------------ | ------------------------------------------------ |
| `tiling`           | Binary space partitioning (dwindle algorithm)    |
| `monocle`          | All windows maximized, only focused visible      |
| `master`           | One master window, remaining stacked on the side |
| `split`            | Two windows split based on screen orientation    |
| `split-vertical`   | Two windows side by side                         |
| `split-horizontal` | Two windows stacked top/bottom                   |
| `floating`         | Free-form window positioning                     |
| `scrolling`        | Niri-style scrolling workspace layout            |

**Key Features:**

- Per-workspace layouts with individual customization
- Configurable gaps (inner/outer, per-axis)
- Window rules for automatic floating/assignment
- Floating presets for common window positions
- Multi-monitor support with workspace assignment
- Window animations (optional)

### 📊 Status Bar

A sleek, transparent menubar that displays:

| Widget          | Description                                     |
| --------------- | ----------------------------------------------- |
| **Workspaces**  | Visual workspace indicator with click-to-switch |
| **Current App** | Active application name and icon                |
| **Media**       | Now playing track with playback controls        |
| **Weather**     | Current conditions and temperature              |
| **CPU**         | Real-time CPU usage monitor                     |
| **Battery**     | Battery level and charging status               |
| **Keep Awake**  | Prevent system sleep toggle                     |
| **Clock**       | Current time and date                           |

### ⌨️ Global Keybindings

Define custom keyboard shortcuts to:

- Switch workspaces and layouts
- Move and resize windows
- Execute shell commands
- Trigger application actions

### 🎨 Dynamic Wallpapers

- Automatic wallpaper rotation (random or sequential)
- Configurable change interval
- Rounded corners and blur effects
- Per-screen wallpaper support
- Pre-generation for instant switching

---

## Installation

### Requirements

- **macOS 10.15** (Catalina) or later
- **Accessibility permissions** (required for window management)

### Download

Download the latest release from the [Releases](https://github.com/marcosmoura/barba-shell/releases) page.

### Build from Source

1. **Install dependencies:**

   ```bash
   # Install Rust
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

   # Install pnpm
   npm install -g pnpm

   # Install project dependencies
   pnpm install
   ```

2. **Build the application:**

   ```bash
   pnpm release
   ```

3. **Install the CLI (optional):**

   ```bash
   pnpm build:cli
   # Binary will be at target/release/barba
   ```

---

## Configuration

Barba Shell uses a JSONC configuration file located at:

`~/.config/barba/config.json`

> **Tip:** JSONC supports comments! Use `//` for single-line and `/* */` for multi-line comments.

### JSON Schema

A JSON Schema is provided for editor autocompletion and validation:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/barba-shell/main/barba.schema.json",
  // Your configuration here...
}
```

### Example Configuration

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/barba-shell/main/barba.schema.json",

  // Status bar configuration
  "bar": {
    "wallpapers": {
      "path": "~/Pictures/Wallpapers",
      "interval": 300,
      "mode": "random",
      "radius": 12,
      "blur": 8,
    },
    "weather": {
      "visualCrossingApiKey": "YOUR_API_KEY",
      "defaultLocation": "San Francisco",
    },
  },

  // Global keybindings
  "keybindings": {
    "Command+Control+R": "barba reload",
    "Command+Option+1": "barba workspace focus 1",
    "Command+Option+2": "barba workspace focus 2",
    "Command+Option+Left": "barba workspace focus previous",
    "Command+Option+Right": "barba workspace focus next",
    "Command+Shift+H": "barba window move left",
    "Command+Shift+L": "barba window move right",
    "Command+Shift+J": "barba window move down",
    "Command+Shift+K": "barba window move up",
  },

  // Tiling window manager
  "tiling": {
    "enabled": true,
    "defaultLayout": "tiling",
    "animations": true,
    "gaps": {
      "inner": { "horizontal": 10, "vertical": 10 },
      "outer": { "top": 45, "bottom": 10, "left": 10, "right": 10 },
    },
    "workspaces": [
      { "name": "1", "layout": "tiling", "screen": "main" },
      { "name": "2", "layout": "tiling", "screen": "main" },
      { "name": "3", "layout": "monocle", "screen": "main" },
      { "name": "chat", "layout": "floating", "screen": "secondary" },
    ],
    "windowRules": [
      {
        "match": { "app": "Finder" },
        "floating": true,
      },
      {
        "match": { "title": "Settings" },
        "floating": true,
        "preset": "centered-large",
      },
    ],
    "floatingPresets": {
      "centered-small": {
        "width": "40%",
        "height": "50%",
        "position": "center",
      },
      "centered-large": {
        "width": "80%",
        "height": "80%",
        "position": "center",
      },
    },
  },
}
```

### Configuration Reference

<details>
<summary><strong>Bar Configuration</strong></summary>

#### Wallpapers

| Option     | Type                         | Default    | Description                                            |
| ---------- | ---------------------------- | ---------- | ------------------------------------------------------ |
| `path`     | `string`                     | `""`       | Directory containing wallpaper images                  |
| `list`     | `string[]`                   | `[]`       | Explicit list of wallpaper paths                       |
| `interval` | `number`                     | `0`        | Seconds between wallpaper changes (0 = no auto-change) |
| `mode`     | `"random"` \| `"sequential"` | `"random"` | Wallpaper selection mode                               |
| `radius`   | `number`                     | `0`        | Corner radius in pixels                                |
| `blur`     | `number`                     | `0`        | Gaussian blur amount in pixels                         |

#### Weather

| Option                 | Type     | Default | Description                                                        |
| ---------------------- | -------- | ------- | ------------------------------------------------------------------ |
| `visualCrossingApiKey` | `string` | `""`    | API key from [visualcrossing.com](https://www.visualcrossing.com/) |
| `defaultLocation`      | `string` | `""`    | Fallback location when geolocation fails                           |

</details>

<details>
<summary><strong>Tiling Configuration</strong></summary>

#### General

| Option          | Type      | Default    | Description                       |
| --------------- | --------- | ---------- | --------------------------------- |
| `enabled`       | `boolean` | `true`     | Enable/disable window management  |
| `defaultLayout` | `string`  | `"tiling"` | Default layout for new workspaces |
| `animations`    | `boolean` | `false`    | Enable window animations          |

#### Gaps

| Option  | Type                                       | Description             |
| ------- | ------------------------------------------ | ----------------------- |
| `inner` | `number` \| `{ horizontal, vertical }`     | Gap between windows     |
| `outer` | `number` \| `{ top, bottom, left, right }` | Gap around screen edges |

#### Workspaces

```jsonc
{
  "name": "coding", // Unique workspace identifier
  "layout": "tiling", // Layout mode for this workspace
  "screen": "main", // Assigned screen (main, secondary, or display name)
}
```

#### Window Rules

```jsonc
{
  "match": {
    "app": "Firefox", // Match by app name (supports regex)
    "title": "Picture-in-*", // Match by window title (supports glob)
    "bundleId": "com.apple.*", // Match by bundle identifier
  },
  "floating": true, // Force floating mode
  "workspace": "media", // Assign to specific workspace
  "preset": "centered-small", // Apply floating preset
}
```

</details>

---

## CLI Reference

Barba Shell includes a powerful CLI for scripting and automation.

### Installation

The CLI binary (`barba`) communicates with the running desktop app via Unix socket.

```bash
# Build the CLI
cargo install --path packages/cli
```

### Shell Completions

```bash
# Zsh (add to ~/.zshrc)
eval "$(barba completions --shell zsh)"

# Bash
barba completions --shell bash > ~/.local/share/bash-completion/completions/barba

# Fish
barba completions --shell fish > ~/.config/fish/completions/barba.fish
```

### Commands

#### General

| Command                             | Description                          |
| ----------------------------------- | ------------------------------------ |
| `barba reload`                      | Reload configuration without restart |
| `barba generate-schema`             | Output JSON schema to stdout         |
| `barba completions --shell <SHELL>` | Generate shell completions           |

#### Workspace Management

```bash
# Focus workspace by name
barba workspace focus coding

# Focus workspace by direction
barba workspace focus next
barba workspace focus previous

# Change layout
barba workspace layout monocle
barba workspace layout master

# Send workspace to another screen
barba workspace send-to-screen secondary

# Balance window sizes
barba workspace balance
```

#### Window Management

```bash
# Move/swap window
barba window move left
barba window move up

# Focus adjacent window
barba window focus right
barba window focus next

# Send to workspace
barba window send-to-workspace 2
barba window send-to-workspace coding --focus=false

# Send to screen
barba window send-to-screen main

# Resize window
barba window resize width 100
barba window resize height -50

# Apply floating preset
barba window preset centered-small

# Close window
barba window close
```

#### Wallpaper Management

```bash
# Set specific wallpaper
barba wallpaper set /path/to/image.jpg

# Set random wallpaper
barba wallpaper set --random

# Target specific screen
barba wallpaper set --random --screen main
barba wallpaper set /path/to/image.jpg --screen 2

# Pre-generate all wallpapers
barba wallpaper generate-all

# List available wallpapers
barba wallpaper list
```

#### Query State

```bash
# List all screens
barba query screens

# List workspaces
barba query workspaces
barba query workspaces --focused
barba query workspaces --screen main

# List windows
barba query windows
barba query windows --focused-workspace
barba query windows --workspace coding
```

---

## Development

### Prerequisites

- [Rust](https://rustup.rs/) (2024 edition)
- [Node.js](https://nodejs.org/) 20+
- [pnpm](https://pnpm.io/) 9+

### Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/)
- [Tauri VS Code extension](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode)
- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

### Project Structure

```text
barba-shell/
├── packages/
│   ├── cli/              # Rust CLI application
│   │   └── src/
│   │       ├── main.rs       # Entry point
│   │       ├── commands.rs   # Clap command definitions
│   │       └── ipc.rs        # Unix socket client
│   │
│   ├── desktop/          # Desktop application
│   │   ├── tauri/            # Rust backend
│   │   │   └── src/
│   │   │       ├── lib.rs        # Tauri entry, command registration
│   │   │       ├── ipc.rs        # IPC server for CLI
│   │   │       └── bar/          # Status bar components
│   │   └── ui/               # React frontend
│   │       ├── main.tsx          # App entry
│   │       ├── bar/              # Bar UI components
│   │       ├── hooks/            # React hooks
│   │       └── design-system/    # Styling tokens
│   │
│   └── shared/           # Shared Rust types
│       └── src/
│           ├── config.rs     # Configuration types
│           └── tiling.rs     # Tiling types
│
├── scripts/              # Build & release scripts
├── barba.schema.json     # JSON Schema for config
└── Cargo.toml            # Workspace root
```

### Available Scripts

| Command            | Description                           |
| ------------------ | ------------------------------------- |
| `pnpm dev`         | Start Vite dev server (frontend only) |
| `pnpm tauri:dev`   | Full app with hot reload              |
| `pnpm tauri:build` | Build production app                  |
| `pnpm build:cli`   | Build CLI binary                      |
| `pnpm test`        | Run all tests                         |
| `pnpm test:ui`     | Run Vitest browser tests              |
| `pnpm test:rust`   | Run Rust tests with nextest           |
| `pnpm lint`        | Run all linters                       |
| `pnpm format`      | Format all code                       |

### Architecture

```text
┌─────────────┐     Unix Socket     ┌──────────────────────────────────────┐
│   CLI       │ ──────────────────► │         Desktop App                  │
│  (barba)    │                     │  ┌─────────────┐   ┌───────────────┐ │
└─────────────┘                     │  │ IPC Server  │──►│ Tauri Events  │ │
                                    │  └─────────────┘   └───────┬───────┘ │
                                    │                            │         │
                                    │  ┌─────────────────────────▼───────┐ │
                                    │  │        React Frontend           │ │
                                    │  │  (React Query + Tauri Invoke)   │ │
                                    │  └─────────────────────────────────┘ │
                                    └──────────────────────────────────────┘
```

### Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Run linting and tests (`pnpm lint && pnpm test`)
5. Commit your changes (`git commit -m 'Add amazing feature'`)
6. Push to the branch (`git push origin feature/amazing-feature`)
7. Open a Pull Request

---

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

<p align="center">
  Made with ❤️ by <a href="https://github.com/marcosmoura">Marcos Moura</a>
</p>
