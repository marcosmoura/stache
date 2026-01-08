# Global Keybindings

Stache allows you to define system-wide keyboard shortcuts that execute commands, scripts, or Stache actions.

## Features

- System-wide shortcuts (work in any application)
- Execute single or multiple commands
- Block system shortcuts
- Support for modifier key combinations

## Configuration

### Basic Setup

```jsonc
{
  "keybindings": {
    "Command+Control+R": "stache reload",
    "Command+Control+W": "stache wallpaper set --random",
  },
}
```

### Multiple Commands

Execute multiple commands sequentially:

```jsonc
{
  "keybindings": {
    "Command+Control+T": ["open -a Terminal", "stache wallpaper set --random"],
  },
}
```

### Block Shortcuts

Capture a shortcut without executing any action:

```jsonc
{
  "keybindings": {
    "Command+H": "", // Block hide window shortcut
  },
}
```

## Key Format

### Modifier Keys

| Key       | Aliases                |
| --------- | ---------------------- |
| `Command` | `Cmd`, `Super`, `Meta` |
| `Control` | `Ctrl`                 |
| `Option`  | `Alt`, `Opt`           |
| `Shift`   | -                      |

### Special Keys

| Key                           | Description        |
| ----------------------------- | ------------------ |
| `Space`                       | Spacebar           |
| `Tab`                         | Tab key            |
| `Return`                      | Enter/Return key   |
| `Escape`                      | Escape key         |
| `Delete`                      | Delete/Backspace   |
| `Backquote`                   | `` ` `` (backtick) |
| `F1` - `F12`                  | Function keys      |
| `Up`, `Down`, `Left`, `Right` | Arrow keys         |

### Format Rules

- Modifiers are joined with `+`
- Order doesn't matter: `Command+Control+R` = `Control+Command+R`
- Case-insensitive: `command+r` = `Command+R`
- The main key comes last: `Command+Control+W`

### Examples

```jsonc
{
  "keybindings": {
    // Simple modifier + key
    "Command+R": "stache reload",

    // Multiple modifiers
    "Command+Control+R": "stache reload",

    // With Shift
    "Command+Shift+W": "stache wallpaper set --random --screen main",

    // Function key
    "Command+F12": "open -a 'Activity Monitor'",

    // Special keys
    "Command+Control+Space": "open -a Spotlight",
    "Command+Control+Backquote": "open -a Terminal",
  },
}
```

## Value Format

### Single Command (String)

```jsonc
{
  "keybindings": {
    "Command+Control+R": "stache reload",
  },
}
```

### Multiple Commands (Array)

Commands are executed sequentially in order:

```jsonc
{
  "keybindings": {
    "Command+Control+T": ["open -a Terminal", "sleep 1", "stache reload"],
  },
}
```

### Block Shortcut (Empty String)

Captures the shortcut without any action:

```jsonc
{
  "keybindings": {
    "Command+H": "", // Prevents hiding windows
  },
}
```

## Common Use Cases

### Stache Commands

```jsonc
{
  "keybindings": {
    // Reload configuration
    "Command+Control+R": "stache reload",

    // Random wallpaper
    "Command+Control+W": "stache wallpaper set --random",

    // Random wallpaper on main screen
    "Command+Control+Shift+W": "stache wallpaper set --random --screen main",

    // Clear cache
    "Command+Control+C": "stache cache clear",
  },
}
```

### Open Applications

```jsonc
{
  "keybindings": {
    "Command+Control+T": "open -a Terminal",
    "Command+Control+F": "open -a Finder",
    "Command+Control+S": "open -a Safari",
    "Command+Control+V": "open -a 'Visual Studio Code'",
  },
}
```

### Run Scripts

```jsonc
{
  "keybindings": {
    // Shell script
    "Command+Control+B": "~/.local/bin/backup.sh",

    // Python script
    "Command+Control+P": "python3 ~/scripts/process.py",

    // Multiple commands
    "Command+Control+D": ["cd ~/Development", "code ."],
  },
}
```

### Block System Shortcuts

```jsonc
{
  "keybindings": {
    // Prevent accidental window hiding
    "Command+H": "",

    // Prevent accidental quit
    "Command+Q": "",

    // Prevent minimize
    "Command+M": "",
  },
}
```

### Window Manager Integration

```jsonc
{
  "keybindings": {
    // yabai workspace switching
    "Command+1": "yabai -m space --focus 1",
    "Command+2": "yabai -m space --focus 2",
    "Command+3": "yabai -m space --focus 3",

    // Move window to workspace
    "Command+Shift+1": "yabai -m window --space 1",
    "Command+Shift+2": "yabai -m window --space 2",
  },
}
```

## Permissions

Global keybindings require **Accessibility permissions**:

1. Open **System Preferences** > **Security & Privacy** > **Privacy**
2. Select **Accessibility** from the sidebar
3. Add Stache to the allowed list

Without this permission, keybindings won't work.

## Conflicts

### With System Shortcuts

If a Stache keybinding conflicts with a system shortcut:

- Stache's keybinding takes precedence
- The system shortcut is blocked

### With Application Shortcuts

If a keybinding conflicts with an application shortcut:

- Stache's keybinding takes precedence
- The application won't receive the shortcut

### Resolution

To avoid conflicts:

- Use modifier combinations like `Command+Control+key`
- Check System Preferences > Keyboard > Shortcuts for existing bindings
- Use the empty string `""` to explicitly block shortcuts

## Hot Reload

After modifying keybindings, reload the configuration:

```bash
stache reload
```

Or use a keybinding (if you have one set up):

```jsonc
{
  "keybindings": {
    "Command+Control+R": "stache reload",
  },
}
```

## Troubleshooting

### Keybinding not working

1. Verify Accessibility permissions are granted
2. Check for typos in the key combination
3. Look for conflicts with system/app shortcuts
4. Run `stache reload` to apply changes

### Command not executing

1. Test the command in Terminal first
2. Check file paths are correct (use absolute paths when possible)
3. Ensure scripts have execute permission: `chmod +x script.sh`

### Modifier keys not recognized

Use the canonical names:

- `Command` (not just `Cmd` in some edge cases)
- `Control` (not just `Ctrl` in some edge cases)
- `Option` (not just `Alt` in some edge cases)

### Shortcut captured but nothing happens

1. If using an empty string `""`, the shortcut is intentionally blocked
2. Check if the command path is correct
3. Verify the command exists and is executable
