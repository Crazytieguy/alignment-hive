import { afterAll, beforeEach, describe, expect, test } from 'bun:test';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

// The deno sandbox's per-session state: sandbox-permissions.sh after a session-id fork (#83) and
// the SessionStart prune (#85).
const pluginRoot = fileURLToPath(new URL('..', import.meta.url));
const root = mkdtempSync(join(tmpdir(), 'autopilot-state-'));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const project = join(root, 'project');
const sandbox = join(project, '.claude', 'deno-sandbox');
const sessions = join(root, 'data', 'sessions');

const OLD = '11111111-1111-4111-8111-111111111111';
const NEW = '22222222-2222-4222-8222-222222222222';
const AGENT = 'a0123456789abcdef';

function run(hook: string, input: unknown) {
  const result = Bun.spawnSync(['bash', join(pluginRoot, 'hooks', hook)], {
    stdin: Buffer.from(JSON.stringify(input)),
    env: { ...process.env, CLAUDE_PLUGIN_ROOT: pluginRoot, CLAUDE_PROJECT_DIR: project, CLAUDE_PLUGIN_DATA: join(root, 'data'), AUTOPILOT_DATA_DIR: '' },
  });
  expect(result.exitCode).toBe(0);
  const out = result.stdout.toString();
  return out ? JSON.parse(out).hookSpecificOutput.decision : undefined;
}
function file(path: string, daysOld = 0) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, '');
  const t = Date.now() / 1000 - daysOld * 86400;
  utimesSync(path, t, t);
}

beforeEach(() => {
  rmSync(join(root, 'data'), { recursive: true, force: true });
  rmSync(project, { recursive: true, force: true });
  file(join(project, '.claude', 'autopilot', 'state.json'));
  writeFileSync(join(project, '.claude', 'autopilot', 'state.json'), '{"deno_sandbox": true}');
  mkdirSync(sandbox, { recursive: true });
});

describe('sandbox-permissions after a session-id fork', () => {
  const write = (path: string, extra: object = {}) =>
    run('sandbox-permissions.sh', { tool_name: 'Write', session_id: NEW, tool_input: { file_path: path }, ...extra });

  test('a write to an unknown script is denied with the live script path', () => {
    expect(write(join(sandbox, `${OLD}.ts`))).toEqual({
      behavior: 'deny',
      message: `Write to your assigned sandbox script file instead: ${sandbox}/${NEW}.ts`,
    });
  });

  test('inside a subagent the deny names the agent\'s script', () => {
    expect(write(join(sandbox, 'other.ts'), { agent_id: AGENT }).message).toEndWith(`${sandbox}/${AGENT}.ts`);
  });

  test('the old id\'s script is allowed while it has grants or an agent registry', () => {
    file(join(sessions, `${OLD}.agents`));
    expect(write(join(sandbox, `${OLD}.ts`))).toEqual({ behavior: 'allow' });
    rmSync(join(sessions, `${OLD}.agents`));
    file(join(sessions, OLD));
    expect(write(join(sandbox, `${OLD}.ts`))).toEqual({ behavior: 'allow' });
  });

  test('a subagent may write its own script even when registered under the old id', () => {
    expect(write(join(sandbox, `${AGENT}.ts`), { agent_id: AGENT })).toEqual({ behavior: 'allow' });
  });

  test('declaration files are neither allowed nor denied', () => {
    file(join(sessions, 'globals'));
    expect(write(join(sandbox, 'globals.d.ts'))).toBeUndefined();
  });
});

describe('prune-sessions', () => {
  test('deletes state untouched for 30 days, keeping the current session, fresh files and non-session files', () => {
    for (const name of [OLD, `${OLD}.agents`, NEW, `${NEW}.agents`]) file(join(sessions, name), 40);
    file(join(sessions, 'fresh'), 5);
    for (const name of [`${OLD}.ts`, `${AGENT}.ts`, `${NEW}.ts`, 'tsconfig.json', 'globals.d.ts', 'deno.d.ts', '.gitignore', 'my-tool.ts'])
      file(join(sandbox, name), 40);
    file(join(sandbox, 'b0123456789abcdef.ts'), 5);

    run('prune-sessions.sh', { session_id: NEW, source: 'resume' });

    expect(readdirSync(sessions).sort()).toEqual([NEW, `${NEW}.agents`, 'fresh'].sort());
    expect(readdirSync(sandbox).sort()).toEqual(
      [`${NEW}.ts`, '.gitignore', 'b0123456789abcdef.ts', 'deno.d.ts', 'globals.d.ts', 'my-tool.ts', 'tsconfig.json'].sort(),
    );
  });

  test('prunes nothing without a session id, and is fine with no state at all', () => {
    file(join(sessions, OLD), 40);
    run('prune-sessions.sh', {});
    expect(existsSync(join(sessions, OLD))).toBe(true);
    rmSync(join(root, 'data'), { recursive: true, force: true });
    rmSync(sandbox, { recursive: true, force: true });
    run('prune-sessions.sh', { session_id: NEW });
  });
});
