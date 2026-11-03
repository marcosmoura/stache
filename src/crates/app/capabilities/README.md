# Capabilities

The tray-only launch creates no webview. The explicit developer **Open Test Window** action creates a
`stache-test-*` window, and only `test-window.json` applies to it: listen/unlisten for host events and
close itself. There is no filesystem, shell, dialog, HTTP, media, weather, system or tray permission.

`app.security.capabilities` lists that capability explicitly, so a new file in this directory is not
silently enabled. Never add a broad `core:default` here; grant named permissions instead.
