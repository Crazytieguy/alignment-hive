import { describe, expect, test } from 'bun:test';
import { parseTranscript } from '@alignment-hive/session-data';
import { search } from '../commands/grep';
import { mandatoryLiteral, mandatoryLiterals, mayMatch } from '../lib/grep-prefilter';

describe('mandatoryLiteral', () => {
  test('the longest run every match must contain', () => {
    expect(mandatoryLiteral('already exchanged', false)).toBe('already exchanged');
    expect(mandatoryLiteral('colou?r', false)).toBe('colo');
    expect(mandatoryLiteral('\\d{4}-\\d\\d', false)).toBe('-');
    expect(mandatoryLiteral('foo(bar)?bazqux', false)).toBe('bazqux');
    expect(mandatoryLiteral('C:\\\\Users', false)).toBe('Users'); // a backslash never counts
    expect(mandatoryLiteral('"type":"image"', true)).toBe('image'); // nor a quote
  });

  test('none when a match need not contain one', () => {
    for (const pattern of ['brunch|jam', '.', '—', '\\d+'])
      expect([pattern, mandatoryLiteral(pattern, false)]).toEqual([pattern, undefined]);
    // D4's P20: a named group makes `\k<q>` a backreference, not the text "k<q>".
    expect(mandatoryLiteral('(?<=\\s)(?<q>[\'"])ok\\k<q>', false)).toBeUndefined();
  });
});

describe('mandatoryLiterals', () => {
  test("one per alternative, also inside a group that is the whole pattern; none when an alternative hasn't one", () => {
    expect(mandatoryLiterals('brunch|jam', false)).toEqual(['brunch', 'jam']);
    expect(mandatoryLiterals('(error|fail(ed)?)', false)).toEqual(['error', 'fail']);
    expect(mandatoryLiterals('(?:a\\[x|b[cd]e)', false)).toEqual(['a[x', 'b']);
    expect(mandatoryLiterals('(foo)bar|baz', false)).toEqual(['bar', 'baz']);
    expect(mandatoryLiterals('colou?r', false)).toEqual(['colo']);
    expect(mandatoryLiterals('a|b', true)).toEqual(['a|b']);
    for (const pattern of ['brunch|', 'x|\\d+', '(a|b)+', '(a|b)c?|d', '(?=a|b)', '(?<q>a)|\\k<q>'])
      expect([pattern, mandatoryLiterals(pattern, false)]).toEqual([pattern, undefined]);
  });
});

