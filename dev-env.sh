export ALIGNMENT_HIVE_DEV=1
_DIR="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
export PATH="$_DIR/.dev:$PATH"
