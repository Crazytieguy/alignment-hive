# remote-kernels crate

MCP server for spinning up cloud GPU instances and interacting with persistent Jupyter kernels.

## Testing

Always run the tests relevant to what you changed locally — don't lean on CI.
For anything touching shared server/jupyter/sync logic that means the fake e2e
suite (`#[ignore]`d, needs `uv`):

```sh
cargo test --features fake-runtime --test fake_e2e -- --ignored --test-threads=1
```
