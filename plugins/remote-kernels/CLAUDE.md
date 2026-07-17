@README.md

## Publishing

Don't publish or release without asking.

Two independent versions:
- `.claude-plugin/plugin.json` version — bump on any plugin content change (skills, scripts, hooks); drives the auto-updater.
- `binary-version` — must match a released `crates/remote-kernels` crate version (`remote-kernels-vX.Y.Z` tag); the bootstrap script reads it to download the binary. Only change it when a new binary release exists.

See `crates/remote-kernels/CLAUDE.md` for the full release flow.
