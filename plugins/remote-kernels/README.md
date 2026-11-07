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
  and an `.ipynb` notebook per kernel. A later session reads those and carries
  on, and losing a machine loses only what wasn't brought back from it.
- **More autonomy** — machines are cattle, not pets. Claude can pick
  marketplace hosts, run several machines concurrently, and clean up after
  itself — inside budget and cleanup policies you set once.

## What This Plugin Does

**Three runtimes, one interface** — RunPod (managed pods with stop/resume,
though a GPU isn't guaranteed on resume), vast.ai (marketplace GPUs, often the
cheapest — Claude can search offers
and pick hosts, or take the cheapest qualifying automatically; VM mode runs
Docker inside, e.g. for UK AISI Inspect's sandboxed evals), and Kubernetes
(lab clusters: pods from a lab-owned template, Kueue queue/priority aware).
Configure via `remote-kernels.toml`; switch per machine with
`start(runtime=...)`.

**Multiple concurrent machines** — Every `start()` provisions a fresh
machine with a unique id. A restarted server picks its own session's
running machines back up automatically; resuming a stopped machine or
adopting another session's is an explicit `attach(machine_id)`. Attaching to a
running machine reconnects its live kernels and recovers their recorded output;
resuming a stopped one keeps what's on persistent storage, but not kernel
memory. Machines start in parallel, each with its own kernels, and kernel calls
route automatically.

**Jupyter kernel execution** — Run code on remote GPUs through persistent
Jupyter kernels. Claude can execute cells, inspect outputs (plots included, as
images), and iterate — all within the conversation. Kernel activity is saved
as `.ipynb` files.

**File sync** — Sync local project files to a machine (`.gitignore`-aware)
and download results back, both rooted at the project directory.

**Budget controls** — Set a spending limit once and each Claude session
gets its own cap on the estimated spend of the machines it pays for, including
storage on stopped ones. Separately managed network volumes aren't counted.
Past the cap, tool calls that add work are refused, and that session's running
machines get a grace window to save their work, then stop or terminate
themselves — enforced on the machine, so it holds even if your laptop dies. The
cap isn't a hard ceiling on the bill: the grace window can go past it, and a
stopped machine keeps billing for storage until someone terminates it.

**Automatic cleanup** — When a session goes quiet (background sessions, closed
laptops, crashes), a RunPod or vast machine gives it five minutes to come back,
waits for its running work, runs your configured command to save results, and
then stops or terminates itself, whichever you
chose; with `cleanup = "disabled"` it does none of this. If saving fails, a
terminate becomes a stop, which keeps persistent storage only. Kubernetes pods
and RunPod machines without SSH don't clean themselves up, so end those
explicitly. A later session can reattach to a machine that is still running.

## Requirements

- An API key for the runtime(s) you use: `RUNPOD_API_KEY` and/or
  `VAST_API_KEY` (a plain console key; vast accounts with 2FA enabled reject
  API writes — the setup skill explains the options, disabling 2FA being the
  supported one), or a kubeconfig for Kubernetes.
- Run the `setup` skill to configure interactively.
