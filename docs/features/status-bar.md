# Status Bar

The status bar is a customizable menu bar replacement that displays at the top of your screen with system information, media playback, and quick actions.

## Overview

The status bar is divided into three sections:

| Section    | Position | Contents                                 |
| ---------- | -------- | ---------------------------------------- |
| **Spaces** | Left     | Workspace indicator, active window/app   |
| **Media**  | Center   | Now playing track with artwork           |
| **Status** | Right    | Weather, CPU, battery, keep awake, clock |

## Widgets

### Workspaces (Spaces Section)

Displays your virtual workspaces and the current active window.

**Features:**

- Visual indicators for all workspaces
- Click to switch workspaces
- Shows applications in the current workspace
- Click an app icon to focus that window

**Requirements:**

- Tiling window manager integration
- Event notifications from your window manager

**Integration:**

For yabai:

```bash
yabai -m signal --add event=window_focused action="stache event window-focus-changed"
yabai -m signal --add event=space_changed action="stache event workspace-changed \$YABAI_SPACE_INDEX"
```

For AeroSpace:

```toml
[callbacks]
on-focus-changed = ["stache event window-focus-changed"]
on-workspace-changed = ["stache event workspace-changed %{workspace}"]
```

---

### Media Widget (Center Section)

Shows currently playing media with artwork and track information.

**Features:**

- Album artwork (128x128, cached for performance)
- Track title and artist
- Scrolling label for long titles
- Click to open the media source application

**Supported Sources:**

- Spotify
- Apple Music
- Tidal
- Any app using macOS Now Playing APIs

**Technical Details:**

- Uses bundled `media-control` sidecar for metadata
- Artwork is cached in `~/Library/Caches/com.marcosmoura.stache/media_artwork/`

---

### Weather Widget

Displays current weather conditions and temperature.

**Features:**

- Current temperature
- Weather condition icon (sunny, cloudy, rain, snow, etc.)
- Supports geolocation or default location fallback

**Configuration:**

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

**API Key Setup:**

1. Get a free API key at [Visual Crossing Weather](https://www.visualcrossing.com/)
2. Create a `.env` file:

   ```text
   VISUAL_CROSSING_API_KEY=your_api_key_here
   ```

3. Reference the file in your config (path relative to config file)

**Weather Icons:**

| Condition     | Icon            |
| ------------- | --------------- |
| Clear/Sunny   | Sun             |
| Partly Cloudy | Sun with cloud  |
| Cloudy        | Cloud           |
| Rain          | Cloud with rain |
| Snow          | Snowflake       |
| Thunderstorm  | Lightning bolt  |
| Fog/Mist      | Fog             |

---

### CPU Widget

Shows real-time CPU usage percentage.

**Features:**

- Updates periodically
- Click to open Activity Monitor

---

### Battery Widget

Displays battery level and charging status.

**Features:**

- Battery percentage
- Charging indicator
- State-specific icons (charging, full, various discharge levels)

**Detailed Information (on hover/click):**

- Battery health percentage
- Cycle count
- Current voltage
- Temperature
- Time to empty/full estimate
- Battery technology (Li-ion, etc.)

**Icons:**

| State           | Icon                       |
| --------------- | -------------------------- |
| Charging        | Battery with lightning     |
| Full (100%)     | Full battery               |
| High (60-99%)   | Nearly full battery        |
| Medium (30-59%) | Half battery               |
| Low (10-29%)    | Low battery                |
| Critical (<10%) | Empty battery with warning |

---

### Keep Awake Widget

Prevents your Mac from sleeping.

**Features:**

- Toggle system sleep prevention
- Visual indicator when active (lock icon)
- Uses macOS `caffeinate` command internally

**Usage:**

- Click to toggle
- When enabled, prevents display sleep and system idle sleep

---

### Clock Widget

Displays the current time.

**Features:**

- Current time display
- Click to toggle the widget overlay (for expanded views like calendar)

---

## Window Behavior

The status bar window has special properties:

- **Sticky**: Visible on all virtual desktops/spaces
- **Below menu bar**: Positioned below the system menu bar layer
- **Auto-repositions**: Adjusts when screen configuration changes
- **Transparent background**: Blends with your desktop

## Customization

Currently, the status bar layout is fixed. Future versions may support:

- Customizable widget order
- Show/hide individual widgets
- Custom widget colors
- Position options (top, bottom)

## Troubleshooting

### Status bar not visible

1. Check if you're in fullscreen mode (bar hides)
2. Verify the app is running (`ps aux | grep -i stache`)
3. Check for multiple displays - bar appears on main display

### Weather not showing

1. Verify API key in `.env` file
2. Check `apiKeys` path in config is correct
3. Ensure internet connectivity
4. Try setting `defaultLocation` explicitly

### Workspace indicators not updating

1. Ensure window manager integration is set up
2. Verify events are being sent: `stache event workspace-changed 1`
3. Check console for errors

### Media not displaying

1. Start playing something in a supported app
2. Wait a few seconds for detection
3. Some apps may require specific permissions
