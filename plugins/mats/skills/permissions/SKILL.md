---
name: permissions
description: This skill should be used when the user asks to "set up permissions", "configure permissions", "fix permission prompts", "allow commands", "update permissions", "reduce prompts", "stop asking for permission", or mentions Claude Code permission configuration. Also use when permission-related friction is causing workflow issues.
---

# Claude Code Permissions Configuration

Generate permission configurations that enable autonomous Claude operation while maintaining security.

## Purpose

Proper permissions let Claude work without constant permission prompts while maintaining security. This is essential for:
- Running Claude asynchronously (without `--dangerously-skip-permissions`)
- Reducing friction in interactive sessions
- Steering toward correct patterns (via deny rules)
- Preventing bypass vectors (via ask rules that require user attention)

## Key Principle: Ask vs Deny

**Deny** is for commands that are **never useful in this project** - steering toward correct alternatives.

**Ask** is safer than deny for **sometimes dangerous** commands. With ask, the session stops until the user provides attention. With deny, a potentially compromised model might try variations autonomously. Use ask for commands that are sometimes legitimate but could be misused.

## Workflow Overview

1. Detect project type and audit existing permissions (automatic)
2. Ask context questions (batch of 5) + edit universally safe commands
3. Ask script execution preference + edit
4. Ask web access preference + edit
5. Ask MCP server permissions + edit (if applicable)
6. Confirm secrets/git/mode/cleanup + edit

---

## Step 1: Project Detection and Audit

Detect the project type and audit existing permissions automatically before asking questions.

### What to Detect

**Package manager:** Check for lock files (bun.lockb, pnpm-lock.yaml, yarn.lock, package-lock.json, uv.lock, Cargo.lock). If ambiguous, ask the user.

**Scripts:** Look for project scripts that might be needed mid-session (linting, testing, building, data processing). Extract from `package.json` scripts, `pyproject.toml` scripts, or similar config. Also look for standalone scripts in the project (e.g., `scripts/train.py`, `tools/analyze.sh`, `bin/setup`). These will be allowed individually as "project scripts". For monorepos, scripts may only exist in specific workspaces - identify the correct invocation pattern (e.g., `bun run --filter <workspace> <script>`).

**MCP servers:** Check `.mcp.json` for configured servers.

**Secret files:** Look for `.env`, `.env.local`, `.envrc`, credentials files, API keys. Note which ones exist for the secrets confirmation later.

**Bash scripts folder:** Look for `scripts/`, `bin/`, or similar directories containing shell scripts. Fall back to `scripts/` if none found. This is used for the "Full execution" tier.

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

### What to Copy Verbatim vs Adapt

- **Verbatim:** Universally safe commands, git permissions, default mode
- **Mostly standard:** Web access patterns - structure is standard, just pick the right option
- **Adapt to project:** Project-specific commands - must match actual scripts and invocation patterns; script execution tier - adapt project name, paths, and package manager
- **Requires judgment:** Secret protection - check what files actually exist; MCP servers - based on usage patterns

---

## Step 2: Context Questions + Universally Safe Commands

Use AskUserQuestion to ask all five questions in a single batch. Answers drive recommendations for later questions.

### Q1: Sensitive Information

> "Is there sensitive information on this machine or that Claude might work with?"

- **No** - Nothing confidential
- **Yes** - There's data where leakage would be a problem

### Q2: Valuable Data

> "Could Claude cause damage that's hard to undo? (local files, databases, cloud resources)"

- **No** - Everything is backed up or easily recovered
- **Yes** - There's valuable data or systems that could be damaged

### Q3: Autonomy Importance

> "How important is it that Claude can work autonomously? (less oversight = some security tradeoff)"

- **Not very** - I can respond to permission prompts when needed
- **Important** - I need Claude to work independently or run multiple sessions

### Q4: Settings Strategy

> "Where should permissions be stored?"

