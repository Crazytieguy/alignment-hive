import { afterAll, describe, expect, test } from 'bun:test';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';


const hook = fileURLToPath(new URL('./session-start.sh', import.meta.url));
const pluginRoot = fileURLToPath(new URL('..', import.meta.url));
const guidance = readFileSync(fileURLToPath(new URL('./memory-guidance.txt', import.meta.url)), 'utf8').trim();
// Outside a repo: the state dir goes under it, not under a checkout.
const project = mkdtempSync(join(tmpdir(), 'hive-session-start-'));
afterAll(() => rmSync(project, { recursive: true, force: true }));

/** The hook for a resumed session (it stops before the CLI's session start), with the plugin's options as Claude Code passes them. */
function run(options: Record<string, string>): string {
  const env: Record<string, string> = { PATH: process.env.PATH ?? '', HOME: process.env.HOME ?? '', CLAUDE_PLUGIN_ROOT: pluginRoot, CLAUDE_PROJECT_DIR: project };
  for (const [key, value] of Object.entries(options)) env[`CLAUDE_PLUGIN_OPTION_${key.toUpperCase()}`] = value;
  const result = Bun.spawnSync(['bash', hook], { cwd: project, env, stdin: Buffer.from(JSON.stringify({ source: 'resume', session_id: '' })) });
  expect(result.exitCode).toBe(0);
  return result.stdout.toString();
}

describe('hive SessionStart: recall memory guidance', () => {
  test('with the tool on, it holds while the tool is listed', () => {
    for (const out of [run({ recall: 'true' }), run({ recall: 'true', automaticRecall: 'true' })]) {
      expect(out).toStartWith('The user turned on recall (the hive plugin): while the `mcp__hive__recall` tool is in your tool list,');
      expect(out).toContain(` ${guidance} `);
      expect(out).toEndWith('none of this applies.\n');
    }
  });

  test('with automatic recall alone, it says recall runs on each prompt, with no condition', () => {
    const out = run({ automaticRecall: 'true' });
    expect(out).toStartWith('The user turned on automatic recall (the hive plugin): before each prompt the user sends,');
    expect(out).toEndWith(` ${guidance}\n`);
  });

  test('with recall off, nothing', () => {
    expect(run({ recall: 'false' })).toBe('');
    expect(run({})).toBe('');
  });
});
