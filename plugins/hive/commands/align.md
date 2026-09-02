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

## Previously Rejected

@.claude/hive/align-rejected.md

## Instructions

### Data Sharing (check first, before recommendations)

Read the consent status output above. Handle errors first, then check if data sharing needs attention.

**If the output looks like a shell error** (e.g. `command not found`, `No such file or directory`) rather than one of the statuses below: the `hive` binary is missing. Direct the user to run `curl -fsSL https://alignment-hive.com/install.sh | bash`, then restart Claude Code.

**If "Not authenticated"**: Not a problem — authentication is optional and only needed to opt into session data sharing. Mention in passing that running `curl -fsSL https://alignment-hive.com/install.sh | bash` enables it later if the user wants. Move on to recommendations.

**If "Failed to fetch consent status"**: Note briefly that sharing status couldn't be checked (offline or API issue). Move on to recommendations.

**If "Data sharing preferences: not set"**: Direct user to `https://alignment-hive.com/consent`. Move on to recommendations.

**If "Session sharing: disabled"**: Note briefly that sharing is declined, changeable at `https://alignment-hive.com/consent`. Move on.

**If "Session sharing: enabled"**: Check whether data sharing state is settled or needs attention.

State is **settled** (note status in one line, move on) when:
- Current project is "enabled", AND either no `Repo visibility` line (not a GitHub repo), or repo is public, or repo is linked, or `.claude/hive/repo-linking-declined` exists
- OR `.claude/hive/sharing-disabled` exists (user previously declined for this project — mention they can re-enable with `/hive:align` or by asking to enable sharing)

State is **unsettled** (load the `manage-data-sharing` skill) when:
- Current project is "not enabled" and `.claude/hive/sharing-disabled` does not exist
- OR project is enabled with `Repo visibility: private` and `Repo link: not-linked` and `.claude/hive/repo-linking-declined` does not exist

### hive-mind Migration

If `hive-mind@alignment-hive` is in `.claude/settings.json` or `.claude/settings.local.json`, offer to remove it — hive handles session sharing now. If not found, skip silently — do not mention it.

### autopilot Deprecation

If `autopilot@alignment-hive` is in `.claude/settings.json` or `.claude/settings.local.json`, note that Claude Code's built-in auto mode supersedes autopilot (except for the deno sandbox) — it uses a model-based classifier instead of a static allow-list.

Ask whether to remove `autopilot@alignment-hive`. Pro users (no auto mode access yet) or users who rely on the deno sandbox may want to keep it.

If the user chooses to keep it, record that in `.claude/hive/align-rejected.md` (e.g. "Kept autopilot") so we don't re-prompt on future runs.

If not installed, skip silently — do not mention it.

### Bundled-binary Migration (remote-kernels, model-router)

The plain `remote-kernels@alignment-hive` and `model-router@alignment-hive` keys download their binary separately from the plugin, so a plugin update briefly runs against the previous binary. The platform-specific entries described in the Plugins checklist below close that gap.

For each of those two plugins listed under **Platform entries available for** in Status above, find every settings file that enables the plain key and offer the switch. If the plain key is in `.claude/settings.json`, say so prominently in the ask: that file is usually checked in, so removing it takes the plugin away from collaborators, who will each need to install the platform entry for their own machine — the user can decline and keep the shared key.

On yes, run the install-then-clean procedure below with `<plugin><suffix>@alignment-hive`.

Record declines in `.claude/hive/align-rejected.md`. If neither plain key is enabled, skip silently — do not mention it.

**Repair:** a platform-specific key enabled in `~/.claude/settings.local.json` (an older version of this migration wrote it there — the plugin loader never reads that file), or enabled anywhere but failing with a "not cached" error, is broken. Offer to fix it with the same install-then-clean procedure; no plain key needs to exist. Idempotent — a healthy platform install needs nothing.

**Install-then-clean procedure** (also used by the Plugins checklist below — editing `enabledPlugins` by hand does not install anything):

1. Pick the scope: `--scope user` if any occurrence of the key being replaced is in `~/.claude/settings.json` or `~/.claude/settings.local.json`, else `--scope local`. Never `--scope project` for a platform entry — a checked-in platform key breaks teammates on other platforms.
2. Run `claude plugin install <entry> --scope <scope>`. Verify: exit 0 and the entry appears in `claude plugin list`. On failure, remove nothing — report and stop.
3. Only then remove every other occurrence of that plugin's keys — plain and platform-specific, across all settings files, `~/.claude/settings.local.json` included — so exactly one enabled entry remains: the one the install just wrote.

### First-Time Setup (recommendations)

Walk through the checklist below. For each item, check if it's already implemented. If not, offer it to the user. Implement if accepted, note the reason in the rejected file if declined.

### Follow-Up Runs

1. Read the rejected items from the file above — respect previous decisions
2. Check what's currently implemented
3. Only show new or missing recommendations (skip rejected ones)
4. If plugin version changed, mention what's new

## Checklist

### Plugins (based on project type)

