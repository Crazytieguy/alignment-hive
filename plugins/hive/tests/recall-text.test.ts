import { expect, test } from 'claude-code/testing'
import { FAILED, citations, collapsePasted, history, promptBlocks, quotesIn, recalledIn, recalledOf, report, reportOf, sessionsFor } from '../hooks/recall-text'
import type { Row } from '../hooks/recall-text'

const HOUR = 3_600_000
const NOW = Date.parse('2026-10-20T15:30:00Z')
const at = (hoursAgo: number) => new Date(NOW - hoursAgo * HOUR).toISOString()
const user = (loc: string, hoursAgo: number, text: string): Row => ({ loc, time: at(hoursAgo), kind: 'user', text })
const many = (session: string, count: number, size: number) =>
  Array.from({ length: count }, (_, i) => user(`${session}:${i + 1}`, (count - i) / 10, `${i} ${'x'.repeat(size)}`))

test('history: messages, replies and answers oldest first, under their session; tool calls left out', () => {
  const h = history(
    [
      { loc: 'aaaa0001', rows: [user('aaaa0001:1', 2.5, 'first'), { loc: 'aaaa0001:2', time: at(2.4), kind: 'assistant', text: 'reply' }] },
      { loc: 'bbbb0002', rows: [user('bbbb0002:1', 1.5, 'other'), { loc: 'bbbb0002:2', time: at(1.4), kind: 'tool', tool: 'Bash', result: 'x' }] },
    ],
    NOW,
    100_000,
  )
  const text = h.closed.join('') + h.open
  expect(text).toContain('[aaaa0001:1] user: first\n[aaaa0001:2] assistant: reply')
  expect(text.indexOf('aaaa0001:1')).toBeLessThan(text.indexOf('bbbb0002:1'))
  expect(text).not.toContain('Bash')
})

test('history: whole chunks leave the window oldest first, so what stays prints the same; it fills most of the budget', () => {
  const rows = many('aaaa0001', 400, 1000)
  const h = history([{ loc: 'aaaa0001', rows }], NOW, 100_000)
  expect(h.closed.length).toBeGreaterThan(2)
  const total = h.closed.join('').length + h.open.length
  expect(total).toBeGreaterThan(75_000)
  expect(total).toBeLessThanOrEqual(100_000)
  // Five more entries: the closed chunks that stay are unchanged.
  const more = Array.from({ length: 5 }, (_, i) => user(`aaaa0001:${401 + i}`, -(i + 1) / 10, 'y'.repeat(1000)))
  const later = history([{ loc: 'aaaa0001', rows: [...rows, ...more] }], NOW + HOUR, 100_000)
  expect(later.closed.some(c => c === h.closed.at(-1))).toBe(true)
})

test('history: a stretch busier than the budget keeps its newest entries', () => {
  const h = history([{ loc: 'aaaa0001', rows: many('aaaa0001', 150, 3000) }], NOW, 100_000)
  const text = h.closed.join('') + h.open
  expect(text).toContain('[aaaa0001:150]')
  expect(text.length).toBeGreaterThan(50_000)
})

test('history: an entry earlier recall quoted prints once, with its own date; it is no news where this session reported it', () => {
  const recall = (loc: string, hoursAgo: number): Row => ({
    loc, time: at(hoursAgo), kind: 'user', origin: 'recall', recall: true,
    text: '[hive recall] Entries.\n\nContext: x\n> [user] use tabs\n> always (cccc0003:4, 2026-09-01)',
  })
  const h = history(
    [
      { loc: 'aaaa0001', rows: [recall('aaaa0001:2', 3), recall('aaaa0001:5', 2)] },
      { loc: 'cccc0003', rows: [user('cccc0003:4', 2.5, 'use tabs\nalways')] },
      { loc: 'dddd0004', rows: [user('dddd0004:1', 4, 'old'), { loc: 'dddd0004:2', time: at(3.5), kind: 'system', subtype: 'compact_boundary' }, recall('dddd0004:3', 1), user('dddd0004:4', 0.5, 'new')] },
    ],
    NOW,
    100_000,
    'dddd0004',
  )
  const text = h.closed.join('') + h.open
  expect(text).toContain('cccc0003:4 (2026-09-01) user: use tabs\nalways')
  expect(text).toContain('cccc0003:4 again')
  expect(text).not.toContain('[cccc0003:4] user:')
  expect(text).toContain('[dddd0004:2] the conversation was compacted here')
  expect(h.seen.get('cccc0003:4')).toEqual({ label: 'user', text: 'use tabs\nalways', date: '2026-09-01' })
  // The current session: before its latest compaction is news, after it not; nor what its own recall reported.
  expect(['dddd0004:1', 'dddd0004:4', 'cccc0003:4', 'aaaa0001:2'].map(h.isNew)).toEqual([true, false, false, true])
})

