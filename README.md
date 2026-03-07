# alignment-hive

Shared infrastructure for alignment researchers. [MATS](https://www.matsprogram.org/) fellows are the first intended users, but this is built for the broader AI safety community.

Large orgs benefit from shared tooling and accumulated knowledge across their agents. This project aims to bring some of those advantages to independent researchers:

- **Plugin marketplace** - Curated Claude Code plugins with skills for common research workflows
- **hive-mind** - A system for sharing session learnings across the community (in development)

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

This adds the marketplace, installs the hive plugin with auto-update, and optionally authenticates for session sharing.

Then open Claude Code in your project and run `/hive:recommendations`.

## Available Plugins

| Plugin | Description | Install |
|--------|-------------|---------|
| hive | Tooling recommendations + session sharing | Included in install script |
| mats | Best practices, fellow handbook, lit review | `/plugin install mats@alignment-hive` |
| github-action | GitHub Action for autonomous `@claude` on issues and PRs | `/plugin install github-action@alignment-hive` |
| autopilot | Autonomous operation + permission management | `/plugin install autopilot@alignment-hive` |
| llms-fetch-mcp | Documentation fetching with [llms.txt](https://llmstxt.org/) support | `/plugin install llms-fetch-mcp@alignment-hive` |
| remote-kernels | Cloud GPU instances with Jupyter kernels ([RunPod](https://runpod.io)) | `/plugin install remote-kernels@alignment-hive` |
| hive-mind | Session sharing (in development) | `/plugin install hive-mind@alignment-hive` |

## Contributing

The [plugin-dev](https://github.com/anthropics/claude-code-plugins) plugin auto-installs when you clone this repo, so Claude can help with plugin development.

Feedback and suggestions welcome—open an issue, send a Slack DM, or reach out however works for you. All changes go through PR review.

## Web App

A web interface for hive-mind is in development at [alignment-hive.com](https://alignment-hive.com).

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for what's planned.
