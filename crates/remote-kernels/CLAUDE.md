# remote-kernels crate

MCP server for spinning up cloud GPU instances and interacting with persistent Jupyter kernels.

## Testing

Always run the tests relevant to what you changed locally — don't lean on CI.
For anything touching shared server/jupyter/sync logic that means the fake e2e
suite (`#[ignore]`d, needs `uv`):

```sh
cargo test --features fake-runtime --test fake_e2e -- --ignored --test-threads=1
```

## Publishing

Don't publish or release without asking.

1. Bump version in `Cargo.toml`
2. Set `plugins/remote-kernels/binary-version` to match (the bootstrap script uses this to download the right binary), and bump `plugins/remote-kernels/.claude-plugin/plugin.json` (plugin content changed)
3. Update README.md if needed
4. Commit the version bumps and `Cargo.lock`
5. `git tag remote-kernels-vX.Y.Z && git push origin remote-kernels-vX.Y.Z`
6. GitHub Actions builds binaries and creates a GitHub Release automatically