test('history: a report takes the budget only for what it adds, so reports of entries in the window leave it full', () => {
  const rows = many('aaaa0001', 300, 1000)
  // After every fifth entry, a report quoting the five before it in full (what recall hands the main agent).
  const reports = Array.from({ length: 60 }, (_, k): Row => ({
    loc: `bbbb0002:${k + 1}`, time: at((300 - 5 * k - 5) / 10 - 0.01), kind: 'user', origin: 'recall', recall: true,
    text: `[hive recall] Entries.\n\n${rows.slice(5 * k, 5 * k + 5).map(r => `> [user] ${r.text} (${r.loc}, 2026-10-20)`).join('\n\n')}`,
  }))
  // A launch receipt quotes nothing and prints nothing.
  const receipt: Row = { loc: 'cccc0003:1', time: at(0.05), kind: 'tool', tool: 'Agent', recall: true, result: 'r'.repeat(20_000) }
  const sessions = [{ loc: 'aaaa0001', rows }, { loc: 'bbbb0002', rows: reports }, { loc: 'cccc0003', rows: [receipt] }]
  const h = history(sessions, NOW, 100_000)
  const text = h.closed.join('') + h.open
  // Counting each report whole would leave about half.
  expect(text.length).toBeGreaterThan(65_000)
  expect(text.length).toBeLessThanOrEqual(100_000)
  expect(text).toContain('[aaaa0001:300]')
  expect(text).toContain('aaaa0001:295 again')
  expect(text).not.toContain('rrrr')
})

test('history: an entry takes the budget at the size it prints, where a later report quotes only an excerpt', () => {
  const rows = many('aaaa0001', 100, 6000)
  const excerpts = Array.from({ length: 20 }, (_, k): Row => ({
    loc: `bbbb0002:${k + 1}`, time: at((96 - 5 * k) / 10 - 0.01), kind: 'tool', tool: 'Agent', recall: true,
    result: rows.slice(5 * k, 5 * k + 5).map(r => `> [user] ${r.text!.slice(0, 100)} (${r.loc}, 2026-10-20)`).join('\n\n'),
  }))
  const h = history([{ loc: 'aaaa0001', rows }, { loc: 'bbbb0002', rows: excerpts }], NOW, 100_000)
  const text = h.closed.join('') + h.open
  expect(text).toContain('aaaa0001:100 again')
  expect(text.length).toBeLessThanOrEqual(100_000)
})

