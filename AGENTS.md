# AGENTS.md

**Stache** is a macOS-only Tauri 2.x desktop app providing a custom status bar, tiling window manager, wallpaper management, and audio device switching.

## Essentials

- **Package manager**: `pnpm`
- **Platform**: macOS only (uses Accessibility APIs, NSDistributedNotificationCenter)
- **Strict linting**: Clippy `pedantic` + `nursery` enabled

## Commands

```bash
pnpm tauri:dev                # Development (full app with hot reload)
pnpm test                     # All tests
pnpm lint                     # All linters
pnpm format                   # Oxfmt + cargo fmt
./scripts/generate-schema.sh  # Regenerate JSON Schema after config changes
```

## Detailed Documentation

See [docs/agents/](docs/agents/README.md) for comprehensive guides:

- [Architecture](docs/agents/architecture.md) — Binary modes, IPC, directory structure
- [Rust Patterns](docs/agents/rust-patterns.md) — Tauri commands, events, config types
- [React Patterns](docs/agents/react-patterns.md) — Components, hooks, Linaria styling
- [Tiling WM](docs/agents/tiling.md) — Layouts, borders, workspace management
- [Common Tasks](docs/agents/common-tasks.md) — Adding widgets, CLI commands, config options
- [Gotchas](docs/agents/gotchas.md) — Critical pitfalls to avoid

## Superpowers Preferences

- Save Superpowers specs in `docs/tasks/specs/`.
- Save Superpowers plans in `docs/tasks/plans/`.
- When a Superpowers skill references `docs/superpowers/specs`, use `docs/tasks/specs/` instead.
- When a Superpowers skill references `docs/superpowers/plans`, use `docs/tasks/plans/` instead.