Check both global (`~/.claude/settings.json`, `~/.claude/settings.local.json`) and project-level (`.claude/settings.json`, `.claude/settings.local.json`) settings to discover already-installed plugins. A plugin enabled in `~/.claude/settings.json` or either project-level file counts as installed — do not recommend it again. A plugin enabled *only* in `~/.claude/settings.local.json` does not count: the plugin loader never reads that file, so the plugin isn't actually working — reinstall it via `claude plugin install --scope user` and remove the stale key (verify first, per the install-then-clean procedure).

**Install with the claude CLI, never by editing `enabledPlugins` by hand** — a settings entry alone installs nothing (archive-sourced plugins in particular never load without a real install). Default to project-level: `--scope project` (shared via `.claude/settings.json`) or `--scope local` (machine-only, `.claude/settings.local.json`), unless the user explicitly asks for a global install (`--scope user`). Infer from existing project-level settings whether the user prefers local-only or shared — if unclear, ask once and use that for all installations.

Propose all relevant plugins in **batched AskUserQuestion calls**. Each plugin gets three options: **Yes** (install), **No** (skip), **Tell me more**. After the user responds, process "Tell me more" answers one plugin at a time in sequence: (1) fetch the full, untruncated content of that plugin's README from the table below (use curl — WebFetch summarizes), (2) present the full README content to the user verbatim (the READMEs are already concise — do not summarize or truncate), (3) ask a fresh AskUserQuestion with only **Yes** / **No**. Do not advance to the next "Tell me more" plugin until the current one has a Yes/No answer.

#### Plugin list

- **MATS**: `mats@alignment-hive` — For MATS fellows (handbook, lit review, best practices)
- **Python + GPU compute**: `remote-kernels@alignment-hive` — Cloud GPU instances with Jupyter kernels (RunPod)
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

These two plugins ship a compiled binary, and the marketplace has a per-platform entry for each that bundles the binary inside the plugin, so a plugin update and its binary always arrive together. For every plugin listed under **Platform entries available for** in Status above, install `<plugin><suffix>@alignment-hive` using the suffix reported there (e.g. `remote-kernels-aarch64-apple-darwin@alignment-hive`) instead of the plain key. Plugins not listed there have no entry for this platform — use the plain key.

Two rules for these entries specifically:

- **Never `--scope project`.** The key names a platform, so it must never land in a settings file that is checked in — a teammate on another OS would get an archive that refuses to run. Install with `--scope local`, or `--scope user` for a global install; this overrides the shared/local preference inferred above. (`~/.claude/settings.local.json` is not a plugin-enable location — the loader ignores it.)
- **Never both.** A platform-specific entry and its plain counterpart define the same commands, skills and hooks, so enabling both loads two copies. Exactly one key per plugin across all settings files — use the install-then-clean procedure from the Bundled-binary Migration section above.

#### README URLs for "Tell me more"

| Plugin | README URL |
|---|---|
| mats | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/mats/README.md` |
| remote-kernels | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/remote-kernels/README.md` |
| precis | `https://raw.githubusercontent.com/Crazytieguy/precis/main/README.md` |
| codex | `https://raw.githubusercontent.com/Crazytieguy/codex-plugin-cc/main/README.md` |
| model-router | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/model-router/README.md` |
| tldr | `https://raw.githubusercontent.com/Crazytieguy/alignment-hive/main/plugins/tldr/README.md` |
| agent-sanitizer | `https://raw.githubusercontent.com/AlexanderMattTurner/agent-sanitizer/main/README.md` |

For non-alignment-hive plugins, add the marketplace and install with the CLI (values from the table below; re-adding an already-known marketplace is harmless):

```
claude plugin marketplace add <github-repo> --scope <scope>
claude plugin install <plugin> --scope <scope>
```

| Plugin | `<plugin>` (install coordinate) | `<marketplace>` | `<github-repo>` |
|---|---|---|---|
| precis | `precis@precis` | `precis` | `Crazytieguy/precis` |
| codex | `codex@codex-plugin-cc` | `codex-plugin-cc` | `Crazytieguy/codex-plugin-cc` |
| agent-sanitizer | `agent-sanitizer@agent-sanitizer` | `agent-sanitizer` | `AlexanderMattTurner/agent-sanitizer` |

Then add `"autoUpdate": true` to the `extraKnownMarketplaces.<marketplace>` entry that `marketplace add` wrote to the scoped settings file — there is no CLI flag for auto-update. It is on by default for recommended non-alignment-hive marketplaces: they iterate quickly and benefit from auto-refresh, and the user has already opted in by accepting the recommendation. Claude Code (v2.1.140+) propagates the field to `~/.claude/plugins/known_marketplaces.json` on next session start.

For alignment-hive plugins:

```
claude plugin install <plugin>@alignment-hive --scope <scope>
```

If the install fails because the alignment-hive marketplace is missing, run `claude plugin marketplace add Crazytieguy/alignment-hive --scope <scope>` first, and add `"autoUpdate": true` to the declaration it writes.

#### Marketplace auto-update — retroactive sweep

Recommended non-alignment-hive marketplaces (hardcoded): `precis`, `codex-plugin-cc`, `agent-sanitizer`. Update this list whenever the plugin list above changes.