test('history: a quote whose loc names an entry in the window, but not the entry it is from, is left out', () => {
  const quote = (loc: string, text: string) => `> [user] ${text} (${loc}, 2026-10-01)`
  const report: Row = {
    loc: 'bbbb0002:1', time: at(1), kind: 'user', origin: 'recall', recall: true,
    text: [
      '[hive recall] Entries.',
      // From their entries, as the retrieval agent writes them: in quotation marks, cut short, with an apostrophe,
      // nested quotation marks, case and a list's formatting changed; an artifact comment over two paragraphs.
      quote('aaaa0001:1', '"Don\'t use tabs ... spaces only"'),
      quote('aaaa0001:6', '"i\'d add that \'max\' is never recommended. Also first item, second item"'),
      quote('aaaa0001:7', 'First paragraph. Second paragraph.'),
      // As recall writes it: clipped, with pasted text collapsed.
      quote('aaaa0001:2', `see ${'L'.repeat(300)} [pasted, +700 chars] please [+3 chars]`),
      // Another entry's text, as a CLI numbering differently would cite it.
      quote('aaaa0001:3', 'Don’t use tabs'),
      // Two entries joined, citing one.
      quote('aaaa0001:4', '"spaces only" / "ship it"'),
      // Entries the window doesn't print: one not read, and a call that started an agent.
      quote('cccc0003:9', 'from an older session'),
      quote('aaaa0001:5', 'review the PR for races'),
    ].join('\n\n'),
  }
  const rows = [
    user('aaaa0001:1', 3, 'Don’t use tabs, use spaces only'),
    user('aaaa0001:2', 2.9, `see <pasted_content id="1">${'L'.repeat(1000)}</pasted_content id="1"> please ok`),
    user('aaaa0001:3', 2.8, 'and ship it'),
    user('aaaa0001:4', 2.7, 'spaces only'),
    { loc: 'aaaa0001:5', time: at(2.6), kind: 'tool', tool: 'Agent' },
    user('aaaa0001:6', 2.5, 'I\'d add that "max" is never recommended.\n\n- Also first item\n- second item'),
    { loc: 'aaaa0001:7', time: at(2.4), kind: 'tool', tool: 'ArtifactComments', result: 'Thread t1\n  [the user (owner) — 2026-10-01]\n      0a1b2c3d| First paragraph.\n      0a1b2c3d|\n      0a1b2c3d| Second paragraph.' },
  ]
  const h = history([{ loc: 'aaaa0001', rows }, { loc: 'bbbb0002', rows: [report] }], NOW, 100_000)
  const text = h.closed.join('') + h.open
  for (const n of [1, 2, 6, 7]) expect(text).toContain(`aaaa0001:${n} again`)
  for (const n of [3, 4]) expect(text).not.toContain(`aaaa0001:${n} again`)
  expect(text).toContain('[aaaa0001:3] user: and ship it')
  expect(text).toContain('cccc0003:9 (2026-10-01) user: from an older session')
  expect(text).toContain('aaaa0001:5 (2026-10-01) user: review the PR for races')
  // Once newer entries push those out of the window, the report's quotes print as written: what an older chunk prints
  // doesn't depend on which sessions happened to be read.
  const filler = { loc: 'dddd0004', rows: many('dddd0004', 150, 1000).map(r => ({ ...r, time: at(1.5 + Number(r.loc.split(':')[1]) / 1000) })) }
  const later = history([{ loc: 'aaaa0001', rows }, filler, { loc: 'bbbb0002', rows: [report] }], NOW, 100_000)
  const laterText = later.closed.join('') + later.open
  expect(laterText).not.toContain('[aaaa0001:')
  expect(laterText).toContain('aaaa0001:3 (2026-10-01) user: Don’t use tabs')
})

