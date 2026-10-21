import { expect, test } from 'bun:test';
import type { Claim } from './claims';
import { carries, conveyed } from './credit';
import type { PoolItem } from './pool4';

const entry =
  'Please replace gpt-6 with astra in the router. Also, the reviewer should run at medium effort, not high. Separately: put every copy change in the artifact as a diff.';

const item = (id: string, systems: Record<string, string>): PoolItem => ({
  id,
  loc: `aaaaaaaa:${id.slice(1)}`,
  exists: true,
  current_session: false,
  future: false,
  entry,
  entry_chars: entry.length,
  systems: Object.fromEntries(Object.entries(systems).map(([k, v]) => [k, { text: v }])),
});

const claims: Array<Claim> = [
  { id: 'c1', statement: 'Router: replace gpt-6 with astra.', spans: [{ item: 'i01', text: 'Please replace gpt-6 with astra in the router.' }] },
  { id: 'c2', statement: 'Reviewer at medium effort.', spans: [{ item: 'i01', text: 'the reviewer should run at medium effort, not high.' }] },
  { id: 'c3', statement: 'Copy changes as diffs.', spans: [{ item: 'i01', text: 'put every copy change in the artifact as a diff.' }, { item: 'i02', text: 'copy changes go in as diffs' }] },
];

test('a quote of one sentence of a multi-claim entry conveys only that claim', () => {
  const pool = [item('i01', { v: 'Separately: put every copy change in the artifact as a diff.' })];
  expect([...conveyed('v', pool, claims)]).toEqual(['c3']);
});

test('the whole entry delivered conveys every claim in it', () => {
  expect(conveyed('v', [item('i01', { v: entry })], claims).size).toBe(3);
});

test('a claim is conveyed through any of its entries, and not through an entry the run did not cite', () => {
  expect([...conveyed('v', [item('i01', {}), item('i02', { v: 'copy changes go in as diffs, he said' })], claims)]).toEqual(['c3']);
  expect(conveyed('w', [item('i01', { v: entry })], claims).size).toBe(0);
});

test('an excerpt cut short or reformatted still carries a span it shares most of', () => {
  expect(carries('… the **reviewer** should run at medium effort …', 'the reviewer should run at medium effort, not high.')).toBe(true);
  expect(carries('the reviewer should run at medium effort', 'the reviewer should run at medium effort, not high.')).toBe(true);
  expect(carries('“Please replace gpt-6 with astra”', 'Please replace gpt-6 with astra in the router.')).toBe(true);
});

test('a quote cut with an ellipsis is credited by the pieces it kept', () => {
  const span =
    'you should delegate important copy-writing to a fresh fable agent and probably stop using the "plan gemini support" agent since it\'s already at over 600k tokens and i don\'t trust opus with copy';
  expect(carries("also you should delegate important copy-writing to a fresh fable agent … i don't trust opus with copy", span)).toBe(true);
});

test('a short shared phrase is not enough for a long span', () => {
  expect(carries('the router', 'Please replace gpt-6 with astra in the router and keep the old route as a fallback for a week.')).toBe(false);
});
