# alignment-hive

A shared tooling and knowledge layer for the AI alignment community.

As AI capabilities accelerate, the alignment community needs to keep pace. Large labs benefit from shared tooling and accumulated knowledge across their researchers and agents, but the broader alignment community doesn't have that. Each person starts from scratch, sifting through noise to find good tools, repeating mistakes others have already solved.

Alignment Hive provides curation, discoverability, and shared knowledge for everyone working on alignment. The best way to scale is for the community to build this together, and contributions are actively encouraged.

**Currently available:**

- **Claude Code plugin marketplace.** Curated plugins for common research workflows.
- **Session sharing.** Opt-in system for sharing Claude Code session learnings across the community, building a collective knowledge base.

## Getting Started

> **Alignment community members:** Check your email for an alignment-hive invite before installing. The invite lets you sign up and set your data sharing preferences. If you didn't receive one, contact yoav.tzfati@gmail.com.

### Prerequisites

Install [Claude Code](https://code.claude.com/docs/en/overview) if you haven't already:
```bash
curl -fsSL https://claude.ai/install.sh | bash
```

### Install

```bash
curl -fsSL https://alignment-hive.com/install.sh | bash
```

This adds the plugin marketplace, installs the hive plugin, authenticates you, and walks you through data sharing preferences and project selection.

Then open Claude Code in your project and run `/hive:align`.

## Available Plugins

| Plugin | Description | Install |
|--------|-------------|---------|
| hive | Tooling recommendations + session sharing | Included in install script |
| mats | MATS fellow handbook, lit review, best practices | `/plugin install mats@alignment-hive` |
| github-action | GitHub Action for autonomous `@claude` on issues and PRs | `/plugin install github-action@alignment-hive` |
| autopilot | Autonomous operation + permission management | `/plugin install autopilot@alignment-hive` |
| llms-fetch-mcp | Documentation fetching with [llms.txt](https://llmstxt.org/) support | `/plugin install llms-fetch-mcp@alignment-hive` |
| remote-kernels | Cloud GPU instances with Jupyter kernels ([RunPod](https://runpod.io)) | `/plugin install remote-kernels@alignment-hive` |

## Contributing

The [plugin-dev](https://github.com/anthropics/claude-code-plugins) plugin auto-installs when you clone this repo, so Claude can help with plugin development.

Feedback and suggestions welcome. Open an [issue](https://github.com/Crazytieguy/alignment-hive/issues) or email yoav.tzfati@gmail.com.

## Web App

The web interface at [alignment-hive.com](https://alignment-hive.com) handles user signup and data sharing consent.
