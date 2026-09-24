# hive-cli

CLI for alignment-hive session sharing and management. Powers the `hive` plugin.

## Development

Before committing, `bun run --filter '@alignment-hive/hive-cli' test` and `bun run --filter '@alignment-hive/hive-cli' lint` must both pass. Never pipe test output (e.g. `bun test 2>&1 | head`): the process stalls indefinitely.

## User-Facing Messages

User-facing strings (CLI output, errors, help) live in `src/lib/messages.ts`.

## Local Transcript Inspection

`hive local` (`sessions`, `outline`, `show`, `grep`) reads local Claude Code transcripts without changing sharing settings or the transcript-directory registry. Its one help page, `localHelp` in `src/lib/messages.ts`, is the reference.

It reads transcripts only through `@alignment-hive/session-data` (`parseTranscript`; hiding and selection in `noise.ts`) and resolves locators only through `src/lib/locators.ts`, which `hive debrief render` shares, so both print and accept the same entry numbers.

## Debriefs

A debrief round lives in `hive debrief dir --session FULL_SESSION_ID --round N --data DIR`, which creates and prints `DIR/<project>/<session>/round-<N>` (`<project>` is Claude Code's project-dir name of the main checkout). The skill passes the debrief plugin's data directory, `${CLAUDE_PLUGIN_DATA}`, as `DIR`; without `--data` the round goes under `$XDG_DATA_HOME/hive/debrief` (default `~/.local/share`). Either way it sits outside the repository, so no worktree or ignore file matters. `render` and `capture` take the same `--data`.

`hive debrief render --session ID --round N [--data DIR]` renders that round's `debrief.md` into `page.html` and `manifest.json` beside it, reading the previous round's manifest when it exists; `hive debrief render debrief.md --out DIR [--prev MANIFEST.json]` does the same for explicit paths. Run it from the worktree the work was done in. Rounds after the first require the preceding round's manifest. In a repository it also prints how many changed lines since base sit in files some item's `diff` ref shows, and warns for each changed text file none shows; binary files are left out, since no ref can show them. The renderer reads only referenced evidence; `head` means the working tree, and the default base is the parent session's hive commit stamp. Git is optional: outside a repository, `diff` and `git` refs and `at:` revisions are errors, and `file` paths are relative to the current directory.

`hive debrief capture --name NAME (--session ID --round N [--data DIR] | --out DIR) -- COMMAND [ARGS...]` records a command and its result in `captures/NAME.json` in the round's directory, or under `DIR` (default: current directory). Run it from the command's intended working directory. Existing captures are not overwritten.

`hive debrief preflight --min VERSION --session FULL_SESSION_ID` checks the `hive` binary on PATH, its `--version`, and, in a git repository, the session-start stamp.

See [the authoring reference](../../plugins/debrief/references/authoring.md) for the debrief format. HTML contains its styles and evidence; the diff component loads four pinned CDN scripts, and the page loads its Google Fonts with local fallbacks.

## Version Sync

For a release, bump `plugins/hive/cli-version` together with `package.json` (`bootstrap.sh` downloads the binary for that version), then bump the plugin version so the marketplace ships the new file. The retrieval skill injects `hive local --help` whole, so a change to that page is a change to the skill.

## Dev Binary

`bun run --filter '@alignment-hive/hive-cli' build:dev` writes `.dev/hive` at the repo root. It is compiled with `ALIGNMENT_HIVE_DEV=1`, so it loads `.env.local` (per-dev overrides such as `ALIGNMENT_HIVE_CONVEX_URL`; created by `bash scripts/setup-web.sh`) and then `.env` (checked-in staging defaults: `ALIGNMENT_HIVE_CLIENT_ID`, `ALIGNMENT_HIVE_AUTH_FILE`, `ALIGNMENT_HIVE_URL`) from the cwd. The production binary never reads env files. `DEBUG=1` enables debug logging in either.
