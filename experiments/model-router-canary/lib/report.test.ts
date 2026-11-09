import { expect, test } from 'bun:test';
import { Recorder, renderMarkdown, type RunReport } from './report';

test('failures filter by tier, and the report lists every check', () => {
  const rec = new Recorder();
  const origError = console.error;
  console.error = () => {};
  try {
    rec.add({ tier: 'candidate', target: 'candidate', name: 'tool-replay', status: 'fail', detail: 'x' });
    rec.add({ tier: 'live', target: 'live', name: 'reach/gpt-6-luna', status: 'pass', detail: 'ok | fine' });
  } finally {
    console.error = origError;
  }
  expect(rec.failures('candidate').map((c) => c.name)).toEqual(['tool-replay']);
  expect(rec.failures('live')).toEqual([]);
  const report: RunReport = {
    startedAt: '2026-11-01T00:00:00Z',
    dir: '/tmp/x',
    command: 'candidate',
    versions: { pinnedCpa: '8.0.44', candidateCpa: '8.0.48' },
    checks: rec.checks,
    attention: [],
  };
  const md = renderMarkdown(report);
  expect(md).toContain('| candidate | candidate | tool-replay | x |');
  expect(md).toContain('ok \\| fine');
});
