# Configuration Reference

This document provides a complete reference for all Stache configuration options.

## File Format

Stache uses JSONC (JSON with Comments) for configuration. Both single-line (`//`) and multi-line (`/* */`) comments are supported.

## File Locations

Configuration files are searched in this order (first found is used):

1. `~/.config/stache/config.jsonc` (recommended)
2. `~/.config/stache/config.json`
3. `~/Library/Application Support/stache/config.jsonc`
4. `~/Library/Application Support/stache/config.json`
5. `~/.stache.jsonc` (legacy)
6. `~/.stache.json` (legacy)

## JSON Schema

For editor autocompletion and validation, add the schema reference at the top of your config:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json",
}
```

## Configuration Structure

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json",
  "bar": {
    /* Status bar settings */
  },
  "wallpapers": {
    /* Wallpaper settings */
  },
  "keybindings": {
    /* Global shortcuts */
  },
  "menuAnywhere": {
    /* Menu anywhere settings */
  },
  "proxyAudio": {
    /* Audio device settings */
  },
  "notunes": {
    /* noTunes settings */
  },
}
```

---

## `bar` - Status Bar Configuration

Settings for the status bar widgets.

### `bar.weather` - Weather Widget

| Property          | Type     | Default | Description                                                                                   |
| ----------------- | -------- | ------- | --------------------------------------------------------------------------------------------- |
| `apiKeys`         | `string` | `""`    | Path to `.env` file containing API keys. Can be relative to config file or absolute.          |
| `defaultLocation` | `string` | `""`    | Fallback location when geolocation is unavailable. Can be city name, address, or coordinates. |

**Example:**

```jsonc
{
  "bar": {
    "weather": {
      "apiKeys": ".env",
      "defaultLocation": "San Francisco, CA",
    },
  },
}
```

**Environment file format:**

```env
VISUAL_CROSSING_API_KEY=your_api_key_here
```

---

## `wallpapers` - Wallpaper Configuration

Dynamic wallpaper management with image processing effects.

| Property   | Type                         | Default    | Description                                                      |
| ---------- | ---------------------------- | ---------- | ---------------------------------------------------------------- |
| `path`     | `string`                     | `null`     | Directory containing wallpaper images. Supports `~` expansion.   |
| `list`     | `string[]`                   | `[]`       | Explicit list of wallpaper file paths. Ignored if `path` is set. |
| `interval` | `integer`                    | `0`        | Seconds between wallpaper changes. `0` disables rotation.        |
| `mode`     | `"random"` \| `"sequential"` | `"random"` | Wallpaper selection mode.                                        |
| `radius`   | `integer`                    | `0`        | Rounded corner radius in pixels. `0` for no rounding.            |
| `blur`     | `integer`                    | `0`        | Gaussian blur amount in pixels. `0` for no blur.                 |

**Example with directory:**

```jsonc
{
  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "interval": 3600,
    "mode": "random",
    "radius": 12,
    "blur": 0,
  },
}
```

**Example with explicit list:**

```jsonc
{
  "wallpapers": {
    "list": [
      "~/Pictures/mountain.jpg",
      "~/Pictures/ocean.png",
      "/Users/shared/wallpapers/forest.webp",
    ],
    "interval": 1800,
    "mode": "sequential",
    "radius": 8,
  },
}
```

**Supported image formats:** `jpg`, `jpeg`, `png`, `webp`, `bmp`, `gif`

---

## `keybindings` - Global Keyboard Shortcuts

Define system-wide keyboard shortcuts that execute commands.

### Key Format

Keys use modifier names separated by `+`:

| Modifier  | Aliases                |
| --------- | ---------------------- |
| `Command` | `Cmd`, `Super`, `Meta` |
| `Control` | `Ctrl`                 |
| `Option`  | `Alt`, `Opt`           |
| `Shift`   | -                      |

