# Hold-to-Quit (planned)

Read when locating the Hold-to-Quit rewrite plan. **NOT SHIPPED: no `commandQuit` config, runtime, HUD or input handling exists in Stache Next yet.**

The [draft spec](../specs/2026-11-03-hold-to-quit/00-overview.md) proposes `stache-hold-to-quit`, a default-disabled `commandQuit` module that keeps the legacy `enabled`/`holdDuration` keys. While enabled, it would:

- Swallow a Command-only physical keycode-12 press only with confirmed frontmost/exemption data and a ready native, non-activating progress HUD. Required HUD failure means Unavailable and pass-through for new presses, not an invisible mode.
- Gracefully terminate the exact application that was frontmost at key-down once the hold reaches `holdDuration`, with no force quit.
- Let ⌘Q pass through when Finder or Stache is frontmost.
- Cancel on frontmost change, modifier release, pause/stop/reconfigure or tap interruption, preserving the swallowed physical press's remaining pairing. Revalidate the exact app on main before HUD/quit; notification delivery is not atomic with physical input.

Defaults would be `enabled: false` and `holdDuration: 1500` milliseconds, preserving the full `u64` duration range including zero. Quit would require successful matching HUD-show acknowledgement. Native display placement would use public geometry when available and report a mouse/main-screen fallback otherwise; pixel design is an engineering proposal.

Input would come from the shared host SDK service planned by Keybindings, not a private tap, with Hold-to-Quit > MenuAnywhere > Keybindings priority and shared ordinary-hotkey conflict registration. Controls would be generic tray Enable and session Pause/Resume; no CLI, endpoint (including read-only status) or Settings screen is planned.

Each draft part needs separate approval. Current `main` `ad34b46` already ships noTunes (`ccae030`), its applications adapter and catalog migration hook; this older worktree must rebase before implementation. Hold-to-Quit still needs its config, required HUD, additive frontmost/graceful-control extensions and the shared input/conflict service. Keep Awake and Wallpapers remain separate-branch drafts. Planned APIs are not callable shipped APIs. This notice will be replaced with verified behavior as parts ship; it does not fix the separate Stache own-Cmd-Q/logout teardown gap.

For the old behavior, read the [archived legacy feature](../../_legacy/docs/features/hold-to-quit.md).
