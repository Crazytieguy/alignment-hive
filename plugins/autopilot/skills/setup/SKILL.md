---
name: setup
description: This skill should be used when the user asks to set up or configure Claude Code permissions, autonomous mode, and/or deno sandbox for a project.
---

# Autopilot Setup

Configure permissions, autonomous mode, and deno sandbox so Claude can accomplish tasks end-to-end.

## Purpose

Without proper permissions, Claude either blocks on every command (waiting for approval) or requires `--dangerously-skip-permissions`. This setup provides a secure middle ground: Claude gets the permissions it needs to work autonomously, with appropriate guardrails.

Settings files are hot-reloaded (no restart needed) and `settings.local.json` is already gitignored by Claude Code via `.git/info/exclude`.

## Key Principle: Ask vs Deny

**Deny** is for commands that are **never useful in this project** - steering toward correct alternatives.

**Ask** is safer than deny for **sometimes dangerous** commands. With ask, the session stops until the user provides attention. With deny, a potentially compromised model might try variations autonomously. Use ask for commands that are sometimes legitimate but could be misused.

## Re-run Behavior

If existing permissions are already configured (settings files exist with non-trivial content), treat this as a re-run. For each step, check whether the existing configuration already matches one of the options (or a close variation). If it does, skip the step silently. Only ask about things that don't seem set up yet.

This means a user running `:setup` for the second time will only see questions for newly added features or unconfigured areas, not a repeat of everything they already decided.

Exception: if the user explicitly asks to reconfigure from scratch or change previous choices, ask all steps regardless of existing configuration.

## Workflow Overview

1. Detect project type and audit existing permissions (automatic)
2. Ask initial preferences (batch of 3) + edit universally safe commands
3. Ask web access + project script preferences (batch of 2) + edit
4. Ask MCP server permissions + edit (if applicable)
5. Confirm secrets/git/mode/cleanup + edit
6. Autopilot features (autonomous mode, then deno sandbox — separate questions, shared action)

---

## Step 1: Project Detection and Audit

Detect the project type and audit existing permissions automatically before asking questions.

### What to Detect

**Package manager:** Check for lock files (bun.lockb, pnpm-lock.yaml, yarn.lock, package-lock.json, uv.lock, Cargo.lock). If ambiguous, ask the user.

**Scripts:** Look for project scripts that might be needed mid-session (linting, testing, building, data processing). Extract from `package.json` scripts, `pyproject.toml` scripts, or similar config. Also look for standalone scripts in the project (e.g., `scripts/train.py`, `tools/analyze.sh`, `bin/setup`). These will be allowed individually as "project scripts". For monorepos, scripts may only exist in specific workspaces - identify the correct invocation pattern (e.g., `bun run --filter <workspace> <script>`).

**MCP servers:** Check `.mcp.json` for configured servers.

**Secret files:** Look for `.env`, `.env.local`, `.envrc`, credentials files, API keys. Note which ones exist for the secrets confirmation later.

**hive-mind plugin:** If the `hive-mind:retrieval` subagent is available, include hive-mind commands in the universally safe commands.

**Existing permissions:** Read `.claude/settings.json` and `.claude/settings.local.json` if they exist. Note any issues found for the cleanup question later.

### Audit: Check for Issues in Existing Permissions

**Bypass vectors** - Commands that can execute arbitrary code should never be in allow lists:

```
Bash(env *)           # env VAR=val COMMAND runs any command
Bash(xargs *)         # pipes input to any command
Bash(bash -c *)       # executes string as bash
Bash(sh -c *)         # executes string as shell
Bash(eval *)          # evaluates arbitrary code
Bash(time *)          # time COMMAND runs any command
Bash(timeout *)       # timeout N COMMAND runs any command
Bash(exec *)          # replaces shell with command
Bash(nohup *)         # nohup COMMAND runs any command
Bash(nice *)          # nice COMMAND runs any command
Bash(python -c *)     # executes Python string
Bash(python3 -c *)    # executes Python string
Bash(node -e *)       # executes JavaScript string
Bash(perl -e *)       # executes Perl string
Bash(ruby -e *)       # executes Ruby string
Bash(bun run *)       # runs arbitrary scripts (allow specific scripts instead)
Bash(npm run *)       # runs arbitrary scripts (allow specific scripts instead)
```

