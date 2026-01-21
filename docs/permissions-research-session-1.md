# Permissions & Safety Research - Session 1

Research session exploring Claude Code's permission system to develop recommendations for MATS fellows.

## Sandbox Mode Experiments

### Test: Basic sandbox behavior

With sandbox enabled (auto-allow mode):

| Operation | Result |
|-----------|--------|
| Write to project dir | ✓ Works |
| Write to /tmp/claude | ✓ Works |
| Write to ~/ | ✗ "Operation not permitted" |
| curl to external domain | ✓ Works (proxied, prompted for domain approval) |
| git fetch | ✓ Works |
| ps (process list) | ✗ "Operation not permitted" |
| lsof | ✓ Works |
| Python execution | ✓ Works |
| Python HTTP requests | ✓ Works (proxied) |

### Test: uv package manager in sandbox

```bash
uv run --with requests python3 -c "import requests; print('ok')"
```

**Result:** Rust panic in `system-configuration` crate - uv crashes when trying to access macOS system configuration APIs (proxy detection).

**With sandbox disabled:** Works fine.

**Conclusion:** macOS Seatbelt blocks system API access that tools like uv need, with no workaround.

### Test: excludedCommands behavior

Added `touch` to `excludedCommands` in settings:

| Test | Result |
|------|--------|
| touch file in project dir | ✓ Works |
| touch file in ~/ | ✗ "Operation not permitted" |

**Finding:** `excludedCommands` does NOT bypass Seatbelt filesystem restrictions on macOS.

### Test: Edit permission rules in sandbox

Added `Edit(~/**)` to allow rules:

| Test | Result |
|------|--------|
| touch ~/sandbox-bypass-test.txt | ✓ Works |

**Finding:** `Edit` permission rules control what the sandbox allows for filesystem writes, not `excludedCommands`.

### Sandbox conclusion

Giving up on sandbox for now - the macOS Seatbelt restrictions block too many common tools (uv, potentially others), and `excludedCommands` doesn't help.

## Permission Mode Experiments

### Test: acceptEdits in -p mode

```bash
# In current directory
claude -p "Create ./tmp-test-file.txt with 'hello'" --permission-mode acceptEdits
# Result: ✓ "Done. Created ./tmp-test-file.txt"

claude -p "Run 'touch ./bash-test-file.txt' in bash" --permission-mode acceptEdits
# Result: ✓ "Done. Created bash-test-file.txt"

# Outside current directory
claude -p "Create /tmp/test.txt with 'hello'" --permission-mode acceptEdits
# Result: ✗ "I don't have permission to write to /tmp/"

claude -p "Run 'rm /tmp/acceptedits-rm-test.txt' in bash" --permission-mode acceptEdits
# Result: ✗ "Blocked by Claude Code's security policy. File deletion is restricted to current working directory."
```

**Key finding:** acceptEdits has **built-in directory-scoped safety** - operations outside current directory are blocked (not prompted, blocked).

### Test: -p without explicit permission mode

```bash
claude -p "Create ./test.txt with 'hello'"
# Result: ✓ Works (same as acceptEdits)
```

**Finding:** `-p` defaults to acceptEdits behavior.

### Test: dontAsk mode

```bash
claude -p "Create ./dontask-test.txt with 'hello'" --permission-mode dontAsk
# Result: ✗ "Write tool is blocked in dontAsk mode"

claude -p "Run 'touch ./test.txt' in bash" --permission-mode dontAsk
# Result: ✗ "Command was blocked by dontAsk mode"
```

**Finding:** dontAsk auto-denies everything unless explicitly allowed.

### Test: dontAsk with --allowedTools

```bash
claude -p "Create ./test.txt with 'hello'" --permission-mode dontAsk --allowedTools "Write,Edit,Bash"
# Result: ✓ Works

claude -p "Run 'rm /tmp/test.txt' in bash" --permission-mode dontAsk --allowedTools "Bash"
# Result: ✓ Works (deleted file in /tmp!)
```

**Critical finding:** dontAsk + `--allowedTools "Bash"` grants **unrestricted bash access** - no directory scoping.

### Test: dontAsk with specific Bash patterns

```bash
claude -p "Run 'rm /tmp/test.txt' in bash" --permission-mode dontAsk --allowedTools "Bash(echo:*),Bash(ls:*)"
# Result: ✗ "Bash command was auto-denied"
```

**Finding:** Specific patterns work, but there's no pattern for "bash but only in current directory".

### Test: --add-dir behavior

```bash
claude -p "Create ./test.txt with 'hello'" --permission-mode dontAsk --add-dir "."
# Result: ✗ "Write tool is blocked"

claude -p "Run 'rm /tmp/test.txt' in bash" --permission-mode dontAsk --add-dir "." --allowedTools "Bash"
# Result: ✓ Works (still deleted /tmp file)
```

**Finding:** `--add-dir` expands accessible directories for Read/Write, does NOT restrict Bash scope.

### Summary: Directory-scoped safety

