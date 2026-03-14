# Dev environment setup. Source this before running bash scripts (install.sh).
# Other ALIGNMENT_HIVE_* vars are in .env (loaded by the CLI's loadEnvFiles).
export ALIGNMENT_HIVE_DEV=1
export ALIGNMENT_HIVE_CLIENT_ID=client_01KE10CYZ10VVZPJVRQBJESK1A
export ALIGNMENT_HIVE_AUTH_FILE=~/.alignment-hive/auth-dev.json
_DIR="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
export PATH="$_DIR/.dev:$PATH"
