---
name: release-plugins
description: Release considerations special to alignment-hive (version bumps, checks, what CI a push triggers, plugin archives, rollback, troubleshooting). Use when releasing this repo, bumping versions, or when a release workflow, binary release, or plugin archive looks wrong.
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
| hive-cli | `packages/hive-cli/package.json`, `plugins/hive/cli-version` to match, and the hive `plugin.json`; run `bun install --lockfile-only` so `bun.lock` follows |

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

## What a push triggers

- Users' marketplaces follow `stable`, the default branch, not `main`. The
  *Promote to stable* workflow moves `stable` up to `main` once every binary
  `main` pins is released; `.github/workflows/promote-stable.yml` says how
  and why. Nothing is tagged by hand.
- A `binary-version` change auto-tags and releases that binary. The binary
  workflow then calls *Plugin archives* and *Promote to stable*.
- A content change to model-router or remote-kernels triggers *Plugin
  archives*, which rebuilds the per-platform zips.
- *Release hive-cli* runs on hive-cli source changes and, only when
  `packages/hive-cli/package.json` has a new version, attaches compiled
  binaries to a `hive-cli-vX.Y.Z` GitHub release (it isn't on npm), then calls
  *Promote to stable*. To roll hive-cli back, release the old code under a
  higher version: the hive plugin never moves `hive` on the PATH to an older
  CLI.
- *Release Notes* posts to Slack when a plugin version reaches `stable`.

## After the push

The release has reached users once `stable` holds the bump commit:
`git fetch origin stable && git merge-base --is-ancestor <sha> origin/stable`.
Wait for that, however long the push's runs take (`gh run list --commit
<sha>`); a *Promote to stable* job that shows as cancelled only gave way to a
newer one. If the runs are over and `stable` still lacks the commit, see
Troubleshooting. Then run
`bash .claude/skills/release-plugins/scripts/update-local.sh`, which installs
the release on this machine now instead of at the next auto-update pass.

`gh workflow run` without `--ref` runs a workflow as it is on `stable`; pass
`--ref main` to anything that builds or publishes.

## Binary-shipping plugins (model-router, remote-kernels)

These are also published as `archive` marketplace entries: one zip per
platform, with that platform's released binary bundled inside, so a plugin
update and its binary install as one artifact. *Plugin archives* builds them
from `main`, not `stable`, each as soon as its own binary is released.
`plugins/<name>/` stays the single source.

**Rollback:** revert the commit and push. The build is byte-deterministic, so
CI reproduces the previous zips exactly (unless that binary release was re-run
since, which replaces its assets) and puts them back; machines on the
bad version move back on their next update pass.

`python3 scripts/plugin-archives.py build` builds the zips into
`dist/plugin-archives/` (git-ignored) to inspect them; `publish` is CI only.

## Troubleshooting

**`stable` is behind `main`, so users aren't getting a release.** A warning
on the last *Promote to stable* run's page names the release it waits for. Fix
that release (next entry); its workflow promotes when it finishes. If nothing
is missing, re-run *Promote to stable* against `main`. It fails, rather than
waits, when `stable` holds commits `main` doesn't, which a PR merged into
`stable` causes: ask the user before touching either branch.

**A binary release failed or is missing its assets.** For model-router or
remote-kernels, re-run *Auto-tag binary releases* against `main`; for the
CLI, re-run the failed jobs of *Release hive-cli*. Each promotes when it
finishes, and the first two also publish the plugin archives. The release is
created with all its assets in one `gh release create`, so a re-run fails if
a partial release exists; delete that release (keep the tag) first.

**The zips are missing but the binary release is fine.** Re-run *Plugin
archives* against `main`. It skips any plugin whose binary release is missing
rather than failing, so check the run's log rather than only its status.

**A binary release's builds are slow again.** They restore cargo caches that
*Release build cache* saves from `stable`, because a cache saved at one tag
can't be read at another. Look for `Restored from cache key` in a build job's
log. A new Rust stable on the runners is a miss until that workflow next runs
(every 6 days); run it against `stable` to refill sooner.

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
