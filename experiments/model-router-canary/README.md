# model-router canary

Checks that model-router still works end to end, and screens each new CLIProxyAPI release so the pin can follow upstream closely. Most of model-router's breakages came from outside the repo: a Claude Code release whose new tool schema the Codex backend rejected, a provider load-shedding a client fingerprint or raising a version floor, a pin too old to know a new model. Two more hid behind fallbacks for weeks (GPT web search silently fell back for 21 days). The canary looks for exactly those instead of waiting for a user report or a hand-run pin audit (`experiments/model-router.md` holds the incident history and the old audits).

It runs on macOS (launchd, and the keychain login the nested `claude` runs share) for a maintainer with model-router installed.

## When it runs

launchd starts `run.sh` every hour. Most hours it checks two versions and the release list and stops there; a release list that can't be fetched for a day is a finding of its own. It runs:

- **The live checks** once a day, and as soon as Claude Code or the router changes version, so a Claude Code update that breaks routing is caught the hour it lands. Once a week they add the context-overflow probes, requests of about 1.2M tokens.
- **A candidate** when the newest CLIProxyAPI release above the pin is at least 4 days old and hasn't been evaluated. Upstream ships about nine releases a week and fixes most regressions it ships within three days, never by pulling the release; a release that is replaced before it ages is never evaluated. An inconclusive candidate is retried six hours later, up to three times.

When a run finds something, `run.sh` starts a background Claude Code session (`claude --bg --name model-router-canary`) in this repository, in auto mode, with the report and `session-prompt.md`. The session fixes a live regression, re-pins to a passing candidate (or works around a failing one, or reports it upstream), or fixes the canary. The same finding starts at most one session a day; a quiet run starts none and spends no Claude quota.

## What a run checks

**Live tier:** the install you actually use.

- `doctor --json`.
- Wire probes over HTTP, on every built-in model the child serves:
  - a streaming and a non-streaming reply per model;
  - on the primary GPT model (`gpt-6-luna`): a two-turn tool round trip that replays the reasoning signature, a prompt-cache hit, an image, a web search that must be answered by the alpha/search backend rather than the fallback (skipped if `[web-search] mode` isn't the default), and request latency against the last two weeks.
- Nested `claude -p` cells through your real wiring (HTTPS proxy or base URL), on the installed Claude Code version: a GPT session that reads and edits a file, and a Claude Haiku "ok" for the passthrough.

With API-key GPT auth, the live probes bill your OpenAI key each day.

**Candidate tier:** needs the managed CLIProxyAPI child and a Codex subscription login; other setups skip it.

1. Downloads the release and checks it against the release's `checksums.txt`.
2. Audits its source against the pinned tag: Codex token refresh behaviour, the config keys the router writes, Codex client identity.
3. Starts two sandbox routers with their own XDG dirs and free ports, one on the pinned CLIProxyAPI and one on the candidate, both on the live router binary and a copy of the Codex login.
4. Runs the same probes (overflow included) and cells against both, interleaved.
5. Decides whether to run the Terminal-Bench subset. A minor or major release always gets it. For a patch release, which is nearly every release, `claude -p` with no tools reads upstream's release notes from the pin to the candidate (and of any release since), the changed Go files and the check results, and answers "repin" or "bench".
6. When needed, runs a 12-task Terminal-Bench 2.1 subset with Claude Code + Luna against both, about an hour with Docker running.

**Verdicts.** Failures only the candidate shows are regressions. One counts once it fails on two tries in a row, or at once if it is structural (the candidate won't start, drops a model, its source audit fails, or the bench shows request errors). Failures both share point at the provider or the login, and leave the candidate inconclusive. A candidate that needed the bench but couldn't run it (Docker off, harbor missing, the login's rate limits) is inconclusive too.

**The bench** fails a candidate on request errors, not on its pass rate: Claude Code's API errors and retries, failed requests in the candidate's CLIProxyAPI log, and agent exceptions other than the time cap, 3 or more above the pinned arm and spread over at least 2 more trials (failed requests in the log count on their own, since CLIProxyAPI's own retries hide them from Claude Code). Model variance never causes those, and both arms logged none in 721 requests over the first two runs. Once the shared login hits its limits (429, quota, cooldown), Claude Code retries on both arms, so only errors the logs tie to the router count, and without those the bench is inconclusive. The pass rate is reported only: two arms differ by about 2.5 trials in 24 by chance, and CLIProxyAPI 8.0.38 lost by 6 on its first run and then won a rerun of those tasks. A gap of 5 or more reruns the tasks the candidate lost, and the rerun warns if it is behind again by a third.

## Running it

From this folder:

```bash
bun canary.ts tick            # what launchd runs: whatever is due, else nothing
bun canary.ts live [--full]   # live tier only (about 3 minutes)
bun canary.ts candidate --version 8.0.40 [--bench | --no-bench]   # evaluate one release now, even if seen before
bun canary.ts bench --version 8.0.40 [--attempts 1] [--tasks a,b]
bun canary.ts status          # recent runs and candidate verdicts
bun test                      # unit tests, no network
```

Schedule it with `sh install.sh` from the main checkout; it keeps that shell's `PATH` and any `XDG_*` overrides. `sh install.sh --remove` takes it out, and `launchctl kickstart gui/$(id -u)/com.alignment-hive.model-router-canary` runs a tick now.

## Where things live

`~/.local/share/alignment-hive-model-router-canary/` (`CANARY_DATA` overrides it):

- `runs/<stamp>/report.{md,json}`, with the cell work dirs, sandbox logs, the judge's prompt and bench jobs
- `history.jsonl`: one line per run, for scheduling, trends and `status`
- `candidates.json`: verdicts per release
- `sessions.json`: when each finding last started a session
- `downloads/`: release archives and source trees
- `harbor/.venv`: the Terminal-Bench runner (Docker keeps the task images, about 4 GB, reused across runs)
- `claude-config/`: the scratch Claude Code config every nested `claude` uses
- `launchd.log`

## Safety

- **Nested `claude`** (the cells and the bench judge, not the hand-off session). It runs on the scratch config dir with no MCP servers: the cells in `acceptEdits` mode inside a fresh directory, the judge with no tools. No bypass, no allowlist. Sandbox GPT cells run logged out with a dummy token; the live cells, the Claude cell and the judge use your login. Inside the Terminal-Bench containers the agent runs with permissions bypassed, as harbor does; nothing in them holds a credential.
- **Credential copies.** Sandboxes copy only `codex-*.json` logins, and delete them with the sandbox. A login is copied only with more than five days left on its token, so the copy won't hit CLIProxyAPI's four-day refresh lead. Refresh tokens are single use, so a refresh inside a sandbox would invalidate the live login. After a run the canary compares refresh-token hashes. If a sandbox refreshed anyway, it writes the new token back to the live login, which it does only if the live file is untouched. The source audit stops a candidate whose refresh behaviour changed before any credential is copied. A run killed hard leaves its sandbox behind; the next tick stops that sandbox's own processes, writes back a rotated token, and deletes it.
- **Processes.** The canary never touches the live service (`model-router service`). It never kills a process that isn't its own (a sandbox router or child, checked by command line), and probes only model IDs the child serves: CLIProxyAPI cools a credential down for 12 hours on an unknown model.
