---
description: Set up session sharing and get tooling recommendations — plugins and dev environment setup. This command should also be used when the user asks about setting up their project, which plugins to install, or when the working directory appears empty or newly created.
allowed-tools: Bash(hive consent status), Bash(${CLAUDE_PLUGIN_ROOT}/scripts/align-status.sh)
---

# Align

## Status

!`${CLAUDE_PLUGIN_ROOT}/scripts/align-status.sh 2>/dev/null || echo "Status unavailable"`

## Consent Status

```
!`hive consent status 2>&1 || true`
```

## Instructions

Every `.claude/hive` file named below lives in the **State dir** printed in Status (the main worktree's, which a cwd-relative path in a git worktree would miss). "The four settings files" means `~/.claude/settings.json`, `~/.claude/settings.local.json`, `.claude/settings.json` and `.claude/settings.local.json`. Note that the plugin loader never reads `~/.claude/settings.local.json`: a plugin or marketplace declared only there is not working.

### Data Sharing (check first, before recommendations)

Read the consent status output above. Handle errors first, then check if data sharing needs attention.

**If the output looks like a shell error** (e.g. `command not found`, `No such file or directory`) rather than one of the statuses below: the `hive` binary is missing. Direct the user to run `curl -fsSL https://alignment-hive.com/install.sh | bash`, then restart Claude Code.

**If "Not authenticated"**: Not a problem — authentication is optional and only needed to opt into session data sharing. Mention in passing that running `curl -fsSL https://alignment-hive.com/install.sh | bash` enables it later if the user wants. Move on to recommendations.

**If "Failed to fetch consent status"**: Note briefly that sharing status couldn't be checked (offline or API issue). Move on to recommendations.

**If "Data sharing preferences: not set"**: Direct user to `https://alignment-hive.com/consent`. Move on to recommendations.

**If "Session sharing: disabled"**: Note briefly that sharing is declined, changeable at `https://alignment-hive.com/consent`. Move on.

**If "Session sharing: enabled"**: load the `manage-data-sharing` skill; it reads the same status output, runs whichever of its steps apply, and skips the rest silently. If it finds nothing to do, note the sharing status in one line and move on.

### Superseded plugins

Check the four settings files for these keys (a key declared only in `~/.claude/settings.local.json` is stale rather than working, and still worth removing):

- `hive-mind@alignment-hive`: hive handles session sharing now; offer to remove it.
- `autopilot@alignment-hive`: Claude Code's built-in auto mode supersedes it (except for the deno sandbox) — it uses a model-based classifier instead of a static allow-list. Ask whether to remove it; users who rely on the deno sandbox may want to keep it.

Record a keep in `.claude/hive/align-rejected.md` (e.g. "Kept autopilot") so it is not asked again. If neither key is present, skip silently.

### Bundled-binary Migration (remote-kernels, model-router)

For each plugin listed under **Platform entries available for** in Status, offer the switch wherever the plain `<plugin>@alignment-hive` key is enabled in any of the four settings files (rationale and rules under Platform-specific entries below). If the plain key is in `.claude/settings.json`, say so prominently: that file is usually checked in, so removing it takes the plugin away from collaborators, who each need the platform entry for their own machine — the user can decline and keep the shared key. On yes, run the install-then-clean procedure with `<plugin><suffix>@alignment-hive`.

**Repair:** a platform-specific key in `~/.claude/settings.local.json` (stale even if a working copy exists elsewhere), or one enabled anywhere but failing with a "not cached" error, is broken. Offer the same install-then-clean procedure; no plain key needs to exist.

Record declines in `.claude/hive/align-rejected.md`. Skip silently if nothing applies.

**Install-then-clean procedure** (also used by the Plugins checklist below — editing `enabledPlugins` by hand does not install anything):

1. Pick the scope: `--scope user` if any occurrence of the key being replaced is in `~/.claude/settings.json` or `~/.claude/settings.local.json`, else `--scope local`.
2. Run `claude plugin install <entry> --scope <scope>`. Verify: exit 0 and the entry appears in `claude plugin list`. On failure, remove nothing — report and stop.
3. Only then remove every other occurrence of that plugin's keys — plain and platform-specific, across the four settings files — so exactly one enabled entry remains: the one the install just wrote.

### Recommendations

Walk through the checklist below. Skip items already implemented and anything listed under Previously Rejected — that list covers the sections above too (kept superseded plugins, declined migrations). If Status was unavailable, read `.claude/hive/align-rejected.md` in the main worktree yourself. Offer everything else; implement if accepted, and note the reason in the rejected file if declined.

## Checklist

### Plugins (based on project type)

Check the four settings files to discover already-installed plugins. A plugin enabled in `~/.claude/settings.json` or either project-level file counts as installed — do not recommend it again. A plugin enabled *only* in `~/.claude/settings.local.json` does not count: reinstall it via `claude plugin install --scope user` and remove the stale key (verify first, per the install-then-clean procedure).

**Install with the claude CLI, never by editing `enabledPlugins` by hand** — a settings entry alone installs nothing (archive-sourced plugins in particular never load without a real install). Default to project-level: `--scope project` (shared via `.claude/settings.json`) or `--scope local` (machine-only, `.claude/settings.local.json`), unless the user explicitly asks for a global install (`--scope user`). Infer from existing project-level settings whether the user prefers local-only or shared — if unclear, ask once and use that for all installations.

Propose all relevant plugins in **batched AskUserQuestion calls**. Each plugin gets three options: **Yes** (install), **No** (skip), **Tell me more**. After the user responds, process "Tell me more" answers one plugin at a time in sequence: (1) fetch the full, untruncated content of that plugin's README (use curl — WebFetch summarizes), (2) present the README to the user — verbatim when it is short, and as a faithful summary that keeps every setup step when it is long, (3) ask a fresh AskUserQuestion with only **Yes** / **No**. Do not advance to the next "Tell me more" plugin until the current one has a Yes/No answer.

#### Plugin list

- **MATS**: `mats@alignment-hive` — For MATS fellows (handbook, lit review, best practices)
- **Python + GPU compute**: `remote-kernels@alignment-hive` — Cloud GPU instances with Jupyter kernels (RunPod, vast.ai, Kubernetes)
- **Codebase exploration**: `precis` — Structural codebase summaries for fast agent context
- **Cross-model review**: `codex@codex-plugin-cc` — Delegate tasks and adversarial code review to Codex from Claude Code
- **Cross-model subagents (experimental)**: `model-router@alignment-hive` — GPT models as native Claude Code subagents via a local gateway; experimental alternative to the codex plugin
- **Reply TL;DRs**: `tldr@alignment-hive` — One-sentence TL;DR after every long reply; /focus then collapses messages to their TL;DRs — **Always recommend**
- **Hidden-payload stripping**: `agent-sanitizer@agent-sanitizer` — Catches prompt injections hidden inside text that looks harmless: invisible characters, hidden HTML and look-alike glyphs are stripped before Claude reads them — **Always recommend**

#### After installing agent-sanitizer

Ask one follow-up, Yes / No:

> **Secret redaction** (off by default): redacts credentials from tool output on your machine before Claude sees them, so they can't end up in a commit, another tool call, or the transcript. Needs python3 or uv on PATH; occasionally over-redacts credential-shaped text.

On Yes, set `AGENT_SANITIZER_SECRETS_ENABLED` to `"1"` in the `env` block of the settings file the install scope wrote to (`.claude/settings.json`, `.claude/settings.local.json`, or `~/.claude/settings.json`). On No, record it in `.claude/hive/align-rejected.md`.

#### Platform-specific entries for remote-kernels and model-router

These two plugins ship a compiled binary; the plain key downloads it separately, so a plugin update briefly runs against the previous binary, while the marketplace's per-platform entry bundles it. For every plugin listed under **Platform entries available for** in Status, install `<plugin><suffix>@alignment-hive` with the suffix reported there (e.g. `remote-kernels-aarch64-apple-darwin@alignment-hive`) instead of the plain key; a plugin not listed there has no entry for this platform, so use the plain key. If Status says the catalog could not be consulted, add the marketplace and re-run `align-status.sh` before choosing.

- **Never `--scope project`.** The key names a platform, so it must never land in a checked-in file — a teammate on another OS would get an archive that refuses to run. Install with `--scope local`, or `--scope user` for a global install; this overrides the shared/local preference inferred above.
- **Never both.** A platform-specific entry and its plain counterpart define the same commands, skills and hooks. Exactly one key per plugin across the four settings files — use the install-then-clean procedure above.

#### Installing and "Tell me more"

alignment-hive plugins: README at `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/<plugin>/README.md`; install with `claude plugin install <plugin>@alignment-hive --scope <scope>`. If that fails because the alignment-hive marketplace is missing, run `claude plugin marketplace add Crazytieguy/alignment-hive --scope <scope>` first, and add `"autoUpdate": true` to the declaration it writes.

Other plugins — add the marketplace (harmless if already known), then install:

```
claude plugin marketplace add <github-repo> --scope <scope>
claude plugin install <coordinate> --scope <scope>
```

| Plugin | `<coordinate>` | `<github-repo>` | README |
|---|---|---|---|
| precis | `precis@precis` | `Crazytieguy/precis` | `https://raw.githubusercontent.com/Crazytieguy/precis/main/README.md` |
| codex | `codex@codex-plugin-cc` | `Crazytieguy/codex-plugin-cc` | `https://raw.githubusercontent.com/Crazytieguy/codex-plugin-cc/main/README.md` |
| agent-sanitizer | `agent-sanitizer@agent-sanitizer` | `AlexanderMattTurner/agent-sanitizer` | `https://raw.githubusercontent.com/AlexanderMattTurner/agent-sanitizer/main/README.md` |

The marketplace name is the part of the coordinate after `@`. After `marketplace add`, add `"autoUpdate": true` to the `extraKnownMarketplaces.<marketplace>` entry it wrote to the scoped settings file — there is no CLI flag for auto-update. Claude Code (v2.1.140+) propagates the field to `~/.claude/plugins/known_marketplaces.json` on next session start.

#### Marketplace auto-update — retroactive sweep

For each marketplace in the table above that has a plugin enabled in some settings file, no `autoUpdate: true` in any settings file's `extraKnownMarketplaces.<marketplace>` or in `~/.claude/plugins/known_marketplaces.json`, and no decline recorded in the rejected file: ask once (Yes / No, batched like the plugin questions). Skip the section if nothing qualifies.

On Yes, add `"autoUpdate": true` to the marketplace's `extraKnownMarketplaces` entry (keep its other fields) in the most local settings file that already declares it (`.claude/settings.local.json` > `.claude/settings.json` > `~/.claude/settings.json`); if none declares it, add a full entry (source from the table) to the file that enables the plugin, same preference order. Never promote a third-party marketplace into a more-shared file than the user chose for the plugin itself. On No, record it in the rejected file.

A marketplace declared in `~/.claude/settings.local.json` (an older version of this flow wrote there) is stale: carry an `"autoUpdate": true` from it into the supported declaration and delete it, or if it is declared nowhere else, `claude plugin marketplace add <github-repo> --scope user`, verify with `claude plugin marketplace list`, set `autoUpdate`, then delete it. On verification failure delete nothing.

#### alignment-hive auto-update verification

Read the `alignment-hive` entry in `~/.claude/plugins/known_marketplaces.json`. If `autoUpdate` is not `true`, mention once that the install script (`curl -fsSL https://alignment-hive.com/install.sh | bash`) is supposed to set this and recommend re-running it. Do not auto-fix.

### Transcript Retention

Claude Code deletes local session transcripts after `cleanupPeriodDays` (default 30). Session retrieval searches these transcripts, so the default silently caps how far back it can reach.

Check `cleanupPeriodDays` in the four settings files. If it's unset or below 365 everywhere, recommend setting it to `99999` in `~/.claude/settings.json` (transcript cleanup is per-machine, not per-project). Explain the tradeoff briefly: transcripts are plain text on disk and accumulate over time, but keeping them makes session retrieval useful long-term. If declined, record it in the rejected file.

### Tooling (varies by project)

Recommend modern, well-maintained tooling for the project (dependency management, build, lint, typecheck, format, anything else that would improve the workflow). Ask before installing anything.

### Reload + Setup

After installs, tell the user to restart Claude (`/exit` then `claude`): plugins with a setup skill make it available then and nudge about it from their SessionStart hook, and any marketplace with auto-update enabled refreshes at session start.

## Completion

Once all recommendations have been either implemented or explicitly rejected:

1. Write the plugin version (shown in Status above) to `.claude/hive/align-version` in the State dir.

2. Write/update `.claude/hive/align-rejected.md` there with natural language descriptions of rejected recommendations. Format:
   ```markdown
   # Rejected Recommendations

   - User prefers pip over uv for Python dependency management
   - No GPU compute needed
   - Declined GitHub Action
   ```

3. If nothing was rejected, don't create the file.