| Mode | rm in current dir | rm outside current dir |
|------|-------------------|------------------------|
| acceptEdits | ✓ Allowed | ✗ Blocked |
| dontAsk + Bash | ✓ Allowed | ✓ Allowed (no safety) |
| dontAsk + specific patterns | Depends on pattern | Depends on pattern |

## Key Behavioral Differences

### acceptEdits: Interactive vs -p mode

**In interactive mode**, acceptEdits:
- Auto-allows operations Claude Code can statically verify only affect allowed directories
- Prompts user for anything else

**In -p mode**, acceptEdits auto-denies anything it would normally prompt for (since prompting isn't possible):

| Scenario | Interactive | -p mode |
|----------|-------------|---------|
| Bash command Claude Code can statically verify only affects allowed dirs | ✓ Auto-allowed | ✓ Auto-allowed |
| Bash command in settings allow rules (even if not statically verifiable) | ✓ Auto-allowed | ✓ Auto-allowed |
| Bash command not in allow rules AND not statically verifiable | Prompts | **Auto-denied** |
| File operation in allowed dirs | ✓ Auto-allowed | ✓ Auto-allowed |
| File operation outside allowed dirs | Prompts | **Auto-denied** |

This makes `-p` good for autonomous work - the agent continues instead of blocking, but it's limited to operations that are either pre-allowed in settings or statically verifiable as only affecting allowed directories.

### dontAsk: Simpler model (no static analysis)

dontAsk does NOT have the static analysis that acceptEdits has. It's purely allow/deny based on settings:

| Scenario | dontAsk |
|----------|---------|
| Tool/command in allow rules | ✓ Auto-allowed |
| Tool/command not in allow rules | ✗ Auto-denied |

No directory-scoped safety, no static verification. If `Bash` is allowed, all bash commands are allowed everywhere.

### dontAsk use case

dontAsk may still be useful for **interactive TUI sessions that need to run unattended for long periods** - the session won't block waiting for input, though it will skip operations it doesn't have permission for.

## Options Still on the Table

1. **acceptEdits** - Good default for interactive and `-p` autonomous work (has static analysis for directory safety)
2. **dontAsk** - For interactive sessions that need to run unattended (no static analysis, pure allow/deny)
3. **--dangerously-skip-permissions** - Not ruled out for autonomous non-sensitive research, especially with:
   - Copy-on-write storage (easy rollback)
   - Git protections (block force push, protect main branch)
4. **Careful permission pre-configuration** - For any mode

## Technical Details

### Settings Precedence Issue

When `settings.json` and `settings.local.json` both have `sandbox` config, they may conflict. The `excludedCommands` in one file can be ignored if the other file's sandbox config takes precedence. Keep all sandbox config in one file.

### Complex Command Parsing

Claude Code's permission system can't parse complex piped commands. This:
```bash
script -q /dev/null sh -c "git diff --no-index --color=always -- /dev/null 'src/main.rs' 2>/dev/null | delta --paging=never"
```

Gets saved literally as:
```json
"Bash(script -q /dev/null sh -c \"git diff --no-index...\":*)"
```

This is useless for matching future similar commands.

### Known Bugs (from GitHub issues)

- **#17360**: dontAsk mode can activate unexpectedly (OPEN)
- **#11934**: Sub-agents auto-denied in dontAsk even with bypass flag (OPEN)
- **#17603**: Model doesn't know it's in -p mode (thinks it's interactive)
- **#1967**: Session resumption by ID in print mode can be buggy

## Open Questions for Next Session

### What permissions to actually recommend?

Need to develop specific recommendations for:

1. **settings.json** (committed, shared) - Safe defaults for the project type
2. **settings.local.json** (gitignored) - User/environment-specific permissions

Considerations:
- Sensitivity of the project (most research isn't sensitive, but some is: cbrn uplift, RL generalization)
- Risk appetite of the user
- Type of project (Python ML, web dev, etc.)
- Environment (local dev vs cloud/runpod)

### Common research domains to pre-allow?

For non-sensitive projects, consider pre-allowing:
- pypi.org, files.pythonhosted.org
- huggingface.co
- arxiv.org
- github.com, raw.githubusercontent.com
- wandb.ai

### Script restructuring for better parsing?

Projects could restructure scripts to optimize for Claude Code's bash parser:
- Use simple script names instead of complex inline commands
- Create wrapper scripts for common multi-step operations

## Infrastructure Entanglement (Workstream 2)

These topics overlap with the compute/infrastructure workstream:

1. **Copy-on-write storage on runpod** - Could make permission issues less painful (easy rollback)

2. **Main machine + ephemeral workers pattern**:
   - Main machine (thin, no GPUs) with full git access
   - Ephemeral runpod instances per job/session
   - Each instance gets a branch
   - Main machine controls merging
   - Config/secrets copied on startup

3. **settings.local.json portability** - Need mechanism to sync environment-specific settings to cloud instances

## Session Metadata

- Date: 2026-01-19
- Focus: Workstream 1 (Permissions & Safety) from `docs/claude-code-productivity-workstreams.md`
- Outcome: Research complete, recommendations emerging, need follow-up session for specific permission configs
