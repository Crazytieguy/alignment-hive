// Execution is neutralised twice, because an allowed action does run inside Claude Code:
// 1. CLAUDE_CODE_SHELL is a fake bash that appends its argv to a log and exits 0 printing nothing, so the Bash tool,
//    Monitor and hooks never run a command.
// 2. The whole claude process runs under /usr/bin/sandbox-exec with a profile that denies every file write outside
//    the case's own scratch root (cwd, CLAUDE_CONFIG_DIR, TMPDIR and output dir all live in it) and every network
//    connection except to 127.0.0.1, where the egress gate is the only proxy.
// The sentinels in bench.ts `prove` check both before any case runs.
import { chmodSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const sbString = (s: string): string => `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;

/** The sandbox-exec profile for one case: writes only under `writable` (real paths), network only to localhost. */
export function sandboxProfile(writable: Array<string>): string {
  if (!writable.length || writable.some((p) => !p.startsWith('/') || p === '/' || p.includes('\n'))) throw new Error(`bad writable roots: ${writable.join(', ')}`);
  return [
    '(version 1)',
    '(allow default)',
    '(deny file-write*)',
    `(allow file-write* ${writable.map((p) => `(subpath ${sbString(p)})`).join(' ')})`,
    '(allow file-write* (literal "/dev/null") (literal "/dev/zero") (literal "/dev/tty") (literal "/dev/dtracehelper") (regex #"^/dev/fd/") (regex #"^/dev/ttys[0-9]+$"))',
    '(deny network*)',
    // Outbound only, and only to loopback. A `local ip` allowance would also match an unbound outbound socket.
    '(allow network-outbound (remote ip "localhost:*"))',
    '',
  ].join('\n');
}

/** The fake bash: one line per invocation (`<epoch> <argv as JSON>`) in $FAKE_BASH_LOG, nothing on stdout, exit 0. */
export const FAKE_BASH = `#!/bin/sh
# amc-bench fake shell: records its argv and runs nothing.
if [ -n "$FAKE_BASH_LOG" ]; then
  {
    printf '%s' "$(date +%s)"
    for a in "$@"; do printf ' %s' "$(printf '%s' "$a" | /usr/bin/base64 | tr -d '\\n')"; done
    printf '\\n'
  } >> "$FAKE_BASH_LOG" 2>/dev/null
fi
exit 0
`;

/** Write the fake shell (named `bash`, so Claude Code accepts it) into `dir`; returns its path. */
export function writeFakeShell(dir: string): string {
  mkdirSync(dir, { recursive: true });
  const p = join(dir, 'bash');
  writeFileSync(p, FAKE_BASH);
  chmodSync(p, 0o755);
  return p;
}

/** Decode one fake-bash log line into its argv. */
export function parseFakeBashLine(line: string): { t: number; argv: Array<string> } | undefined {
  const parts = line.trim().split(' ');
  const t = Number(parts[0]);
  if (!Number.isFinite(t)) return undefined;
  return { t, argv: parts.slice(1).map((b) => Buffer.from(b, 'base64').toString('utf8')) };
}

/**
 * A stdio MCP server exposing the given tool names with permissive schemas; every call returns empty text and does
 * nothing. Lets a case whose action is an MCP tool reach Claude Code's classifier (an unknown tool never does).
 */
export const FAKE_MCP = `// amc-bench fake MCP server: lists the tools named in argv[2] (JSON array), answers every call with empty text.
const tools = JSON.parse(process.argv[2] ?? '[]');
let buf = '';
const send = (m) => process.stdout.write(JSON.stringify(m) + '\\n');
process.stdin.on('data', (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf('\\n')) >= 0) {
    const line = buf.slice(0, i).trim();
    buf = buf.slice(i + 1);
    if (!line) continue;
    let m;
    try { m = JSON.parse(line); } catch { continue; }
    if (m.id === undefined) continue;
    if (m.method === 'initialize') send({ jsonrpc: '2.0', id: m.id, result: { protocolVersion: m.params?.protocolVersion ?? '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'amc-fake', version: '0' } } });
    else if (m.method === 'tools/list') send({ jsonrpc: '2.0', id: m.id, result: { tools: tools.map((name) => ({ name, description: name, inputSchema: { type: 'object', additionalProperties: true } })) } });
    else if (m.method === 'tools/call') send({ jsonrpc: '2.0', id: m.id, result: { content: [{ type: 'text', text: '' }] } });
    else send({ jsonrpc: '2.0', id: m.id, result: {} });
  }
});
`;

/** The --mcp-config JSON for a case's MCP tools (server -> tool names), running the fake with `bun`. */
export function mcpConfig(servers: Record<string, Array<string>>, bun: string, script: string): string {
  const mcpServers: Record<string, unknown> = {};
  for (const [name, tools] of Object.entries(servers)) mcpServers[name] = { type: 'stdio', command: bun, args: [script, JSON.stringify(tools)] };
  return JSON.stringify({ mcpServers });
}
