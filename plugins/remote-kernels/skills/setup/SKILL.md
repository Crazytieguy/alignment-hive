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

## 0. Wrong platform (check this first)

`${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap.sh platform-check` — silent and exit 0
means all is well, go on. It only fails when a platform-specific plugin entry
for a *different* machine is enabled: those bundle one target's binary, so the
wrong one cannot run and the MCP server will not start. It prints the entry
that should be there, e.g. `remote-kernels-aarch64-apple-darwin`.

Fix it by editing whichever settings file enables the wrong key: replace it
with `<printed-name>@alignment-hive`, keeping the value `true`. Exactly one
remote-kernels key may be enabled across all settings files — the plain
`remote-kernels@alignment-hive` and any platform entry declare the same MCP
server, so two of them load twice. A platform key belongs only in a
machine-local settings file; if the wrong one came from a checked-in
`.claude/settings.json`, moving it to `.claude/settings.local.json` is the fix,
and collaborators need to add the entry for their own machines.

Then tell the user to restart Claude Code and re-run this skill.

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
- **Budget** — set `REMOTE_KERNELS_BUDGET` in `.claude/settings.json`'s
  `env` section rather than `budget-cap` in remote-kernels.toml: the env var
  overrides the TOML, and settings.json edits get extra protection from
  Claude Code that an ordinary project file doesn't — a strong guardrail,
  though not an absolute guarantee in fully-automatic permission modes.
  Optional: no budget means the user manages spend manually, and that is a
  supported choice. When set, the cap is **per Claude session**: it covers
  total provider spend attributable to that session — machines it started
  plus, from the moment of attach, machines it adopted — including storage
  that keeps billing on a stopped machine until someone terminates it
  (status() shows that tail). The count is cumulative for the life of the
  session (it survives restarts, backgrounding, and machine termination;
  it resets only with a genuinely new session), and concurrent sessions
  have independent caps — total exposure is cap x live sessions. A machine
  left behind by an ended session keeps self-enforcing that session's
  remaining budget until adopted. Frame the cap as a generous upper limit,
  not a spending target — Claude should always stop or terminate machines
  that are no longer in use, and the budget is the backstop for when that
  fails. The money-safety windows (orphan halt, provision timeouts, budget
  grace) are also config with sane defaults — mention they exist in the
  template rather than walking through each.
- **Notebooks** — everything executed on a kernel is saved as an `.ipynb`
  file under `remote-kernels/` at the project root (one notebook per kernel;
  configurable via `notebook-dir`). Decide with the user whether to commit
  these or gitignore the directory.

## 5. Data persistence & cleanup

The most consequential conversation — have it explicitly. Two linked
decisions: **where remotely-generated data (checkpoints, results, logs)
lives**, and **what a machine may do to itself when the session ends** (the
per-runtime `cleanup` key). They must be decided together: `terminate`, the
default, deletes everything still on the machine — on every runtime — and
the plugin itself backs up nothing.

When a session ends or disconnects, the machine finishes running work, runs
the matching finalize command, and only then applies the cleanup mode. So
data can reach safety along three routes:

- **Continuous push (strongest)** — long-running jobs write checkpoints and
  results to storage that outlives the machine *as they run* (wandb, S3 via
  rclone/s5cmd, git — whatever store the user's stack already trusts), so
  there is never much unsynced work to lose. This pattern only protects
  runs that actually follow it: encode it durably — best in reusable
  project code that every job goes through (and that `sync()` ships to the
  machine), otherwise in CLAUDE.md, a skill, or a memory — so future
  sessions apply it without being reminded.
- **Download after each run** — for shorter runs, `download()` results back
  to the project right after they're produced. No machine-side setup, but a
  disconnect between producing and downloading falls through to the
  finalize command below.
- **Finalize command (the safety net)** — the per-runtime
  `pre-terminate-command` / `pre-stop-command` run on the machine after
  work drains and before cleanup acts. Ask directly: "if a machine has to
  clean itself up while you're away, where should results go?" — and set
  the command that puts them there. If the command fails, terminate
  degrades to stop so the data stays collectable. When the finalize command
  reliably syncs everything, in-session downloads of the same artifacts are
  redundant (and doubly transferred) — pick which route owns which data.

Leaving all of this unset is a valid answer, spelled out plainly: terminate
may then lose unsynced work.

With the persistence plan settled, have the user actively choose the
cleanup mode rather than silently keeping the default:

- `terminate` (default) — machine and remaining data deleted, no residual
  costs. Safe exactly to the extent the plan above is real.
- `stop` — machine preserved for a later `attach()`; storage keeps billing
  until someone terminates it, and on vast a stopped machine may never be
  resumable (its GPU can be re-rented) — prefer terminate there.
- `disabled` — nothing automatic; the user owns the lifecycle and the bill.

## 6. Finish

Tell the user to reload the MCP server (run `/mcp` or restart Claude Code)
so the new config takes effect, then offer to try starting a machine for
them.
