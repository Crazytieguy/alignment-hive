// Branch states over synthetic fixtures (contract test g).
import { describe, expect, test } from 'bun:test';
import { hiddenBy, isHumanMessage } from './noise';
import { branchFixtures } from './test-fixtures';
import { parseTranscript } from './transcript';
import type { Fixture } from './test-fixtures';
import type { Entry, Transcript } from './transcript';

interface Parsed {
  t: Transcript;
  /** Entries of the record with this label. */
  of: (label: string) => Array<Entry>;
  /** The one branch state of the record's entries ('current' when absent). */
  state: (label: string) => string;
  /** The first entry number of the record. */
  n: (label: string) => number;
}

function parseFixture(f: Fixture): Parsed {
  const t = parseTranscript(f.files[0].content);
  const of = (label: string) => {
    const u = f.uuid(label);
    const es = t.entries.filter((e) => e.uuid === u);
    if (es.length === 0) throw new Error(`${f.name}: record ${label} has no entries`);
    return es;
  };
  const state = (label: string) => {
    const states = new Set(of(label).map((e) => e.branch ?? 'current'));
    if (states.size !== 1) throw new Error(`${f.name}: ${label} has mixed states ${[...states]}`);
    return [...states][0];
  };
  return { t, of, state, n: (label) => of(label)[0].n };
}

/** Label -> branch state, for the given records. */
function stateMap(p: Parsed, labels: Array<string>): Record<string, string> {
  return Object.fromEntries(labels.map((l) => [l, p.state(l)]));
}

const F = Object.fromEntries(branchFixtures().map((f) => [f.name, f]));

