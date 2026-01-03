#!/bin/bash
set -euo pipefail

# hive-mind login script
# Implements WorkOS device authorization flow

# Load shared configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/config.sh"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_error() {
    echo -e "${RED}Error:${NC} $1" >&2
}

print_success() {
    echo -e "${GREEN}$1${NC}"
}

print_info() {
    echo -e "${BLUE}$1${NC}"
}

print_warning() {
    echo -e "${YELLOW}$1${NC}"
}

# Check for required dependencies
check_dependencies() {
    local missing=()

    if ! command -v curl &> /dev/null; then
        missing+=("curl")
    fi

    if ! command -v jq &> /dev/null; then
        missing+=("jq")
    fi

    if [ ${#missing[@]} -gt 0 ]; then
        print_error "Missing required dependencies: ${missing[*]}"
        echo ""
        echo "Please install them first:"
        if [[ "$OSTYPE" == "darwin"* ]]; then
            echo "  brew install ${missing[*]}"
        elif command -v apt-get &> /dev/null; then
            echo "  sudo apt-get install ${missing[*]}"
        elif command -v dnf &> /dev/null; then
            echo "  sudo dnf install ${missing[*]}"
        else
            echo "  Use your package manager to install: ${missing[*]}"
        fi
        exit 1
    fi
}

# Check if already authenticated
check_existing_auth() {
    if [ -f "$AUTH_FILE" ]; then
        # Check if token exists and is not expired
        local access_token
        access_token=$(jq -r '.access_token // empty' "$AUTH_FILE" 2>/dev/null || echo "")

        if [ -n "$access_token" ]; then
            # Decode JWT to check expiry (simple base64 decode of payload)
            local payload
            payload=$(echo "$access_token" | cut -d'.' -f2 | base64 -d 2>/dev/null || echo "{}")
            local exp
            exp=$(echo "$payload" | jq -r '.exp // 0' 2>/dev/null || echo "0")
            local now
            now=$(date +%s)

            if [ "$exp" -gt "$now" ]; then
                print_warning "You're already logged in."
                echo ""
                read -p "Do you want to log in again? [y/N] " -n 1 -r
                echo
                if [[ ! $REPLY =~ ^[Yy]$ ]]; then
                    exit 0
                fi
            fi
        fi
    fi
}

# Try to refresh existing token
try_refresh_token() {
    if [ ! -f "$AUTH_FILE" ]; then
        return 1
    fi

    local refresh_token
    refresh_token=$(jq -r '.refresh_token // empty' "$AUTH_FILE" 2>/dev/null || echo "")

    if [ -z "$refresh_token" ]; then
        return 1
    fi

    print_info "Attempting to refresh existing session..."

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
        print_success "Session refreshed successfully!"
        return 0
    fi

    return 1
}

# Open URL in browser (best effort)
open_browser() {
    local url="$1"

    if [[ "$OSTYPE" == "darwin"* ]]; then
        open "$url" 2>/dev/null && return 0
    elif command -v xdg-open &> /dev/null; then
        xdg-open "$url" 2>/dev/null && return 0
    elif command -v wslview &> /dev/null; then
        wslview "$url" 2>/dev/null && return 0
    fi

    return 1
}

# Main device authorization flow
device_auth_flow() {
    print_info "Starting hive-mind authentication..."
    echo ""

    # Step 1: Request device code
    local response
    response=$(curl -s -X POST "$WORKOS_API_URL/authorize/device" \
        -H "Content-Type: application/x-www-form-urlencoded" \
        -d "client_id=$WORKOS_CLIENT_ID")

    local error
    error=$(echo "$response" | jq -r '.error // empty')

    if [ -n "$error" ]; then
        print_error "Failed to start authentication: $error"
        echo "$response" | jq -r '.error_description // empty'
        exit 1
    fi

    local device_code user_code verification_uri verification_uri_complete interval expires_in
    device_code=$(echo "$response" | jq -r '.device_code')
    user_code=$(echo "$response" | jq -r '.user_code')
    verification_uri=$(echo "$response" | jq -r '.verification_uri')
    verification_uri_complete=$(echo "$response" | jq -r '.verification_uri_complete')
    interval=$(echo "$response" | jq -r '.interval // 5')
    expires_in=$(echo "$response" | jq -r '.expires_in // 300')

    # Step 2: Display instructions to user
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "  To authenticate, visit this URL in your browser:"
    echo ""
    echo "    $verification_uri"
    echo ""
    echo "  Confirm this code matches:"
    echo ""
    echo -e "    ${GREEN}${user_code}${NC}"
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    # Try to open browser
    if open_browser "$verification_uri_complete"; then
        print_info "Browser opened. Confirm the code matches and approve."
    else
        print_info "Open the URL in your browser, then confirm the code."
    fi
    echo ""
    print_info "Waiting for authentication... (expires in ${expires_in}s)"

    # Step 3: Poll for token
    local start_time
    start_time=$(date +%s)

    while true; do
        sleep "$interval"

        local elapsed
        elapsed=$(($(date +%s) - start_time))

        if [ "$elapsed" -ge "$expires_in" ]; then
            print_error "Authentication timed out. Please try again."
            exit 1
        fi

        local token_response
        token_response=$(curl -s -X POST "$WORKOS_API_URL/authenticate" \
            -H "Content-Type: application/x-www-form-urlencoded" \
            -d "grant_type=urn:ietf:params:oauth:grant-type:device_code" \
            -d "device_code=$device_code" \
            -d "client_id=$WORKOS_CLIENT_ID")

        error=$(echo "$token_response" | jq -r '.error // empty')

        if [ -z "$error" ]; then
            # Success!
            mkdir -p "$AUTH_DIR"
            echo "$token_response" > "$AUTH_FILE"
            chmod 600 "$AUTH_FILE"

            echo ""
            print_success "Authentication successful!"
            echo ""

            # Show user info
            local user_email user_name
            user_email=$(echo "$token_response" | jq -r '.user.email // "unknown"')
            user_name=$(echo "$token_response" | jq -r '.user.first_name // ""')

            if [ -n "$user_name" ]; then
                echo "Welcome, $user_name ($user_email)!"
            else
                echo "Logged in as: $user_email"
            fi
            echo ""
            echo "Your Claude Code sessions will now contribute to the hive-mind."
            echo "You'll have 24 hours to review and exclude sessions before they're submitted."

            exit 0

        elif [ "$error" == "authorization_pending" ]; then
            # Still waiting, show progress
            printf "\r  Waiting... (%ds elapsed)" "$elapsed"
            continue

        elif [ "$error" == "slow_down" ]; then
            # Increase interval
            interval=$((interval + 1))
            continue

        else
            # Terminal error
            echo ""
            print_error "Authentication failed: $error"
            echo "$token_response" | jq -r '.error_description // empty'
            exit 1
        fi
    done
}

# Main entry point
main() {
    echo ""
    echo "  hive-mind login"
    echo "  ───────────────"
    echo ""

    check_dependencies
    check_existing_auth

    # Try refresh first if we have a token
    if try_refresh_token; then
        exit 0
    fi

    # Full device auth flow
    device_auth_flow
}

main "$@"