// Files that exercise what JSON encoding and the parser do to text: escaped quotes and backslashes,
// real and escaped newlines, placeholders, `/name args`, a stripped reminder joining two words, and
// spaced JSON, which Claude Code does not write. It never escapes printable ASCII (`\u0041`, `\/`).
const J = (...rs: Array<object>) => Buffer.from(rs.map((r) => JSON.stringify(r)).join('\n') + '\n');
const user = (content: unknown) => ({ type: 'user', uuid: `u${Math.random()}`, message: { content } });
const say = (text: string) => ({
  type: 'assistant',
  uuid: `a${Math.random()}`,
  message: { content: [{ type: 'text', text }] },
});
const text = (t: string) => ({ type: 'text', text: t });
const use = (id: string) => ({ type: 'tool_use', id, name: 'Bash', input: {} });
const result = (id: string, content: string) => ({ type: 'tool_result', tool_use_id: id, content });
const files = [
  J(user('say "hello" from C:\\Users\\me; match \\d+ in the regex')),
  J(say("return lines.join('\\n');"), say("cat <<'EOF'\nimport x from 'y'")),
  J(say('Refresh token already exchanged. Sunday brunch, then the jam.'), say('the color on 2026-09-12 — then → done')),
  J(user([{ type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'x' } }])),
  J(
    user(
      '<command-message>security-review</command-message>\n<command-name>/security-review</command-name>\n<command-args>the fix</command-args>',
    ),
  ),
  J(user('<command-name>/permissions</command-name>\n<command-args>Alright we just finished</command-args>')),
  J(say('pass --agents to include them'), user('Another session:\n<agent-message from="a1">hi</agent-message>')),
  J(say("it said 'ok' and left")),
  J(user('abc<system-reminder>note</system-reminder>def')),
  J(user('ab<system-reminder>1</system-reminder>cd<system-reminder>2</system-reminder>ef "quoted" too')),
  // A reminder that closes in the record's next text block, which the parser joins before stripping.
  J(user([text('see abc<system-reminder> a'), text('note </system-reminder>def')])),
  // One result's unclosed reminder must not hide a join in the next result.
  J(
    { type: 'assistant', uuid: 'a1', message: { content: [use('t1'), use('t2')] } },
    user([result('t1', 'x\n<system-reminder> y'), result('t2', 'zzq<system-reminder>z</system-reminder>wwk')]),
  ),
  J(say('no join: <system-reminder>abc</system-reminder>'), user('abc\n<system-reminder>x</system-reminder>def')),
  J(say('nothing to see here')),
  J(user([{ type: 'image', source: { type: 'url', url: 'https://example.com/a.png' } }])),
  Buffer.from('{"type": "user", "uuid": "s1", "message": {"content": [{"type": "document", "source": {}}]}}\n'),
];
const patterns: Array<[string, boolean, boolean]> = [
  ['"hello"', false, false],
  ['"type":"image"', true, false],
  ['C:\\\\Users', false, false],
  ['\\\\d\\+', false, false],
  ["join\\('\\\\n'\\)", false, false],
  ["'EOF'\\nimport", false, false],
  ['ALREADY exchanged', false, true],
  ['brunch|jam', false, false],
  ['colou?r', false, false],
  ['\\d{4}-\\d\\d', false, false],
  ['.', false, false],
  ['—', false, false],
  ['→', false, false],
  ['\\[image:', false, false],
  ['/security-review', false, false],
  ['--agents', false, false],
  ['image: image/png', true, false],
  ['/permissions Alright', false, false],
  ['<agent-message from=', true, false],
  ['(?<=\\s)(?<q>[\'"])ok\\k<q>', false, false],
  ['abcdef', false, false],
  ['ABCDEF', false, true],
  ['zzqwwk', false, false],
  ['(frob|zzqwwk)', false, false],
  ['nothing|BRUNCH', false, true],
  ['(?:\\[image:|quux)', false, false],
  ['frob|/security-review the', false, false],
  ['de"', false, false],
  ['UNKNOWN', false, true],
  ['REFRESH token', false, true],
  ['COLOR ON 2026-09', false, true],
  ['\\[document:', false, false],
];

describe('mayMatch', () => {
  test('never skips a file with a match (D4 patterns, plus a reminder join)', () => {
    let skipped = 0;
    for (const [pattern, fixed, ignoreCase] of patterns) {
      const source = fixed ? pattern.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&') : pattern;
      const re = new RegExp(source, ignoreCase ? 'mi' : 'm');
      const literals = mandatoryLiterals(pattern, fixed);
      for (const [i, buf] of files.entries()) {
        const hits = parseTranscript(buf.toString('utf8')).entries.some((e) => search(e, re).length > 0);
        const kept = literals === undefined || literals.some((l) => mayMatch(buf, l, ignoreCase, re));
        if (hits) expect([pattern, i, kept]).toEqual([pattern, i, true]);
        if (!kept) skipped++;
      }
    }
    expect(skipped).toBeGreaterThan(100); // the filter does skip files
  });

  test('under -i, finds the literal wherever it lies, across the pieces it decodes', () => {
    const big = 9 * 2 ** 20;
    const re = /needle/im;
    for (const at of [0, 4 * 2 ** 20 - 3, 4 * 2 ** 20, 8 * 2 ** 20 - 1, big - 6]) {
      const buf = Buffer.alloc(big, 'x');
      buf.write('NeEdLe', at, 'latin1');
      expect([at, mayMatch(buf, 'needle', true, re)]).toEqual([at, true]);
    }
    expect(mayMatch(Buffer.alloc(big, 'x'), 'needle', true, re)).toBe(false);
  });

  test('a reminder joining text inside one string keeps only files whose joined text may match', () => {
    const joined = J(user('abc<system-reminder>note</system-reminder>def'));
    expect(mayMatch(joined, 'cde', false, /cde/m)).toBe(true);
    expect(mayMatch(joined, 'CDE', true, /CDE/im)).toBe(true);
    expect(mayMatch(joined, 'xyz', false, /xyz/m)).toBe(false);
  });
});
