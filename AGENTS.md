# AGENTS.md

**Stache** is a macOS-only Tauri 2.x desktop app: custom status bar, tiling window manager, wallpapers, audio device switching, keybindings and small system tweaks.

## Essentials

- **Package manager**: `pnpm`
- **Platform**: macOS only (Accessibility APIs, `NSDistributedNotificationCenter`)
- **Rust**: nightly toolchain, Clippy `pedantic` + `nursery`, warnings are errors
- **UI**: React + TypeScript + Linaria, tests run in a real browser (Vitest + Playwright)

## Commands

```bash
pnpm tauri:dev                # Run the full app with hot reload
pnpm test                     # All tests (vitest + cargo nextest)
pnpm lint                     # All linters (tsc, oxlint, stylelint, cargo sort, clippy)
pnpm format                   # oxfmt + cargo fmt
./scripts/generate-schema.sh  # Regenerate stache.schema.json after config type changes
```

## Documentation

Start at [docs/index.md](docs/index.md). It routes you to the one or two docs your task needs. Don't read the whole `docs/` tree.

## Workflow

- Simple task (one obvious change, a few files): work on it directly in the current checkout.
- Substantial task: use the orchestrator (OpenCode agent `orchestrator`, or `claude --agent orchestrator` / `/orchestrate` in Claude Code). See [docs/workflow.md](docs/workflow.md).
- This project's subagents are `explorer`, `spec-writer`, `worker` and `reviewer`. The global `scout`, `shipper`, `explore` and `general` agents are disabled here: use `explorer` for scoping, and do git operations yourself.

## Hard rules

- Never merge a branch whose spec in `docs/specs/` has a part that isn't `status: done`.
- When you change behavior a doc describes, update that doc in the same change.
- Never set a git author or identity yourself.
