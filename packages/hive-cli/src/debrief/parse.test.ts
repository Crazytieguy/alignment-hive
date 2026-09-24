import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, test } from 'bun:test';
import { parseReview, rangeSchema, rangesSchema, refuseSecret } from './parse';

export const SESSION = '11111111-1111-4111-8111-111111111111';
const FRONT = `title: Review\nheading: Review the change\nsession: ${SESSION}\nstory: What happened.\nsections: [{ id: main, title: Main }]\n`;
export function document(items: string, front = ''): string {
  return `---\n${FRONT}${front}---\n${items}`;
}
const item = '## Changed behavior\n```yaml\nid: behavior\nsection: main\nlede: Preserve compatibility\n```\n';
const withRef = (ref: string) => document(item + '```ref\n' + ref + '\n```');
/** Line of the ref fence in `withRef`; its YAML starts on the next line. */
const REF_LINE = 14;

describe('debrief parser', () => {
  test('parses a whole page: front matter, 15 items and every ref kind', () => {
    const parsed = parseReview(readFileSync(join(import.meta.dir, 'fixtures/structure.md'), 'utf8'));
    expect(parsed.items).toHaveLength(15);
    expect(parsed.page).toMatchObject({ title: 'Rain-aware watering', head: '5d6e7f8', base: '1a2b3c4', round: 1 });
    expect(parsed.page.asks).toEqual(['22222222:1', '22222222-2222-4222-8222-222222222222:40', '22222:52', '22222222:90'].map((locator) => ({ locator })));
    expect(parsed.page.stats.map((stat) => [stat.n, stat.item, stat.warn])).toEqual([['1', 'git', true], ['4', 'git', false], ['42', 'tests-lint', false]]);
    expect(parsed.page.sections.map((section) => section.id)).toEqual(['wrong', 'fix']);
    expect(parsed.items.filter((i) => i.metadata['judgement-call'])).toHaveLength(8);
    expect(parsed.items.flatMap((i) => i.metadata.alternatives)).toHaveLength(6);
    const refs = parsed.items.flatMap((i) => i.refs.map((r) => r.ref));
    expect(refs).toHaveLength(12);
    expect(refs.find((r) => 'transcript' in r && r.transcript.length === 4)).toMatchObject({ transcript: ['22222222:70', '22222222:68', '22222222:69', '22222222:75'] });
    expect(refs.find((r) => 'file' in r && r.written)).toMatchObject({ written: '22222222:35' });
    expect(refs.filter((r) => 'git' in r)).toHaveLength(1);
    expect(refs.find((r) => 'diff' in r && r.title === 'the start-time read')).toBeDefined();
    expect(parsed.lines.asks).toEqual([7, 7, 7, 7]);
  });
  test('splits only top-level token headings, preserving nested headings and fences', () => {
    const parsed = parseReview(document(item + '\n### Detail\n```md\n## Not an item\n```\n> ## Quoted heading\n\n## Another\n```yaml\nid: next\nsection: checked\nlede: Another effect\n```'));
    expect(parsed.items.map((i) => i.metadata.id)).toEqual(['behavior', 'next']);
    expect(parsed.items[0].body).toContain('## Not an item');
  });
  test('ref kinds, locator lists, summaries and multi-range focus', () => {
    const parsed = parseReview(withRef('diff: src/test.ts\ntitle: the check\nfocus: L2-L4, L9\nold-focus: L1\nlabels: [{line: 3, where: Login prompt}]'));
    expect(parsed.items[0].refs[0].line).toBe(REF_LINE);
    expect(parsed.items[0].refs[0].ref).toMatchObject({ diff: 'src/test.ts', title: 'the check', focus: [{ start: 2, end: 4 }, { start: 9, end: 9 }], 'old-focus': [{ start: 1, end: 1 }] });
    expect(parseReview(withRef('transcript: abc:1, def:2\nsummary: [one, two]')).items[0].refs[0].ref).toMatchObject({ transcript: ['abc:1', 'def:2'], summary: ['one', 'two'] });
    expect(parseReview(withRef('transcript: abc:3, capture:tests\nsummary: [the run, the tests]')).items[0].refs[0].ref).toMatchObject({ transcript: ['abc:3', 'capture:tests'] });
    expect(parseReview(withRef('git: true')).items[0].refs[0].ref).toEqual({ git: true });
    expect(parseReview(withRef('file: /abs/notes.md\nwritten: abc:4')).items[0].refs[0].ref).toMatchObject({ written: 'abc:4' });
  });
  test('plain scalars may contain colons, apostrophes and leading slashes; comments and anchors are rejected', () => {
    expect(parseReview(withRef("transcript: a4eff20e:66\nsummary: the hook's check")).items[0].refs[0].ref).toMatchObject({ summary: "the hook's check" });
    expect(() => parseReview(withRef('transcript: abc:1\nsummary: fix #12 only'))).toThrow(`behavior, line ${REF_LINE + 2}: Multiline strings require |`);
    expect(parseReview(withRef('transcript: abc:1\nsummary: "fix #12 only"')).items[0].refs[0].ref).toMatchObject({ summary: 'fix #12 only' });
    expect(() => parseReview(document(item.replace('lede: Preserve compatibility', 'lede: &a Preserve')))).toThrow('anchors');
    expect(() => parseReview(document(item.replace('lede: Preserve compatibility', 'lede: !!str Preserve')))).toThrow('tags');
    expect(() => parseReview(document(item.replace('section: main', 'section: &k main').replace('Preserve compatibility', '*k')))).toThrow('aliases');
    const story = document(item).replace('story: What happened.', 'story: |\n  First paragraph\n\n  Second');
    expect(parseReview(story).page.story).toContain('\n\n');
  });
  test.each([
    ['conf: 60', 'conf'], ['kind: finding', 'kind'], ['rank: 3', 'rank'], ['open: true', 'open'], ['questions: [x]', 'questions'],
    ['suggestions: [x]', 'suggestions'], ['verified: yes', 'verified'], ['alternatives-conf: 50', 'alternatives-conf'],
  ])('rejects the removed item key %s with its line', (line, key) => {
    expect(() => parseReview(document(item.replace('lede: Preserve compatibility', `lede: Preserve compatibility\n${line}`)))).toThrow(new RegExp(`behavior, line \\d+: .*${key}`));
  });
  test.each(['task: Old task', 'kicker: Session review', 'dates: Sep 5', 'foryou-title: Left', 'asks-conf: 55', 'stats-conf: 60', 'released: [v1]'])('rejects the removed page key %s', (line) => {
    expect(() => parseReview(document(item, line + '\n'))).toThrow(/page, line \d+: .*Unrecognized key/);
  });
  test.each(['status: new', 'conf: 50', 'open: true', 'fields: assistant', 'expand: true'])('rejects the ref key %s', (line) => {
    expect(() => parseReview(withRef(`file: notes.md\n${line}`))).toThrow(new RegExp(`behavior, line ${REF_LINE + 2}: .*Unrecognized key`));
  });
  test('section, stat, id and judgement-call cross-checks carry lines', () => {
    expect(() => parseReview(document(item.replace('section: main', 'section: outside')))).toThrow('behavior, line 11: Unknown section outside; expected one of checked, unverified, landing, side-effects, main');
    expect(() => parseReview(document(item, 'stats: [{ n: "3", label: tests, item: missing }]\n'))).toThrow('page, line 7: Stat links to unknown item: missing');
    expect(parseReview(document(item, 'stats: [{ n: 3, label: tests, item: behavior }]\n')).page.stats[0].n).toBe('3');
    expect(() => parseReview(document(item + item))).toThrow('behavior, line 14: Duplicate item id');
    expect(() => parseReview(document(item).replace('sections: [{ id: main, title: Main }]', 'sections: [{ id: main, title: Main }, { id: landing, title: Landing }]'))).toThrow('Duplicate or reserved section id: landing');
    expect(() => parseReview(document(item.replace('lede:', 'alternatives: [Another way]\nlede:')))).toThrow('alternatives require judgement-call');
    expect(() => parseReview(document(item.replace('lede:', 'judgement-call: true\nlede:')))).toThrow('behavior, line 12: A judgement call outside side-effects');
    expect(parseReview(document(item.replace('section: main', 'section: unverified').replace('lede:', 'judgement-call: true\nlede:'))).items[0].metadata['judgement-call']).toBe(true);
    expect(() => parseReview(document(item.replace('section: main', 'section: side-effects').replace('lede:', 'judgement-call: true\nalternatives: [x]\nlede:')))).toThrow('Side-effect items carry no alternatives');
  });
  test('lower-priority cannot be a judgement call nor carry alternatives, reported at its own line', () => {
    const quiet = item.replace('lede:', 'lower-priority: true\nlede:');
    expect(parseReview(document(quiet)).items[0].metadata['lower-priority']).toBe(true);
    expect(parseReview(document(item)).items[0].metadata['lower-priority']).toBe(false);
    expect(() => parseReview(document(quiet.replace('section: main', 'section: unverified').replace('lede:', 'judgement-call: true\nlede:')))).toThrow('behavior, line 12: lower-priority cannot be combined with judgement-call or alternatives');
    expect(() => parseReview(document(quiet.replace('lede:', 'judgement-call: true\nalternatives: [x]\nlede:')))).toThrow('behavior, line 12: lower-priority cannot be combined');
  });
  test('locator lists, summary counts, written/at and ref kinds', () => {
    expect(() => parseReview(withRef('transcript: abc:1, def:2\nsummary: one'))).toThrow(`behavior, line ${REF_LINE + 2}: summary: Expected summary: a list of 2 lines`);
    expect(() => parseReview(withRef('transcript: abc:1\nsummary: [one, two]'))).toThrow('Expected summary: one line');
    expect(() => parseReview(withRef('transcript: capture:tests'))).toThrow('summary');
    expect(() => parseReview(withRef('transcript: abc:1-3\nsummary: x'))).toThrow(`behavior, line ${REF_LINE + 1}: transcript.0: Expected a transcript locator`);
    expect(() => parseReview(withRef('transcript: reviewer\nsummary: x'))).toThrow('Expected a transcript locator');
    expect(() => parseReview(withRef('file: a.md\nwritten: abc:1\nat: base'))).toThrow('Use written or at, not both');
    expect(() => parseReview(withRef('diff: x\nfile: y'))).toThrow(`behavior, line ${REF_LINE + 1}: metadata: A ref needs exactly one of`);
    expect(() => parseReview(withRef('title: orphan'))).toThrow('A ref needs exactly one of');
    expect(() => parseReview(withRef('diff: x\nfocus: L4-L2'))).toThrow(`behavior, line ${REF_LINE + 2}: focus.0: Expected a 1-based inclusive range`);
    expect(() => parseReview(withRef('diff: x\nfocus: L1,'))).toThrow('focus.1');
  });
  test('rejects missing fences, invalid page and preamble', () => {
    expect(() => parseReview(document('## Empty'))).toThrow('yaml metadata');
    expect(() => parseReview('## No frontmatter')).toThrow('frontmatter');
    expect(() => parseReview(document(item).replace(SESSION, '11111111'))).toThrow('session');
    expect(() => parseReview(document('ignored\n' + item))).toThrow('before the first item');
    expect(() => parseReview(document(item).replace('story: What happened.\n', ''))).toThrow('page, line 2: story');
  });
  test('the concrete ref examples in the authoring reference parse', () => {
    const reference = readFileSync(join(import.meta.dir, '../../../../plugins/debrief/references/authoring.md'), 'utf8');
    const examples = [
      ['diff: src/a.ts\ntitle: the check\n', 'focus: L10-L20, L88'], ['file: src/a.ts\n', 'range: L1-L2'], ['', 'git: true'], ['summary: the run\n', 'transcript: capture:tests'],
      ['transcript: a4eff20e:3\nsummary: the reply\n', 'clip: false'], ['transcript: a4eff20e:3\nsummary: the call\n', 'results: false'],
    ];
    for (const [context, example] of examples) {
      expect(reference).toContain(`\`${example.replace('capture:tests', 'capture:<name>')}`);
      expect(() => parseReview(withRef(context + example))).not.toThrow();
    }
  });
  test('a secret refused in the debrief file names its line; one the file does not hold came from evidence', () => {
    const hit = { kind: 'jwt' as const, start: 0, end: 5, match: 'abcde' };
    expect(() => refuseSecret(hit, 'page', ['one', 'x abcde y'], 1)).toThrow('debrief: page, line 2: The page would show a JWT; remove it from the debrief file');
    expect(() => refuseSecret(hit, 'page', ['one'], 1)).toThrow('debrief: page, line 1: The page would show a JWT; remove it from the debrief file');
    expect(() => refuseSecret(hit, 'page', ['one'])).toThrow('debrief: page, line 1: The page would show a JWT that redaction missed; it comes from evidence, not the debrief file: leave that evidence out and report the shape');
  });
  test('range grammar', () => {
    for (const range of ['L0', 'L4-L2', '1-2', 'L9007199254740993']) expect(rangeSchema.safeParse(range).success).toBe(false);
    for (const range of ['L1', 'L1-L2', 'L1-2']) expect(rangeSchema.safeParse(range).success).toBe(true);
    expect(rangesSchema.parse('L1, L3-L4').map((r) => [r.start, r.end])).toEqual([[1, 1], [3, 4]]);
  });
});