The general pattern: any command that takes another command or code string as an argument is a bypass vector.

**Deprecated syntax** - Look for rules using `:*` instead of ` *` (space-star). The colon syntax is deprecated **except** for commands that use heredoc arguments (e.g., `git commit:*`). The ` *` pattern fails to match heredoc syntax, so `:*` is required there. For all other commands, replace `:*` with `cmd` + `cmd *` patterns (not `cmd*`).

**Redundancy** - Check for duplicate rules, overly broad rules that subsume specific ones, or rules that conflict with the new configuration. Fix each issue individually - the user might have niche permissions they want to keep.

If it's unclear how to run commands in this project (e.g., Python project without pyproject.toml, unusual monorepo structure), ask the user about their preferred toolchain and the correct way to run scripts.

---

## Step 2: Initial Preferences + Universally Safe Commands

Use AskUserQuestion to ask all three questions in a single batch.

### Q1: Settings Strategy

> "Where should permissions be stored?"

- **Split (Recommended)** - Shared file for universally safe commands, personal file for preferences. Collaborators get safe defaults.
- **Shared only** - All permissions in settings.json. Works across collaborators and environments.
- **Personal only** - All permissions in settings.local.json. For projects where collaborators have existing preferences.

### Q2: Skills

Detect available skills from the system prompt (which lists all available skills).

> "Allow all skills without permission requests?"

- **Yes (Recommended)** - All skills can be used freely
- **Specific skills only** - Only allow specific skills: [list detected skills]

### Q3: Universally Safe Commands

> "Add universally safe commands? (read-only inspection, safe utilities, git status/diff/log)"

- **Yes (Recommended)** - Standard set of read-only commands that are safe in any project
- **No** - Start from scratch, I'll configure manually

### Edit: Skills

**Determine target file based on settings strategy:**
- Split: → settings.local.json (skills may vary per collaborator)
- Shared only: → settings.json
- Personal only: → settings.local.json

If user chose "Yes" (all skills):

```json
{
  "permissions": {
    "allow": [
      "Skill"
    ]
  }
}
```

If user chose "Specific skills only", list individual skills:

```json
{
  "permissions": {
    "allow": [
      "Skill(plugin-name:skill-name)",
      "Skill(plugin-name:other-skill)",
      "Skill(local-skill)"
    ]
  }
}
```

### Edit: Universally Safe Commands (if user confirmed)

**Determine target file based on settings strategy:**
- Split or Shared only: → settings.json
- Personal only: → settings.local.json

Copy verbatim. Note the wildcard patterns - always use `cmd` + `cmd *` (never `cmd*`). Exception: use `cmd:*` for commands that receive heredoc arguments (e.g., `git commit:*`), since ` *` fails to match heredoc syntax.

If hive-mind plugin is detected, also include:
- `Bash(hive-mind)`
- `Bash(hive-mind read)`
- `Bash(hive-mind read *)`
- `Bash(hive-mind search)`
- `Bash(hive-mind search *)`

