---
name: release-plugins
description: Versioning and releasing this repo's plugins and their binaries. Use when changing anything under plugins/ or crates/, registering a new plugin, or troubleshooting a release.
---

# Releasing a plugin

Ask before releasing.

## Registering a plugin

Add it to `.claude-plugin/marketplace.json` with a `name` matching its
`plugins/` folder. The platform-specific archive entries are the exception:
their names carry a target triple and their urls are fixed, so they are
written once (see Design notes).

## Versions

| File | Bump when |
|---|---|
| `plugins/<name>/.claude-plugin/plugin.json` | any plugin content change — the auto-updater compares these |
| `plugins/hive/cli-version` | alongside `packages/hive-cli/package.json` |
| `plugins/<name>/binary-version` | a new crate binary is released; must equal the crate's `Cargo.toml` version |

Patch bumps unless the user asks for a minor.

For model-router, CI enforces that `binary-version` and
`crates/model-router/Cargo.toml` match, and a push to `main` that changes
`binary-version` auto-tags and releases the binary. remote-kernels releases its
binary from a manual tag push — see `crates/remote-kernels/CLAUDE.md`.

## Plain plugins

Bump `plugin.json` and commit. The marketplace entry is a path source, so
landing on `main` is the release.

## Binary-shipping plugins (model-router, remote-kernels)

These are also published as `archive` marketplace entries: one zip per
platform, with that platform's released binary bundled inside, so a plugin
update and its binary install as one artifact. `plugins/<name>/` stays the
single source.

**Content change:** bump `plugin.json`, commit, land. CI rebuilds the zips and
replaces the release assets.

**Binary change:** bump the crate's `Cargo.toml`, `binary-version` and
`plugin.json` together, commit, land — one push. The binary release workflow
calls the plugin-archives workflow after it has cut the binary, which then
publishes the zips around it.

**Rollback:** revert the commit and land. The build is byte-deterministic, so
CI reproduces the previous zips exactly and puts them back; machines on the
bad version move back on their next update pass.

## Commands

`python3 scripts/plugin-archives.py <command>`:

- `build` — build the zips into `dist/plugin-archives/` (git-ignored) to
  inspect them
- `publish` — build and upload; CI only

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
