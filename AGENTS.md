# AGENTS.md

**Stache Next** is a macOS 26+ Tauri 2 tray-only skeleton. Typed configuration,
the standalone CLI and the control-socket foundation ship; runtime modules do not.
New sources live in `src/crates/` and `src/ui/`; root configs/scripts/docs remain at repository root.
The independent old application lives in `_legacy/`; do not port its behavior without an approved part.

## Essentials

- Package manager: Bun 1.4.4; pnpm is allowed only inside `_legacy/`.
- Rust: retained nightly/rustfmt, workspace Clippy pedantic + nursery, warnings as errors.
- UI: React + TypeScript + StyleX + React Compiler/Vite; TanStack Query, zustand, Motion, Hugeicons.
- UI tests: Vitest browser mode + Playwright Test e2e, both WebKit, not Bun's test runner.

```sh
bun run tauri:dev
bun run lint
bun run format:check
bun run test        # test:ui + test:e2e + test:scripts + test:native
bun run test:ui:coverage
bun run generate-schema
cargo run -q -p stache-app --bin stache -- schema   # one binary, CLI mode
bun run format # explicit mutation
```

Start at [docs/index.md](docs/index.md), then read only task-relevant docs.
Root checks exclude archive/build/generated/worktree inputs. Never launch legacy automation for a build.

## Workflow

Small changes can be done directly. Substantial work uses the orchestrator: `claude --agent orchestrator`
or `/orchestrate` (Claude Code), the `orchestrator` agent (OpenCode) or `$orchestrate` (Codex);
see [docs/workflow.md](docs/workflow.md). Its only subagents are `worker` and `reviewer`.

## Hard rules

- Never merge a branch with an unfinished part in `docs/specs/`.
- When merging, always squash commits and preserve the original author.
- After a merge into `main`, remove the branch's worktree and delete the branch.
- Ask approval of the spec before implementing it; planned APIs are not shipped APIs.
- Update behavior docs in the same change. Preserve archived historical instructions.
- Never set git identity. Linked-worktree prepare must not change shared hook configuration.
- Keep legacy/new identities distinct; never terminate a process by shared executable name.
