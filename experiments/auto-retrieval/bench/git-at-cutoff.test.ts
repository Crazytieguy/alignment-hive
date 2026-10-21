// The bench git never shows the repository after the cutoff. Runs against the real repository (read-only).
import { expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const SCRIPT = join(import.meta.dir, 'git-at-cutoff.sh');
const CUTOFF = '2026-08-01T00:00:00Z';
const env = { ...process.env, HIVE_LOCAL_CUTOFF: CUTOFF, GIT_AT_CUTOFF_SNAPSHOTS: mkdtempSync(join(tmpdir(), 'git-at-cutoff-')) };
const git = (...args: Array<string>) => spawnSync('sh', [SCRIPT, ...args], { env, encoding: 'utf8' });
const real = (...args: Array<string>) => spawnSync('/usr/bin/git', ['-C', '/Users/yoav/projects/alignment-hive', ...args], { encoding: 'utf8' }).stdout.trim();
const latestDate = (out: string) => out.trim().split('\n').map((l) => Date.parse(l)).filter((t) => !Number.isNaN(t)).reduce((a, b) => Math.max(a, b), 0);

test('flags-only show and log see the snapshot head, not today', () => {
  expect(latestDate(git('show', '--no-patch', '--format=%cI').stdout)).toBeLessThan(Date.parse(CUTOFF));
  expect(latestDate(git('log', '-5', '--format=%cI').stdout)).toBeLessThan(Date.parse(CUTOFF));
  expect(latestDate(git('log', '--all', '-5', '--format=%cI').stdout)).toBeLessThan(Date.parse(CUTOFF));
}, 120_000);

test('diff and grep read the working tree as of the cutoff', () => {
  expect(git('diff', '--stat').stdout.trim()).toBe('');
  const today = real('log', '-1', '--format=%H', 'main');
  expect(git('ls-tree', '-r', '--name-only', 'HEAD').stdout).not.toBe(real('ls-tree', '-r', '--name-only', today) + '\n');
}, 120_000);

test('a commit after the cutoff is refused, alone, in a range, or with a path', () => {
  const future = real('log', '-1', '--format=%H', 'main');
  for (const args of [['show', future], ['log', `${future.slice(0, 10)}..HEAD`], ['log', `HEAD..${future.slice(0, 8)}`], ['show', `${future.slice(0, 12)}:README.md`], ['diff', `HEAD`, `${future}^`]]) {
    const r = git(...args);
    expect(r.status).toBe(1);
    expect(r.stderr).toContain('not in the history');
  }
}, 120_000);

test('commands that write or reach other repositories are refused', () => {
  for (const cmd of ['checkout', 'fetch', 'commit', 'reset', 'config']) expect(git(cmd).status).toBe(1);
});
