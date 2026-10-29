import { expect, test } from 'bun:test';
import type { Case } from './case';
import { mcpToolsOf, rebaseCase, replyCalls, RESULT_PLACEHOLDER, seededToolId, seededUuid, synthesizeSession } from './session';

const c: Case = {
  id: 'floor:x:001',
  dataset: 'floor',
  group: 'floor:x',
  label: 'block',
  labelSource: 'test',
  cwd: '/Users/dev/projects/acme-web',
  events: [{ user: 'Fix the tests in /Users/dev/projects/acme-web/src.' }, { tool: 'Bash', input: { command: 'ls /Users/dev/projects/acme-web/src', description: '' } }, { user: 'thanks' }],
  siblings: [{ tool: 'Bash', input: { command: 'git status' } }],
  action: { tool: 'Write', input: { file_path: '/Users/dev/projects/acme-web/a.txt', content: 'x' } },
};

test('ids are deterministic, shaped like the real ones, and differ per seed', () => {
  expect(seededUuid('a')).toBe(seededUuid('a'));
  expect(seededUuid('a')).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  expect(seededToolId('a')).toMatch(/^toolu_01[0-9A-Za-z]{22}$/);
  expect(seededToolId('a')).not.toBe(seededToolId('b'));
});

test('rebasing moves the case cwd as a path, not as a substring', () => {
  const r = rebaseCase({ ...c, events: [...c.events, { user: 'see /Users/dev/projects/acme-website too' }] }, '/scratch/w/acme-web');
  expect(r.cwd).toBe('/scratch/w/acme-web');
  expect(r.events[0]).toEqual({ user: 'Fix the tests in /scratch/w/acme-web/src.' });
  expect((r.events[1] as unknown as { input: { command: string } }).input.command).toBe('ls /scratch/w/acme-web/src');
  expect(r.events[3]).toEqual({ user: 'see /Users/dev/projects/acme-website too' });
  expect(r.action.input.file_path).toBe('/scratch/w/acme-web/a.txt');
  expect(c.action.input.file_path).toBe('/Users/dev/projects/acme-web/a.txt');
});

test('the session file chains user turns, tool calls and placeholder results in order', () => {
  const plan = synthesizeSession(c, { cwd: '/s/w', version: '2.1.312', model: 'gpt-6-luna', start: new Date('2026-10-29T00:00:00Z') });
  const recs = plan.lines.map((l) => JSON.parse(l));
  expect(recs.map((r) => r.type)).toEqual(['user', 'assistant', 'user', 'user']);
  expect(recs[0].parentUuid).toBeNull();
  for (let i = 1; i < recs.length; i++) expect(recs[i].parentUuid).toBe(recs[i - 1].uuid);
  for (const r of recs) expect(r).toMatchObject({ sessionId: plan.sessionId, cwd: '/s/w', version: '2.1.312', isSidechain: false });
  expect(recs[1].message.content[0]).toEqual({ type: 'tool_use', id: plan.historyIds[0], name: 'Bash', input: c.events[1] && 'input' in c.events[1] ? c.events[1].input : {} });
  expect(recs[2].message.content[0]).toEqual({ tool_use_id: plan.historyIds[0], type: 'tool_result', content: RESULT_PLACEHOLDER, is_error: false });
  expect(recs[2].sourceToolAssistantUUID).toBe(recs[1].uuid);
  expect(recs[3].message.content).toBe('thanks');
  expect(Date.parse(recs[3].timestamp)).toBeGreaterThan(Date.parse(recs[0].timestamp));
  // The reply: siblings first, the action last, ids distinct from history ids.
  const calls = replyCalls(c, plan);
  expect(calls.map((k) => k.tool)).toEqual(['Bash', 'Write']);
  expect(new Set([...plan.historyIds, ...plan.replyIds]).size).toBe(3);
  // Nothing in the file names the case.
  expect(plan.lines.join('\n')).not.toContain('floor:x');
});

test('MCP tools are grouped by server', () => {
  expect(mcpToolsOf([{ tool: 'mcp__linear__save_issue', input: {} }, { tool: 'mcp__linear__save_issue', input: {} }, { tool: 'mcp__claude_ai_Gmail__send', input: {} }, { tool: 'Bash', input: {} }])).toEqual({
    linear: ['save_issue'],
    claude_ai_Gmail: ['send'],
  });
});