```json
{
  "permissions": {
    "allow": [
      "Bash(ls)",
      "Bash(ls *)",
      "Bash(find *)",
      "Bash(cat *)",
      "Bash(head *)",
      "Bash(tail *)",
      "Bash(wc)",
      "Bash(wc *)",
      "Bash(file *)",
      "Bash(stat *)",
      "Bash(du)",
      "Bash(du *)",
      "Bash(df)",
      "Bash(df *)",
      "Bash(diff *)",
      "Bash(tree)",
      "Bash(tree *)",
      "Bash(realpath *)",
      "Bash(basename *)",
      "Bash(dirname *)",
      "Bash(mkdir *)",
      "Bash(grep *)",
      "Bash(rg *)",
      "Bash(jq *)",
      "Bash(yq *)",
      "Bash(sort)",
      "Bash(sort *)",
      "Bash(uniq)",
      "Bash(uniq *)",
      "Bash(cut *)",
      "Bash(tr *)",
      "Bash(printf *)",
      "Bash(md5sum *)",
      "Bash(sha256sum *)",
      "Bash(base64 *)",
      "Bash(echo *)",
      "Bash(pwd)",
      "Bash(which *)",
      "Bash(type *)",
      "Bash(command -v *)",
      "Bash(uname)",
      "Bash(uname *)",
      "Bash(whoami)",
      "Bash(date)",
      "Bash(date *)",
      "Bash(ps)",
      "Bash(ps *)",
      "Bash(pgrep *)",
      "Bash(nvidia-smi)",
      "Bash(nvidia-smi *)",
      "Bash(id)",
      "Bash(hostname)",
      "Bash(uptime)",
      "Bash(sleep *)",
      "Bash(export *)",
      "Bash(test *)",
      "Bash(git status)",
      "Bash(git status *)",
      "Bash(git diff)",
      "Bash(git diff *)",
      "Bash(git log)",
      "Bash(git log *)",
      "Bash(git show *)",
      "Bash(git branch)",
      "Bash(git remote)",
      "Bash(git remote -v)",
      "Bash(git stash list)",
      "Bash(git rev-parse *)",
      "Bash(git ls-files)",
      "Bash(git ls-files *)",
      "Bash(xargs cat)",
      "Bash(xargs cat *)",
      "Bash(xargs file)",
      "Bash(xargs file *)",
      "Bash(xargs head)",
      "Bash(xargs head *)",
      "Bash(xargs tail)",
      "Bash(xargs tail *)",
      "Bash(xargs wc)",
      "Bash(xargs wc *)",
      "Bash(xargs stat)",
      "Bash(xargs stat *)",
      "Bash(xargs sort)",
      "Bash(xargs sort *)",
      "Bash(xargs du)",
      "Bash(xargs du *)",
      "Bash(xargs diff)",
      "Bash(xargs diff *)",
      "Bash(xargs basename)",
      "Bash(xargs basename *)",
      "Bash(xargs dirname)",
      "Bash(xargs dirname *)",
      "Bash(xargs grep *)",
      "Bash(xargs cut *)",
      "Bash(xargs -I{} cat *)",
      "Bash(xargs -I{} file *)",
      "Bash(xargs -I{} head *)",
      "Bash(xargs -I{} tail *)",
      "Bash(xargs -I{} wc *)",
      "Bash(xargs -I{} stat *)",
      "Bash(xargs -I{} sort *)",
      "Bash(xargs -I{} du *)",
      "Bash(xargs -I{} diff *)",
      "Bash(xargs -I{} basename *)",
      "Bash(xargs -I{} dirname *)",
      "Bash(xargs -I{} grep *)",
      "Bash(xargs -I{} cut *)"
    ],
    "ask": [
      "Bash(find * -exec *)",
      "Bash(find * -execdir *)"
    ],
    "deny": [
      "Bash(timeout *)",
      "Bash(env *)",
      "Bash(bash -c *)",
      "Bash(sh -c *)",
      "Bash(zsh -c *)",
      "Bash(xargs sh *)",
      "Bash(xargs -I{} sh *)",
      "Bash(xargs bash *)",
      "Bash(xargs -I{} bash *)",
      "Bash(git -C *)"
    ]
  }
}
```

---

## Step 3: Web Access + Project Scripts Questions + Edit

Use AskUserQuestion to ask both questions in a single batch.

### Web Access Question

> "What level of web access do you want?"

**Options:**

