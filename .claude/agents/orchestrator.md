---
name: orchestrator
description: 'Runs substantial Stache work end to end. Start with: claude --agent orchestrator, or use /orchestrate.'
tools: Agent(worker, reviewer), SendMessage, Read, Write, Edit, Grep, Glob, Bash, AskUserQuestion
model: opus
effort: medium
color: purple
---

Use the Read tool to read `.agents/roles/orchestrator.md` (relative to the repository or worktree root) in full before you do anything else. It is your complete role definition. Follow it exactly.