test('history: a comment thread prints whole where it first appears; a later read repeating it, as its loc', () => {
  const thread = (status: string, nonce: string, comments: Array<string>) =>
    [`Thread 79cff63a-231a-426f-b106-f112accd6dc5`, `  ${status} · Claude: NOT activated · created 2026-10-29`, '  [location] Entry numbers', ...comments.flatMap(c => ['  [the user (owner) — 2026-10-29T21:22]', `      ${nonce}| ${c}`])].join('\n')
  const read = (loc: string, hoursAgo: number, result: string): Row => ({ loc, time: at(hoursAgo), kind: 'tool', tool: 'ArtifactComments', result })
  // Reads of a carried thread leave out the anchored element's text.
  const first = thread('open', '0a1b2c3d', ['Less scary than I thought.']).replace('\n  [location] Entry numbers', '\n  [location] Entry numbers\n  [anchored element] <p> Entry numbers…')
  const rows = [
    read('aaaa0001:3#79cff63a', 3, first),
    // The next version of the page carries it, under the next read's line markers.
    read('aaaa0001:9#79cff63a', 2, thread('open · carried from an earlier version', '9f8e7d6c', ['Less scary than I thought.'])),
    read('aaaa0001:14#79cff63a', 1, thread('open', '11223344', ['Less scary than I thought.', 'Now ship it.'])),
    read('aaaa0001:20#79cff63a', 0.8, thread('resolved (by Claude)', '55667788', ['Less scary than I thought.', 'Now ship it.'])),
  ]
  const h = history([{ loc: 'aaaa0001', rows }], NOW, 100_000, 'bbbb0002')
  const text = h.closed.join('') + h.open
  expect(text).toContain(`[aaaa0001:3#79cff63a] comments: ${first}`)
  expect(text).toContain('[aaaa0001:9#79cff63a] comments: unchanged since aaaa0001:3#79cff63a')
  expect(text).toContain('[aaaa0001:14#79cff63a] comments: Thread 79cff63a')
  expect(text).toContain('Now ship it.')
  expect(text).toContain('[aaaa0001:20#79cff63a] comments: unchanged since aaaa0001:14#79cff63a')
  // Cited, a repeat gives its own read's text. That report, back in the history, still counts as reported: its quote
  // is from the entry it names, and the thread it quotes is unchanged since its first read.
  const repeat = rows[1]!.result!
  const cited = report('- aaaa0001:9#79cff63a |', h)!
  expect(cited).toContain(`> [comments] ${repeat.replace(/\n/g, '\n> ')} (aaaa0001:9#79cff63a, ${at(2).slice(0, 10)})`)
  const next = history([{ loc: 'aaaa0001', rows }, { loc: 'bbbb0002', rows: [{ loc: 'bbbb0002:2', time: at(0.5), kind: 'user', origin: 'recall', recall: true, text: cited }] }], NOW, 100_000, 'bbbb0002')
  expect(next.closed.join('') + next.open).toContain('[bbbb0002:2] recall:\n  aaaa0001:9#79cff63a again')
  expect(next.isNew('aaaa0001:9#79cff63a')).toBe(false)
  const before = history([{ loc: 'aaaa0001', rows: rows.slice(0, 1) }, { loc: 'bbbb0002', rows: [{ loc: 'bbbb0002:2', time: at(0.5), kind: 'user', origin: 'recall', recall: true, text: cited }] }], NOW, 100_000, 'cccc0003')
  expect(before.closed.join('') + before.open).toContain('  aaaa0001:9#79cff63a unchanged since aaaa0001:3#79cff63a')
  // A quote citing the read the threads came from is checked against all of them.
  const quoted: Row = { loc: 'cccc0003:1', time: at(0.5), kind: 'user', origin: 'recall', recall: true, text: '[hive recall] Entries.\n\n> [tool] Now ship it. (aaaa0001:14, 2026-10-29)\n\n> [tool] Ship nothing. (aaaa0001:9, 2026-10-29)' }
  const both = history([{ loc: 'aaaa0001', rows }, { loc: 'cccc0003', rows: [quoted] }], NOW, 100_000, 'bbbb0002')
  const bothText = both.closed.join('') + both.open
  expect(bothText).toContain('aaaa0001:14 (2026-10-29) tool: Now ship it.')
  expect(bothText).not.toContain('Ship nothing.')
})

test('history: a message the user typed prints whole; a reply is clipped', () => {
  const long = 'w'.repeat(9_000)
  const h = history([{ loc: 'aaaa0001', rows: [user('aaaa0001:1', 2, long), { loc: 'aaaa0001:2', time: at(1), kind: 'assistant', text: long }] }], NOW, 100_000)
  const text = h.closed.join('') + h.open
  expect(text).toContain(`[aaaa0001:1] user: ${long}\n`)
  expect(text).toContain(`[aaaa0001:2] assistant: ${'w'.repeat(6_000)} [+3000 chars]`)
})

test('promptBlocks: one block per closed chunk; the last marked for the cache, or each of them for GPT', () => {
  const h = history([{ loc: 'aaaa0001', rows: many('aaaa0001', 400, 1000) }], NOW, 100_000)
  const marked = (blocks: ReturnType<typeof promptBlocks>) => blocks.map(b => b.cache === true)
  const closed = h.closed.map(() => true)
  expect(h.closed.length).toBeGreaterThan(2)
  expect(marked(promptBlocks(h, 'abcd1234', 'hi'))).toEqual([...closed.map((_, i) => i === closed.length - 1), false])
  expect(marked(promptBlocks(h, 'abcd1234', 'hi', true))).toEqual([...closed, false])
})