- **WebFetch + WebSearch (Recommended)** - All domains via built-in tools. Built-in prompt injection protections.

- **Specific domains only** - Only documentation sites relevant to the project. For when prompt injection protection must be absolute.

### Project Scripts Question

> "Which project scripts should Claude be allowed to run?"

Not all tiers apply to all projects — only show options that make a meaningful difference given the detected scripts. No recommendation.

**Options:**

- **Lint/format/typecheck only** - Safe, deterministic tools only. Tell the user exactly which commands this includes based on what was detected.

- **Tests + lint/format/typecheck** - Also allow test commands.

- **All project scripts** - All detected project scripts (dev, build, test, lint, format, etc.).

### Edit Immediately After

Based on the user's choice, edit the appropriate settings file(s).

**Determine target file based on settings strategy:**
- Split or Shared only: → settings.json
- Personal only: → settings.local.json

**Web access edit:** — Pick option; specific domains requires judgment.

**For "WebFetch + WebSearch":**

```json
{
  "permissions": {
    "allow": [
      "WebFetch",
      "WebSearch"
    ]
  }
}
```

**For "Specific domains only":**

Generate 10+ relevant documentation domains based on project type:

Example for a TypeScript/React project:

```json
{
  "permissions": {
    "allow": [
      "WebFetch(domain:react.dev)",
      "WebFetch(domain:typescriptlang.org)",
      "WebFetch(domain:developer.mozilla.org)",
      "WebFetch(domain:nodejs.org)",
      "WebFetch(domain:bun.sh)",
      "WebFetch(domain:vitejs.dev)",
      "WebFetch(domain:tailwindcss.com)",
      "WebFetch(domain:ui.shadcn.com)",
      "WebFetch(domain:tanstack.com)",
      "WebFetch(domain:zod.dev)",
      "WebFetch(domain:trpc.io)",
      "WebFetch(domain:nextjs.org)",
      "WebSearch"
    ]
  }
}
```

Example for a Python ML project:

```json
{
  "permissions": {
    "allow": [
      "WebFetch(domain:docs.python.org)",
      "WebFetch(domain:pytorch.org)",
      "WebFetch(domain:numpy.org)",
      "WebFetch(domain:pandas.pydata.org)",
      "WebFetch(domain:scikit-learn.org)",
      "WebFetch(domain:huggingface.co)",
      "WebFetch(domain:wandb.ai)",
      "WebFetch(domain:docs.ray.io)",
      "WebFetch(domain:jax.readthedocs.io)",
      "WebFetch(domain:einops.rocks)",
      "WebSearch"
    ]
  }
}
```

**Project scripts edit:** Based on detected package manager and the user's chosen tier, add allow rules for the applicable scripts. These are **examples** — adapt to the actual project structure, scripts, and invocation patterns.

**Pattern note:** For each script, use two patterns: exact match (`bun run dev`) and with-arguments (`bun run dev *`). This allows passing flags like `--port 3000` without accidentally matching other scripts (e.g., `bun run dev*` would also match `bun run devscript.js`).

Which scripts to include depends on the user's chosen tier:
- **Lint/format/typecheck only** → only lint, format, typecheck commands
- **Tests + lint/format/typecheck** → test commands + lint, format, typecheck
- **All project scripts** → all detected scripts (dev, build, test, lint, format, typecheck, etc.)

Always include deny rules for bypass vectors regardless of tier.

**Example for bun project — "All project scripts" tier:**

```json
{
  "permissions": {
    "allow": [
      "Bash(bun run dev)",
      "Bash(bun run dev *)",
      "Bash(bun run build)",
      "Bash(bun run build *)",
      "Bash(bun run test)",
      "Bash(bun run test *)",
      "Bash(bun run lint)",
      "Bash(bun run lint *)",
      "Bash(bun run format)",
      "Bash(bun run format *)",
      "Bash(bun run typecheck)",
      "Bash(bun run typecheck *)",
      "Bash(bun --version)"
    ],
    "deny": [
      "Bash(eslint *)",
      "Bash(prettier *)",
      "Bash(tsc *)",
      "Bash(jest *)",
      "Bash(vitest *)",
      "Bash(node *)",
      "Bash(npx *)",
      "Bash(npm *)",
      "Bash(pnpm *)",
      "Bash(yarn *)"
    ]
  }
}
```

