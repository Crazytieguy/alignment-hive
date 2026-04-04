---
name: resolve-denied-toolcall
description: This skill should be used when a tool call is denied in autonomous mode. Guides through checking existing permissions for alternative paths, and if none exist, proposing well-scoped permission rules — without it, denied tool calls tend to either block on user input or cause subtasks to be silently skipped.
---

# Resolve Denied Tool Call

Proposing a permission is costly — it blocks progress until the user notices and approves it, and they may be away, asleep, or running other agents. Invest real effort into checking whether the allow list already covers what's needed through a different path.

## Check for Alternatives First

Read `.claude/settings.json` and `.claude/settings.local.json` (in the working directory, not global). Look for permitted commands that accomplish the same thing — especially project scripts and allowed package manager invocations.

If the deno sandbox is available, consider whether the task can be done by writing TypeScript in the sandbox instead.

Examples of non-obvious alternatives:

- `Bash(bash scripts/check-gpu.sh *)` is allowed — no need for `ssh lab-server nvidia-smi`. The script already SSHes to the configured server.
- `Bash(bun run db:query *)` is allowed — no need for `psql -c "SELECT..."`. The script runs queries with proper credentials.
- `Bash(uv run scripts/download_model.py *)` is allowed — no need for `curl -L -o models/gpt2.bin https://...`. The script handles authenticated downloads from the project's model registry.

## Continue With Other Work

If the denied command only blocks one part of the task, move on to other parts. Batch the permission proposal for when no further progress is possible.

## Proposing a Permission

Only after confirming no permitted alternative exists, read `references/adding-permissions.md` for how to propose a well-scoped rule.
