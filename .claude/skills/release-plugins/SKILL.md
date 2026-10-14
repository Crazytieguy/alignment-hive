---
name: release-plugins
description: alignment-hive's release steps — bump versions for everything changed since its last release, push main, and the CI that push triggers.
disable-model-invocation: true
---

# Releasing plugins

The user invoking this skill, or `/release` in this repo, is the go-ahead to
bump, commit, and push `main` (and push a remote-kernels tag). Code changes
land without version bumps; all bumping happens here.

These are alignment-hive's specifics. The general flow (syncing with
`origin/main`, reviewing what ships, pushing, seeing CI through) is the user's
global `release` skill; if it isn't loaded, read
`~/.claude/skills/release/SKILL.md` and follow both.

## 1. Find what changed

Run `bash .claude/skills/release-plugins/scripts/unreleased.sh` for each
component's commits since its version last changed on `origin/main`.

Skip changes that don't reach users: tests, lint fixes, and `CLAUDE.md`
files. Whatever else is listed is a release. A version already bumped in an
unpushed commit (the script flags it) ships as-is, with no second bump.

## 2. Pick versions

Patch bumps by default. If a change might warrant more than a patch (a
breaking change, or a new feature in an otherwise stable component), ask the
user before bumping; otherwise just proceed.

| Component changed | Bump |
|---|---|
| `plugins/<name>/` | its `.claude-plugin/plugin.json` (the auto-updater compares these) |
| `crates/<name>/` | its `Cargo.toml`, `plugins/<name>/binary-version` to match, and that plugin's `plugin.json`; run `cargo check -p <name>` so `Cargo.lock` follows |
| hive-cli | `packages/hive-cli/package.json`, `plugins/hive/cli-version` to match, and the hive `plugin.json` |

## 3. Commit and push

Commit only the version files, titled with what ships (e.g. `release: hive
0.6.6, model-router 0.1.37 (binary 0.1.25)`), and push `main`. For a
remote-kernels binary, also `git tag remote-kernels-vX.Y.Z && git push origin
remote-kernels-vX.Y.Z`.

`gh run list --commit <full sha>` lists the runs the push triggered (a short
sha matches nothing). If a run fails, see Troubleshooting. What triggers what:

- Plain plugins are path sources, so the push itself is the release.
- A model-router `binary-version` change auto-tags and releases the binary; a
  remote-kernels binary releases from its tag. Either binary workflow then calls
  *Plugin archives*.
- A content change to model-router or remote-kernels triggers *Plugin
  archives*, which rebuilds the per-platform zips.
- A hive-cli version change triggers *Release hive-cli*.

## Binary-shipping plugins (model-router, remote-kernels)

These are also published as `archive` marketplace entries: one zip per
platform, with that platform's released binary bundled inside, so a plugin
update and its binary install as one artifact. `plugins/<name>/` stays the
single source. CI enforces that model-router's `binary-version` matches its
`Cargo.toml`.

**Rollback:** revert the commit and push. The build is byte-deterministic, so
CI reproduces the previous zips exactly and puts them back; machines on the
bad version move back on their next update pass.

`python3 scripts/plugin-archives.py build` builds the zips into
`dist/plugin-archives/` (git-ignored) to inspect them; `publish` is CI only.

## Troubleshooting

**A binary release is missing its assets.** Re-run *Auto-tag model-router
release* against `main`; for remote-kernels, re-run *Release* at the
`remote-kernels-vX.Y.Z` tag. Both are idempotent, and each publishes the plugin
archives when it finishes.

**The zips are missing but the binary release is fine.** Re-run *Plugin
archives* against `main`. It skips any plugin whose binary release is missing
rather than failing, so check the run's log rather than only its status.

**A push seems to have triggered nothing.** Check
[githubstatus.com](https://www.githubstatus.com/history), then
`gh api "repos/Crazytieguy/alignment-hive/actions/runs?head_sha=<sha>"`.
Dropped push events are not replayed; repeat the triggering action.

## Design notes

Every archive url is fixed, on one rolling release tagged `plugin-archives`;
publishing replaces the assets in place. Claude Code downloads an archive
plugin on each update pass and reads the version out of the zip, so a url that
never changes still delivers updates — and `marketplace.json` never needs
editing at release time.

Entries deliberately carry no `sha256`: a pin can only be written by whoever
built the zip, so it would force a second push per binary release, and whoever
can change the pin can change the release it points at.
