# Claude Code Productivity for MATS Fellows

Brainstorming document for improving MATS fellows' productivity with Claude Code.

**Context**: After two weeks of MATS, the lowest hanging fruit for helping fellows' productivity is likely helping them set up Claude Code better, rather than hive-mind features.

**Target users**: ~100 fellows total, ~20 onboarded to alignment-hive tooling so far. Variable technical comfort (some may need an hour of command line basics).

**Work types**: ML training, evals, interpretability, theory, policy - technical use cases benefit most but all streams can benefit.

---

## Workstream 1: Permissions & Safety

How to configure Claude Code permissions effectively for research workflows.

### User Preferences & Decisions

- Likes dontAsk mode because it lets the model work within its permissions rather than waiting for approval
- Prefers protecting against real risks (data loss) directly rather than broad permission restrictions
- Wants to find a path that's both efficient and secure

### What We Discussed

**Pain points identified by user:**
- Bash commands and web fetches are the most common permission prompts
- Basic harmless commands (mkdir, ls) require approval by default
- Claude often needs unexpected commands when debugging or fetching documentation
- No UI option for "allow in this project" (only "allow once" or "allow in this folder" which goes to gitignored settings.local.json)
- Complex commands that Claude Code fails to parse only show "Yes" or "No"
- Many workflows require writing Python then running it, so arbitrary execution happens anyway

**Risk assessment (user's observations):**
- Bad outcomes from --dangerously-skip-permissions are rare
- When they happen, it's mostly loss of work or data
- Leaking private data may not be a major concern for many fellows
- Remaining risk after data protection would be prompt injection, but that's harder to exploit if destructive commands are blocked

**Ideas from user:**
- Copy-on-write storage or snapshots could protect against data loss
- Specifically blocking git force pushes would eliminate a major risk vector
- Could set `allow: Bash(*)` and `deny: Bash(git push --force)` - deny takes precedence
- Even better: agent doesn't have credentials to push to main at all (but more effort, different fellows have different workflows)
- Permission generator inside best-practices skill that asks questions and builds config

**On sandbox mode (user's experience):**
- Tried it before, felt buggy
- Issues: programs send HTTP requests or access files in harmless ways not allowed by sandbox
- Agent often asks to run without sandbox, returning to permission paradigm
- Worth re-testing - maybe sandbox + semantic permissions could combine

### Claude's Suggestions (Not Yet Validated)

- Preset profiles like "research-local", "research-remote-trusted"
- Testing session to document what breaks in sandbox mode

### To Investigate

- What currently breaks in sandbox mode?
- What's a good default permission set for research workflows?
- Can sandbox mode and semantic permissions be combined effectively?
- Exact deny patterns for git safety

---

## Workstream 2: Sessions & Interaction

Running multiple Claude instances (locally and remotely) and interacting with them through various modalities.

### User Preferences & Decisions

- Has optimized local setup with git worktrees (doesn't require GPU compute personally)
- Remote instances should allow autonomous work AND user attachment via Claude Code TUI
- Session continuation via `--continue` flag is important - terminal session doesn't need to be preserved
- Prefers SSH + tmux/screen simplicity over complex web terminals
- Wants ability to start sessions from phone
- For voice: primary use case is talking while walking/jogging, not at desk
- Fellows work in offices with others, so won't use voice at desk much

### What We Discussed

**User's local worktree setup (~/projects/worktree-scripts):**
- One script creates worktree, copies gitignored files, starts Claude session
- Another script rebases worktree onto main, fast-forwards main to include commits, removes worktree and deletes branch
- Run manually when starting a session and when happy with results
- Works well but has limitations: storage duplication, no remote support

**MATS compute resources:**
- $12K budget per fellow ($24K for extension, doesn't roll over)
- RunPod: commonly used, ephemeral by default, need network volumes for persistence, spot instances for savings
- MATS Cluster: 8x L40 GPUs (single node), Slurm-based, 24hr max walltime, NFS storage at /mnt/nw
- Various API credits available (Anthropic, OpenAI, etc.)

**Use cases (from user):**
- Parallelizing independent tasks
- Running long autonomous tasks in background
- Interactively developing multiple experiments simultaneously
- Analysis or writing while running experiments
- Some fellows need GPUs, some don't - even GPU users can benefit from non-GPU sessions for analysis

**On Claude Code for Web (user's experience):**
- Can configure "environments" with sandbox permissions and runtime dependencies
- Can "teleport" sessions to/from web
- Feels clunky, hasn't had much success
- SSH + continue might be simpler and give more control

**Asynchronous interaction (from user):**
- Slack integration would provide monitoring "for free"
- Could look like GitHub Actions Claude integration: "@claude" on issue/PR kicks off session, last assistant message becomes comment
- Light visibility into running sessions is desired
- Light interaction should be possible (ideally Question tool still works)
- Full monitoring of all tool calls and permission prompts is likely harder

**Voice interaction:**
- User's concept: read assistant messages aloud, let user respond in voice as prompt
- Probably requires system prompting / skill for concise responses so conversation flows
- One fellow already asked for this capability

**Clawdbot (~/libs/clawdbot) findings:**
- Much more sophisticated than expected - full multi-channel assistant framework
- Voice: Whisper for STT, OpenAI TTS for output
- Has wake words, push-to-talk, phone calls via Twilio/Telnyx
- Gateway architecture with WebSocket API
- Could be adapted but it's significant infrastructure

### Claude's Suggestions (Not Yet Validated)

- Options for "attach" experience: SSH+tmux, VS Code Remote, custom web terminal, Slack-native
- Minimal async might just be: start task, get notification when done/blocked

### To Investigate

- Remote equivalent of worktree workflow
- How to pre-seed environments with dependencies and secrets
- Session persistence patterns (tmux vs --continue vs custom)
- Secrets management across instances
- Whether Claude Code for Web is worth recommending for non-GPU users
- What's the minimal viable async interaction?
- Slack bot architecture for session management
- How does Claude Code GitHub integration work in detail?
- Design space for asynchronous Question tool
- Lighter alternatives to clawdbot for "talk while walking"
- What minimal setup achieves the voice use case?
- System prompting patterns for voice-friendly responses

---

## Workstream 3: Jupyter & Stateful Execution

Working with notebooks and long-running computations.

### User Preferences & Decisions

- Must be autonomous (not acceptable to have Claude say "run this cell and tell me output")
- Main reason to use Jupyter is statefulness (keep weights in memory while iterating)
- Open to alternative stateful solutions if they work better for agents
- But Jupyter is nice because user can also see it easily

### What We Discussed

**Problem (from user reports):**
- Fellows using Jupyter have reported poor experience with Claude editing and running notebooks
- One user tried Goodfire MCP server for Jupyter interaction
- Long-running cells (can be hours) blocked the agent - MCP tool couldn't run in background like other tools

**On built-in tools:**
- Claude Code has NotebookEdit tool for editing cells
- User thinks editing might not be the issue
- Unclear if built-in tools handle execution and output viewing

### To Investigate

- What specifically is broken with built-in Jupyter tools?
- How do long-running cells (hours) need to be handled?
- Async/background execution patterns
- Alternative stateful execution environments that might work better

---

## Cross-Cutting Notes

**Best-practices skill**: Can be used to distribute recommendations through Claude Code itself. Permission generator could live here.

**Plugin versioning**: When updating plugin content, must bump version in plugin.json for users to receive updates.

**Research vs web dev**: This is research tooling, not web development. Workflows involve experiments, analysis, iteration.
