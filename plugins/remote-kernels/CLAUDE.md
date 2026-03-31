@README.md

## Publishing

Don't publish or release without asking.

The plugin version in `.claude-plugin/plugin.json` must match the crate version in `crates/remote-kernels/Cargo.toml` — the bootstrap script reads this version to download the matching binary from GitHub Releases.

See `crates/remote-kernels/CLAUDE.md` for the full release flow.