For "Tests + lint/format/typecheck", the allow list would include only `bun run test`, `bun run lint`, `bun run format`, `bun run typecheck` (and their `*` variants). For "Lint/format/typecheck only", it would include only `bun run lint`, `bun run format`, `bun run typecheck`.

Note: `bun install`, `bun add`, `bun remove` are left on default ask - rarely needed mid-session and users often want control.

**Example for uv/Python project with scripts in scripts/ directory — "All project scripts" tier:**

```json
{
  "permissions": {
    "allow": [
      "Bash(uv run pytest)",
      "Bash(uv run pytest *)",
      "Bash(uv run mypy)",
      "Bash(uv run mypy *)",
      "Bash(uv run ruff)",
      "Bash(uv run ruff *)",
      "Bash(uv run scripts/train.py)",
      "Bash(uv run scripts/train.py *)",
      "Bash(uv run scripts/eval.py)",
      "Bash(uv run scripts/eval.py *)",
      "Bash(uv run scripts/analyze.py)",
      "Bash(uv run scripts/analyze.py *)",
      "Bash(uv --version)"
    ],
    "deny": [
      "Bash(pytest *)",
      "Bash(mypy *)",
      "Bash(ruff *)",
      "Bash(python *)",
      "Bash(python3 *)",
      "Bash(pip *)",
      "Bash(pip3 *)",
      "Bash(poetry *)",
      "Bash(pipenv *)"
    ]
  }
}
```

For "Tests + lint/format/typecheck", allow `uv run pytest`, `uv run mypy`, `uv run ruff` only. For "Lint/format/typecheck only", allow `uv run mypy`, `uv run ruff` only.

Note: `uv sync`, `uv add`, `uv remove` are left on default ask - rarely needed mid-session and users often want control.

**Example for Cargo/Rust project — "All project scripts" tier:**

```json
{
  "permissions": {
    "allow": [
      "Bash(cargo build)",
      "Bash(cargo build *)",
      "Bash(cargo test)",
      "Bash(cargo test *)",
      "Bash(cargo run)",
      "Bash(cargo run *)",
      "Bash(cargo check)",
      "Bash(cargo check *)",
      "Bash(cargo clippy)",
      "Bash(cargo clippy *)",
      "Bash(cargo fmt)",
      "Bash(cargo fmt *)",
      "Bash(cargo --version)"
    ],
    "deny": [
      "Bash(rustc *)"
    ]
  }
}
```

For "Tests + lint/format/typecheck", allow `cargo test`, `cargo check`, `cargo clippy`, `cargo fmt`. For "Lint/format/typecheck only", allow `cargo check`, `cargo clippy`, `cargo fmt`.

Note: `cargo add`, `cargo remove` are left on default ask.

---

## Step 4: MCP Server Permissions (if detected)

For each MCP server found in `.mcp.json`, ask in a **question batch** (one question per server) whether to allow all tools or leave on default (permission request per tool).

Use judgment for recommendations:
- **Needed for autonomous operation** (playwright, remote-kernels) → recommend allow
- **Only needed with user oversight** (database, Airtable, email) → recommend default (permission request per use, auto-denied in autonomous mode)

### Edit Immediately After

For servers the user approved for full access:

```json
{
  "permissions": {
    "allow": [
      "mcp__playwright__*",
      "mcp__remote-kernels__*"
    ]
  }
}
```

Servers left on default ask don't need any configuration.

---

## Step 5: Final Batch - Secrets + Git + Mode + Cleanup

