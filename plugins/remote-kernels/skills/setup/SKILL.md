---
name: setup
description: This skill should be used when the user asks to "set up remote-kernels", "configure remote kernels", "set up GPU", "configure GPU access", "set up RunPod", "set up vast.ai", "set up Kubernetes GPU pods", "configure cloud GPU", or wants to run code on cloud GPUs for the first time.
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh:*)
---

# Remote Kernels Setup

One-time setup that lets Claude control cloud compute through the
remote-kernels MCP server. The goal is to encode the user's preferences and
project conventions into `remote-kernels.toml`, and to set up the required
environment variables. Prefer inferring configuration from the project's
code, docs, and existing infrastructure; ask the user only when inference
isn't possible or is genuinely ambiguous.

## 1. Pick the runtime(s)

- **RunPod** — managed pods, reliable stop/resume; the simplest choice
  (MATS default)
- **vast.ai** — cheapest marketplace GPUs; machines are best treated as
  ephemeral. VM mode runs Docker inside (e.g. Inspect sandboxed evals)
- **Kubernetes** — lab clusters; pods from a lab-owned template, Kueue-aware

Infer from the project when possible (Kubernetes manifests or kubeconfig
references → kubernetes; an existing `RUNPOD_API_KEY`/`VAST_API_KEY` in
`.env.local`; mentions in the README or docs). Otherwise ask, using the
comparison above. Multiple runtimes can coexist — one is the default,
others stay reachable via `start(runtime=...)`.

## 2. Generate the config

Generate the template for exactly the chosen runtime(s) — the flag is
repeatable — and save it as `remote-kernels.toml` at the project root:

```sh
${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh config-template --runtime <runtime>
```

## 3. Configure it

The generated file's comments document every field, its default, and the
tradeoffs. Walk through the file with the user and edit it, preferring
values inferred from the project (existing images, GPU needs implied by the
workload, install steps from the README).

Then read the reference file for each chosen runtime — it covers setup steps
and pitfalls that are NOT config fields:

- RunPod: `references/runpod.md`
- vast.ai: `references/vast.md`
- Kubernetes: `references/kubernetes.md`

Each reference file has an "Advanced" section (custom images, Jupyter
exposure and security tradeoffs, search-filter overrides). Most users don't
need any of it: ask once whether they want to hear about advanced
configuration or security tradeoffs, and skip those sections unless they say
yes.

## 4. Cross-cutting decisions

Cover these with the user regardless of runtime:

- **Credentials** — API keys go in `.env.local` (gitignored); the reference
  file names the exact variable and where to get it. Kubernetes needs no
  key (kubeconfig).
- **Environment on the machine** — what does the workload need (HF_TOKEN,
  WANDB_API_KEY, ...)? `inherit-env` forwards local variables (including
  from `.env`/`.env.local`); explicit values go in `[env]`.
- **Cleanup mode** — the per-runtime `cleanup` key (semantics in the template
  comments). Make sure the user actively chooses: the default, terminate,
  deletes the machine and its data at session end, which is only safe once
  the data-persistence plan below is in place.
- **Data persistence** — IMPORTANT: agree where remotely-generated data
  (checkpoints, results, logs) should live, and make sure it reliably gets
  there. vast and Kubernetes machines are effectively ephemeral, and
  terminate deletes data on every runtime — anything still on the machine is
  lost with it. The simple default is to `download` results back to the
  project after every run. Data too large to shuttle needs storage that
  outlives the machine: a RunPod network volume, a PVC in the Kubernetes pod
  template, or an external store the user already trusts (S3, HF Hub, W&B)
  that jobs push to. The plugin itself backs up nothing.
- **Budget** — set `REMOTE_KERNELS_BUDGET` in `.claude/settings.json`'s
  `env` section (not in remote-kernels.toml, so Claude can't modify it).
  Enforced across ALL concurrent machines; requires cleanup != "disabled" on
  every metered runtime. Frame it as a generous upper limit, not a spending
  target — Claude should always stop or terminate machines that are no
  longer in use, and the budget is the backstop for when that fails. The
  money-safety windows (orphan halt, watchdog staleness, provision timeouts)
  are also config with sane defaults — mention they exist in the template
  rather than walking through each.
- **Notebooks** — everything executed on a kernel is saved as an `.ipynb`
  file under `remote-kernels/` at the project root (one notebook per kernel;
  configurable via `notebook-dir`). Decide with the user whether to commit
  these or gitignore the directory.

Finish by telling the user to reload the MCP server (run `/mcp` or restart
Claude Code) so the new config takes effect, then offer to try starting a
machine for them.
