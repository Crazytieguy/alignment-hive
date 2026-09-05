import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  addTranscriptsDirs,
  getOrCreateCheckoutId,
  loadTranscriptsDirs,
  statePaths,
  toClaudeProjectDirName,
} from '../lib/config';

describe('toClaudeProjectDirName', () => {
  test('replaces all non-alphanumeric characters, not just slashes', () => {
    // Dotted paths like .claude/worktrees/* are the common real-world case.
    expect(toClaudeProjectDirName('/Users/foo/proj/.claude/worktrees/fix-1')).toBe(
      '-Users-foo-proj--claude-worktrees-fix-1',
    );
    expect(toClaudeProjectDirName('/Users/foo/my_repo v2')).toBe('-Users-foo-my-repo-v2');
  });

  test('leaves names at exactly 200 chars untruncated', () => {
    const path = `/${'a'.repeat(199)}`;
    const name = toClaudeProjectDirName(path);
    expect(name).toBe(`-${'a'.repeat(199)}`);
    expect(name.length).toBe(200);
  });

  test('truncates names over 200 chars and appends a hash of the original path', () => {
    // Reference value verified against a real Claude Code 2.1.224 session dir.
    const path =
      '/Users/yoav/.claude/jobs/a68fc070/tmp/longpath-test/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/cccccccccccccccccccccccccccccccccccccccc/dddddddddddddddddddddddddddddddddddddddd/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee';
    expect(toClaudeProjectDirName(path)).toBe(
      '-Users-yoav--claude-jobs-a68fc070-tmp-longpath-test-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-cccccccccccccccccccccccccccccccccccccccc-ddddddddddddddddddddddddd-3bbzwn',
    );
  });

  test('negative hashes take their absolute value (verified against 2.1.224)', () => {
    // Reference value verified against a real Claude Code 2.1.224 session dir;
    // this path's hash is negative before Math.abs.
    const path = `/Users/yoav/.claude/jobs/a68fc070/tmp/neg2/${'y'.repeat(180)}/n0`;
    expect(toClaudeProjectDirName(path)).toBe(`-Users-yoav--claude-jobs-a68fc070-tmp-neg2-${'y'.repeat(157)}-4qn7dy`);
  });
});

describe('state dir files', () => {
  let stateDir: string;
  beforeEach(async () => {
    stateDir = await mkdtemp(join(tmpdir(), 'hive-state-'));
  });
  afterEach(async () => {
    await rm(stateDir, { recursive: true, force: true });
  });

  test('two concurrent addTranscriptsDirs calls for different dirs both survive', async () => {
    await Promise.all([
      addTranscriptsDirs(stateDir, ['/projects/alpha']),
      addTranscriptsDirs(stateDir, ['/projects/beta']),
    ]);

    const dirs = await loadTranscriptsDirs(stateDir);
    expect(dirs.sort()).toEqual(['/projects/alpha', '/projects/beta']);
  });

  test('adding an already-registered dir does not duplicate it', async () => {
    await addTranscriptsDirs(stateDir, ['/projects/alpha']);
    await addTranscriptsDirs(stateDir, ['/projects/alpha', '/projects/beta']);

    expect(await readFile(statePaths(stateDir).transcriptsDirs, 'utf-8')).toBe('/projects/alpha\n/projects/beta\n');
  });

  test('duplicate lines in the file are deduped on load', async () => {
    await writeFile(
      statePaths(stateDir).transcriptsDirs,
      '/projects/alpha\n/projects/beta\n/projects/alpha\n',
      'utf-8',
    );

    expect(await loadTranscriptsDirs(stateDir)).toEqual(['/projects/alpha', '/projects/beta']);
  });

  test('two processes minting a checkout id at once keep the same one', async () => {
    const ids = await Promise.all([getOrCreateCheckoutId(stateDir), getOrCreateCheckoutId(stateDir)]);
    expect(ids[0]).toBe(ids[1]);
    expect(await getOrCreateCheckoutId(stateDir)).toBe(ids[0]);
  });
});
