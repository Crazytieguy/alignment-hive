# remote-kernels

Cloud GPU machines with Jupyter kernels for AI/ML workloads — on RunPod,
vast.ai, or Kubernetes.

## Motivation

This plugin decouples the agent from the execution environment, in the same
spirit as Anthropic's [managed agents](https://www.anthropic.com/engineering/managed-agents)
architecture: Claude and your session stay on your machine (the "brain"),
while cloud GPUs are interchangeable "hands" it attaches, uses, and discards.
The common alternatives fall short of this: running Claude Code inside a
cloud container couples the two outright, and piping SSH commands at a
long-lived dev box decouples them only awkwardly — manual provisioning,
error-prone command plumbing, weaker security. Doing it properly buys:

- **Less manual work** — Claude starts, stops, and terminates machines on
  demand. No pre-provisioning, no manual SSH setup, and your local Claude
  Code configuration (permissions, plugins, settings) applies unchanged
  whichever GPU is in use.
- **Cross-session persistence** — everything durable lives with the agent,
  not on a disposable GPU box: transcripts, memories, downloaded results,
  and an `.ipynb` notebook per kernel. A later session reconnects to running
  machines and reads the notebooks to recover context; the machine itself is
  safe to lose.
- **More autonomy** — machines are cattle, not pets. Claude can pick
  marketplace hosts, run several machines concurrently, and clean up after
  itself — inside budget and cleanup policies you set once and it can't
  modify.

## What This Plugin Does

**Three runtimes, one interface** — RunPod (managed pods, reliable
stop/resume), vast.ai (cheapest marketplace GPUs — Claude can search offers
and pick hosts, or take the cheapest qualifying automatically; VM mode runs
Docker inside, e.g. for UK AISI Inspect's sandboxed evals), and Kubernetes
(lab clusters: pods from a lab-owned template, Kueue queue/priority aware).
Configure via `remote-kernels.toml`; switch per machine with
`start(runtime=...)`.

**Multiple concurrent machines** — Named instances (`start(name="gpu-2")`),
started in parallel (`wait=false` + `status()` polling), each with its own
kernels. Kernel calls route automatically — no instance bookkeeping.

**Jupyter kernel execution** — Run code on remote GPUs through persistent
Jupyter kernels. Claude can execute cells, inspect outputs, and iterate — all
within the conversation. Kernel activity is saved as `.ipynb` files.

**File sync** — Sync local project files to a machine (`.gitignore`-aware)
and download results back, both rooted at the project directory.

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
  `VAST_API_KEY` (a plain console key; vast accounts with 2FA enabled reject
  API writes — the setup skill explains the options, disabling 2FA being the
  supported one), or a kubeconfig for Kubernetes.
- Run the `setup` skill to configure interactively.
