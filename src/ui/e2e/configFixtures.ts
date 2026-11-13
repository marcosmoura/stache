/** Realistic configuration files with comments for the settings editor's end-to-end runs. */

export const TILING = `#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/tiling.schema.json
# Tiling for the laptop and the studio display.
enabled = true
default_layout = "dwindle" # most workspaces tile

[gaps]
# Pixels between windows.
inner = 8
outer = 12

[master]
ratio = 60 # percent

# Coding first.
[[workspaces]]
name = "coding"
layout = "master"

# Browsing.
[[workspaces]]
name = "browser"
screen = "secondary"

# Chat and mail.
[[workspaces]]
name = "comms"
`;

export const KEYBINDINGS = `#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/keybindings.schema.json
# Global shortcuts.
enabled = true

[bindings]
# Terminal.
"cmd+shift+t" = "open -a Ghostty"
# Browser, then a note.
"cmd+shift+b" = ["open -a Safari", "echo done"] # two steps
"cmd+shift+m" = "open -a Mail"
`;

export const BAR = `#:schema https://raw.githubusercontent.com/marcosmoura/stache/main/schemas/bar.schema.json
# The status bar.
enabled = true
height = 32 # points

[layout]
# Left to right.
start = ["spaces"]
end = ["cpu", "clock"]
`;
