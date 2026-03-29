#!/bin/bash
# Outputs the deno-sandbox additionalContext string.
# Usage: bash sandbox-instructions.sh <script-path> <sandbox-dir>
# Both session-start.sh and subagent-start.sh call this.

SCRIPT_PATH="$1"
SANDBOX_DIR="$2"

cat <<INSTRUCTIONS
## deno-sandbox

\`deno-sandbox\` runs JavaScript/TypeScript in a secure Deno sandbox. Read access to current directory by default, other permissions need to be granted.

### Usage

Your sandbox script file is $SCRIPT_PATH (use this exact path). Write code to it using the Write or Edit tools, then run with \`Bash(deno-sandbox $SCRIPT_PATH)\` or \`Bash(cat data.csv | deno-sandbox $SCRIPT_PATH)\`

\`npm:\` and \`jsr:\` imports work out of the box (packages are fetched from their registries; sandbox permissions still apply to what the code can do): \`import { parse } from "npm:csv-parse/sync";\`

### Granting permissions

Each grant requires user approval. \`--allow-read=.\` is included by default.

\`\`\`
deno-sandbox-grant --allow-write=. --allow-net=api.example.com
\`\`\`

Available: \`--allow-{read,write,run,net,env,import}=<scope>\`. Follow the principle of least privilege — request only the most specific permissions needed. Read and write are separate permissions — granting write to a path does not grant read.

Request any necessary permissions as early as possible. The user is typically available to review grants at the start of a session and will want you to work without interruption afterward.

Generated Deno API types are at \`$SANDBOX_DIR/deno.d.ts\`.
INSTRUCTIONS