Some users may have these marketplaces installed without `autoUpdate: true` (installed before this skill enabled it by default, or installed via the `/plugin` TUI). For each affected marketplace, ask once whether to enable auto-update; idempotent — skip anything already enabled or already declined.

1. Read all four settings files (`~/.claude/settings.json`, `~/.claude/settings.local.json`, `.claude/settings.json`, `.claude/settings.local.json`) and `~/.claude/plugins/known_marketplaces.json`.
2. For each settings file, record where each plugin is enabled (`enabledPlugins` keys with value `true` → extract `<plugin>@<marketplace>`) and where each marketplace is declared (`extraKnownMarketplaces.<marketplace>` and its `autoUpdate` value if any).
3. For each candidate marketplace where: name ∈ `{precis, codex-plugin-cc, agent-sanitizer}` AND at least one plugin from it is enabled in some settings file AND `autoUpdate` is not already `true` in **any** settings file's `extraKnownMarketplaces.<marketplace>.autoUpdate` AND `autoUpdate` is not already `true` in the registry entry AND not already recorded as declined in `.claude/hive/align-rejected.md` → include in the ask.
4. Ask via `AskUserQuestion` using the same pattern as the plugin recommendations above: one `Question` per candidate marketplace, two options each (Yes / No), all questions batched in a single tool call.
5. For Yes answers, determine the target settings file by walking this preference order until a match is found, then edit that file:
   - The file that already declares the marketplace in its `extraKnownMarketplaces` (add `"autoUpdate": true` to the existing entry, preserve other fields). If multiple files declare it, prefer the most local: `.claude/settings.local.json` > `.claude/settings.json` > `~/.claude/settings.json`. Declarations in `~/.claude/settings.local.json` don't count here — step 6 handles them.
   - Otherwise, the file that enables the plugin (add a full `extraKnownMarketplaces.<marketplace>` entry with the source from the install mapping table above + `"autoUpdate": true`). Same preference order if multiple files enable it.

   This keeps personal vs shared state aligned with the user's existing choices and never promotes a third-party marketplace declaration into a more-shared file than the user already chose for the plugin itself. The change takes effect on the next session start (Claude Code propagates `autoUpdate` to the registry then).
6. If any marketplace declaration sits in `~/.claude/settings.local.json` (an older version of this flow could write it there; the CLI never uses that file), clean it up: when the marketplace is also declared in a supported settings file, merge an `"autoUpdate": true` from the stale entry into the supported one and delete the stale entry; when it is declared only there, run `claude plugin marketplace add <github-repo> --scope user`, verify it exited 0 and the marketplace appears in `claude plugin marketplace list`, add `"autoUpdate": true` to the declaration it wrote, then delete the stale entry. On verification failure, delete nothing.
7. For No answers: append to `.claude/hive/align-rejected.md` (e.g. "Declined auto-update for `precis` marketplace") so we don't re-prompt.

Skip the sweep entirely if no candidates remain after filtering.

#### alignment-hive auto-update verification

Read the `alignment-hive` entry in `~/.claude/plugins/known_marketplaces.json`. If `autoUpdate` is not `true`, mention once that the install script (`curl -fsSL https://alignment-hive.com/install.sh | bash`) is supposed to set this and recommend re-running it. Do not auto-fix.

### Transcript Retention

Claude Code deletes local session transcripts after `cleanupPeriodDays` (default 30). Session retrieval searches these transcripts, so the default silently caps how far back it can reach.

Check `cleanupPeriodDays` in all four settings files (`~/.claude/settings.json`, `~/.claude/settings.local.json`, `.claude/settings.json`, `.claude/settings.local.json`). If it's unset or below 365 everywhere, recommend setting it to `99999` in `~/.claude/settings.json` (transcript cleanup is per-machine, not per-project). Explain the tradeoff briefly: transcripts are plain text on disk and accumulate over time, but keeping them makes session retrieval useful long-term. If declined, record it in the rejected file.

### Tooling (varies by project)

Use your judgement to recommend modern, well-maintained tooling appropriate for the project. Consider dependency management, build tools, linters, typecheckers, formatters, and anything else that would improve the development workflow.

If a tool would be useful and isn't installed, ask if the user would like to install it.

### Reload + Setup

Mention that some plugins have setup skills that will be available after reloading — each plugin's SessionStart hook will nudge about its own setup when the session starts. Also mention which marketplaces just had auto-update enabled, so the user understands those will refresh automatically on session start.

After all plugins are installed, tell the user to exit and start a fresh Claude session (`/exit` then `claude`).

## Completion

Once all recommendations have been either implemented or explicitly rejected:

1. Write the plugin version (shown in Status above) to `.claude/hive/align-version` (the directory is created by the hook)

2. Write/update `.claude/hive/align-rejected.md` with natural language descriptions of rejected recommendations. Format:
   ```markdown
   # Rejected Recommendations

   - User prefers pip over uv for Python dependency management
   - No GPU compute needed
   - Declined GitHub Action
   ```

   This captures any rejection in flexible natural language, including tooling suggestions not explicitly listed here.

3. If nothing was rejected, don't create the file.
