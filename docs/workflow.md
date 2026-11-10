# Agent workflow

Read when you start a task and need to decide how to run it, or when you change the agent setup.

## Two modes

| Task                                                                            | How                                                                       |
| ------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Simple: one obvious change, a few files, no design questions                    | Work directly in the current checkout, in a normal session.               |
| Substantial: new feature, multi-file change, design decisions, several sessions | Run the orchestrator: spec first, then implement in an isolated worktree. |

Starting the orchestrator:

- Claude Code: `claude --agent orchestrator`, or `/orchestrate <task>` inside a normal session.
- OpenCode: select the `orchestrator` primary agent. The `plan` agent is disabled in this project.
- Codex: `$orchestrate <task>` inside a normal session. Codex loads `.codex/agents/` only when you trust the project.

## Agents

| Agent          | Role                                                                                          | Writes            |
| -------------- | --------------------------------------------------------------------------------------------- | ----------------- |
| `orchestrator` | Refines the request, writes the spec, creates the worktree, delegates, reviews diffs, commits | spec, git, status |
| `worker`       | Implements one spec part with tests                                                           | code, docs        |
| `reviewer`     | Reviews the finished branch against the spec                                                  | nothing           |

The steps are defined in one place, [`.agents/roles/orchestrator.md`](../.agents/roles/orchestrator.md). In short: refine, write the spec, get your approval once, then implement every part in a worktree with one commit per part, review, fix, review again, and ask you what to do with the branch.

Role definitions live in `.agents/roles/<agent>.md`. The harness files (`.claude/agents/*.md`, `.opencode/agents/*.md`, `.opencode/opencode.jsonc`, `.codex/agents/*.toml`) only add model, effort, tools and permissions. Edit the role, not the wrapper. Codex role files cannot restrict the sandbox, so its `reviewer` is read-only by its role definition only.

Subagents never spawn other agents. The orchestrator does all git operations. There are no global subagents; a repository without this setup runs as a single session.

## Merge gate

A branch with a spec may only be merged, or proposed for merge, when every part file (`NN-*.md`, not the overview) in its spec directory has `status: done`. If any part is `draft` or `in-progress`, the orchestrator refuses and names the unfinished parts. Keeping or discarding the branch is always allowed.

This gate applies to implementation branches, not to landing an approved spec on `main` before implementation.

Unfinished work can resume in a later session: start the orchestrator and name the slug.

## Worktrees

- Location: `.worktrees/<slug>` (gitignored, and ignored by oxlint, oxfmt and the Vite watcher).
- Branch: `feat/<slug>` or `fix/<slug>`, created from `main` after the spec is committed there.
- Each worktree has its own `node_modules` and `target/`. The first build is slow.
- Linked-worktree prepare retains the shared hook configuration.
- After a merge: `git worktree remove .worktrees/<slug>`, and delete the branch if you agree.
