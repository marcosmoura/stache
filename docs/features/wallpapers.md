# Wallpapers

Stache provides dynamic wallpaper management with automatic rotation and image processing effects.

## Features

- Automatic wallpaper rotation (random or sequential)
- Configurable change interval
- Rounded corners effect
- Gaussian blur effect
- Per-screen wallpaper support
- Pre-caching for instant switching

## Configuration

### Basic Setup

```jsonc
{
  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "interval": 3600,
    "mode": "random",
  },
}
```

### Configuration Options

| Option     | Type       | Default    | Description                                                 |
| ---------- | ---------- | ---------- | ----------------------------------------------------------- |
| `path`     | `string`   | `null`     | Directory containing wallpaper images                       |
| `list`     | `string[]` | `[]`       | Explicit list of wallpaper paths (ignored if `path` is set) |
| `interval` | `integer`  | `0`        | Seconds between changes (`0` = no rotation)                 |
| `mode`     | `string`   | `"random"` | Selection mode: `"random"` or `"sequential"`                |
| `radius`   | `integer`  | `0`        | Rounded corner radius in pixels                             |
| `blur`     | `integer`  | `0`        | Gaussian blur amount in pixels                              |

### Using a Directory

```jsonc
{
  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "interval": 1800,
    "mode": "random",
  },
}
```

All supported images in the directory will be used.

### Using an Explicit List

```jsonc
{
  "wallpapers": {
    "list": [
      "~/Pictures/mountain.jpg",
      "~/Pictures/ocean.png",
      "/Volumes/External/wallpapers/forest.webp",
    ],
    "interval": 3600,
    "mode": "sequential",
  },
}
```

**Note:** If both `path` and `list` are specified, `path` takes precedence.

## Image Processing

### Rounded Corners

Apply rounded corners to your wallpapers:

```jsonc
{
  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "radius": 12,
  },
}
```

Set `radius` to `0` to disable.

### Gaussian Blur

Apply a blur effect to your wallpapers:

```jsonc
{
  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "blur": 8,
  },
}
```

Set `blur` to `0` to disable.

### Combined Effects

```jsonc
{
  "wallpapers": {
    "path": "~/Pictures/Wallpapers",
    "radius": 16,
    "blur": 4,
  },
}
```

## Supported Formats

| Format | Extension                 |
| ------ | ------------------------- |
| JPEG   | `.jpg`, `.jpeg`           |
| PNG    | `.png`                    |
| WebP   | `.webp`                   |
| BMP    | `.bmp`                    |
| GIF    | `.gif` (first frame only) |

## CLI Commands

### Set Wallpaper

```bash
# Set specific wallpaper
stache wallpaper set ~/Pictures/mountain.jpg

# Random from configured sources
stache wallpaper set --random

# Random on main screen only
stache wallpaper set --random --screen main

# Random on specific screen (1-based index)
stache wallpaper set --random --screen 2
```

### List Wallpapers

```bash
stache wallpaper list
```

Returns JSON array of available wallpapers:

```json
["/Users/john/Pictures/Wallpapers/mountain.jpg", "/Users/john/Pictures/Wallpapers/ocean.png"]
```

### Pre-generate Wallpapers

```bash
stache wallpaper generate-all
```

Pre-processes all wallpapers with configured effects (blur, rounded corners) and caches them. This ensures instant wallpaper changes.

**Run this:**

- After adding new wallpapers
- After changing `radius` or `blur` settings
- To prepare wallpapers in advance

## Multiple Displays

Stache supports multiple displays:

- **Main screen**: The display with the menu bar
- **Secondary screens**: Numbered 2, 3, etc. (1-based)
- **All screens**: Default target

```bash
# All screens (default)
stache wallpaper set --random

# Main screen only
stache wallpaper set --random --screen main

# Secondary screen
stache wallpaper set --random --screen 2
```

## Caching

Processed wallpapers are cached for performance:

**Location:** `~/Library/Caches/com.marcosmoura.stache/wallpapers/`

**Clear cache:**

```bash
stache cache clear
```

**View cache location:**

```bash
stache cache path
```

## Keybinding Examples

Set up keyboard shortcuts for wallpaper control:

```jsonc
{
  "keybindings": {
    "Command+Control+W": "stache wallpaper set --random",
    "Command+Control+Shift+W": "stache wallpaper set --random --screen main",
  },
}
```

## Automation

### Cron Job

Change wallpaper every hour:

```bash
0 * * * * /usr/local/bin/stache wallpaper set --random
```

### Integration with Window Managers

Trigger wallpaper change on workspace switch:

```bash
# yabai
yabai -m signal --add event=space_changed action="stache wallpaper set --random"
```

## Troubleshooting

### Wallpaper not changing

1. Verify path exists and contains images
2. Check file permissions
3. Ensure images are in supported formats
4. Run `stache wallpaper list` to see detected wallpapers

### Slow wallpaper changes

1. Run `stache wallpaper generate-all` to pre-cache
2. Consider reducing `blur` value (more blur = more processing)
3. Use smaller image files

### Effects not applied

1. Verify `radius` and `blur` values are greater than 0
2. Clear cache and regenerate: `stache cache clear && stache wallpaper generate-all`
3. Check config file syntax

### Multi-monitor issues

1. Use `--screen` flag to target specific displays
2. Verify display numbering with System Preferences > Displays
3. Main screen is always available; secondary screens are numbered 2+