test('sessionsFor reads newest sessions in batches until older ones cannot reach the window', async () => {
  const listed = Array.from({ length: 60 }, (_, i) => ({ loc: `s${String(i).padStart(7, '0')}`, end: at(i) }))
  const read: Array<number> = []
  const outline = async (locs: Array<string>) => {
    read.push(locs.length)
    return locs.map(loc => user(`${loc}:1`, Number(loc.slice(1)), 'x'.repeat(3000)))
  }
  const sessions = await sessionsFor(listed, outline, 50_000)
  expect(read).toEqual([20])
  expect(sessions).toHaveLength(20)
  expect(await sessionsFor(listed, outline, 1_000_000)).toHaveLength(60)
  // Long replies count as the history clips them, so they don't end the reading early.
  const long = async (locs: Array<string>) => locs.map(loc => ({ ...user(`${loc}:1`, Number(loc.slice(1)), ''), kind: 'assistant', text: 'y'.repeat(30_000) }))
  expect(await sessionsFor(listed.slice(0, 40), long, 600_000)).toHaveLength(40)
  // Nor do reports, which mostly quote what the history prints anyway.
  const reported = async (locs: Array<string>) => locs.map(loc => ({ ...user(`${loc}:1`, Number(loc.slice(1)), 'z'.repeat(30_000)), origin: 'recall', recall: true }))
  expect(await sessionsFor(listed.slice(0, 40), reported, 600_000)).toHaveLength(40)
  // A thread every session's reads repeat counts once, as the history prints it.
  const repeated = async (locs: Array<string>) =>
    locs.flatMap(loc => Array.from({ length: 20 }, (_, k): Row => ({
      loc: `${loc}:${k + 1}#79cff63a`, time: at(Number(loc.slice(1)) + k / 100), kind: 'tool', tool: 'ArtifactComments',
      result: `Thread 79cff63a-231a-426f-b106-f112accd6dc5\n      ${String(k).padStart(8, '0')}| ${'c'.repeat(3000)}`,
    })))
  expect(await sessionsFor(listed.slice(0, 21), repeated, 800_000)).toHaveLength(21)
  // Sessions a script ran are not read.
  const scripted = listed.slice(0, 3).map((s, i) => ({ ...s, scripted: i === 1 || undefined }))
  expect((await sessionsFor(scripted, outline, 1_000_000)).map(s => s.loc)).toEqual([listed[0]!.loc, listed[2]!.loc])
})

test('quotesIn reads the recall and retrieval agent formats, and a report handed back indented', () => {
  const report = '## Findings\n\nctx\n> [user] a\n>   b (aaaa0001:3, 2026-09-01)\n\n> [assistant] c (bbbb0002/agent-x1:9, 2026-09-02)\n\n> [tool] d (cccc0003:4#79cff63a, 2026-09-03)'
  const quotes = [
    { kind: 'user', text: 'a\n  b', loc: 'aaaa0001:3', date: '2026-09-01' },
    { kind: 'assistant', text: 'c', loc: 'bbbb0002/agent-x1:9', date: '2026-09-02' },
    { kind: 'tool', text: 'd', loc: 'cccc0003:4#79cff63a', date: '2026-09-03' },
  ]
  expect(quotesIn(report)).toEqual(quotes)
  const handedBack = `<agent-message from="a1">\n[Subagent hand-back] The report follows:\n${report.replace(/^/gm, '  ')}\n</agent-message>`
  expect(quotesIn(handedBack)).toEqual(quotes)
  // The retrieval agent's closing parentheses as it writes them: no date, or the loc among other words. A quote with
  // no loc (an artifact comment's id) is not one.
  const varied = [
    '> [tool] "3. Plan, then plan review." (a4aee255:100)',
    '> [user] a separate section (c014, bbc0a27d:349 area, 2026-09-15)',
    '> [user] tests and lint pass (c034, 2026-09-15)',
    '> [user] longer prefix (abcdef012:5, 2026-09-15)',
  ].join('\n\n')
  expect(quotesIn(varied)).toEqual([
    { kind: 'tool', text: '"3. Plan, then plan review."', loc: 'a4aee255:100', date: '' },
    { kind: 'user', text: 'a separate section', loc: 'bbc0a27d:349', date: '2026-09-15' },
  ])
})

