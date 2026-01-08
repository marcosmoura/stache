# MenuAnywhere

MenuAnywhere lets you summon any application's menu bar right at your cursor position using a keyboard + mouse combination.

## Overview

Instead of moving your mouse to the top of the screen to access menus, MenuAnywhere brings the menu to you. Hold modifier keys and click to open the focused application's menu at your cursor.

## Features

- Access any app's menu bar at cursor position
- Configurable modifier keys
- Right-click or middle-click trigger
- Works with any macOS application

## Requirements

**Accessibility permissions are required** for MenuAnywhere to:

- Read the frontmost application's menu structure
- Capture mouse and keyboard events

Grant permissions in: **System Preferences** > **Security & Privacy** > **Privacy** > **Accessibility**

## Configuration

### Basic Setup

```jsonc
{
  "menuAnywhere": {
    "enabled": true,
    "modifiers": ["control", "command"],
    "mouseButton": "rightClick",
  },
}
```

### Configuration Options

| Option        | Type       | Default                  | Description            |
| ------------- | ---------- | ------------------------ | ---------------------- |
| `enabled`     | `boolean`  | `false`                  | Enable MenuAnywhere    |
| `modifiers`   | `string[]` | `["control", "command"]` | Required modifier keys |
| `mouseButton` | `string`   | `"rightClick"`           | Mouse button trigger   |

### Modifier Keys

Available modifier keys:

| Value       | Key            |
| ----------- | -------------- |
| `"control"` | Control key    |
| `"option"`  | Option/Alt key |
| `"command"` | Command key    |
| `"shift"`   | Shift key      |

### Mouse Button

| Value           | Description                              |
| --------------- | ---------------------------------------- |
| `"rightClick"`  | Right mouse button                       |
| `"middleClick"` | Middle mouse button (scroll wheel click) |

## Usage

1. Focus on any application
2. Hold the configured modifier keys (e.g., Control + Command)
3. Click the configured mouse button (e.g., right-click)
4. The application's menu appears at your cursor

The menu behaves like a normal context menu:

- Click an item to activate it
- Press Escape or click outside to dismiss
- Navigate with keyboard arrows

## Example Configurations

### Default Setup

Control + Command + Right Click:

```jsonc
{
  "menuAnywhere": {
    "enabled": true,
    "modifiers": ["control", "command"],
    "mouseButton": "rightClick",
  },
}
```

### Option + Right Click

For simpler access:

```jsonc
{
  "menuAnywhere": {
    "enabled": true,
    "modifiers": ["option"],
    "mouseButton": "rightClick",
  },
}
```

### Middle Click with Modifiers

For mice with middle-click:

```jsonc
{
  "menuAnywhere": {
    "enabled": true,
    "modifiers": ["command"],
    "mouseButton": "middleClick",
  },
}
```

### Shift + Control + Right Click

Avoid conflicts with other tools:

```jsonc
{
  "menuAnywhere": {
    "enabled": true,
    "modifiers": ["shift", "control"],
    "mouseButton": "rightClick",
  },
}
```

## How It Works

When triggered, MenuAnywhere:

1. Identifies the frontmost application
2. Uses macOS Accessibility APIs to read the app's menu bar structure
3. Reconstructs the menu as a native `NSMenu`
4. Displays the menu at the current cursor position
5. Forwards the selection to the original application

## Compatibility

MenuAnywhere works with most macOS applications:

**Works well:**

- Native macOS apps (Finder, Safari, Mail, etc.)
- Most third-party apps
- Electron apps (VS Code, Slack, Discord)

**May have issues:**

- Apps with custom menu implementations
- Apps that dynamically generate menus
- Some Java applications

## Conflicts

### With Context Menus

If an app shows a context menu on right-click:

- Without modifiers: App's context menu appears
- With modifiers: MenuAnywhere's menu appears

### With Other Tools

MenuAnywhere may conflict with:

- BetterTouchTool (if using similar triggers)
- Keyboard Maestro
- Other menu bar tools

**Resolution:** Use a unique modifier combination that doesn't conflict.

## Troubleshooting

### Menu not appearing

1. Verify `enabled` is set to `true`
2. Check Accessibility permissions are granted
3. Ensure modifier keys are held before clicking
4. Try with a simple app like Finder first

### Wrong menu appears

1. The frontmost app's menu is shown
2. Click on the desired app to focus it first
3. Check that Stache has Accessibility access

### Menu items not working

1. Some apps may not respond to programmatic menu activation
2. Try the native menu bar as a fallback
3. Check if the app has unusual menu implementation

### Accessibility permission issues

1. Remove Stache from Accessibility list
2. Re-add it and ensure the checkbox is checked
3. Restart Stache
4. If issues persist, restart your Mac

### Conflict with existing shortcuts

1. Change modifiers to a unique combination
2. Disable conflicting tools temporarily to test
3. Use `middleClick` if right-click conflicts exist
