# remote-kernels crate

MCP server for spinning up cloud GPU instances and interacting with persistent Jupyter kernels.

## Publishing

Don't publish or release without asking.

1. Bump version in `Cargo.toml`
2. Set `plugins/remote-kernels/binary-version` to match (the bootstrap script uses this to download the right binary), and bump `plugins/remote-kernels/.claude-plugin/plugin.json` (plugin content changed)
3. Update README.md if needed
4. Commit the version bumps and `Cargo.lock`
5. `git tag remote-kernels-vX.Y.Z && git push origin remote-kernels-vX.Y.Z`
6. GitHub Actions builds binaries and creates a GitHub Release automatically
