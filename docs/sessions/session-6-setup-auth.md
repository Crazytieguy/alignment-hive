# Session 6: First-Time Setup & Multi-Environment

## Overview

Designed and implemented the authentication flow for hive-mind, including WorkOS device flow, token refresh, and SessionStart hook.

## Key Decisions

### Authentication Flow

**Approach**: Bundled bash script using WorkOS device authorization flow.

Why bash script over other options:
- No Node.js dependency (works on cloud VMs without it)
- Works on mac/linux/wsl with just `curl`, `jq`, and `bash`
- Bundled with plugin, so version always matches
- No external distribution needed

**Flow**:
1. User runs `scripts/login.sh`
2. Script calls WorkOS `/authorize/device` → gets `user_code` + `verification_uri`
3. Opens browser (or prints URL for headless environments)
4. User confirms code matches in browser
5. Script polls `/authenticate` every 5 seconds
6. On success: stores tokens in `~/.claude/hive-mind/auth.json`

### Token Management

- **Access tokens**: 5-minute expiry (WorkOS default)
- **Refresh tokens**: Used for silent refresh in SessionStart hook
- **Storage**: Plain JSON file with `chmod 600` (standard CLI pattern, same as AWS/GitHub CLIs)

The SessionStart hook automatically refreshes expired tokens before displaying status, so users rarely need to re-login.

### Multi-Environment Handling

Each machine is independent:
- Auth token is per-machine (`~/.claude/hive-mind/auth.json`)
- Raw transcripts are per-machine (Claude Code's `~/.claude/projects/`)
- Extracted sessions are per-project (`.claude/hive-mind/`)

User works on same project from laptop and cloud VM:
- Each machine logs in separately (same WorkOS identity)
- Each machine extracts its own sessions from its own transcripts
- Convex deduplicates by `session_id` if needed

### First-Run Experience

SessionStart hook checks auth and shows helpful message:
```
hive-mind:
To contribute sessions, run:
  /path/to/scripts/login.sh
```

Also checks for missing dependencies (jq, curl) with platform-specific install instructions.

### No Slash Commands for Login

Decided against `/hive-login` slash command because:
- Claude doesn't need to be in the loop for authentication
- Slash commands always prompt Claude, which is awkward
- Direct script execution is cleaner

## Implementation

Created `plugins/hive-mind/` with:

```
plugins/hive-mind/
├── .claude-plugin/
│   └── plugin.json
├── hooks/
│   └── hooks.json
└── scripts/
    ├── config.sh          # Shared configuration
    ├── login.sh           # Device flow authentication
    └── session-start.sh   # SessionStart hook
```

Features:
- WorkOS device flow with automatic browser opening
- Silent token refresh in SessionStart hook
- Platform-specific dependency instructions
- Combines multiple issues in one message (missing deps + need login)
- Shared config to avoid duplication

## WorkOS Setup

**Using staging environment** - production credentials to be configured later.

Setup completed:
1. Created WorkOS account with hosted AuthKit
2. Default client ID works for device flow: `client_01KE10CYZ10VVZPJVRQBJESK1A`
3. API key saved separately (needed for Convex backend later)
4. Redirect URI placeholder set for future admin web app

Note: Initially created an OAuth application for web auth, then deleted it (not needed for CLI auth).

## Testing

Verified:
- Login flow works end-to-end
- Token refresh works when access token expires
- Hook output is valid JSON
- Plugin loads correctly with `claude --plugin-dir`
- SessionStart hook fires and displays status

## Open Items for Future Sessions

- Session extraction and sanitization (Session 8)
- Convex backend setup with WorkOS API key
- Production WorkOS credentials when ready to launch
