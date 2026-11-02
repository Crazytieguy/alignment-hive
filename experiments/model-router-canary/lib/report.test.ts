import { expect, test } from 'bun:test';
import { Recorder, TOOL_ADDITION_DIRECT, hoistSignal, renderMarkdown, type Check, type RunReport } from './report';

const direct = (target: string, status: Check['status']): Check => ({ tier: 'candidate', target, name: TOOL_ADDITION_DIRECT, status, detail: status, info: true });

test('the hoist signal needs a candidate pass that the pinned child lacks', () => {
  expect(hoistSignal([direct('pinned', 'warn'), direct('candidate', 'pass')])).toContain('#122');
  expect(hoistSignal([direct('pinned', 'skip'), direct('candidate', 'pass')])).toContain('#122');
  expect(hoistSignal([direct('pinned', 'pass'), direct('candidate', 'pass')])).toBeUndefined();
  expect(hoistSignal([direct('pinned', 'warn'), direct('candidate', 'warn')])).toBeUndefined();
  expect(hoistSignal([])).toBeUndefined();
});

test('informational checks are labelled in the report and never counted as failures', () => {
  const rec = new Recorder();
  const origError = console.error;
  console.error = () => {};
  try {
    rec.add({ ...direct('candidate', 'fail') });
    rec.add({ tier: 'candidate', target: 'candidate', name: 'tool-replay', status: 'fail', detail: 'x' });
  } finally {
    console.error = origError;
  }
  expect(rec.failures('candidate').map((c) => c.name)).toEqual(['tool-replay']);
  const report: RunReport = {
    startedAt: '2026-11-01T00:00:00Z',
    dir: '/tmp/x',
    command: 'candidate',
    versions: { pinnedCpa: '8.0.44', candidateCpa: '8.0.48' },
    checks: [direct('pinned', 'warn'), direct('candidate', 'pass')],
    attention: [],
  };
  const md = renderMarkdown(report);
  expect(md).toContain(`| ${TOOL_ADDITION_DIRECT} (info) |`);
  expect(md).toContain('**GPT tool-change hoist:**');
});
