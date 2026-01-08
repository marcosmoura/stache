# CLI Reference

Stache provides a command-line interface for controlling the application and integrating with other tools.

## Overview

The `stache` binary serves dual purposes:

- **Without arguments:** Launches the desktop application
- **With arguments:** Executes CLI commands

## Installation

To use the CLI, ensure the binary is in your PATH:

```bash
# Symlink method (recommended)
sudo ln -s /Applications/Stache.app/Contents/MacOS/Stache /usr/local/bin/stache

# Or add to PATH in ~/.zshrc
export PATH="$PATH:/Applications/Stache.app/Contents/MacOS"
```

## Global Options

| Option            | Description           |
| ----------------- | --------------------- |
| `--help`, `-h`    | Show help information |
| `--version`, `-V` | Show version number   |

---

## Commands

### `stache`

Launch the desktop application.

```bash
stache
```

---

### `stache reload`

Reload the configuration file. The running app will apply changes.

```bash
stache reload
```

**Use case:** After editing `~/.config/stache/config.jsonc`, run this to apply changes without restarting.

---

### `stache schema`

Output the JSON Schema for the configuration file.

```bash
stache schema
```

**Example:** Save schema locally for offline editor support:

```bash
stache schema > ~/.config/stache/schema.json
```

---

### `stache completions`

Generate shell completions for command auto-completion.

```bash
stache completions --shell <SHELL>
```

**Options:**

| Option          | Values                                        | Description  |
| --------------- | --------------------------------------------- | ------------ |
| `--shell`, `-s` | `bash`, `zsh`, `fish`, `powershell`, `elvish` | Target shell |

**Examples:**

```bash
# Zsh (add to ~/.zshrc)
eval "$(stache completions --shell zsh)"

# Bash (add to ~/.bashrc)
eval "$(stache completions --shell bash)"

# Fish (add to ~/.config/fish/config.fish)
stache completions --shell fish | source
```

---

### `stache event`

Send events to the running desktop application. This is useful for integrating with window managers and automation tools.

#### `stache event window-focus-changed`

Notify that window focus has changed.

```bash
stache event window-focus-changed
```

#### `stache event workspace-changed`

Notify that the workspace has changed.

```bash
stache event workspace-changed <WORKSPACE_NAME>
```

**Arguments:**

| Argument         | Description                             |
| ---------------- | --------------------------------------- |
| `WORKSPACE_NAME` | Name or identifier of the new workspace |

**Integration examples:**

```bash
# yabai
yabai -m signal --add event=window_focused action="stache event window-focus-changed"
yabai -m signal --add event=space_changed action="stache event workspace-changed \$YABAI_SPACE_INDEX"

# skhd
ctrl - 1 : yabai -m space --focus 1 && stache event workspace-changed 1
```

---

### `stache wallpaper`

Manage wallpapers.

#### `stache wallpaper set`

Set a wallpaper.

```bash
stache wallpaper set [OPTIONS] [PATH]
```

**Arguments:**

| Argument | Description                                              |
| -------- | -------------------------------------------------------- |
| `PATH`   | Path to wallpaper image (optional if `--random` is used) |

**Options:**

| Option           | Description                                              |
| ---------------- | -------------------------------------------------------- |
| `--random`, `-r` | Select a random wallpaper from configured sources        |
| `--screen`, `-s` | Target screen: `main`, `all`, or screen number (1-based) |

**Examples:**

```bash
# Set specific wallpaper
stache wallpaper set ~/Pictures/mountain.jpg

# Random wallpaper on all screens
stache wallpaper set --random

# Random wallpaper on main screen only
stache wallpaper set --random --screen main

# Random wallpaper on second screen
stache wallpaper set --random --screen 2
```

#### `stache wallpaper list`

List available wallpapers from configured sources.

```bash
stache wallpaper list
```

**Output:** JSON array of wallpaper file paths.

```json
[
  "/Users/john/Pictures/Wallpapers/mountain.jpg",
  "/Users/john/Pictures/Wallpapers/ocean.png",
  "/Users/john/Pictures/Wallpapers/forest.webp"
]
```

#### `stache wallpaper generate-all`

Pre-generate all processed wallpapers (with blur and rounded corners applied).

```bash
stache wallpaper generate-all
```

**Use case:** Run this after adding new wallpapers to pre-cache them, ensuring instant wallpaper changes.

---

### `stache audio`

Manage audio devices.

#### `stache audio list`

List connected audio devices.

```bash
stache audio list [OPTIONS]
```

**Options:**

| Option           | Description              |
| ---------------- | ------------------------ |
| `--json`, `-j`   | Output in JSON format    |
| `--input`, `-i`  | Show input devices only  |
| `--output`, `-o` | Show output devices only |

**Examples:**

```bash
# Table format (default)
stache audio list

# JSON format
stache audio list --json

# Input devices only
stache audio list --input

# Output devices in JSON
stache audio list --output --json
```

**Table output example:**

```text
┌──────────────────────────┬────────┬──────────┐
│ Name                     │ Type   │ Default  │
├──────────────────────────┼────────┼──────────┤
│ MacBook Pro Speakers     │ Output │ Yes      │
│ AirPods Pro              │ Output │ No       │
│ MacBook Pro Microphone   │ Input  │ Yes      │
└──────────────────────────┴────────┴──────────┘
```

---

### `stache cache`

Manage the application cache.

#### `stache cache clear`

Clear all cached data (wallpapers, media artwork, etc.).

```bash
stache cache clear
```

#### `stache cache path`

Show the cache directory path.

```bash
stache cache path
```

**Output:**

```text
/Users/john/Library/Caches/com.marcosmoura.stache
```

---

## Exit Codes

| Code | Description       |
| ---- | ----------------- |
| `0`  | Success           |
| `1`  | General error     |
| `2`  | Invalid arguments |

---

## Integration Examples

### Window Manager Integration

#### yabai

Add to your `yabairc`:

```bash
# Refresh workspace display on focus change
yabai -m signal --add event=window_focused action="stache event window-focus-changed"
yabai -m signal --add event=application_activated action="stache event window-focus-changed"

# Update workspace indicator
yabai -m signal --add event=space_changed action="stache event workspace-changed \$YABAI_SPACE_INDEX"
```

#### AeroSpace

Add to your `aerospace.toml`:

```toml
[callbacks]
on-focus-changed = ["stache event window-focus-changed"]
on-workspace-changed = ["stache event workspace-changed %{workspace}"]
```

### Keyboard Shortcuts (skhd)

Add to your `skhdrc`:

```bash
# Reload Stache config
ctrl + cmd - r : stache reload

# Random wallpaper
ctrl + cmd - w : stache wallpaper set --random

# Random wallpaper on main screen
ctrl + cmd + shift - w : stache wallpaper set --random --screen main
```

### Automation (Shortcuts.app, Automator)

Create a Shortcut or Automator workflow with "Run Shell Script" action:

```bash
/usr/local/bin/stache wallpaper set --random
```

### Cron Jobs

Add to crontab (`crontab -e`):

```bash
# Change wallpaper every hour
0 * * * * /usr/local/bin/stache wallpaper set --random

# Clear cache weekly
0 0 * * 0 /usr/local/bin/stache cache clear
```

### Hammerspoon

Add to your `init.lua`:

```lua
-- Reload Stache config
hs.hotkey.bind({"cmd", "ctrl"}, "R", function()
  hs.execute("/usr/local/bin/stache reload")
end)

-- Random wallpaper
hs.hotkey.bind({"cmd", "ctrl"}, "W", function()
  hs.execute("/usr/local/bin/stache wallpaper set --random")
end)
```
