#!/bin/bash
set -e

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Installing git hooks..."

cat > "$REPO_ROOT/.git/hooks/pre-commit" << 'EOF'
#!/bin/bash
set -e

if git diff --cached --name-only | grep -q "^hive-mind-cli/src/"; then
    echo "hive-mind-cli source changed, rebuilding..."
    cd "$(git rev-parse --show-toplevel)/hive-mind-cli"
    bun run build
    if ! git diff --quiet ../plugins/hive-mind/cli.js 2>/dev/null; then
        echo "Staging updated cli.js..."
        git add ../plugins/hive-mind/cli.js
    fi
fi
EOF

chmod +x "$REPO_ROOT/.git/hooks/pre-commit"
echo "Done."
