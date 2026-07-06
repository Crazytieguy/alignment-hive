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
    needs Docker inside (Inspect sandboxes) — VM images ship Docker
    preinstalled. Two vendor traps the runtime handles automatically
    (relevant only when creating VMs by hand): the image must be
    registry-qualified (`docker.io/vastai/kvm:...`) or vast silently
    creates a container instead of a VM, and vast's SSH proxy can't reach
    VMs — only direct-port hosts work
  - Kubernetes: for lab clusters; needs a pod template YAML from the lab
- **API key(s)** — per runtime, in `.env.local` (gitignored):
  - RunPod: `RUNPOD_API_KEY` — https://docs.runpod.io/get-started/api-keys
  - vast.ai: `VAST_API_KEY` — a plain console key from
    https://cloud.vast.ai/manage-keys/ (plain keys never expire). The plugin
    does not support 2FA-enabled vast accounts: with 2FA on, API writes
    (instance creation) fail with a 401 mentioning Two Factor Authentication.
    If the user hits that, have them disable 2FA on the vast account
    (cloud.vast.ai → Account → Security) — warn that the vast UI makes it
    easy to enable 2FA accidentally. A user who insists on keeping 2FA can
    mint a session key by hand (`POST /api/v0/tfa/` with the console key as
    Bearer and a fresh TOTP code; store the returned `session_key`), but it
    expires after ~1-2 days and needs re-minting each time — say so before
    they choose
  - Kubernetes: no key; uses kubeconfig (`context` in `[kubernetes]`)
- **GPU selection** — what workload? RunPod: `gpu-type-ids` (fallback list);
  vast: `[vast] gpu-name` + `max-dph` price ceiling; Kubernetes: GPU
  resources live in the pod template
- **Host selection (vast)** — two modes, both worth explaining:
  - *Claude picks*: `search_vast_offers()` returns a table of hosts plus
    picking advice; Claude ranks a shortlist and passes it to
    `start(vast_offers=[...])`. The user can just say "pick a host for me" /
    "find me a good deal" in any session
  - *Automatic*: plain `start()` takes the cheapest offers that pass the
    configured filters — zero friction, but cheapest-first can land on
    slower hosts
  Ask what the user tends to care about when picking GPUs (price, locality,
  bandwidth, host quality) and write it into `selection-guidance` — it is
  appended to the advice Claude sees on every search. Power users: the
  baseline search filters are documented in the `[vast]` template section
  and every one can be overridden via `[vast.query]`
- **Image** — RunPod default `runpod/pytorch` works for most ML; vast default
  is `vastai/base-image` (VMs: `vastai/kvm:ubuntu_terminal`, which ships
  Docker + CUDA; onstart installs anything else, e.g. uv — for
  Docker-dependent workloads add a guard onstart line:
  `docker info >/dev/null 2>&1 || (curl -fsSL https://get.docker.com | sh)`);
  Kubernetes images come from the template (must provide `sh`, `tar`, Python
  with `jupyter-server` + `ipykernel`)
- **RunPod custom image → `image-start-cmd`** — pods carry a pre-SSH orphan
  guard: if the server that created a pod dies before ever reaching it (crash
  in the first minutes of provisioning), the pod cleans itself up after 45
  minutes instead of billing until someone notices. The guard wraps the
  image's own start command via dockerStartCmd, so it needs to know that
  command. The default image is handled automatically; for a custom RunPod
  image, find its Dockerfile `CMD` (check the image's Dockerfile or docs, run
  `docker inspect --format '{{.Config.Entrypoint}} {{.Config.Cmd}}' <image>`
  locally if available, or ask the user) and set `[runpod] image-start-cmd`
  to it. A wrong value keeps SSH/Jupyter from starting (the pod is then
  terminated by the provision timeout — bounded cost, but a broken start).
  Images that define an ENTRYPOINT keep it (the wrapper only replaces CMD) —
  if the ENTRYPOINT is the workload and CMD is just its arguments, leave
  `image-start-cmd` unset. When the CMD can't be determined confidently, set
  `image-start-cmd = ""` and warn the user explicitly: a crash during the
  first minutes of provisioning leaves the pod billing until stopped by hand
  (RunPod console or `start()` from a later session, which reconnects and
  resumes supervision). The guard is also skipped — with a note at start() —
  on community cloud unless `support-public-ip = true` (no SSH heartbeat to
  disarm it), and silently when `cleanup = "disabled"` (that mode promises no
  automatic cleanup). A raw `docker-start-cmd` passthrough conflicts with the
  guard; migrate it to `image-start-cmd`
- **Kubernetes pod template** — if using k8s, the lab owns a pod YAML
  (resources, tolerations, volumes, Kueue `queue-name` label). Point
  `[kubernetes] pod-template` at it. `start(priority="high")` sets the Kueue
  workload-priority label. If the template has multiple containers, set
  `container-name` to the workload container (the one that gets env vars +
  the Jupyter token and runs kernels) — otherwise the FIRST container is
  assumed. Docs: https://kueue.sigs.k8s.io/docs/tasks/run/plain_pods/
  and https://kubernetes.io/docs/concepts/workloads/pods/
- **Kubernetes pod lifetime** — ALWAYS ask explicitly; do not assume. The
  user knows their lab's usage patterns (interactive sessions vs. overnight
  runs); we don't. Kubernetes is unmetered — no budget applies — so
  `max-lifetime-secs` is the only lifetime bound the plugin provides: it
  becomes the pod's `activeDeadlineSeconds` (when the template doesn't set
  one), and when it fires the pod is KILLED mid-run — anything not synced
  back is lost (tie this to the data-persistence discussion). `0` disables
  it, leaving lifecycle to the template — a legitimate choice when the lab's
  template owns policy, but then nothing bounds forgotten pods. Write the
  chosen value into the config even if it matches the fallback (43200 = 12h)
  so the choice is explicit.
- **Data persistence** — IMPORTANT: discuss where remotely-generated data
  should live, and make sure that reliably happens; vast and Kubernetes have
  weak-or-no stop/resume, so anything not brought back is lost at terminate.
  Options: `download` results after runs; RunPod network volumes; Kubernetes
  PVCs in the template; a bucket the user manages (sync via
  `startup-commands`/onstart, e.g. rclone/s5cmd). RunPod volumes have no
  snapshots — recommend external backup (HF Hub, W&B, S3) for anything precious
- **Cleanup mode** — per-runtime: set `cleanup` under `[runpod]` / `[vast]` /
  `[kubernetes]` (the top-level `cleanup` key is deprecated; existing configs
  still work, it acts as a fallback). Modes: stop (preserve machine) /
  terminate (delete) / disabled (manual). Kubernetes accepts
  terminate/disabled only — pods have no stop. If vast is configured and the
  user considers `stop`, explain the tradeoff: stop on vast is unreliable —
  the GPU can be re-rented to someone else while stopped so resume may hang
  forever, and storage keeps billing until the instance is terminated
- **Budget** — goes in `.claude/settings.json` `env` section as
  `REMOTE_KERNELS_BUDGET` (not in remote-kernels.toml, so Claude can't modify
  it). Enforced across ALL concurrent machines. Requires cleanup !=
  "disabled" on every metered runtime (runpod, vast); Kubernetes is unmetered
  and exempt — `max-lifetime-secs` bounds pods instead
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
