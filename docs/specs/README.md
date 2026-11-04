# Specs

Read when you write, implement or check the status of a spec.

Each piece of substantial work gets a spec directory. Specs stay here after merge as a record of decisions. The lasting description of how a feature works belongs in `docs/features/`, not in the spec.

An approved spec lands on `main` before implementation; its `branch:` frontmatter names the branch where the code is built. When that branch merges later, do not re-add a conflicting copy: keep `main`'s spec files and take only the code and status updates.

## Layout

```text
docs/specs/<yyyy-mm-dd>-<slug>/
  00-overview.md     # goal, non-goals, decisions, parts table
  01-<part>.md       # one file per part, in dependency order
  02-<part>.md
```

Medium work can have a single part. Split when the work won't fit comfortably in one worker session. Each part must be implementable, testable and committable on its own.

## `00-overview.md`

```markdown
---
branch: feat/<slug>
---

# <Title>

## Goal

## Non-goals

## Decisions

## Parts

| Part               | Summary | Status |
| ------------------ | ------- | ------ |
| [01](01-<part>.md) | …       | draft  |

## Affected docs
```

## Part file

```markdown
---
status: draft # draft | in-progress | done
---

# NN — <Part title>

## Scope

## Requirements

## Acceptance criteria

- [ ] …

## Tests

## Notes
```

## Status rules

- `draft`: written, not started.
- `in-progress`: a worker is implementing it, or it was started and isn't finished.
- `done`: implemented, verified by the orchestrator, committed.
- Status lives only in the part files' frontmatter. The overview's parts table mirrors it, and the orchestrator keeps both in sync.
- Only the orchestrator changes `status`.
- The merge gate in [../workflow.md](../workflow.md#merge-gate) blocks merging while any part isn't `done`.
