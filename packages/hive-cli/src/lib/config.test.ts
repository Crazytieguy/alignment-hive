import { describe, expect, test } from 'bun:test';
import { toClaudeProjectDirName } from './config';

describe('toClaudeProjectDirName', () => {
  test('replaces slashes with dashes', () => {
    expect(toClaudeProjectDirName('/Users/foo/bar')).toBe('-Users-foo-bar');
  });

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
    expect(toClaudeProjectDirName(path)).toBe(
      `-Users-yoav--claude-jobs-a68fc070-tmp-neg2-${'y'.repeat(157)}-4qn7dy`,
    );
  });

  test('long paths sharing a 200-char sanitized prefix get distinct names', () => {
    const prefix = `/${'a'.repeat(220)}`;
    const nameA = toClaudeProjectDirName(`${prefix}/one`);
    const nameB = toClaudeProjectDirName(`${prefix}/two`);
    expect(nameA).not.toBe(nameB);
    expect(nameA.slice(0, 200)).toBe(nameB.slice(0, 200));
  });
});
