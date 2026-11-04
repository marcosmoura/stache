# Agent workflow

Read when you start a task and need to decide how to run it, or when you run as the orchestrator or one of its subagents.

## Two modes

| Task                                                                            | Agent                                              | How                                                 |
| ------------------------------------------------------------------------------- | -------------------------------------------------- | --------------------------------------------------- |
| Simple: one obvious change, a few files, no design questions                    | `build` (OpenCode) / default session (Claude Code) | Work directly in the current checkout.              |
| Substantial: new feature, multi-file change, design decisions, several sessions | `orchestrator`                                     | Spec first, then implement in an isolated worktree. |

Starting the orchestrator:

- OpenCode: select the `orchestrator` primary agent. The `plan` agent is disabled in this project.
- Claude Code: `claude --agent orchestrator`, or `/orchestrate <task>` inside a normal session.

## Agents

| Agent          | Role                                                                                                           | Writes?          |
| -------------- | -------------------------------------------------------------------------------------------------------------- | ---------------- |
| `orchestrator` | Asks you questions, owns scope and decisions, creates the worktree, delegates, reviews, commits, guards merges | git, spec status |
| `explorer`     | Maps the relevant docs and code, cites `path:line`                                                             | no               |
| `spec-writer`  | Writes `docs/specs/<dir>/` and updates the feature docs                                                        | docs             |
| `worker`       | Implements one spec part with tests, runs lint and tests                                                       | code, docs       |
| `reviewer`     | Reviews the branch against the spec. Runs only when you ask                                                    | no               |

Role definitions live in `.agents/roles/<agent>.md`. The harness files (`.opencode/agents/*.md`, `.claude/agents/*.md`, `.opencode/opencode.jsonc`) only add model, tools and permissions. Edit the role, not the wrapper.

Subagents never spawn other agents. The orchestrator does all git operations.

## Orchestrator flow

1. **Refine.** Ask questions until scope, non-goals and acceptance criteria are agreed. Optionally run `explorer`.
2. **Spec.** `spec-writer` writes the spec, split into parts if large. The orchestrator reviews it, then commits `docs(spec): add <slug> spec` on `main` before implementation.
3. **Isolate.** Create the branch and worktree from the updated `main`: `git worktree add .worktrees/<slug> -b <feat|fix>/<slug> main`, then `bun install --frozen-lockfile` in it, so the worktree starts with its spec. Implementation happens there. Linked-worktree prepare retains shared hook configuration.
4. **Confirm.** The orchestrator asks you before starting each part.
5. **Implement.** For each approved part: set `in-progress`, run `worker`, review the diff, run `bun run lint`, `bun run format:check` and `bun run test`, set `done`, commit.
6. **Summarize.** Parts and status, commits, verification, open items.
7. **Review (on request).** `reviewer`, then `worker` fixes, then a commit.
8. **Finish.** The orchestrator asks you: merge, push or open a PR, keep, or discard.

## Merge gate

A branch with a spec may only be merged, or proposed for merge, when every part file (`NN-*.md`, not the overview) in its spec directory has `status: done`. If any part is `draft` or `in-progress`, the orchestrator refuses and names the unfinished parts. Keeping or discarding the branch is always allowed.

This gate applies to implementation branches, not to landing approved draft specs on `main` before implementation.

Unfinished work can resume in a later session: start the orchestrator and name the slug. It reads the spec's `00-overview.md` and the worktree's git state, then continues with the next part that isn't done.

## Worktrees

- Location: `.worktrees/<slug>` (gitignored, and ignored by oxlint, oxfmt and the Vite watcher).
- Branch: `feat/<slug>` or `fix/<slug>`.
- Each worktree has its own `node_modules` and `target/`. The first build is slow.
- After a merge: `git worktree remove .worktrees/<slug>`, and delete the branch if you agree.