- **Split (Recommended)** - Shared file for universally safe commands, personal file for preferences. Collaborators get safe defaults.
- **Shared only** - All permissions in settings.json. Works across collaborators and environments.
- **Personal only** - All permissions in settings.local.json. For projects where collaborators have existing preferences.

### Q5: Universally Safe Commands

> "Add universally safe commands? (read-only inspection, safe utilities, git status/diff/log)"

- **Yes (Recommended)** - Standard set of read-only commands that are safe in any project
- **No** - Start from scratch, I'll configure manually

### Edit Immediately After (if user confirmed universally safe commands)

**Determine target file based on settings strategy:**
- Split or Shared only: → settings.json
- Personal only: → settings.local.json

**Edit 1: Universally Safe Commands**

Note the wildcard patterns - always use `cmd` + `cmd *` (never `cmd*`). Exception: use `cmd:*` for commands that receive heredoc arguments (e.g., `git commit:*`), since ` *` fails to match heredoc syntax.

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
      "Bash(awk *)",
      "Bash(sed -n *)",
      "Bash(jq *)",
      "Bash(yq *)",
      "Bash(sort)",
      "Bash(sort *)",
      "Bash(uniq)",
      "Bash(uniq *)",
      "Bash(cut *)",
      "Bash(tr *)",
      "Bash(printf *)",
      "Bash(tee *)",
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
      "Bash(printenv)",
      "Bash(printenv *)",
      "Bash(id)",
      "Bash(hostname)",
      "Bash(uptime)",
      "Bash(sleep *)",
      "Bash(export *)",
      "Bash(test *)",
      "Bash(man *)",
      "Bash(less *)",
      "Bash(readlink *)",
      "Bash(curl *://localhost*)",
      "Bash(curl *://127.0.0.1*)",
      "Bash(curl *://0.0.0.0*)",
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
      "Bash(xargs sed -n *)",
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
      "Bash(xargs -I{} cut *)",
      "Bash(xargs -I{} sed -n *)"
    ],
    "deny": [
      "Bash(for *)",
      "Bash(while *)",
      "Bash(until *)",
      "Bash(timeout *)",
      "Bash(env *)",
      "Bash(bash -c *)",
      "Bash(sh -c *)",
      "Bash(zsh -c *)",
      "Bash(find * -exec *)",
      "Bash(find * -execdir *)",
      "Bash(awk *system\\(*)",
      "Bash(xargs awk *system\\(*)",
      "Bash(xargs -I{} awk *system\\(*)",
      "Bash(xargs sh *)",
      "Bash(xargs -I{} sh *)",
      "Bash(xargs bash *)",
      "Bash(xargs -I{} bash *)",
      "Bash(cat)",
      "Bash(git -C *)"
    ]
  }
}
```

**Edit 2: CLAUDE.md Bash Operations Guidance** (to CLAUDE.md for split/shared, CLAUDE.local.md for personal)

Add basic bash operations guidance that applies regardless of script execution tier:

```markdown
## Bash Operations

Complex bash syntax is hard for Claude Code to permission correctly. Keep commands simple.

Simple operations are fine: `|`, `||`, `&&`, `>` redirects.

For bulk operations on multiple files, use xargs:
- Plain: `ls *.md | xargs wc -l`
- With placeholder: `ls *.md | xargs -I{} head -1 {}`

Avoid string interpolation (`$()`, backticks, `${}`), heredocs, loops, and advanced xargs flags (`-P`, `-L`, `-n`) - these require scripts or simpler alternatives.

**Patterns:**
- File creation: Write tool, not `cat << 'EOF' > file`
- Env vars: `export VAR=val && command`, not `VAR=val command` or `env VAR=val command`
- Bulk operations: `ls *.md | xargs wc -l`, not `for f in *.md; do cmd "$f"; done`
- Parallel/batched xargs: use scripts, not `xargs -P4` or `xargs -L1`
- Per-item shell logic: use scripts, not `xargs sh -c '...'`
- Git: `git <command>`, not `git -C <path> <command>` (breaks permissions)

