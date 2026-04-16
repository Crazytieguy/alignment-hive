---
name: setup
description: This skill should be used when the user asks to "set up remote-kernels", "configure remote kernels", "set up GPU", "configure GPU access", "set up RunPod", "configure cloud GPU", or wants to run code on cloud GPUs for the first time.
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh:*)
---

# Remote Kernels Setup

Guided configuration for cloud GPU instances with Jupyter kernels via RunPod.

## Config Template

All available fields with defaults (generated from the MCP server source code):

```toml
!`${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh config-template 2>/dev/null || echo "# (binary not cached yet — config-template unavailable)"`
```

## Workflow

Walk through this interactively with the user using AskUserQuestion. Start by writing the config template to `remote-kernels.toml`, then go through **every area below** and edit the file based on their answers.

### Areas to cover

- **API Key** — `RUNPOD_API_KEY` needed. Check if it's already set. If not, `.env.local` is the typical place for it
- **GPU Selection** — what kind of workload? Set `gpu-type-ids` with fallback options
- **Docker Image** — default `runpod/pytorch` works for most ML. Custom images must have Jupyter + SSH; missing prereqs can go in `startup-commands`
- **Cloud Type** — default is `SECURE`. `COMMUNITY` is cheaper but less reliable. Recommend sticking with `SECURE` unless cost is a priority
- **Network Volume** — for persistent data across pod terminations. Important: RunPod volumes have no snapshots/backups — recommend external backup (HF Hub, W&B, S3)
- **Cleanup Mode** — stop (preserve pod) / terminate (delete pod) / disabled (manual)
- **Budget** — goes in `.claude/settings.json` `env` section as `REMOTE_KERNELS_BUDGET` (not in remote-kernels.toml, so Claude can't modify it). Incompatible with cleanup=disabled
- **Environment Variables** — what needs to be available on the pod? `inherit-env` forwards vars from the local environment (including `.env`/`.env.local` files). The user may also want to set explicit vars in the `[env]` section
- **Notebooks** — the MCP server saves kernel activity as `.ipynb` files. Decide whether to commit them or gitignore
- **Clean up** — remove commented-out lines from the config, keeping only what was configured

Finish by telling the user to reload the MCP server (run `/mcp` or restart Claude Code) so the new config takes effect, then offer to try starting a pod for them.
