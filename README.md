# alignment-hive

A shared tooling and knowledge layer for the AI alignment community. The repo behind [alignment-hive.com](https://alignment-hive.com).

As soft takeoff picks up, the alignment community needs shared infrastructure to keep pace. Alignment Hive aims to provide the benefits of scale that large labs have, through shared tooling and accumulated knowledge. AI tooling is moving fast, and it's hard to keep up with what's available and what works.

**What's here:**

- **Claude Code plugin marketplace.** Curated plugins shaped by concrete bottlenecks in real research work.
- **Session sharing.** Opt-in system for sharing Claude Code session data with AI safety research organizations.

## Getting Started

### Prerequisites

Install [Claude Code](https://code.claude.com/docs/en/overview) if you haven't already:
```bash
curl -fsSL https://claude.ai/install.sh | bash
```

### Install

```bash
curl -fsSL https://alignment-hive.com/install.sh | bash
```

This adds the plugin marketplace, installs the hive plugin, and walks you through data sharing preferences and project selection.

Session sharing requires an alignment-hive invite; everything else works without one. If you'd like an invite, email yoav.tzfati@gmail.com.

To add the plugin marketplace without the install script (no CLI, no hive plugin), run this inside Claude Code:

```
/plugin marketplace add Crazytieguy/alignment-hive
```

After installation open Claude Code in your project and run `/hive:align`.

## Available Plugins

| Plugin | Description | Install |
|--------|-------------|---------|
| [hive](plugins/hive) | Tooling recommendations, session memory + sharing | Included in install script |
| [mats](plugins/mats) | MATS fellow handbook, lit review, best practices | `/plugin install mats@alignment-hive` |
| [github-action](plugins/github-action) | GitHub Action for autonomous `@claude` on issues and PRs | `/plugin install github-action@alignment-hive` |
| [llms-fetch-mcp](plugins/llms-fetch-mcp) | Documentation fetching with [llms.txt](https://llmstxt.org/) support | `/plugin install llms-fetch-mcp@alignment-hive` |
| [remote-kernels](plugins/remote-kernels) | Cloud GPU machines with Jupyter kernels (RunPod, vast.ai, or Kubernetes) | `/plugin install remote-kernels@alignment-hive` |

[Codex for Claude Code](https://github.com/Crazytieguy/codex-plugin-cc) — Codex reviews and task delegation without leaving Claude Code — is maintained in a separate repo.

## Repo Layout

- `plugins/` — the Claude Code plugins above
- `packages/web/` — alignment-hive.com, including the data-sharing backend
- `packages/hive-cli/` — CLI powering the hive plugin
- `crates/remote-kernels/` — Rust binary behind the remote-kernels plugin

## Contributing

Feedback and suggestions welcome. Open an [issue](https://github.com/Crazytieguy/alignment-hive/issues) or email yoav.tzfati@gmail.com.
