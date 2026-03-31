# remote-kernels

Cloud GPU instances with Jupyter kernels for AI/ML workloads, powered by RunPod.

## Motivation

Common alternatives are running Claude Code inside a cloud container, or having Claude run SSH commands against a remote machine. This plugin avoids both:

- **Dynamic instance management** — Claude can start, stop, and terminate GPU pods on demand rather than relying on a pre-provisioned machine.
- **Multi-session isolation** — Multiple Claude sessions can each have their own GPU instance without interference.
- **No reconfiguration** — Your local Claude Code setup (permissions, plugins, settings) stays the same regardless of which GPU you're using.
- **No manual SSH** — Code runs through Jupyter kernels rather than piping commands over SSH, which is error-prone and tedious to set up.

## What This Plugin Does

**Managed GPU pods** — Start, stop, and terminate RunPod GPU instances directly from Claude Code. Configure GPU type, Docker image, and startup commands via a `remote-kernels.toml` file.

**Jupyter kernel execution** — Run code on remote GPUs through Jupyter kernels. Claude can execute cells, inspect outputs, and iterate — all within the conversation. Kernel activity is saved as `.ipynb` files.

**File sync** — Sync local project files to the pod and download results back.

**Budget controls** — Set a spending limit via environment variable. The plugin tracks costs and enforces the limit — when the budget is reached, the running pod is stopped or terminated and further operations are blocked.

**Automatic cleanup** — Configurable cleanup modes: stop the pod (preserving state), terminate it (deleting everything), or leave it for manual management.

## Requirements

- A RunPod API key.
