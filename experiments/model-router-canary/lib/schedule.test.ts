import { expect, test } from 'bun:test';
import type { HistoryRow } from './report';
import { isPatch, liveDue, MAX_TRIES, newFindings, nextCandidate } from './schedule';
import type { CandidateState } from './schedule';

const NOW = Date.parse('2026-10-28T12:00:00Z');
const ago = (hours: number): string => new Date(NOW - hours * 3_600_000).toISOString();
const row = (hours: number, versions: HistoryRow['versions'], live?: { full: boolean }): HistoryRow => ({
  at: ago(hours),
  command: 'tick',
  versions,
  attention: [],
  checks: [],
  live,
});

test('live checks are due daily, and at once when Claude Code or the router changes', () => {
  const v = { claudeCode: '2.1.300', router: '0.1.30' };
  expect(liveDue([], v, NOW)).toMatchObject({ due: true, full: true });
  const recent = [row(30, v, { full: true }), row(2, v, { full: false })];
  expect(liveDue(recent, v, NOW)).toMatchObject({ due: false, full: false });
  expect(liveDue(recent, { ...v, claudeCode: '2.1.301' }, NOW)).toMatchObject({ due: true, why: 'Claude Code 2.1.300 -> 2.1.301' });
  expect(liveDue(recent, { ...v, router: '0.1.31' }, NOW).due).toBe(true);
  // A run without the live tier (a candidate-only tick) doesn't count as a live run.
  expect(liveDue([row(25, v, { full: false }), row(1, v)], v, NOW).due).toBe(true);
  // An unknown current version (claude missing from PATH) doesn't force a run.
  expect(liveDue(recent, {}, NOW).due).toBe(false);
});

test('the overflow probes come back weekly', () => {
  const v = { claudeCode: '2.1.300', router: '0.1.30' };
  expect(liveDue([row(8 * 24, v, { full: true }), row(25, v, { full: false })], v, NOW)).toMatchObject({ due: true, full: true });
  expect(liveDue([row(6 * 24, v, { full: true }), row(25, v, { full: false })], v, NOW)).toMatchObject({ due: true, full: false });
});

const releases = [
  { version: '8.0.46', publishedAt: ago(10) },
  { version: '8.0.45', publishedAt: ago(4 * 24) },
  { version: '8.0.44', publishedAt: ago(5 * 24) },
  { version: '8.0.30', publishedAt: ago(60 * 24) },
];
const entry = (verdict: CandidateState[string]['verdict'], hours: number, tries = 1): CandidateState[string] => ({
  verdict,
  reason: '',
  at: ago(hours),
  tries,
  report: '',
});

test('the newest aged release above the pin is the candidate; older ones are passed over', () => {
  expect(nextCandidate(releases, {}, '8.0.34', NOW).version).toBe('8.0.45');
  // 8.0.44 was retried but 8.0.45 aged since: the newer one goes, the older one is abandoned.
  expect(nextCandidate(releases, { '8.0.44': entry('inconclusive', 1) }, '8.0.34', NOW).version).toBe('8.0.45');
  expect(nextCandidate(releases, {}, '8.0.45', NOW).version).toBeUndefined();
  expect(nextCandidate(releases, {}, '8.0.34', NOW, 0).version).toBe('8.0.46');
});

test('a settled candidate is not evaluated again; an unsettled one waits before its retry', () => {
  expect(nextCandidate(releases, { '8.0.45': entry('pass', 100) }, '8.0.34', NOW).version).toBeUndefined();
  expect(nextCandidate(releases, { '8.0.45': entry('fail', 100) }, '8.0.34', NOW).version).toBeUndefined();
  expect(nextCandidate(releases, { '8.0.45': entry('inconclusive', 2) }, '8.0.34', NOW).version).toBeUndefined();
  expect(nextCandidate(releases, { '8.0.45': entry('inconclusive', 7) }, '8.0.34', NOW).version).toBe('8.0.45');
  expect(nextCandidate(releases, { '8.0.45': entry('inconclusive', 7, MAX_TRIES) }, '8.0.34', NOW).version).toBeUndefined();
  // Skips don't use up tries, so a login about to expire just waits.
  expect(nextCandidate(releases, { '8.0.45': entry('skipped', 7, 0) }, '8.0.34', NOW).version).toBe('8.0.45');
});

test('patch releases share major and minor with the pin', () => {
  expect(isPatch('8.0.34', '8.0.46')).toBe(true);
  expect(isPatch('8.0.34', '8.1.0')).toBe(false);
  expect(isPatch('8.0.34', '9.0.0')).toBe(false);
});

test('a finding starts a session once a day', () => {
  const f = [
    { key: 'live:reach/gpt-6-luna', text: 'live checks failed: reach/gpt-6-luna' },
    { key: 'candidate:8.0.45:pass', text: 'candidate 8.0.45: pass' },
  ];
  expect(newFindings(f, {}, NOW)).toEqual(f);
  expect(newFindings(f, { 'live:reach/gpt-6-luna': ago(3) }, NOW)).toEqual([f[1]!]);
  expect(newFindings(f, { 'live:reach/gpt-6-luna': ago(25), 'candidate:8.0.45:pass': ago(1) }, NOW)).toEqual([f[0]!]);
});
