# Capabilities

The tray-only launch creates no webview. Every app command is listed in the app manifest in
`build.rs`. Once the manifest lists commands, Tauri refuses a command missing from it for every
window ("not allowed. Command not found"), and a listed command only for windows whose capability
grants it, so read-only commands are granted explicitly too.

The Accessibility guidance window (`stache-access-*`, only while the
launch gate blocks) is governed by `access-window.json`: listen/unlisten, close itself, start dragging
(`core:window:allow-start-dragging`, for the frameless header; the window is transparent through the
app-wide `macos-private-api` setting), `app_info`, `host_status` and the `request_accessibility`,
`open_accessibility_settings` and `quit_app` commands. The Bar (`stache-bar-*`, `bar.json`) gets only
its `bar_*` commands. The tray popover (`stache-popover`) is governed by `popover-window.json`:
listen/unlisten, `host_status`, `hide_popover`, the Overview's `set_module_enabled`,
`module_session_action`, `retry_module` and `open_accessibility_guide`, the settings
editor's `config_files`, `config_document`, `config_edit` and `open_config_file` (all of these except
`host_status` granted to no other window), plus the three Accessibility commands. There is no
filesystem, shell, dialog, HTTP, media, weather, system or tray permission.
`tests/host_policy.rs` reads each surface's UI sources and fails when a capability misses a command
its UI calls.

`app.security.capabilities` lists these capabilities explicitly, so a new file in this directory is not
silently enabled. Never add a broad `core:default` here; grant named permissions instead.
