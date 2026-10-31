# Auto-mode classifier benchmark

Measures how closely the router-side auto-mode classifier (`[auto-mode-classifier]`, opt-in) judges Claude Code tool calls compared with Claude Code's own local classifier on Sonnet 5. Every arm runs real Claude Code on a synthesized, resumed session whose next model reply is the case's action: arm S is the local classifier on Sonnet 5, C the same local prompt on Sol, R the router's classifier. Usage is in the header of `bench.ts`, run through `sh run.sh`, which strips the calling session's proxy variables. Prompt tuning without Claude Code is in `climb/`. Data, runs and builds live in `~/.cache/auto-mode-classifier-bench/` (`AMC_BENCH`), never in the repo: the corpus is the user's own transcripts.

## Confinement

An allowed action does run inside Claude Code, so execution is neutralised twice, and `bun bench.ts prove` checks both before any case runs (`run` refuses without a passing `prove.json`):

1. **Fake shell.** `CLAUDE_CODE_SHELL` is a fake `bash` that appends its argv (base64) to a per-case log, prints nothing and exits 0. The Bash tool, Monitor and hooks run nothing.
2. **sandbox-exec.** The whole `claude` process runs under `/usr/bin/sandbox-exec`. The profile (`lib/confine.ts`) denies every file write outside the case's scratch root and denies all network except outbound to loopback. `TMPDIR` and `CLAUDE_CODE_TMPDIR` point into the root.

On loopback, the only proxy is the egress gate. The router's HTTPS-proxy front tunnels any host it does not intercept, so without the gate an allowed WebFetch would reach the internet. The hop also refuses every write-like Anthropic endpoint (artifacts, notifications, telemetry) and every model except the arm's classifier model, with a hard per-process call budget (`--claude-budget`). `CLAUDE_CODE_SUBAGENT_MODEL=gpt-6-luna` keeps an allowed Agent call on the fake. The GPT side is fenced as well: `[web-search] mode = "off"`, and the dispatcher refuses web-search tools and any model other than the classifier model.

Nested `claude` always runs with a scratch `CLAUDE_CONFIG_DIR`, `CLAUDE_SECURESTORAGE_CONFIG_DIR=` (empty, for the login), `--strict-mcp-config`, `ENABLE_CLAUDEAI_MCP_SERVERS=false` and `--permission-mode auto`. It never runs in a bypass mode. The bench never modifies a request to Anthropic: the hop forwards the bytes unchanged, so Claude Code's billing-header integrity hash stays valid. It only refuses requests.

The bench uses its own canary data dir (`<bench>/canary`), because the canary's sweep deletes every sandbox it did not start. One bench process runs at a time (`canary/bench.lock`). The live router (127.0.0.1:8787) and its ports are never used.
