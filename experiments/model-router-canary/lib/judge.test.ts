import { expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { changedGoFiles, judgePrompt, parseJudge } from './judge';

function tree(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), 'judge-'));
  for (const [p, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, p)), { recursive: true });
    writeFileSync(join(root, p), text);
  }
  return root;
}

test('changed files count code lines, not comments or formatting', () => {
  const a = tree({ 'x.go': 'package x\nfunc A() int {\n\treturn 1\n}\n', 'same.go': 'package y // old comment\n', 'gone.go': 'package z\n' });
  const b = tree({ 'x.go': 'package x\n// new comment\nfunc A() int {\n    return 2\n}\n', 'same.go': 'package y // new comment\n', 'new.go': 'package w\nvar v = 1\n' });
  try {
    expect(changedGoFiles(a, b)).toEqual([
      { path: 'new.go', added: 2, removed: 0 },
      { path: 'x.go', added: 1, removed: 1 },
      { path: 'gone.go', added: 0, removed: 1 },
    ]);
  } finally {
    rmSync(a, { recursive: true, force: true });
    rmSync(b, { recursive: true, force: true });
  }
});

test('the prompt carries the notes of every release after the pin up to the candidate, oldest first', () => {
  const p = judgePrompt({
    pinned: '8.0.34',
    candidate: '8.0.36',
    releases: [
      { version: '8.0.37', publishedAt: '2026-10-20T00:00:00Z', notes: 'too new' },
      { version: '8.0.36', publishedAt: '2026-10-19T00:00:00Z', notes: 'fix codex stream' },
      { version: '8.0.35', publishedAt: '2026-10-18T00:00:00Z' },
      { version: '8.0.34', publishedAt: '2026-10-17T00:00:00Z', notes: 'PINNED-NOTES' },
    ],
    files: [{ path: 'internal/runtime/executor/codex_executor.go', added: 12, removed: 3 }],
    checks: [{ tier: 'candidate', target: 'candidate', name: 'tool-replay', status: 'pass', detail: 'ok' }],
  });
  expect(p.indexOf('### 8.0.35')).toBeLessThan(p.indexOf('### 8.0.36'));
  expect(p).toContain('(no notes)');
  // Later releases come after the candidate's own section, as evidence about it.
  expect(p.indexOf('too new')).toBeGreaterThan(p.indexOf('Releases after the candidate'));
  expect(p.indexOf('fix codex stream')).toBeLessThan(p.indexOf('Releases after the candidate'));
  expect(p).not.toContain('PINNED-NOTES');
  expect(p).toContain('codex_executor.go +12 -3');
  expect(p).toContain('pass candidate/candidate tool-replay: ok');
});

test('a release list that stops short of the pin says so', () => {
  const p = judgePrompt({
    pinned: '8.0.10',
    candidate: '8.0.12',
    releases: [{ version: '8.0.12', publishedAt: '2026-10-19T00:00:00Z', notes: 'x' }],
    files: [],
    checks: [],
  });
  expect(p).toContain("[the notes of releases before 8.0.12 weren't fetched]");
});

test('the decision comes from the structured output only', () => {
  expect(parseJudge(JSON.stringify({ result: 'x', structured_output: { decision: 'bench', reason: 'executor changed' } }))).toEqual({ decision: 'bench', reason: 'executor changed' });
  expect(parseJudge(JSON.stringify({ result: '{"decision":"repin"}' }))).toBeUndefined();
  expect(parseJudge(JSON.stringify({ structured_output: { decision: 'ship', reason: '' } }))).toBeUndefined();
  expect(parseJudge('not json')).toBeUndefined();
});