Ask these questions in a single batch, then make a single edit.

### Secrets (only if secret files were detected in Step 1)

> "Block Claude from reading these sensitive files? [list detected files]"

- **Yes (Recommended)** - Deny access to .env, credentials, etc.
- **No** - Allow access (you trust the project boundaries)

### Git

> "Allow Claude to stage and commit without permission requests?"

- **Yes** - Commits can always be reverted, this isn't push permission
- **No** - Permission request for each commit

### Default Mode

> "Start sessions in plan mode by default? (Recommended)"

- **Yes (Recommended)** - Claude researches and plans before making changes. Users often find plan mode useful in situations they didn't anticipate.
- **No** - Standard mode where Claude acts immediately.

### Cleanup (only if issues were found in Step 1 audit)

> "Clean up existing permission issues? [describe issues found: bypass vectors, deprecated syntax, redundant rules]"

- **Yes** - Fix the issues found
- **No** - Leave existing permissions as-is

### Edit for All

**Secret protection (if user confirmed):** — Requires judgment, check what files actually exist.

Check what secret files actually exist - don't use a generic list:

```json
{
  "permissions": {
    "deny": [
      "Read(**/.env)",
      "Read(**/.env.local)",
      "Read(**/.envrc)"
    ]
  }
}
```

Note: `.env.example` and `.env.test` are typically safe to read.

**Git permissions (if user allows commits):** — Copy verbatim.

Note: `git commit` uses `:*` instead of ` *` because commit messages use heredoc syntax, which ` *` fails to match.

```json
{
  "permissions": {
    "allow": [
      "Bash(git add *)",
      "Bash(git commit:*)"
    ]
  }
}
```

**Default mode (if user chose plan mode):** — Copy verbatim.

```json
{
  "permissions": {
    "defaultMode": "plan"
  }
}
```

---

## Step 6: Autopilot Features

Ask about autonomous mode and the deno sandbox in separate question batches — each question must be its own AskUserQuestion call so the user reads the explanation before answering. Execute both together after.

### Q1: Autonomous Mode

**Explanation to give the user right before the question:**

The autopilot plugin includes an autonomous mode that changes how Claude handles permission requests when you're in `acceptEdits` mode:

- **Without autonomous mode:** Claude blocks on every unpermitted command, waiting for you to approve or deny each one.
- **With autonomous mode:** Unpermitted commands are automatically denied. Claude will try alternatives or propose adding a permission instead of blocking.

This means you can leave Claude running unattended in `acceptEdits` mode. If you need to approve a one-off command that isn't in your allow list, switch out of `acceptEdits` mode first (toggle with Shift+Tab).

> "Enable autonomous mode?"

- **Yes** - Enable autonomous mode
- **No** - Keep standard behavior (block on unpermitted commands)

### Q2: Deno Sandbox

**Explanation to give the user right before the question:**

The deno sandbox gives Claude a secure way to run JavaScript/TypeScript code. By default, scripts can only read files in the project directory — network, writes, env, and subprocess execution are all blocked.

When Claude needs additional capabilities, it will run `deno-sandbox-grant` to request a scoped permission. Each grant requires your approval and looks like this:

```
deno-sandbox-grant --allow-net=api.example.com
deno-sandbox-grant --allow-write=./output
deno-sandbox-grant --allow-run=python3
```

Review grants before approving — especially `--allow-run`, which gives unsandboxed subprocess execution. Grants persist for the session only.

> "Enable deno sandbox? (Recommended)"

- **Yes (Recommended)** - Enable the deno sandbox
- **No** - Disable the deno sandbox

### Action

Create the state directory and write the state file:

```bash
mkdir -p .claude/autopilot
```

Write `.claude/autopilot/state.json` with both keys:
```json
{
  "autonomous_mode": true|false,
  "deno_sandbox": true|false
}
```

Ensure `.claude/autopilot/` is gitignored — add to `.gitignore` if not already covered.
