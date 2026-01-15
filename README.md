# alignment-hive

Shared infrastructure for alignment researchers. [MATS](https://www.matsprogram.org/) fellows are the first intended users, but this is built for the broader AI safety community.

Large orgs benefit from shared tooling and accumulated knowledge across their agents. This project aims to bring some of those advantages to independent researchers:

- **Plugin marketplace** - Curated Claude Code plugins with skills for common research workflows
- **hive-mind** - A system for sharing session learnings across the community (in development)

## Installation

### Prerequisites

Install [Claude Code](https://code.claude.com/docs/en/overview) if you haven't already:
```bash
curl -fsSL https://claude.ai/install.sh | bash
```

### Add the marketplace

Run the following commands from within Claude Code (the `/` prefix indicates a Claude Code command):
```
/plugin marketplace add Crazytieguy/alignment-hive
```

Enable auto-update to get new plugins and updates automatically:
1. Run `/plugin`
2. Go to the **Marketplaces** tab
3. Select **alignment-hive**
4. Select **Enable auto-update**
5. Press **Esc** twice to exit the menu

Install the mats plugin (recommended for MATS fellows):
```
/plugin install mats@alignment-hive
```

The mats plugin includes:
- **project-setup** - Guidance for starting new projects with good architecture decisions
- **fellow-handbook** - Quick lookup of MATS policies, compute access, housing, and program logistics

Also available:
```
/plugin install llms-fetch-mcp@alignment-hive
```

Adds an MCP server for fetching documentation with [llms.txt](https://llmstxt.org/) support.

### Start your project

Exit Claude Code and navigate to your project directory:
```
/exit
```
```bash
cd ~/my-project && claude
```

Works for both new and existing projects. Ask Claude to help you set it up!

## Contributing

The [plugin-dev](https://github.com/anthropics/claude-code-plugins) plugin auto-installs when you clone this repo, so Claude can help with plugin development.

Feedback and suggestions welcome—open an issue, send a Slack DM, or reach out however works for you. All changes go through PR review.

## Web App

A web interface for hive-mind is in development at [alignment-hive.com](https://alignment-hive.com).

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for what's planned.
