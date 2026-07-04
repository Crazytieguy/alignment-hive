# remote-kernels

Cloud GPU machines with Jupyter kernels for AI/ML workloads — on RunPod,
vast.ai, or Kubernetes.

## Motivation

Common alternatives are running Claude Code inside a cloud container, or having Claude run SSH commands against a remote machine. This plugin avoids both:

- **Dynamic machine management** — Claude can start, stop, and terminate GPU machines on demand rather than relying on a pre-provisioned machine.
- **Multi-session isolation** — Multiple Claude sessions can each have their own GPU machines without interference.
- **No reconfiguration** — Your local Claude Code setup (permissions, plugins, settings) stays the same regardless of which GPU you're using.
- **No manual SSH** — Code runs through Jupyter kernels rather than piping commands over SSH, which is error-prone and tedious to set up.

## What This Plugin Does

**Three runtimes, one interface** — RunPod (managed pods, reliable
stop/resume), vast.ai (cheapest marketplace GPUs; VM mode runs Docker inside,
e.g. for UK AISI Inspect's sandboxed evals), and Kubernetes (lab clusters:
pods from a lab-owned template, Kueue queue/priority aware). Configure via
`remote-kernels.toml`; switch per machine with `start(runtime=...)`.

**Multiple concurrent machines** — Named instances (`start(name="gpu-2")`),
started in parallel (`wait=false` + `status()` polling), each with its own
kernels. Kernel calls route automatically — no instance bookkeeping.

**Jupyter kernel execution** — Run code on remote GPUs through persistent
Jupyter kernels. Claude can execute cells, inspect outputs, and iterate — all
within the conversation. Kernel activity is saved as `.ipynb` files.

**File sync** — Sync local project files to a machine (`.gitignore`-aware)
and download results back.

**Budget controls** — Set a spending limit via environment variable. Costs
are tracked across all machines (from allocation, not first use) and the
limit is enforced: on exhaustion every machine is stopped or terminated and
further operations are blocked. Each machine also carries an on-machine
watchdog as a crash-independent backstop.

**Automatic cleanup** — Configurable cleanup modes: stop (preserve state),
terminate (delete everything), or disabled (manual management). Machines that
outlive a crashed session are reconnected or reaped on the next start.

## Requirements

- An API key for the runtime(s) you use: `RUNPOD_API_KEY` and/or
  `VAST_API_KEY` (vast instance creation requires a key from a 2FA-enabled
  login), or a kubeconfig for Kubernetes.
- Run the `setup` skill to configure interactively.