describe('branch states', () => {
  test('new root after earlier history: nothing abandoned, a reset diagnostic', () => {
    const f = F['new-root'];
    const p = parseFixture(f);
    expect(p.t.entries.filter((e) => e.branch || e.resumes !== undefined)).toEqual([]);
  });

  test('missing link on the final chain: nothing abandoned, no transition', () => {
    const p = parseFixture(F['missing-final']);
    expect(p.t.entries.filter((e) => e.branch || e.resumes !== undefined)).toEqual([]);
  });

  test('missing link on the abandoned walk: walked entries unknown, the rest current', () => {
    const f = F['missing-walk'];
    const p = parseFixture(f);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'A3', 'R', 'A4'])).toEqual({
      U1: 'current',
      A1: 'current',
      U2: 'current',
      A2: 'current',
      A3: 'unknown',
      R: 'current',
      A4: 'current',
    });
    expect(p.of('R')[0].resumes).toBe(p.n('A1'));
  });

  test('rewind across a compaction with a usable logicalParentUuid: abandoned, boundary included', () => {
    const f = F['compact-usable'];
    const p = parseFixture(f);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'B', 'S', 'U3', 'A3', 'R', 'A4'])).toEqual({
      U1: 'current',
      A1: 'current',
      U2: 'abandoned',
      A2: 'abandoned',
      B: 'abandoned',
      S: 'abandoned',
      U3: 'abandoned',
      A3: 'abandoned',
      R: 'current',
      A4: 'current',
    });
    expect(p.of('R')[0].resumes).toBe(p.n('A1'));
  });

  test('compaction whose logicalParentUuid target is absent: unknown', () => {
    const f = F['compact-absent'];
    const p = parseFixture(f);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'B', 'S', 'U3', 'A3', 'R', 'A4'])).toEqual({
      U1: 'current',
      A1: 'current',
      U2: 'current',
      A2: 'current',
      B: 'unknown',
      S: 'unknown',
      U3: 'unknown',
      A3: 'unknown',
      R: 'current',
      A4: 'current',
    });
  });

  test('message.id reused non-contiguously is not one node; a contiguous response is', () => {
    const f = F['message-id-reuse'];
    const p = parseFixture(f);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'A3', 'R', 'A4', 'U5', 'A5', 'A6', 'U6', 'A7'])).toEqual({
      U1: 'current',
      A1: 'current',
      U2: 'abandoned',
      A2: 'abandoned',
      A3: 'abandoned',
      R: 'current',
      A4: 'current',
      U5: 'current',
      A5: 'current',
      A6: 'current',
      U6: 'current',
      A7: 'current',
    });
    expect(p.of('R')[0].resumes).toBe(p.n('A1'));
    expect(p.of('U6')[0].resumes).toBeUndefined(); // parent A5, previous A6: one response
  });

  test('parallel tool calls and sibling blocks of one record: no transition', () => {
    const f = F['parallel-siblings'];
    const p = parseFixture(f);
    expect(p.t.entries.filter((e) => e.branch || e.resumes !== undefined)).toEqual([]);
    const a1 = p.of('A1');
    expect(a1.map((e) => [e.kind, e.block])).toEqual([
      ['thinking', 0],
      ['assistant', 1],
      ['tool', 2],
    ]);
    const call = a1[2];
    expect(call.kind === 'tool' && call.result).toBe('{"a":1}');
  });

  test('inline sidechain records are excluded from walks, flagged and hidden', () => {
    const f = F['inline-sidechain'];
    const p = parseFixture(f);
    expect(p.t.entries.filter((e) => e.branch || e.resumes !== undefined)).toEqual([]);
    for (const l of ['SU1', 'SA1', 'SU2', 'SA2']) {
      expect(p.of(l)[0].sidechain).toBe(true);
      expect(hiddenBy(p.of(l)[0])).toBe('sidechain');
    }
    for (const l of ['U1', 'U2', 'U3']) expect(isHumanMessage(p.of(l)[0])).toBe(true);
  });

  test('queued messages: abandoned inside the run, current after the transition', () => {
    const f = F['queued'];
    const p = parseFixture(f);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'QC1', 'A3', 'R', 'A4', 'QC2', 'A5', 'QC3', 'QC4'])).toEqual({
      U1: 'current',
      A1: 'current',
      U2: 'abandoned',
      A2: 'abandoned',
      QC1: 'abandoned',
      A3: 'abandoned',
      R: 'current',
      A4: 'current',
      QC2: 'current',
      A5: 'current',
      QC3: 'current',
      QC4: 'current',
    });
    expect(p.of('R')[0].resumes).toBe(p.n('A1'));
    const [qc1, qc2, qc3, qc4] = ['QC1', 'QC2', 'QC3', 'QC4'].map((l) => p.of(l)[0]);
    expect(qc1).toMatchObject({ kind: 'user', text: 'also sort the entries by date' });
    expect(qc1.kind === 'user' && qc1.origin).toBeUndefined();
    expect(isHumanMessage(qc2)).toBe(true);
    expect(qc3).toMatchObject({ origin: 'peer' });
    expect(hiddenBy(qc3)).toBeUndefined();
    expect(qc4).toMatchObject({ origin: 'task-notification' });
    expect(hiddenBy(qc4)).toBe('task-notification');
    // A queued entry never starts a transition and is never the previous record.
    expect(p.t.entries.filter((e) => e.resumes !== undefined).map((e) => e.n)).toEqual([p.n('R')]);
  });

  test('return to an abandoned branch: the returned-to entries are current, the branch left abandoned', () => {
    const f = F['return'];
    const p = parseFixture(f);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'R1', 'A3', 'U4', 'A4'])).toEqual({
      U1: 'current',
      A1: 'current',
      U2: 'current',
      A2: 'current',
      R1: 'abandoned',
      A3: 'abandoned',
      U4: 'current',
      A4: 'current',
    });
    expect(p.of('R1')[0].resumes).toBe(p.n('A1'));
    expect(p.of('U4')[0].resumes).toBe(p.n('A2'));
  });

  test('rewind to the start: the line before R is abandoned, and R resumes no entry', () => {
    const p = parseFixture(F['rewind-to-start']);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'R', 'A3'])).toEqual({
      U1: 'abandoned',
      A1: 'abandoned',
      U2: 'abandoned',
      A2: 'abandoned',
      R: 'current',
      A3: 'current',
    });
    expect(p.of('R')[0].resumes).toBeUndefined();
  });

  test('a resume copy, a command-only walk and a task-notification run are not transitions', () => {
    for (const name of ['resume-copy', 'command-only', 'task-notification-run']) {
      const p = parseFixture(F[name]);
      expect([name, p.t.entries.filter((e) => e.branch || e.resumes !== undefined)]).toEqual([name, []]);
    }
  });

  test('rewind right after a compaction: Q is the last record before the boundary', () => {
    const p = parseFixture(F['after-compaction']);
    expect(stateMap(p, ['U1', 'A1', 'U2', 'A2', 'B', 'R', 'A3'])).toEqual({
      U1: 'current',
      A1: 'current',
      U2: 'abandoned',
      A2: 'abandoned',
      B: 'current',
      R: 'current',
      A3: 'current',
    });
    expect(p.of('R')[0].resumes).toBe(p.n('A1'));
  });

  test('edit through system records; resend after an interrupt is not a transition', () => {
    const f = F['edit-interrupt-agent'];
    const p = parseFixture(f);
    expect(stateMap(p, ['U1', 'A1', 'S1', 'U2', 'A2', 'A3', 'S3', 'R', 'A4', 'INT', 'U5', 'A5'])).toEqual({
      U1: 'current',
      A1: 'current',
      S1: 'current',
      U2: 'abandoned',
      A2: 'abandoned',
      A3: 'abandoned',
      S3: 'current',
      R: 'current',
      A4: 'current',
      INT: 'current',
      U5: 'current',
      A5: 'current',
    });
    expect(p.of('R')[0].resumes).toBe(p.n('A1'));
    expect(p.t.entries.filter((e) => e.resumes !== undefined).length).toBe(1);
    const a2 = p.of('A2')[0];
    expect(a2.kind === 'tool' && a2.agentId).toBe('afb00000b00000a11');
  });

  test('the final chain clears marks only when it resolves fully', () => {
    // F11 with the last record's parent broken: S1 lies between R and P, so it is never walked
    // (the walk stops at R's own ancestors); U2..A3 stay abandoned without the final chain.
    const f = F['edit-interrupt-agent'];
    const lines = f.files[0].content.trimEnd().split('\n');
    const last = JSON.parse(lines[lines.length - 1]);
    last.parentUuid = 'fb00000b-dead-4000-8000-0000000000ff';
    lines[lines.length - 1] = JSON.stringify(last);
    const p = parseFixture({ ...f, files: [{ ...f.files[0], content: lines.join('\n') + '\n' }] });
    expect(stateMap(p, ['S1', 'U2', 'A2', 'A3', 'R'])).toEqual({
      S1: 'current',
      U2: 'abandoned',
      A2: 'abandoned',
      A3: 'abandoned',
      R: 'current',
    });
  });
});