If a command that should be allowed is denied, or if project structure changes significantly, ask about running `/mats:permissions` to update settings.
```

---

## Step 3: Script Execution Question + Edit

### Recommendation Mapping

Use the context answers to determine the recommended option:

| Hard to undo? | Autonomy important? | Recommend |
|----------------|---------------------|-----------|
| Yes | Yes | Temp folder scripts |
| Yes | No | Project scripts only |
| No | Yes | Full execution |
| No | No | Temp folder scripts |

### Question

> "What level of script execution do you want to allow?"

**Options (mark recommended based on mapping above):**

- **No scripts** - Only linting/formatting. Testing requires permission prompts. *Test files could be edited to run unintended code.*

- **Project scripts only** - Only scripts defined in package.json/pyproject.toml, plus any detected in scripts/. *Permission prompts for one-off scripts.*

- **Temp folder scripts** - Scripts in `/tmp/claude-execution-allowed/<project>/` allowed. *One permission prompt per session when first writing there. Enables arbitrary code execution.*

- **Full execution** - `uv run *`, `bun run *`, `bash scripts/*` fully allowed. *Enables arbitrary code execution via the package manager.*

### Edit Immediately After

Based on the user's choice, edit the appropriate settings file(s) and CLAUDE.md/CLAUDE.local.md.

**Determine target files based on settings strategy:**
- Split: project-specific commands → settings.json (shared infrastructure), script tier permissions → settings.local.json (personal preference), guidance → CLAUDE.local.md
- Shared only: everything → settings.json, guidance → CLAUDE.md
- Personal only: everything → settings.local.json, guidance → CLAUDE.local.md

**Edit 1: Project-Specific Commands** (to settings.json for split/shared, settings.local.json for personal)

Based on detected package manager, add commands for enumerated scripts. These are **examples** - adapt to the actual project structure, scripts, and invocation patterns.

**Pattern note:** For each script, use two patterns: exact match (`bun run dev`) and with-arguments (`bun run dev *`). This allows passing flags like `--port 3000` without accidentally matching other scripts (e.g., `bun run dev*` would also match `bun run devscript.js`).

**Example for bun project with scripts: dev, build, test, lint, format, typecheck:**

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

Note: `bun install`, `bun add`, `bun remove` are left on default ask - rarely needed mid-session and users often want control.

**Example for uv/Python project with scripts in scripts/ directory:**

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

Note: `uv sync`, `uv add`, `uv remove` are left on default ask - rarely needed mid-session and users often want control.

**Example for Cargo/Rust project:**

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

Note: `cargo add`, `cargo remove` are left on default ask.

**Edit 3: Script Execution Permissions** (to settings.local.json for split strategy, otherwise per strategy)

**For "Full execution" tier:**

Adapt to project's package manager and detected scripts folder:

```json
{
  "permissions": {
    "allow": [
      "Bash(uv run *)",
      "Bash(bun run *)",
      "Bash(bash scripts/*)",
      "Bash(xargs uv run *)",
      "Bash(xargs bun run *)",
      "Bash(xargs bash scripts/*)",
      "Bash(xargs -I{} uv run *)",
      "Bash(xargs -I{} bun run *)",
      "Bash(xargs -I{} bash scripts/*)"
    ]
  }
}
```

**For "Temp folder scripts" tier:**

Use the project name in the path:

```json
{
  "permissions": {
    "allow": [
      "Bash(uv run /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(bun run /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(bash /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(xargs uv run /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(xargs bun run /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(xargs bash /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(xargs -I{} uv run /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(xargs -I{} bun run /tmp/claude-execution-allowed/<project-name>/*)",
      "Bash(xargs -I{} bash /tmp/claude-execution-allowed/<project-name>/*)"
    ]
  }
}
```

**For "Project scripts only" and "No scripts" tiers:** No additional script permissions.

**Edit 4: CLAUDE.md or CLAUDE.local.md guidance**

Add to whichever file corresponds to where script permissions were stored.

**For "Full execution" tier:**

```markdown
## Running Commands

Run scripts via `[package-manager] run <script>`. Available scripts: [list enumerated scripts].

Non-bash scripts run with `[package-manager] run [scripts/]<script-name>`. Bash scripts run with `bash [scripts/]<script-name>`.

For string interpolation, heredocs, loops, or advanced xargs flags, write a script in `[scripts/]` instead.
```

**For "Temp folder scripts" tier:**

```markdown
## Running Commands

Run scripts via `[package-manager] run <script>`. Available scripts: [list enumerated scripts].

**Ad-hoc scripts:** Only `/tmp/claude-execution-allowed/[project-name]/` is approved for ad-hoc scripts. Non-bash scripts run with `[package-manager] run /tmp/claude-execution-allowed/[project-name]/<script-name>`. Bash scripts run with `bash /tmp/claude-execution-allowed/[project-name]/<script-name>`.

When you create a new reusable script, offer to add a permission for it. Example: "I created scripts/analyze.py. Want me to add `Bash(uv run scripts/analyze.py *)` to your permissions?"

For string interpolation, heredocs, loops, or advanced xargs flags, write a script in `/tmp/claude-execution-allowed/[project-name]/` instead.
```

**For "Project scripts only" tier:**

```markdown
## Running Commands

Run scripts via `[package-manager] run <script>`. Available scripts: [list enumerated scripts].

When you create a new reusable script, offer to add a permission for it. Example: "I created scripts/analyze.py. Want me to add `Bash(uv run scripts/analyze.py *)` to your permissions?"

For complex bash operations, break into simpler sequential commands or ask the user for help.
```

**For "No scripts" tier:**

```markdown
## Running Commands

Run lint/typecheck via `[package-manager] run <script>`. Available scripts: [list the lint/format/typecheck scripts that were configured].

For anything requiring code execution beyond linting, ask the user for help or request a permission change.

For complex bash operations, break into simpler sequential commands or ask the user for help.
```

---

## Step 4: Web Access Question + Edit

### Recommendation Mapping

| Sensitive data? | Recommend |
|-----------------|-----------|
| Yes | WebFetch + WebSearch |
| No | Full GET with curl |

### Question

> "What level of web access do you want?"

**Options (mark recommended based on mapping above):**

- **Specific domains only** - Only documentation sites relevant to the project. *Claude can't fetch arbitrary URLs.*

- **WebFetch + WebSearch** - All domains via built-in tools. *Built-in prompt injection protections.*

- **Full GET with curl** - Adds curl for GET requests. *No built-in protections - some prompt injection risk.*

### Edit Immediately After

**For "Full GET with curl":**

```json
{
  "permissions": {
    "allow": [
      "WebFetch",
      "WebSearch",
      "Bash(curl)",
      "Bash(curl *)"
    ],
    "ask": [
      "Bash(curl -X POST *)",
      "Bash(curl -X PUT *)",
      "Bash(curl -X DELETE *)",
      "Bash(curl -X PATCH *)",
      "Bash(curl * -X POST)",
      "Bash(curl * -X PUT)",
      "Bash(curl * -X DELETE)",
      "Bash(curl * -X PATCH)",
      "Bash(curl * -X POST *)",
      "Bash(curl * -X PUT *)",
      "Bash(curl * -X DELETE *)",
      "Bash(curl * -X PATCH *)",
      "Bash(curl -XPOST *)",
      "Bash(curl -XPUT *)",
      "Bash(curl -XDELETE *)",
      "Bash(curl -XPATCH *)",
      "Bash(curl * -XPOST)",
      "Bash(curl * -XPUT)",
      "Bash(curl * -XDELETE)",
      "Bash(curl * -XPATCH)",
      "Bash(curl * -XPOST *)",
      "Bash(curl * -XPUT *)",
      "Bash(curl * -XDELETE *)",
      "Bash(curl * -XPATCH *)",
      "Bash(curl --request POST *)",
      "Bash(curl --request PUT *)",
      "Bash(curl --request DELETE *)",
      "Bash(curl --request PATCH *)",
      "Bash(curl * --request POST)",
      "Bash(curl * --request PUT)",
      "Bash(curl * --request DELETE)",
      "Bash(curl * --request PATCH)",
      "Bash(curl * --request POST *)",
      "Bash(curl * --request PUT *)",
      "Bash(curl * --request DELETE *)",
      "Bash(curl * --request PATCH *)",
      "Bash(curl -d*)",
      "Bash(curl * -d*)",
      "Bash(curl --data*)",
      "Bash(curl * --data*)",
      "Bash(curl -F*)",
      "Bash(curl * -F*)",
      "Bash(curl --form*)",
      "Bash(curl * --form*)",
      "Bash(curl -T *)",
      "Bash(curl * -T *)",
      "Bash(curl --upload-file *)",
      "Bash(curl * --upload-file *)",
      "Bash(curl -H *)",
      "Bash(curl * -H *)",
      "Bash(curl --header *)",
      "Bash(curl * --header *)",
      "Bash(curl -b *)",
      "Bash(curl * -b *)",
      "Bash(curl --cookie *)",
      "Bash(curl * --cookie *)",
      "Bash(curl -u *)",
      "Bash(curl * -u *)",
      "Bash(curl --user *)",
      "Bash(curl * --user *)"
    ]
  }
}
```

Also add to CLAUDE.md/CLAUDE.local.md (same file as other guidance):

```markdown
Use `curl` when you need to see the full response content (WebFetch summarizes).
```

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

---

## Step 5: MCP Server Permissions (if detected)

For each MCP server found in `.mcp.json`, ask in a **question batch** (one question per server) whether to allow all tools or leave on default (permission prompt per tool).

Use judgment for recommendations:
- **Many tools used mid-session** (playwright, puppeteer for testing) → recommend allow
- **Accesses private data or modifies external state** (database, Airtable, email) → recommend default ask

### Edit Immediately After

For servers the user approved for full access:

```json
{
  "permissions": {
    "allow": [
      "mcp__playwright__*",
      "mcp__puppeteer__*"
    ]
  }
}
```

Servers left on default ask don't need any configuration.

---

## Step 6: Final Batch - Secrets + Git + Mode + Cleanup

Ask these questions in a single batch, then make a single edit.

### Secrets (only if secret files were detected in Step 1)

> "Block Claude from reading these sensitive files? [list detected files]"

- **Yes (Recommended)** - Deny access to .env, credentials, etc.
- **No** - Allow access (you trust the project boundaries)

### Git

> "Allow Claude to stage and commit without permission prompts?"

- **Yes** - Commits can always be reverted, this isn't push permission
- **No** - Permission prompt for each commit

### Default Mode

> "Start sessions in plan mode by default? (Recommended)"

- **Yes (Recommended)** - Claude researches and plans before making changes. Users often find plan mode useful in situations they didn't anticipate.
- **No** - Standard mode where Claude acts immediately.

### Cleanup (only if issues were found in Step 1 audit)

> "Clean up existing permission issues? [describe issues found: bypass vectors, deprecated syntax, redundant rules]"

- **Yes** - Fix the issues found
- **No** - Leave existing permissions as-is

### Edit for All

**Git permissions (if user allows commits):**

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

**Secret protection (if user confirmed):**

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

For projects with credentials files (if present):

```json
{
  "permissions": {
    "deny": [
      "Read(**/.aws/credentials)",
      "Read(**/.ssh/*)",
      "Read(**/*.pem)",
      "Read(**/*_rsa)",
      "Read(**/*_ed25519)"
    ]
  }
}
```

**Default mode (if user chose plan mode):**

```json
{
  "permissions": {
    "defaultMode": "plan"
  }
}
```

