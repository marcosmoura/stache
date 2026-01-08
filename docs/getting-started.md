# Getting Started

This guide will help you install, configure, and start using Stache on your Mac.

## Requirements

- **macOS 10.15** (Catalina) or later
- For development: Rust toolchain, Node.js 20+, pnpm

## Installation

### Option 1: Download Pre-built Binary

1. Go to the [Releases](https://github.com/marcosmoura/stache/releases) page
2. Download the latest `.dmg` file
3. Open the DMG and drag Stache to your Applications folder
4. Launch Stache from Applications or Spotlight

### Option 2: Build from Source

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install pnpm (if not already installed)
npm install -g pnpm

# Clone the repository
git clone https://github.com/marcosmoura/stache.git
cd stache

# Install dependencies
pnpm install

# Build for release
pnpm tauri:build

# The built app will be at:
# app/native/target/release/bundle/macos/Stache.app
```

## Initial Setup

### 1. Create Configuration File

Create your configuration file at the recommended location:

```bash
mkdir -p ~/.config/stache
touch ~/.config/stache/config.jsonc
```

Add a minimal configuration:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json",
}
```

### 2. Launch Stache

Launch the application from:

- **Applications folder**: Double-click Stache.app
- **Spotlight**: Press Cmd+Space, type "Stache", press Enter
- **Terminal**: Run `stache` (if binary is in your PATH)

### 3. Grant Permissions

Stache requires certain permissions to function properly:

#### Accessibility Access (Required for some features)

When prompted, or manually:

1. Open **System Preferences** > **Security & Privacy** > **Privacy**
2. Select **Accessibility** from the left sidebar
3. Click the lock icon and authenticate
4. Add Stache to the list or check its checkbox

This permission is required for:

- MenuAnywhere (reading app menus)
- Hold-to-Quit (intercepting Cmd+Q)
- Global keybindings

#### Screen Recording (Optional)

For window management features:

1. Open **System Preferences** > **Security & Privacy** > **Privacy**
2. Select **Screen Recording** from the left sidebar
3. Add Stache to the list

## Configuration File Locations

Stache searches for configuration files in this order:

1. `~/.config/stache/config.jsonc` (recommended)
2. `~/.config/stache/config.json`
3. `~/Library/Application Support/stache/config.jsonc`
4. `~/Library/Application Support/stache/config.json`
5. `~/.stache.jsonc` (legacy)
6. `~/.stache.json` (legacy)

The first file found is used. JSONC format (JSON with comments) is supported.

## Basic Configuration

Here's a starter configuration with common features enabled:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json",

  // Wallpaper rotation
  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "interval": 1800, // 30 minutes
    "mode": "random",
    "radius": 12, // Rounded corners
    "blur": 0, // No blur
  },

  // Global keyboard shortcuts
  "keybindings": {
    "Command+Control+R": "stache reload",
    "Command+Control+W": "stache wallpaper set --random",
  },

  // Prevent Apple Music from launching
  "notunes": {
    "enabled": true,
    "targetApp": "spotify", // or "tidal" or "none"
  },
}
```

## Setting Up Weather Widget

The weather widget requires a free API key from Visual Crossing:

### 1. Get an API Key

1. Go to [Visual Crossing Weather](https://www.visualcrossing.com/)
2. Create a free account
3. Navigate to your account to find your API key

### 2. Create Environment File

Create a `.env` file to store your API key:

```bash
echo "VISUAL_CROSSING_API_KEY=your_api_key_here" > ~/.config/stache/.env
```

### 3. Configure Weather

Add the weather configuration to your config file:

```jsonc
{
  "bar": {
    "weather": {
      "apiKeys": ".env", // Relative to config file location
      "defaultLocation": "San Francisco, CA",
    },
  },
}
```

## CLI Setup

To use Stache commands from the terminal, add the binary to your PATH.

### Option 1: Symlink (Recommended)

```bash
sudo ln -s /Applications/Stache.app/Contents/MacOS/Stache /usr/local/bin/stache
```

### Option 2: Add to PATH

Add this to your `~/.zshrc` or `~/.bashrc`:

```bash
export PATH="$PATH:/Applications/Stache.app/Contents/MacOS"
alias stache="Stache"
```

### Verify Installation

```bash
stache --help
```

## Shell Completions

Generate shell completions for your shell:

```bash
# For zsh (add to ~/.zshrc)
eval "$(stache completions --shell zsh)"

# For bash (add to ~/.bashrc)
eval "$(stache completions --shell bash)"

# For fish (add to ~/.config/fish/config.fish)
stache completions --shell fish | source
```

## Hot Reload

Stache supports hot-reloading configuration changes. After modifying your config file, run:

```bash
stache reload
```

Or use a keybinding:

```jsonc
{
  "keybindings": {
    "Command+Control+R": "stache reload",
  },
}
```

## Integration with Window Managers

Stache works well with tiling window managers. Here are example integrations:

### yabai

Add to your yabai config:

```bash
# Notify Stache of window focus changes
yabai -m signal --add event=window_focused action="stache event window-focus-changed"

# Notify Stache of workspace changes
yabai -m signal --add event=space_changed action="stache event workspace-changed \$YABAI_SPACE_INDEX"
```

### AeroSpace

Add to your aerospace config:

```toml
[callbacks]
on-focus-changed = ["stache event window-focus-changed"]
on-workspace-changed = ["stache event workspace-changed %{workspace}"]
```

### skhd

Define shortcuts that trigger Stache commands:

```bash
# Random wallpaper
ctrl + cmd - w : stache wallpaper set --random

# Reload config
ctrl + cmd - r : stache reload
```

## Troubleshooting

### Stache doesn't start

- Check Console.app for crash logs
- Verify macOS version is 10.15 or later
- Try running from Terminal to see error output

### Permissions issues

- Reset permissions: Remove Stache from Accessibility list, re-add it
- Ensure you granted permissions to the correct app (not a symlink)

### Weather not showing

- Verify API key is correct in `.env` file
- Check that `apiKeys` path in config is correct
- Ensure internet connectivity

### Status bar not visible

- Check if you're using fullscreen mode (status bar hides)
- Verify multiple displays are detected correctly

## Next Steps

- Read the [Configuration Reference](./configuration.md) for all options
- Explore [Features](./features/) documentation
- Check the [CLI Reference](./cli.md) for available commands
