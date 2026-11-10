# Kickoff: Tiling snapshot animation

Read when starting implementation of this spec in a fresh session. Paste the prompt below.

```text
Implement the Stache spec docs/specs/2026-11-10-tiling-snapshot-animation/ (start at 00-overview.md; follow AGENTS.md and docs/workflow.md).
Model (decision 1): park the real window in a hide corner at its final size, animate its snapshot overlay, then reveal it with one position-only write.

1. On main, verify every path:line citation in the spec against the current code. If any drifted, fix the spec and commit it alone as `docs(spec): re-check tiling-snapshot-animation citations against main`.
2. OQ-1..OQ-4 are resolved in 00-overview.md. Record any new question under Open questions and get it answered before the affected part starts.
3. Create the worktree per docs/workflow.md: `git worktree add .worktrees/tiling-snapshot-animation -b feat/tiling-snapshot-animation main`, then `bun install --frozen-lockfile` in it.
4. Present the plan for parts 01→05 and ask for approval once. After approval, implement 01, 02, 03, 04, 05 in order (01–03 may run in parallel), each one compiling, testing and committing alone and leaving animations working (live).
5. Validate every part with `bun run lint`, `bun run format:check` and `bun run test`.
6. Update docs/features/tiling.md, the other affected docs and docs/roadmap.md in the same change as the behavior.
7. Native gates N1–N9 each need my explicit per-run approval naming apps, display, temporary config and cleanup. Never run one without it.
8. Merge only when every part is `done`, and only when I ask.

Environment:
- In a worktree, git needs `-c core.fsmonitor=false`. bun, tests and working-tree git must run outside the sandbox.
- Use Context7 or crates.io to verify the objc2-screen-capture-kit version and features and how Tauri 2 merges Info.plist keys.

Lessons from the previous animation run:
- Fakes must model real AX write latency (5–10 ms per write) and several windows per application on a 120 Hz display. Per-slice fakes alone missed broken basics, so write realistic end-to-end scenarios over the real actor and the real sharded adapter.
- Before guessing at a real-desktop failure, get a real debug log: set the key `[logging] level = "debug"` in `global.toml` (not `tiling.toml`) and run `bun run tauri:dev`.
- Every "give up" heuristic (budgets, fallbacks) needs a test showing that a healthy application under realistic latency does not trip it.
```
