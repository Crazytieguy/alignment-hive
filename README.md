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

After installation open Claude Code in your project and run `/hive:align`.

### Manual install

To add the plugin marketplace without the install script (no CLI, no hive plugin), merge this into your `~/.claude/settings.json`:

```json
{
  "extraKnownMarketplaces": {
    "alignment-hive": {
      "source": { "source": "github", "repo": "Crazytieguy/alignment-hive" },
      "autoUpdate": true
    }
  }
}
```

## Available Plugins

Install a plugin by adding it to `enabledPlugins` in `settings.json`, e.g.:

```json
{
  "enabledPlugins": {
    "mats@alignment-hive": true
  }
}
```

| Plugin | Description | `enabledPlugins` key |
|--------|-------------|----------------------|
| [hive](plugins/hive) | Tooling recommendations, session memory + sharing | Included in install script |
| [mats](plugins/mats) | MATS fellow handbook, lit review, best practices | `mats@alignment-hive` |
| [llms-fetch-mcp](plugins/llms-fetch-mcp) | Documentation fetching with [llms.txt](https://llmstxt.org/) support | `llms-fetch-mcp@alignment-hive` |
| [remote-kernels](plugins/remote-kernels) | Cloud GPU machines with Jupyter kernels (RunPod, vast.ai, or Kubernetes) | `remote-kernels@alignment-hive` |
| [model-router](plugins/model-router) | GPT models as native Claude Code subagents via a local gateway (experimental) | `model-router@alignment-hive` |

[Codex for Claude Code](https://github.com/Crazytieguy/codex-plugin-cc) — Codex reviews and task delegation without leaving Claude Code — is maintained in a separate repo.

## Repo Layout

- `plugins/` — the Claude Code plugins above
- `packages/web/` — alignment-hive.com, including the data-sharing backend
- `packages/hive-cli/` — CLI powering the hive plugin
- `crates/remote-kernels/` — Rust binary behind the remote-kernels plugin
- `crates/model-router/` — Rust gateway behind the model-router plugin

## Contributing

Feedback and suggestions welcome. Open an [issue](https://github.com/Crazytieguy/alignment-hive/issues) or email yoav.tzfati@gmail.com.
