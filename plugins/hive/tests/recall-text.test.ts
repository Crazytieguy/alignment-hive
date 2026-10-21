import { expect, test } from 'claude-code/testing'
import { citations, collapsePasted, history, promptBlocks, quotesIn, report, sessionsFor } from '../hooks/recall-text'
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
})

test('quotesIn reads the recall and retrieval agent formats', () => {
  expect(quotesIn('## Findings\n\nctx\n> [user] a\n> b (aaaa0001:3, 2026-09-01)\n\n> [assistant] c (bbbb0002/agent-x1:9, 2026-09-02)')).toEqual([
    { kind: 'user', text: 'a\nb', loc: 'aaaa0001:3', date: '2026-09-01' },
    { kind: 'assistant', text: 'c', loc: 'bbbb0002/agent-x1:9', date: '2026-09-02' },
  ])
})

test('collapsePasted keeps the opening of pasted text and the words around it', () => {
  const pasted = `see <pasted_content id="1">${'L'.repeat(1000)}</pasted_content id="1"> please`
  expect(collapsePasted(pasted)).toBe(`see ${'L'.repeat(300)} [pasted, +700 chars] please`)
})

test('report: each entry as the reader saw it, with context only where given, and how far back recall read', () => {
  const h = history([{ loc: 'aaaa0001', rows: [user('aaaa0001:3', 30, 'We use tabs. Also, lunch is at noon.')] }], NOW, 100_000, 'abcd1234')
  expect(citations('- `aaaa0001:3` | \n- aaaa0001:3 | again\n- [ffff0000:1] | made up')).toEqual([
    { loc: 'aaaa0001:3', context: '' },
    { loc: 'ffff0000:1', context: 'made up' },
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
