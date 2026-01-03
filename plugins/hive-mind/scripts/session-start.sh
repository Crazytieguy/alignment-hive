#!/bin/bash
set -euo pipefail

# hive-mind SessionStart hook
# Checks authentication status and displays session info

# Load shared configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/config.sh"

# Plugin paths
PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-$(dirname "$SCRIPT_DIR")}"
LOGIN_SCRIPT="$PLUGIN_ROOT/scripts/login.sh"

# Output message for the user (not Claude)
# Format: {"systemMessage": "..."} where message appears to user
output_message() {
    local message="$1"
    echo "{\"systemMessage\": \"$message\"}"
}

# Try to silently refresh an expired token
# Returns 0 on success, 1 on failure
try_silent_refresh() {
    local refresh_token
    refresh_token=$(jq -r '.refresh_token // empty' "$AUTH_FILE" 2>/dev/null || echo "")

    if [ -z "$refresh_token" ]; then
        return 1
    fi

    local response
    response=$(curl -s -X POST "$WORKOS_API_URL/authenticate" \
        -H "Content-Type: application/x-www-form-urlencoded" \
        -d "grant_type=refresh_token" \
        -d "refresh_token=$refresh_token" \
        -d "client_id=$WORKOS_CLIENT_ID" 2>/dev/null || echo '{"error": "network_error"}')

    local error
    error=$(echo "$response" | jq -r '.error // empty')

    if [ -z "$error" ]; then
        # Success - save new tokens
        echo "$response" > "$AUTH_FILE"
        chmod 600 "$AUTH_FILE"
        return 0
    fi

    return 1
}

# Build issues list
issues=""

# Check for missing dependencies
missing_deps=()
if ! command -v jq &> /dev/null; then
    missing_deps+=("jq")
fi
if ! command -v curl &> /dev/null; then
    missing_deps+=("curl")
fi

if [ ${#missing_deps[@]} -gt 0 ]; then
    deps_str="${missing_deps[*]}"
    if [[ "$OSTYPE" == "darwin"* ]]; then
        install_cmd="brew install $deps_str"
    elif command -v apt-get &> /dev/null; then
        install_cmd="sudo apt-get install $deps_str"
    elif command -v dnf &> /dev/null; then
        install_cmd="sudo dnf install $deps_str"
    else
        install_cmd="Install: $deps_str"
    fi
    issues="Missing dependencies. Run:\\n  $install_cmd"
fi

# Check if authenticated (only if we have jq)
needs_login=false
if command -v jq &> /dev/null; then
    if [ ! -f "$AUTH_FILE" ]; then
        needs_login=true
    else
        # Check if token exists and is valid
        access_token=$(jq -r '.access_token // empty' "$AUTH_FILE" 2>/dev/null || echo "")

        if [ -z "$access_token" ]; then
            needs_login=true
        else
            # Decode JWT to check expiry
            payload_base64=$(echo "$access_token" | cut -d'.' -f2)

            # Add padding if needed for base64 decode
            padding=$((4 - ${#payload_base64} % 4))
            if [ "$padding" -lt 4 ]; then
                payload_base64="${payload_base64}$(printf '%*s' "$padding" | tr ' ' '=')"
            fi

            payload=$(echo "$payload_base64" | base64 -d 2>/dev/null || echo "{}")
            exp=$(echo "$payload" | jq -r '.exp // 0' 2>/dev/null || echo "0")
            now=$(date +%s)

            if [ "$exp" -le "$now" ]; then
                # Token expired - try silent refresh
                if command -v curl &> /dev/null && try_silent_refresh; then
                    # Refresh succeeded, re-read the token
                    access_token=$(jq -r '.access_token // empty' "$AUTH_FILE" 2>/dev/null || echo "")
                else
                    needs_login=true
                fi
            fi
        fi
    fi
else
    # Can't check auth without jq, assume needs login
    needs_login=true
fi

# Add login issue if needed
if [ "$needs_login" = true ]; then
    login_msg="To contribute sessions, run:\\n  $LOGIN_SCRIPT"
    if [ -n "$issues" ]; then
        issues="$issues\\n\\n$login_msg"
    else
        issues="$login_msg"
    fi
fi

# Output issues if any
if [ -n "$issues" ]; then
    output_message "hive-mind:\\n$issues"
    exit 0
fi

# User is authenticated - show status
user_email=$(jq -r '.user.email // "unknown"' "$AUTH_FILE" 2>/dev/null || echo "unknown")
user_name=$(jq -r '.user.first_name // ""' "$AUTH_FILE" 2>/dev/null || echo "")

if [ -n "$user_name" ]; then
    identity="$user_name"
else
    identity="$user_email"
fi

# TODO: Count pending sessions from .claude/hive-mind/state.json
# For now, just show logged-in status
output_message "hive-mind: Logged in as $identity"
