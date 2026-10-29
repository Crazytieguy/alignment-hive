---
name: release-plugins
description: Release considerations special to alignment-hive (version bumps, checks, the remote-kernels tag, what CI a push triggers, plugin archives, rollback, troubleshooting). Use when releasing this repo, bumping versions, or when a release workflow, binary release, or plugin archive looks wrong.
---

# alignment-hive release considerations

The git process of a release isn't detailed here; it comes from the user.
These are the considerations special to this repo.

## Versions

Code changes land without version bumps, so a release needs a bump commit.
`bash .claude/skills/release-plugins/scripts/unreleased.sh` lists each
component's commits since its version last changed on `origin/main`.

- Changes that don't reach users don't need a release: tests, lint fixes, and
  `CLAUDE.md` files. Whatever else is listed does.
- A version already bumped in an unpushed commit (the script flags it) ships
  as-is, with no second bump.
- Patch bumps by default. If a change might warrant more than a patch (a
  breaking change, or a new feature in an otherwise stable component), ask the
  user before bumping.
- The bump commit holds only version files, titled with what ships (e.g.
  `release: hive 0.6.6, model-router 0.1.37 (binary 0.1.25)`).

| Component changed | Bump |
|---|---|
| `plugins/<name>/` | its `.claude-plugin/plugin.json` (the auto-updater compares these) |
| `crates/<name>/` | its `Cargo.toml`, `plugins/<name>/binary-version` to match, and that plugin's `plugin.json`; run `cargo check -p <name>` so `Cargo.lock` follows |
| hive-cli | `packages/hive-cli/package.json`, `plugins/hive/cli-version` to match, and the hive `plugin.json` |

## Checks

- `bun run --filter '@alignment-hive/review-app' build` first: a hive-cli test
  compiles the CLI, which embeds the review app's bundle.
- `bun run --filter '*' lint` and `bun run --filter '*' test`.
- Mods' own tests (`plugins/*/tests`): `claude plugin test plugins/<name>`.
- A changed crate: `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings`, and `cargo test` in it. For remote-kernels, add
  `--features fake-runtime` to clippy and test, and run its ignored E2E suite
  with `cargo test --features fake-runtime --test fake_e2e -- --ignored
  --test-threads=1`.

## Tags

A remote-kernels binary release needs the tag `remote-kernels-vX.Y.Z` on the
bump commit. Nothing else is tagged by hand.

## What a push triggers

- Plain plugins are path sources, so the push itself is the release.
- A model-router `binary-version` change auto-tags and releases the binary; a
  remote-kernels binary releases from its tag. Either binary workflow then calls
  *Plugin archives*.
- A content change to model-router or remote-kernels triggers *Plugin
  archives*, which rebuilds the per-platform zips.
- *Release hive-cli* runs on hive-cli source changes and, only when
  `packages/hive-cli/package.json` has a new version, attaches compiled
  binaries to a `hive-cli-vX.Y.Z` GitHub release (it isn't on npm). To roll
  hive-cli back, release the old code under a higher version: the hive plugin
  never moves `hive` on the PATH to an older CLI.

## Binary-shipping plugins (model-router, remote-kernels)

These are also published as `archive` marketplace entries: one zip per
platform, with that platform's released binary bundled inside, so a plugin
update and its binary install as one artifact. `plugins/<name>/` stays the
single source. CI enforces that model-router's `binary-version` matches its
`Cargo.toml`.

**Rollback:** revert the commit and push. The build is byte-deterministic, so
CI reproduces the previous zips exactly (unless that binary release was re-run
since, which replaces its assets) and puts them back; machines on the
bad version move back on their next update pass.

`python3 scripts/plugin-archives.py build` builds the zips into
`dist/plugin-archives/` (git-ignored) to inspect them; `publish` is CI only.

## Troubleshooting

**A binary release is missing its assets.** Re-run *Auto-tag model-router
release* against `main`; for remote-kernels, re-run *Release* at the
`remote-kernels-vX.Y.Z` tag. Each publishes the plugin archives when it
finishes. The release is created with all its assets in one `gh release
create`, so a re-run fails if a partial release exists; delete that release
(keep the tag) first.

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
