# autopilot

Configure Claude Code permissions and autonomous operation so Claude can work without constant approval prompts.

## Motivation

Out of the box, Claude Code prompts for permission on nearly every shell command. This is safe but disruptive — especially for longer tasks or when you step away. The alternative (`--dangerously-skip-permissions`) removes all guardrails.

## What This Plugin Does

**Permission setup** — Interactive walkthrough that detects your project type and configures scoped allow/deny rules. Claude gets permission for the commands it needs (build, test, lint) while dangerous operations stay gated.

**Autonomous mode** — Optional mode where Claude auto-denies unknown commands instead of blocking on a prompt. Requires `acceptEdits` mode (Shift+Tab). Claude works through what it can and reports what it couldn't. When a command is denied, the plugin guides Claude to check for permitted alternatives before proposing new permission rules.

**Deno sandbox** — Optional sandboxed JavaScript/TypeScript execution environment. Claude can write and run scripts with controlled filesystem and network access, without needing broad `bash` permissions. Sandbox grants (e.g. network, write access) are scoped to the session.