test('collapsePasted keeps the opening of pasted text and the words around it', () => {
  const pasted = `see <pasted_content id="1">${'L'.repeat(1000)}</pasted_content id="1"> please`
  expect(collapsePasted(pasted)).toBe(`see ${'L'.repeat(300)} [pasted, +700 chars] please`)
})

test('report: each entry as the reader saw it, with context only where given, and how far back recall read', () => {
  const h = history([{ loc: 'aaaa0001', rows: [user('aaaa0001:3', 30, 'We use tabs. Also, lunch is at noon.')] }], NOW, 100_000, 'abcd1234')
  expect(citations('- `aaaa0001:3` | \n- aaaa0001:3 | again\n- [ffff0000:1] | made up\n- aaaa0001:4#79cff63a |')).toEqual([
    { loc: 'aaaa0001:3', context: '' },
    { loc: 'ffff0000:1', context: 'made up' },
    { loc: 'aaaa0001:4#79cff63a', context: '' },
  ])
  const whole = report('- aaaa0001:3 |', h)!
  expect(whole).toContain('> [user] We use tabs. Also, lunch is at noon. (aaaa0001:3, 2026-10-19)')
  expect(whole).not.toContain('Context:')
  expect(whole).toContain('messages and replies since 2026-10-19;')
  expect(report('- aaaa0001:3 | the repo', h)).toContain('Context: the repo\n> [user] We use tabs. Also')
  expect(report('- ffff0000:1 |', h)).toBeUndefined()
  expect(report('none', h)).toBeUndefined()
  expect(promptBlocks(h, 'abcd1234', 'hi').map(b => b.text).join('')).toContain('<new_prompt>\nhi\n</new_prompt>')
})

test('recalledOf reads a report back as reportOf wrote it, and FAILED as failed', () => {
  const found = [
    { loc: 'aaaa0001:3', label: 'user', text: 'Use tabs.\n\n> quoted line\n- a list (with parens)', date: '2026-10-19', context: '' },
    { loc: 'bbbb0002/agent-a1b2:7', label: 'user answer', text: 'Yes, ship it', date: '2026-10-20', context: 'which branch to ship' },
    // An earlier retrieval report's quote can come without a date.
    { loc: 'cccc0003:12', label: 'assistant', text: 'Landed as abc123.', date: '', context: '' },
  ]
  expect(recalledOf(reportOf(found, '2026-10-17'))).toEqual({ entries: found, from: '2026-10-17' })
  expect(recalledOf(FAILED)).toBe('failed')
  expect(recalledOf('[hive recall] something else')).toBeUndefined()
})

test('recalledIn: each automatic report goes to the prompt nearest before it', () => {
  const one = reportOf([{ loc: 'aaaa0001:3', label: 'user', text: 'Use tabs.', date: '2026-10-19', context: '' }], '2026-10-17')
  const rows: Array<Row> = [
    { loc: 'cccc0003:1', uuid: 'p1', kind: 'user', text: 'Format the file.' },
    { loc: 'cccc0003:2', uuid: 'r1', kind: 'user', origin: 'recall', recall: true, text: one },
    { loc: 'cccc0003:3', uuid: 'a1', kind: 'assistant', text: 'Done.' },
    { loc: 'cccc0003:4', uuid: 'p2', kind: 'user', text: 'And the other one.' },
    { loc: 'cccc0003:5', uuid: 'm1', kind: 'user', origin: 'peer', text: 'a message from another session' },
    { loc: 'cccc0003:6', uuid: 'r2', kind: 'user', origin: 'recall', recall: true, text: FAILED },
    { loc: 'cccc0003:7', uuid: 'p3', kind: 'user', text: 'Look it up.' },
    // A retrieval agent's report handed back: recall, but not automatic recall's.
    { loc: 'cccc0003:8', uuid: 'r3', kind: 'user', origin: 'recall', recall: true, text: 'Found 2 sessions about tabs.' },
  ]
  expect(recalledIn(rows)).toEqual([
    ['p1', { entries: [{ loc: 'aaaa0001:3', label: 'user', text: 'Use tabs.', date: '2026-10-19', context: '' }], from: '2026-10-17' }],
    ['p2', 'failed'],
  ])
})
