#!/bin/bash
set -euo pipefail

# Sets up the web app for local development.
# Run from the repo root: bash scripts/setup-web.sh

WEB_DIR="packages/web"
WEB_ENV="$WEB_DIR/.env.local"
ROOT_ENV=".env.local"

if [ ! -d "$WEB_DIR" ]; then
  echo "Error: Run this from the repo root (packages/web/ not found)"
  exit 1
fi

# Step 1: Convex project setup
if grep -q "CONVEX_DEPLOYMENT" "$WEB_ENV" 2>/dev/null; then
  echo "Convex already configured in $WEB_ENV"
else
  echo "Configuring Convex dev deployment..."
  (cd "$WEB_DIR" && bunx convex dev --once)
  echo ""
fi

if ! grep -q "CONVEX_DEPLOYMENT" "$WEB_ENV" 2>/dev/null; then
  echo "Error: Convex setup did not create $WEB_ENV"
  exit 1
fi

# Step 2: WorkOS secrets
if grep -q "WORKOS_API_KEY" "$WEB_ENV" 2>/dev/null; then
  echo "WorkOS already configured in $WEB_ENV"
else
  echo ""
  echo "The web app needs a staging WorkOS API key for authentication."
  echo "Ask a team member for the staging sk_test_... key."
  echo ""
  read -rp "WORKOS_API_KEY: " api_key
  if [ -z "$api_key" ]; then
    echo "Skipping WorkOS setup (no key provided). Auth won't work until you add WORKOS_API_KEY to $WEB_ENV"
  else
    cookie_pwd=$(openssl rand -base64 32)
    echo "" >> "$WEB_ENV"
    echo "WORKOS_API_KEY=$api_key" >> "$WEB_ENV"
    echo "WORKOS_COOKIE_PASSWORD=$cookie_pwd" >> "$WEB_ENV"
    echo "WorkOS configured."
  fi
fi

# Step 3: Root .env.local for CLI development (ALIGNMENT_HIVE_CONVEX_URL)
convex_url=$(grep "^VITE_CONVEX_URL=" "$WEB_ENV" | cut -d= -f2-)
if [ -n "$convex_url" ]; then
  if grep -q "ALIGNMENT_HIVE_CONVEX_URL=" "$ROOT_ENV" 2>/dev/null; then
    echo "ALIGNMENT_HIVE_CONVEX_URL already in $ROOT_ENV"
  else
    echo "" >> "$ROOT_ENV"
    echo "ALIGNMENT_HIVE_CONVEX_URL=$convex_url" >> "$ROOT_ENV"
    echo "Added ALIGNMENT_HIVE_CONVEX_URL to $ROOT_ENV (for CLI dev)"
  fi
fi

echo ""
echo "Done! Start the dev server with:"
echo "  bun run --filter '@alignment-hive/web' dev"
