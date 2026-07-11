# remote-kernels

Cloud GPU machines with Jupyter kernels for AI/ML workloads — on RunPod,
vast.ai, or Kubernetes.

## Motivation

This plugin decouples the agent from the execution environment, in the same
spirit as Anthropic's [managed agents](https://www.anthropic.com/engineering/managed-agents)
architecture: Claude and your session stay on your machine (the "brain"),
while cloud GPUs are interchangeable "hands" it attaches, uses, and
discards. Running Claude Code inside a cloud container couples the two
outright; piping SSH commands at a long-lived dev box decouples them only
awkwardly. Doing it properly buys:

- **Less manual work** — Claude starts, stops, and terminates machines on
  demand. No pre-provisioning, no manual SSH setup, and your local Claude
  Code configuration (permissions, plugins, settings) applies unchanged
  whichever GPU is in use.
- **Cross-session persistence** — everything durable lives with the agent,
  not on a disposable GPU box: transcripts, memories, downloaded results,
  and an `.ipynb` notebook per kernel. A later session reconnects and picks
  up where it left off; the machine itself is safe to lose.
- **More autonomy** — machines are cattle, not pets. Claude can pick
  marketplace hosts, run several machines concurrently, and clean up after
  itself — inside budget and cleanup policies you set once.

## What This Plugin Does

**Three runtimes, one interface** — RunPod (managed pods, reliable
stop/resume), vast.ai (cheapest marketplace GPUs — Claude can search offers
and pick hosts, or take the cheapest qualifying automatically; VM mode runs
Docker inside, e.g. for UK AISI Inspect's sandboxed evals), and Kubernetes
(lab clusters: pods from a lab-owned template, Kueue queue/priority aware).
Configure via `remote-kernels.toml`; switch per machine with
`start(runtime=...)`.

**Multiple concurrent machines** — Every `start()` provisions a fresh
machine with a unique id; reconnecting to an existing machine is always an
explicit `attach(machine_id)`, with kernel state and output recovered
across sessions. Machines start in parallel, each with its own kernels, and
kernel calls route automatically — no instance bookkeeping.

**Jupyter kernel execution** — Run code on remote GPUs through persistent
Jupyter kernels. Claude can execute cells, inspect outputs, and iterate — all
within the conversation. Kernel activity is saved as `.ipynb` files.

**File sync** — Sync local project files to a machine (`.gitignore`-aware)
and download results back, both rooted at the project directory.

**Budget controls** — Set a spending limit once and each Claude session
gets its own cap, covering all the machines that session pays for —
including storage that keeps billing on stopped ones. On exhaustion, that
session's machines get a grace window to save their work before being
stopped or terminated — enforced on the machine itself, so it holds even if
your laptop dies.

**Automatic cleanup that never races your work** — Disconnects (background
sessions, closed laptops, crashes) never destroy a machine mid-work: it
finishes what it's running, runs your configured command to push results
out, and only then cleans itself up the way you chose — stop, terminate, or
leave alone. If saving fails, the machine is preserved rather than deleted.
A later session reattaches and picks up everything that happened while you
were away.

## Requirements

- An API key for the runtime(s) you use: `RUNPOD_API_KEY` and/or
  `VAST_API_KEY` (a plain console key; vast accounts with 2FA enabled reject
  API writes — the setup skill explains the options, disabling 2FA being the
  supported one), or a kubeconfig for Kubernetes.
- Run the `setup` skill to configure interactively.
