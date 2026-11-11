---
status: draft
---

# 06 — Spaces

## Scope

The Spaces widget in `widgets/Spaces/`: the workspace list, the app list, focus clicks, motion and the extended icon tables. Depends on part 02. No Rust change.

## Requirements

- **Data.** `BarView.tiling` through a `select`; no query, listener or timer of its own.
- **States.** `null`, `absent` and zero workspaces render nothing. `loading` renders a `Loading...` item. `available` renders the workspace list followed by the app list.
- **Workspaces.** Sorted terminal, coding, browser, music, design, communication, guitar, misc, files, tasks, then unknown names alphabetically. Each is an icon-only item with its name as the accessible name; the focused one is marked active, and a shared highlight moves between workspaces with today's spring (`layoutId`, bounce 0).
- **Apps.** The windows of the focused workspace in Tiling's order. Each shows the app's icon; the focused app also shows its label, revealed by width and opacity with a duration that scales with the label length, as today. The label is the app name; when an app has more than one window it is the non-empty window title truncated to 40 characters plus `...` (25 in compact mode). In compact mode only the focused app is listed.
- Items enter and leave with presence animations; everything is instant under reduced motion.
- **Clicks.** A workspace click sends `focusWorkspace { name }` and an app click sends `focusWindow { window: key }` through the Tiling mutation, with the generation and sequence of the strip that was rendered. Nothing is sent unless the strip is `available`.
- **Icon tables** (`widgets/Spaces/icons.ts`, loaded with the widget chunk):
  - Workspace icons for the ten known names and a default.
  - App icons keyed by exact application name, trimmed, and matched after removing a leading U+200E mark (WhatsApp's bundle name has one).
  - Coverage: every application in the user's Tiling configuration on 2026-11-11 (Ghostty, DeviceHub, Herdr, Code, Zed, Zed Preview, Aside, Safari, Spotify, AIOStreams, Nuvio, WhatsApp, Discord, Slack, Figma, FigCraft, Transmission, Audio MIDI Setup, Ableton Live, the Neural DSP plugins, Finder, Bloom, Reminders, Proton Pass, Proton VPN), every other entry `main` has today, and widely used macOS apps: the Apple apps (Mail, Calendar, Notes, Messages, FaceTime, Music, Podcasts, TV, Photos, Preview, Maps, Books, Calculator, System Settings, Terminal, Xcode, Simulator, App Store, Activity Monitor), browsers (Google Chrome, Firefox, Arc, Brave Browser, Microsoft Edge, Opera, Vivaldi, Orion), terminals and editors (iTerm2, Warp, kitty, Alacritty, WezTerm, Cursor, Sublime Text, IntelliJ IDEA, WebStorm, PyCharm, Android Studio, Neovide), work and chat (Notion, Obsidian, Linear, Telegram, Signal, Microsoft Teams, Microsoft Outlook, Microsoft Word, Microsoft Excel, Microsoft PowerPoint, zoom.us, Google Meet, ChatGPT, Claude), tools (1Password, Bitwarden, Docker Desktop, Postman, GitHub Desktop, TablePlus, Raycast), media and design (VLC, IINA, OBS, Steam, TIDAL, Sketch, Adobe Photoshop, Adobe Illustrator, Blender).
  - Source order per app: a Hugeicons free brand icon; otherwise a `@icons-pack/react-simple-icons` brand icon; otherwise the Hugeicons icon for its category. Unknown apps use the dashboard default.
  - Confirm each application's real bundle name (`CFBundleName`) where it is installed; for apps that are not installed use the published name.
- `components/Icon` accepts a simple-icons component as well as a Hugeicons icon, with the same size, color and accessibility behavior.
- Register `spaces`; the production registry now covers all eight kinds.

## Acceptance criteria

- [ ] MIG-06-01: Every strip state renders as specified; the sort order holds for known and unknown names.
- [ ] MIG-06-02: Label rules hold for single and multiple windows, empty titles, both truncation lengths and the compact single-app rule.
- [ ] MIG-06-03: Focus changes move the highlight and reveal the new label; adding and removing apps animates; reduced motion makes all of it instant.
- [ ] MIG-06-04: Clicks send the exact request with the rendered generation and sequence; nothing is sent in any state other than `available`.
- [ ] MIG-06-05: Every key in both icon tables renders an icon; every app listed above has an entry; the U+200E case resolves; unknown names use the default.
- [ ] MIG-06-06: A strip update re-renders only the changed items, and frame-only snapshot changes re-render nothing in Spaces.
- [ ] MIG-06-07: `docs/features/bar.md` restores the Tiling strip UI section as shipped, including how to add an app icon.
- [ ] MIG-06-08: `bun run lint`, `bun run format:check` and `bun run test` pass.

## Tests

- Widget tests for states, sorting, labels, compact mode and clicks.
- Icon table tests that enumerate every entry and assert the required names exist.
- Motion tests with animations enabled for the highlight, the label reveal and presence, plus reduced motion.
- Render-count test.
- End-to-end `spaces.spec.ts`: a realistic session through the fake host: several workspaces and apps, focus another app, focus another workspace, open and close windows, an app with two windows, Tiling going absent and returning, clicks with the recorded requests, and a stale click outcome that changes nothing.
- Screenshot baselines of the strip in dark and light, wide and compact.

## Notes

- Behavior reference on `main`: `docs/features/bar.md` (Tiling strip UI) and the `Spaces/` directory.
- The user's Tiling configuration is `~/.config/stache/tiling.toml`; it lists bundle identifiers, and the names above were resolved from it on 2026-11-11.