**Special keys:** `Backquote` (or `` ` ``), `Space`, `Tab`, `Return`, `Escape`, `Delete`, `F1`-`F12`, arrow keys, etc.

### Value Format

Values can be:

| Type         | Description                             |
| ------------ | --------------------------------------- |
| `string`     | Single command to execute               |
| `string[]`   | Multiple commands executed sequentially |
| `""` (empty) | Block the shortcut without action       |

**Example:**

```jsonc
{
  "keybindings": {
    // Single command
    "Command+Control+R": "stache reload",

    // Multiple commands (sequential execution)
    "Command+Control+T": ["open -a Terminal", "stache wallpaper set --random"],

    // Block system shortcut
    "Command+H": "",

    // Open applications
    "Command+Control+S": "open -a Safari",

    // Run scripts
    "Command+Control+B": "~/.local/bin/my-script.sh",
  },
}
```

---

## `menuAnywhere` - Menu Anywhere Configuration

Summon the current application's menu bar at cursor position.

| Property      | Type                              | Default                  | Description                      |
| ------------- | --------------------------------- | ------------------------ | -------------------------------- |
| `enabled`     | `boolean`                         | `false`                  | Enable MenuAnywhere feature.     |
| `modifiers`   | `string[]`                        | `["control", "command"]` | Modifier keys that must be held. |
| `mouseButton` | `"rightClick"` \| `"middleClick"` | `"rightClick"`           | Mouse button to trigger menu.    |

**Valid modifiers:** `"control"`, `"option"`, `"command"`, `"shift"`

**Example:**

```jsonc
{
  "menuAnywhere": {
    "enabled": true,
    "modifiers": ["control", "command"],
    "mouseButton": "rightClick",
  },
}
```

**Requires:** Accessibility permissions

---

## `proxyAudio` - Audio Device Management

Automatic audio device switching based on priority rules.

### Top-level Properties

| Property  | Type      | Default | Description                              |
| --------- | --------- | ------- | ---------------------------------------- |
| `enabled` | `boolean` | `false` | Enable audio device management.          |
| `input`   | `object`  | -       | Input device (microphone) configuration. |
| `output`  | `object`  | -       | Output device (speakers) configuration.  |

### Input/Output Configuration

| Property     | Type           | Default                         | Description                                             |
| ------------ | -------------- | ------------------------------- | ------------------------------------------------------- |
| `name`       | `string`       | `"Stache Virtual Input/Output"` | Virtual device name (if applicable).                    |
| `bufferSize` | `integer`      | `256`                           | Audio buffer size (output only). Values: 128, 256, 512. |
| `priority`   | `DeviceRule[]` | `[]`                            | Priority-ordered list of device rules.                  |

### Device Rule Format

| Property    | Type         | Default   | Description                      |
| ----------- | ------------ | --------- | -------------------------------- |
| `name`      | `string`     | required  | Device name or pattern to match. |
| `strategy`  | `string`     | `"exact"` | Matching strategy (see below).   |
| `dependsOn` | `DeviceRule` | `null`    | Optional dependency rule.        |

### Matching Strategies

| Strategy       | Description                              |
| -------------- | ---------------------------------------- |
| `"exact"`      | Exact match (case-insensitive). Default. |
| `"contains"`   | Device name contains the string.         |
| `"startsWith"` | Device name starts with the string.      |
| `"regex"`      | Regular expression match.                |

**Example:**

```jsonc
{
  "proxyAudio": {
    "enabled": true,
    "input": {
      "name": "Stache Virtual Input",
      "priority": [
        { "name": "AirPods Pro" },
        { "name": "AT2020", "strategy": "contains" },
        { "name": "MacBook Pro Microphone", "strategy": "contains" },
      ],
    },
    "output": {
      "name": "Stache Virtual Output",
      "bufferSize": 256,
      "priority": [
        { "name": "AirPods", "strategy": "startsWith" },
        {
          "name": "External Speakers",
          "strategy": "exact",
          "dependsOn": {
            "name": "MiniFuse",
            "strategy": "startsWith",
          },
        },
        { "name": "^(Sony|Bose).*", "strategy": "regex" },
        { "name": "MacBook Pro Speakers", "strategy": "contains" },
      ],
    },
  },
}
```

**Note:** AirPlay devices are automatically given highest priority, even if not listed.

---

## `notunes` - noTunes Configuration

Prevent Apple Music from auto-launching.

| Property    | Type                                 | Default   | Description                           |
| ----------- | ------------------------------------ | --------- | ------------------------------------- |
| `enabled`   | `boolean`                            | `true`    | Enable noTunes feature.               |
| `targetApp` | `"tidal"` \| `"spotify"` \| `"none"` | `"tidal"` | App to launch instead of Apple Music. |

**Example:**

```jsonc
{
  "notunes": {
    "enabled": true,
    "targetApp": "spotify",
  },
}
```

---

## Path Handling

Paths in the configuration support:

- **Tilde expansion:** `~` expands to your home directory
- **Relative paths:** Resolved relative to the config file location
- **Absolute paths:** Used as-is

```jsonc
{
  "wallpapers": {
    "path": "~/Pictures/Wallpapers", // Tilde expansion
  },
  "bar": {
    "weather": {
      "apiKeys": ".env", // Relative to config file
    },
  },
}
```

---

## Complete Example

Here's a comprehensive configuration example:

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/marcosmoura/stache/main/stache.schema.json",

  "bar": {
    "weather": {
      "apiKeys": ".env",
      "defaultLocation": "San Francisco, CA",
    },
  },

  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "interval": 3600,
    "mode": "random",
    "radius": 12,
    "blur": 0,
  },

  "keybindings": {
    "Command+Control+R": "stache reload",
    "Command+Control+W": "stache wallpaper set --random",
    "Command+Control+Shift+W": "stache wallpaper set --random --screen main",
  },

  "menuAnywhere": {
    "enabled": true,
    "modifiers": ["control", "command"],
    "mouseButton": "rightClick",
  },

  "proxyAudio": {
    "enabled": true,
    "input": {
      "priority": [
        { "name": "AirPods Pro" },
        { "name": "MacBook Pro Microphone", "strategy": "contains" },
      ],
    },
    "output": {
      "bufferSize": 256,
      "priority": [
        { "name": "AirPods", "strategy": "startsWith" },
        { "name": "MacBook Pro Speakers", "strategy": "contains" },
      ],
    },
  },

  "notunes": {
    "enabled": true,
    "targetApp": "spotify",
  },
}
```

---

## Hot Reload

After modifying your configuration, reload it with:

```bash
stache reload
```

Or use a keybinding to reload instantly.
