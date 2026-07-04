---
name: setup
description: This skill should be used when the user asks to "set up remote-kernels", "configure remote kernels", "set up GPU", "configure GPU access", "set up RunPod", "set up vast.ai", "set up Kubernetes GPU pods", "configure cloud GPU", or wants to run code on cloud GPUs for the first time.
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh:*)
---

# Remote Kernels Setup

Guided configuration for cloud GPU machines with Jupyter kernels. Three
runtimes: **RunPod** (managed pods, reliable stop/resume), **vast.ai**
(cheapest marketplace GPUs; VM mode supports Docker-in-Docker for e.g.
Inspect sandboxed evals), and **Kubernetes** (lab clusters — pods from a
lab-owned template, Kueue-aware). Multiple machines can run concurrently
(named instances via `start(name=...)`).

## Config Template

All available fields with defaults (generated from the MCP server source code):

```toml
!`${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh config-template 2>/dev/null || echo "# (binary not cached yet — config-template unavailable)"`
```

## Workflow

Walk through this interactively with the user using AskUserQuestion. Start by
writing the config template to `remote-kernels.toml`, then go through **every
area below** and edit the file based on their answers.

### Areas to cover

- **Runtime choice** — which platform(s)? Set `default-runtime`; others stay
  available via `start(runtime=...)`. Guidance:
  - RunPod: simplest, reliable stop/resume, MATS default
  - vast.ai: cheapest; stopped instances may not resume (GPU can be rented
    out) so treat machines as ephemeral; set `vm = true` for anything that
    needs Docker inside (Inspect sandboxes)
  - Kubernetes: for lab clusters; needs a pod template YAML from the lab
- **API key(s)** — per runtime, in `.env.local` (gitignored):
  - RunPod: `RUNPOD_API_KEY` — https://docs.runpod.io/get-started/api-keys
  - vast.ai: `VAST_API_KEY` — https://docs.vast.ai/api-reference (create at
    https://cloud.vast.ai/manage-keys/). Instance creation requires a key
    from a **2FA-enabled** vast login — have the user enable 2FA first
  - Kubernetes: no key; uses kubeconfig (`context` in `[kubernetes]`)
- **GPU selection** — what workload? RunPod: `gpu-type-ids` (fallback list);
  vast: `[vast] gpu-name` + `max-dph` price ceiling; Kubernetes: GPU
  resources live in the pod template
- **Image** — RunPod default `runpod/pytorch` works for most ML; vast default
  is `vastai/base-image` (VMs: `vastai/kvm:ubuntu_terminal` + onstart to
  install tooling); Kubernetes images come from the template (must provide
  `sh`, `tar`, Python with `jupyter-server` + `ipykernel`)
- **Kubernetes pod template** — if using k8s, the lab owns a pod YAML
  (resources, tolerations, volumes, Kueue `queue-name` label). Point
  `[kubernetes] pod-template` at it. `start(priority="high")` sets the Kueue
  workload-priority label. Docs: https://kueue.sigs.k8s.io/docs/tasks/run/plain_pods/
  and https://kubernetes.io/docs/concepts/workloads/pods/
- **Data persistence** — IMPORTANT: discuss where remotely-generated data
  should live, and make sure that reliably happens; vast and Kubernetes have
  weak-or-no stop/resume, so anything not brought back is lost at terminate.
  Options: `download` results after runs; RunPod network volumes; Kubernetes
  PVCs in the template; a bucket the user manages (sync via
  `startup-commands`/onstart, e.g. rclone/s5cmd). RunPod volumes have no
  snapshots — recommend external backup (HF Hub, W&B, S3) for anything precious
- **Cleanup mode** — stop (preserve machine; RunPod only, unreliable on
  vast, unsupported on k8s) / terminate (delete) / disabled (manual)
- **Budget** — goes in `.claude/settings.json` `env` section as
  `REMOTE_KERNELS_BUDGET` (not in remote-kernels.toml, so Claude can't modify
  it). Enforced across ALL concurrent machines. Incompatible with
  cleanup=disabled. Kubernetes is unmetered — `max-lifetime-secs` bounds pods
  instead
- **Environment variables** — what needs to be available on the machine?
  `inherit-env` forwards vars from the local environment (including
  `.env`/`.env.local` files). Explicit vars go in the `[env]` section
- **Notebooks** — kernel activity is saved as `.ipynb` files. Decide whether
  to commit them or gitignore
- **Clean up** — remove commented-out lines from the config, keeping only
  what was configured

Finish by telling the user to reload the MCP server (run `/mcp` or restart
Claude Code) so the new config takes effect, then offer to try starting a
machine for them.
