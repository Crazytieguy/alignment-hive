#!/bin/bash
# Shared configuration for hive-mind scripts

# WorkOS configuration
WORKOS_CLIENT_ID="${HIVE_MIND_CLIENT_ID:-client_01KE10CYZ10VVZPJVRQBJESK1A}"
WORKOS_API_URL="https://api.workos.com/user_management"

# Auth storage
AUTH_DIR="$HOME/.claude/hive-mind"
AUTH_FILE="$AUTH_DIR/auth.json"
