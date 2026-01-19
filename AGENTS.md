# AGENTS.md

**Stache** is a macOS-only Tauri 2.x desktop app providing a custom status bar, tiling window manager, wallpaper management, and audio device switching.

## Essentials

- **Package manager**: `pnpm`
- **Platform**: macOS only (uses Accessibility APIs, NSDistributedNotificationCenter)
- **Strict linting**: Clippy `pedantic` + `nursery` enabled

## Commands

```bash
pnpm tauri:dev      # Development (full app with hot reload)
pnpm test           # All tests
pnpm lint           # All linters
pnpm format         # Prettier + cargo fmt
./scripts/generate-schema.sh  # Regenerate JSON Schema after config changes
```

## Detailed Documentation

See [.agents/](.agents/README.md) for comprehensive guides:

- [Architecture](.agents/architecture.md) — Binary modes, IPC, directory structure
- [Rust Patterns](.agents/rust-patterns.md) — Tauri commands, events, config types
- [React Patterns](.agents/react-patterns.md) — Components, hooks, Linaria styling
- [Tiling WM](.agents/tiling.md) — Layouts, borders, workspace management
- [Common Tasks](.agents/common-tasks.md) — Adding widgets, CLI commands, config options
- [Gotchas](.agents/gotchas.md) — Critical pitfalls to avoid
