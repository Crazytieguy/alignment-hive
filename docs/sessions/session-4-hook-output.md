# Session 4: Hook Output Testing

**Date**: 2026-01-02

## Goal

Determine the best way to display information to users from hooks without polluting Claude's context.

## Tests Conducted

Created test hooks for SessionStart with different output methods.

### Test 1: Plain stdout

```bash
echo "Plain text message"
```

**Result**: Output goes to model context only, NOT displayed to user.

### Test 2: JSON with systemMessage

```bash
echo '{"systemMessage": "Message with\nnewlines"}'
```

**Result**:
- User sees: `SessionStart:startup says: Message with` (newlines rendered)
- Claude sees: `SessionStart:startup hook success: Success`

Newlines (`\n`) render correctly. First line gets `SessionStart:startup says:` prepended.

### Test 3: JSON with suppressOutput

```bash
echo '{"suppressOutput": true}'
```

**Result**: Nothing shown to user, Claude sees only "hook success".

### Test 4: Slash command with disable-model-invocation

```yaml
---
disable-model-invocation: true
---
```

**Result**: Command content displayed, model still responds. This frontmatter prevents the model from invoking the command autonomously - it doesn't prevent model response when user invokes the command.

## Findings

| Method | User Sees | Claude Sees |
|--------|-----------|-------------|
| Plain stdout | Nothing | Full output |
| `systemMessage` JSON | Message (formatted) | "hook success" |
| `suppressOutput` JSON | Nothing | "hook success" |

## Decision

Use `{"systemMessage": "..."}` for user-facing hook notifications. Design messages knowing the first line gets `SessionStart:startup says:` prepended.

## Test Artifacts

Test hooks and configs created in `test-hooks/` directory. Can be deleted after session.
