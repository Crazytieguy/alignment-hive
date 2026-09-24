import { afterAll, describe, expect, test } from 'bun:test';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const hook = fileURLToPath(new URL('./register-transcript-dir.sh', import.meta.url));
const pluginRoot = fileURLToPath(new URL('..', import.meta.url));
const root = mkdtempSync(join(tmpdir(), 'hive-register-'));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function git(cwd: string, ...args: Array<string>) {
  const result = Bun.spawnSync(['git', '-c', 'user.name=t', '-c', 'user.email=t@t', ...args], { cwd });
  if (result.exitCode !== 0) throw new Error(result.stderr.toString());
}
function run(input: unknown, cwd: string) {
  return Bun.spawnSync(['bash', hook], { cwd, stdin: Buffer.from(JSON.stringify(input)), env: { ...process.env, CLAUDE_PLUGIN_ROOT: pluginRoot } });
}
/** A transcript file in its own project dir, as Claude Code writes it. */
function transcript(name: string): { path: string; dir: string } {
  const dir = join(root, 'projects', name);
  mkdirSync(dir, { recursive: true });
  const path = join(dir, '00000000-0000-4000-8000-000000000001.jsonl');
  writeFileSync(path, '');
  return { path, dir };
}

describe('hive PostToolUse register-transcript-dir', () => {
  const main = join(root, 'repo');
  mkdirSync(main);
  git(main, 'init', '-q');
  git(main, 'commit', '-q', '--allow-empty', '-m', 'init');
  const worktree = join(root, 'wt');
  git(main, 'worktree', 'add', '-q', worktree);
  const registry = join(main, '.claude', 'hive', 'transcripts-dirs');

  test('a session moved into a worktree registers its new transcript dir in the main worktree\'s registry, once', () => {
    const moved = transcript('-wt');
    for (let i = 0; i < 2; i++) expect(run({ transcript_path: moved.path, cwd: worktree }, worktree).exitCode).toBe(0);
    expect(readFileSync(registry, 'utf8')).toBe(`${moved.dir}\n`);
    expect(readFileSync(join(main, '.claude', 'hive', '.gitignore'), 'utf8')).toBe('*\n');
  });

  test('back in the main worktree, that dir is appended to the same registry', () => {
    const back = transcript('-repo');
    expect(run({ transcript_path: back.path, cwd: main }, main).exitCode).toBe(0);
    expect(readFileSync(registry, 'utf8').trim().split('\n').at(-1)).toBe(back.dir);
  });

  for (const input of [{}, { transcript_path: '' }, { transcript_path: join(root, 'missing', 'x.jsonl'), cwd: main }, 'not json']) {
    test(`exits 0 silently on ${JSON.stringify(input)}`, () => {
      const before = existsSync(registry) ? readFileSync(registry, 'utf8') : '';
      const result = run(input, main);
      expect([result.exitCode, result.stdout.toString(), result.stderr.toString()]).toEqual([0, '', '']);
      expect(existsSync(registry) ? readFileSync(registry, 'utf8') : '').toBe(before);
    });
  }
});
