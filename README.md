# alignment-hive

Shared infrastructure for alignment researchers. [MATS](https://www.matsprogram.org/) scholars are the first intended users, but this is built for the broader AI safety community.

Large orgs benefit from shared tooling and accumulated knowledge across their agents. This project aims to bring some of those advantages to independent researchers:

- **Plugin marketplace** - Curated Claude Code plugins with skills for common research workflows
- **hive-mind** - A system for sharing session learnings across the community (in development)

## Installation

Add the marketplace to Claude Code:
```
/plugin marketplace add Crazytieguy/alignment-hive
```

Install the project-setup plugin (recommended for most users):
```
/plugin install project-setup@alignment-hive
```

It includes guidance on when to install other plugins from this marketplace.

## Contributing

The [plugin-dev](https://github.com/anthropics/claude-code-plugins) plugin auto-installs when you clone this repo, so Claude can help with plugin development.

Feedback and suggestions welcome—open an issue, send a Slack DM, or reach out however works for you. All changes go through PR review.

## Roadmap

See [docs/roadmap.md](docs/roadmap.md) for what's planned.
