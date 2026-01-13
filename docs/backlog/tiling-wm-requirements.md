# Tiling Window Manager Requirements

> This document captures the complete requirements for the Stache tiling window manager feature.

## Overview

A tiling window manager that is highly customizable and efficient. The goal is to allow users to easily manage their windows through a keyboard-centric interface with a simple configuration file, with support for multiple screens and various window arrangements. The project will be built using Rust, ensuring a responsive and user-friendly interface.

### References

- [Aerospace](https://github.com/nikitabobko/AeroSpace)
- [Rift](https://github.com/acsandmann/rift)

## Features

- Keyboard-centric window management
- Customizable configuration file through the config.jsonc
- Support for multiple screens
- Support for virtual workspaces (similar to Aerospace tiling window manager)
- Various window arrangements: tiling, split (vertical/horizontal), floating, monocle, master
- Dynamic window resizing and moving
- Ability to move windows between workspaces and screens as well as move focus between them
- Gaps between windows for better aesthetics
- Simple CLI for launching and managing the window manager
- Windows animations for opening, closing, moving windows, switching workspaces, etc.
- Rules system through the configuration file
  - The rules should be able to match applications by their window class, title, bundle identifier, or other properties
- Lightweight and efficient performance
- Configurable window borders with support for different states, colors/gradients, and animations

## Window Management Approach

When managing windows, some other projects often keep the windows "visible" but move them to the corner of the screen, so macOS can still consider them. However, this implementation uses a different approach: using the built-in capability of hiding windows, similarly to what can be done using Cmd+H shortcut in macOS. This way, the window manager can hide windows that are not in the current workspace, and show them when switching to that workspace.

## Configuration File (config.jsonc)

```jsonc
{
  "tiling": {
    "enable": true, // Whether to enable the tiling window manager
    "workspaces": [], // Define workspaces here
    "ignore": [], // Define apps to ignore here
    "animations": {}, // Define animations settings here
    "gaps": {}, // Define gaps settings here
    "borders": {}, // Define window borders settings here
    "floating": {}, // Define floating settings here
    "master": {}, // Define master layout settings here
    "monocle": {}, // Define monocle layout settings here
    "split": {}, // Define split layout settings here
    "dwindle": {}, // Define Dwindle layout settings here
  },
}
```

Workspaces rules and ignore list use the same matching system, where each rule is an object that can contain one or more properties to match against window properties.

### Workspaces Definition Example

```jsonc
{
  "workspaces": [
    {
      "name": "1",
      "layout": "dwindle",
      "screen": "main", // Accepts 'main', 'secondary', or the name of the screen
      "rules": [
        {
          // Properties here match using AND logic
          "title": "Code Editor",
          "app-id": "com.visualstudio.code",
        },
        {
          // In this case, it should only match the title
          "title": "Code Editor",
        },
        {
          // In this case, it should only match the app-id
          "app-id": "com.visualstudio.code",
        },
      ],
    },
    {
      "name": "2",
      "layout": "monocle",
      // No screen specified, defaults to main
    },
    {
      "name": "3",
      // No layout specified, defaults to floating
    },
  ],
}
```

### Ignore Definition Example

```jsonc
{
  "ignore": [{ "app-id": "com.apple.Spotlight" }, { "title": "Loginwindow" }],
}
```

### Animations Configuration Example

```jsonc
{
  "animations": {
    "enabled": false, // Disable animations - default behavior
    "duration": 200, // Duration in milliseconds
    "easing": "spring", // Options: "linear", "ease-in", "ease-out", "ease-in-out", "spring"
  },
}
```

### Gaps Configuration Example

#### For All Screens

```jsonc
{
  "gaps": {
    "inner": {},
    "outer": {},
  },
}
```

#### Per Screen Settings

```jsonc
{
  // Per screen settings
  "gaps": [
    {
      "screen": "main",
      "inner": {},
      "outer": {},
    },
    {
      "screen": "DP-1",
      "inner": {},
      "outer": {},
    },
  ],
}
```

#### Inner and Outer Config

##### Gaps settings for all axes and sides with same values

```jsonc
{
  "gaps": {
    "inner": 10,
    "outer": 15,
  },
}
```

##### Gaps settings for inner gaps with same value for all axes and outer gaps with different values per side

```jsonc
{
  "gaps": {
    "inner": 10,
    "outer": {
      "top": 15,
      "right": 20,
      "bottom": 15,
      "left": 20,
    },
  },
}
```

##### Gaps settings for inner gaps with different values per axis and outer gaps with same value for all sides

```jsonc
{
  "gaps": {
    "inner": {
      "horizontal": 10,
      "vertical": 15,
    },
    "outer": 20,
  },
}
```

### Floating Configuration Example

```jsonc
{
  "floating": {
    "default-position": "center", // "center" | "default" (last known position)
    "presets": [
      {
        "name": "aligned-left",
        "width": "50%", // Can be in pixels (e.g., 800) or percentage (e.g., "50%")
        "height": "100%", // Can be in pixels (e.g., 600) or percentage (e.g., "100%")
        // Position is relative to the screen dimensions, respecting gaps
        "x": 0, // Can be in pixels (e.g., 0) or percentage (e.g., "0%")
        "y": 0, // Can be in pixels (e.g., 0) or percentage (e.g., "0%")
      },
      {
        "name": "centered-small",
        "width": 1440,
        "height": 900,
        // If center is true, x and y are ignored
        // If width and height are in percentage, they are calculated based on the screen dimensions, respecting gaps
        // If width and height are bigger than the screen dimensions, they are clamped to fit within the screen, respecting gaps
        // The window will be centered on the screen, respecting gaps
        "center": true,
      },
    ],
  },
}
```

### Borders Configuration Example

Window borders provide visual feedback for window state (focused, unfocused, monocle, floating). Borders are rendered by [JankyBorders](https://github.com/FelixKratz/JankyBorders), a high-performance border rendering tool that Stache integrates with.

> **Note**: JankyBorders must be installed and running for borders to work. Stache dynamically updates JankyBorders' configuration based on window state.

#### Basic Configuration

```jsonc
{
  "borders": {
    "enabled": true,
    "width": 4, // Border width in pixels
    "style": "round", // "round" or "square"
    "hidpi": true, // Enable HiDPI/Retina support
    "colors": {
      "focused": "#89b4fa", // Blue for focused window
      "unfocused": "#6c7086", // Gray for unfocused windows
      "monocle": "#cba6f7", // Purple for monocle layout
      "floating": "#a6e3a1", // Green for floating windows
    },
  },
}
```

#### With Disabled States

You can disable borders for specific states by setting their color to `null` or omitting them:

```jsonc
{
  "borders": {
    "enabled": true,
    "width": 4,
    "colors": {
      "focused": "#89b4fa",
      "unfocused": null, // No border for unfocused windows
      "monocle": null, // No border in monocle layout
      "floating": "#a6e3a1",
    },
  },
}
```

#### Border Behavior Notes

- Borders adapt dynamically when windows are resized or moved (handled by JankyBorders)
- Borders do not interfere with window content or layout calculations
- Borders have rounded corners matching the target window's corner radius
- JankyBorders provides smooth, GPU-accelerated rendering at up to 240fps
- Stache updates border colors based on focus state and layout type
- Gradient colors are not supported (JankyBorders limitation)
- Borders can be toggled at runtime via CLI commands

### Complete Configuration Example

```jsonc
{
  "tiling": {
    "enabled": true,
    "animations": {
      "duration": 225,
      "easing": "spring",
    },
    "gaps": [
      {
        "screen": "main",
        "inner": 16,
        "outer": {
          "top": 52,
          "right": 16,
          "bottom": 16,
          "left": 16,
        },
      },
      {
        "screen": "secondary",
        "inner": 12,
        "outer": 12,
      },
    ],
    "master": {
      "ratio": 70,
    },
    "borders": {
      "enabled": true,
      "width": 4,
      "animation": {
        "duration_ms": 150,
        "easing": "ease-out",
      },
      "colors": {
        "focused": "#89b4fa",
        "unfocused": "#6c7086",
        "monocle": "#cba6f7",
        "floating": "#a6e3a1",
      },
    },
    "floating": {
      "presets": [
        {
          "name": "centered",
          "width": 1920,
          "height": 1080,
          "center": true,
        },
        {
          "name": "full",
          "width": "100%",
          "height": "100%",
          "center": true,
        },
        {
          "name": "aligned-left",
          "width": "50%",
          "height": "100%",
          "x": 0,
          "y": 0,
        },
        {
          "name": "aligned-right",
          "width": "50%",
          "height": "100%",
          "x": "50%",
          "y": 0,
        },
      ],
    },
    "workspaces": [
      // Main screen workspaces
      {
        "name": "coding",
        "layout": "monocle",
        "screen": "main",
        "rules": [{ "app-id": "dev.zed.Zed-Preview" }, { "app-id": "com.microsoft.VSCode" }],
      },
      {
        "name": "browser",
        "layout": "monocle",
        "screen": "main",
        "rules": [
          { "app-id": "com.microsoft.edgemac.Dev" },
          { "app-id": "company.thebrowser.dia" },
        ],
      },
      {
        "name": "communication",
        "layout": "dwindle",
        "screen": "main",
        "rules": [
          { "app-id": "com.microsoft.teams2" },
          { "app-id": "net.whatsapp.WhatsApp" },
          { "app-id": "com.hnc.Discord" },
        ],
      },

      // Secondary screen workspaces
      {
        "name": "files",
        "layout": "split",
        "screen": "secondary",
        "rules": [{ "app-id": "com.apple.finder" }],
      },
      {
        "name": "tasks",
        "layout": "split",
        "screen": "secondary",
        "rules": [
          { "app-id": "com.apple.reminders" },
          { "app-id": "me.proton.pass.electron" },
          { "app-id": "com.microsoft.AzureVpnMac" },
        ],
      },
    ],
    "ignore": [
      // Default macOS apps to ignore
      { "app-id": "com.apple.Spotlight" },
      { "app-id": "com.apple.dock" },
      { "app-id": "com.apple.notificationcenterui" },
      { "app-id": "com.apple.systempreferences" },
      { "app-id": "com.apple.loginwindow" },
      { "app-id": "com.apple.weather.menu" },

      // Third-party apps
      { "app-id": "cc.ffitch.shottr" },
      { "app-id": "com.raycast.macos" },
    ],
  },
}
```

## CLI Commands

### Query Commands

Options that can be used with `stache tiling query`:

- `stache tiling query --json` - Outputs all query results in JSON format.

**Screens:**

- `stache tiling query screens` - Lists all screens connected to the system.

**Workspaces:**

- `stache tiling query workspaces` - Lists all workspaces.
- `stache tiling query workspaces --focused-screen` - Lists workspaces on the focused screen.
- `stache tiling query workspaces --screen <screen>` - Lists workspaces on the specified screen.

**Windows:**

- `stache tiling query windows` - Lists all windows.
- `stache tiling query windows --focused-screen` - Lists windows on the focused screen.
- `stache tiling query windows --focused-workspace` - Lists windows on the focused workspace.
- `stache tiling query windows --screen <screen>` - Lists windows on the specified screen.
- `stache tiling query windows --workspace <workspace>` - Lists windows in the specified workspace.

### Window Commands

- `stache tiling window --focus <direction>|<window_id>` - Focuses a window in the specified direction.
- `stache tiling window --swap <direction>` - Swaps the focused window with another window in the specified direction.
- `stache tiling window --preset <preset-name>` - Applies a predefined preset layout for floating windows.
- `stache tiling window --resize width|height <amount>` - Resizes the focused window by the specified amount in pixels. Can accept negative values.
- `stache tiling window --send-to-screen <screen>` - Sends the focused window to the specified screen.
- `stache tiling window --send-to-workspace <workspace>` - Sends the focused window to the specified workspace.

### Workspace Commands

- `stache tiling workspace --balance` - Balances the windows in the focused workspace, distributing them correctly according to the current layout.
- `stache tiling workspace --focus <workspace>` - Focuses the specified workspace.
- `stache tiling workspace --layout <layout>` - Changes the layout of the focused workspace to the specified layout.
- `stache tiling workspace --send-to-screen <screen>` - Sends the focused workspace to the specified screen.

### Border Commands

- `stache tiling borders --enable` - Enables window borders at runtime.
- `stache tiling borders --disable` - Disables window borders at runtime.
- `stache tiling borders --refresh` - Rebuilds all borders from current window state (useful after config changes).

### Parameter Definitions

- `<direction>`: up, down, left, right, previous, next
- `<window_id>`: ID of the window
- `<layout>`: dwindle, split, split-vertical, split-horizontal, monocle, master, floating
- `<screen>`: main, secondary, or the name of the screen
- `<workspace>`: name of the workspace
- `<preset-name>`: name of the floating preset defined in the configuration file
- `<amount>`: positive or negative integer representing pixels

## State Representation

### Window Object

```json
{
  "id": 67,
  "pid": 1005,
  "bundleId": "me.proton.pass.electron",
  "appName": "Proton Pass",
  "title": "Add new login",
  "frame": {
    "x": -1428.0,
    "y": 827.0,
    "width": 1416.0,
    "height": 1261.0
  },
  "isMinimized": false,
  "isFullscreen": false,
  "isFloating": false,
  "workspace": "tasks",
  "screen": "secondary"
}
```

### Workspace Object

```json
{
  "name": "tasks",
  "screen": "primary",
  "layout": "floating",
  "isVisible": false,
  "isFocused": false
}
```

### Screen Object

```json
{
  "id": "2",
  "name": "DELL U2719D",
  "isMain": false,
  "isBuiltIn": false,
  "frame": {
    "x": -1440.0,
    "y": -661.0,
    "width": 1440.0,
    "height": 2560.0
  },
  "scale": 1.0,
  "index": 1
}
```

## Development Steps

For each step, implement the necessary functionality, test it thoroughly, and ensure it integrates well with the existing codebase.

1. Test everything thoroughly and fix any bugs, formatting, clippy warnings, etc.
2. Optimize performance and resource usage.

### Planning and Setup

1. Analyze existing tiling window managers for inspiration and best practices.
2. Analyze the requirements and design the architecture of the window manager.
3. Create the configuration file parser.

### State Creation, Tracking, Management and Retrieval

#### Create Stub CLI Commands

Create CLI all CLI commands to manage the tiling window manager. At this stage, the commands should be no-ops.

#### Create the State Representation

Implement a system to create a state that represents screens, workspaces, and windows.
The system should be able to track windows and their properties (position, size, state, etc.).

The state should be able to represent:

- Multiple screens
- Multiple workspaces per screen
- Multiple windows per workspace

The default screen should be considered the "main" screen. Workspaces without a screen specified should default to the "main" screen. It is possible to have empty workspaces with no windows assigned to them.

The system should be driven by the configuration file.

- When no configuration is provided, the system should create ONE workspace per screen by default, named "1", "2", etc. The default layout for these workspaces should be "floating".
- When a configuration is provided, the system should create workspaces based on the configuration file.
- Apps in the ignore list should not be tracked or managed by the window manager.

#### Window Tracking and Workspace Assignment

Implement the system to manage the workspace rules.
At this stage, the app should be able to, on launch, track ALL the windows and update the state, assigning them to the correct workspace and screen.
When running, it should watch for window resize, move, open, minimize, hide, close events and update the state accordingly.
When launching new applications, the window manager should also check the rules defined in the configuration file and update the state, assigning the window to the appropriate workspace based on the matching criteria.
When no match is found, the window should be placed in the currently focused workspace by default.

At this stage, the window manager is not doing any actual window management yet, just tracking windows and assigning them to virtual workspaces based on the rules.

#### Implement Query Commands

Implement the query commands to list screens, workspaces, and windows based on the current state.

### Window Manipulation

#### Switching Workspaces with Hiding/Showing Windows

When switching from one workspace to another, the window manager should hide all windows from the previous workspace and show all windows from the new workspace.
At this stage, this should be done without any layout applied yet and triggered by the CLI command:
`stache tiling workspace --focus <workspace>`
This should respect the multi-screen setup, only switching the workspace on the screen where the focused window is located.
The focus should move to the previously focused window in that workspace, or to the first/largest window if the workspace has never been focused before.

#### Starting Workspaces on App Start

When the window manager starts, it should automatically detect the FOCUSED WINDOW and switch to the workspace that contains that window, hiding all workspaces on that screen. For the other screens, it should default to the first workspace defined for that screen.

#### Switching Workspaces When Window Focus Changes

When the user focuses a window that belongs to a different workspace than the currently focused one, the window manager should automatically switch to that workspace, hiding the windows from the previous workspace and showing the windows from the new workspace.
This should respect the multi-screen setup, only switching the workspace on the screen where the focused window is located.

### Tiling Layouts

#### Tiling Layout with DWINDLE Algorithm

Implement the tiling layout using the DWINDLE algorithm for arranging windows.
When switching from one workspace to another, the window manager should arrange the windows in a tiled manner using the DWINDLE algorithm.

#### Monocle Layout

Implement the monocle layout.
When the workspace layout is set to monocle, all windows in that workspace should be maximized to fill the entire screen.

#### Grid Layout

Implement the grid layout.
When the workspace layout is set to grid, the windows should be arranged in a grid manner, filling the screen evenly. When the number is odd, the bigger half should be on the top/left side.

#### Split Layout

Implement the split layout with vertical and horizontal options.
When the workspace layout is set to split, the windows should be arranged in a split manner, either vertically or horizontally based on the specified option. When no option is specified, it should act as a hybrid split, based on the screen ratio. If the screen is wider than taller, it should split vertically, otherwise horizontally.

#### Master Layout

Implement the master layout.
When the workspace layout is set to master, one window (the master window) should occupy a larger portion of the screen, while the other windows (the stack windows) should be arranged in the remaining space, according to the master ratio defined in the configuration file. The remaining windows should be arranged in a tiled manner using the DWINDLE algorithm.

#### Floating Layout

Implement the floating layout.
When the workspace layout is set to floating, windows should be able to be moved and resized freely by the user, but the window manager should still move them to the appropriate workspace based on the rules defined in the configuration file when they are opened.

#### Layout Commands

Implement the ability to change the layout of the focused workspace with a command:
`stache tiling workspace --layout <layout>`
At this stage, the window manager should support switching between layouts with the CLI command

### Window Commands

#### Swapping Windows

Implement the ability to swap the focused window with another window in the specified direction (up, down, left, right) within the current workspace layout.
`stache tiling window --swap <direction>`

#### Focusing Windows

Implement the ability to focus a window in the specified direction (up, down, left, right, previous, next) within the current workspace layout. When reaching the end of the window list, it should wrap around to the first/last window.
`stache tiling window --focus <direction>|<window_id>`

#### Resizing Windows

Implement the ability to resize the focused window by the specified amount in pixels (positive or negative) in width or height. When resizing a window, essentially we are changing the layout ratios for the current layout, so the window manager should update the layout accordingly to maintain the specified arrangement.
`stache tiling window --resize width|height <amount>`

#### Balance Layout

Implement the ability to balance the windows in the current layout, basically restoring the default size ratios for the current layout.
`stache tiling workspace --balance`

#### Sending Windows to Different Workspaces

Implement the ability to send the focused window to a different workspace. When a window is sent to a different workspace, it should be removed from the current workspace and added to the target workspace.
`stache tiling window --send-to-workspace <workspace>`

#### Sending Windows to Different Screens

Implement the ability to send the focused window to a different screen. When a window is sent to a different screen, it should be removed from the current screen and added to the target screen, in the currently focused workspace on that screen.
`stache tiling window --send-to-screen <screen>`

### Workspace Commands

#### Sending Workspaces to Different Screens

Implement the ability to send the focused workspace to a different screen. When a workspace is sent to a different screen, it should be removed from the current screen and added to the target screen.
`stache tiling workspace --send-to-screen <screen>`

Multi-monitor behavior: When a workspace is sent to another screen:
Assuming this example:
Screen 1: ['workspace 1', 'workspace 2', 'workspace 3']
Screen 2: ['workspace 4', 'workspace 5']

When moving workspace 1 to screen 2, it should result in:
Screen 1: ['workspace 2', 'workspace 3']
Screen 2: ['workspace 4', 'workspace 5', 'workspace 1']

If moved back to screen 1, it should restore the previous order:
Screen 1: ['workspace 1', 'workspace 2', 'workspace 3']
Screen 2: ['workspace 4', 'workspace 5']

#### Focusing Workspaces

Implement the ability to focus a specific workspace by name.
`stache tiling workspace --focus <workspace>`

### Window/Screen Gaps

Implement support for gaps between windows and screen borders.

### Floating Window Presets

Implement presets for floating windows, allowing users to quickly position and size floating windows based on predefined configurations defined in the configuration file.
`stache tiling window --preset <preset-name>`

### Drag-and-Drop Support

Implement drag-and-drop support for moving windows when the layout is set to dwindle, split or master. It should allow users to swap windows by dragging one window over another, causing them to exchange positions in the layout.

### Animations

Implement animations for window transitions, including opening, closing, moving, resizing windows, and switching workspaces. Animations should be configurable through the configuration file and achieve smooth transitions without impacting performance. 240fps is the target.

### Window Borders

Implement configurable borders around tiled windows to provide visual feedback for window state. Borders are rendered by integrating with [JankyBorders](https://github.com/FelixKratz/JankyBorders), a high-performance border rendering tool.

#### JankyBorders Integration

- Detect if JankyBorders is installed and running
- Send runtime configuration updates via the `borders` CLI
- Map Stache border configuration to JankyBorders settings
- Handle graceful degradation when JankyBorders is not available

#### Border States

- **Focused**: Border color when window has keyboard focus (maps to `active_color`)
- **Unfocused**: Border color for visible but unfocused windows (maps to `inactive_color`)
- **Monocle**: Special color for windows in monocle layout (updates `active_color`)
- **Floating**: Color for windows in floating layout (updates `active_color`)

#### Border Integration

- Update JankyBorders colors on focus change events
- Update JankyBorders colors on layout change events (monocle, floating)
- Send configuration updates for width, style, and hidpi settings
- Borders automatically follow windows (handled by JankyBorders)

#### Limitations

- Gradient colors are not supported (JankyBorders uses solid colors only)
- Border animations are handled by JankyBorders, not configurable from Stache
- Requires JankyBorders to be installed separately

## Design Decisions

| Decision                  | Resolution                                           |
| ------------------------- | ---------------------------------------------------- |
| Accessibility Permissions | Prompt using unified `utils/accessibility.rs` module |
| Hyprspace Integration     | Keep separate, user will integrate later             |
| Default Behavior          | Disabled by default (`enabled: false`)               |
| Window Restore on Quit    | No persistence, rely on macOS auto-unhide            |
| Status Bar Events         | Emit events via `events.rs`, UI handled separately   |
