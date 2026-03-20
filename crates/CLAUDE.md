# Rust Development

## Quality Gates

Before committing:
1. `cargo fmt`
2. `cargo clippy`
3. `cargo test`

## Dependencies

Use `cargo add <crate>` to get latest versions. Check generated docs in `target/doc-md` with `cargo doc-md` (run from the crate directory, not workspace root).

@remote-kernels/target/doc-md/index.md

## Code Style

- Edition 2024
- `#![warn(clippy::pedantic)]` in main.rs with selective `#[allow(...)]` for justified cases
- `McpError` for MCP tool errors, internal errors via anyhow
- Follow rmcp patterns: `#[tool_router]`, `#[tool]`, `#[tool_handler]` macros
