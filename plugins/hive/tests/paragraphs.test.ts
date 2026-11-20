import { expect, test } from 'claude-code/testing'
import { GAP, blocks, chosenBlocks, excerptOf, isExcerpt, numbered, paragraphed } from '../hooks/paragraphs'

// Moved from the benchmark (experiments/auto-retrieval/bench/excerpt.test.ts, round 13), which now runs these functions.
const para = (word: string, n: number) => Array.from({ length: n }, (_, i) => `${word}${i}`).join(' ') + '.'
const slice = (text: string) => blocks(text).map(b => text.slice(b.start, b.end))

test('blocks split at blank lines and are exact source slices', () => {
  const text = `${para('alpha', 20)}\n\n${para('beta', 20)}\n\n\n${para('gamma', 20)}`
  expect(slice(text)).toEqual([para('alpha', 20), para('beta', 20), para('gamma', 20)])
})

test('a short piece (a heading) joins the next; a short last piece joins the one before', () => {
  const text = `## Findings\n\n${para('alpha', 20)}\n\n${para('beta', 20)}\n\nDone.`
  expect(slice(text)).toEqual([`## Findings\n\n${para('alpha', 20)}`, `${para('beta', 20)}\n\nDone.`])
})

test('never splits inside fenced code', () => {
  const code = '```ts\nconst a = 1\n\nconst b = 2\n\n\nconst c = 3 // a long enough line to pass the short rule\n```'
  const text = `${para('alpha', 20)}\n\n${code}\n\n${para('beta', 20)}`
  expect(slice(text)).toEqual([para('alpha', 20), code, para('beta', 20)])
})

test('a long list splits at line breaks into pieces of about 1,200 characters', () => {
  const items = Array.from({ length: 40 }, (_, i) => `- item ${i}: ${'x'.repeat(60)}`)
  const text = `Here is the list:\n${items.join('\n')}`
  const bs = slice(text)
  expect(bs.length).toBeGreaterThan(1)
  for (const b of bs) expect(b.length).toBeLessThanOrEqual(1200)
  expect(bs.join('\n')).toBe(text)
  expect(bs[0]!.startsWith('Here is the list:\n- item 0')).toBe(true)
})

test('a long list with a fence inside splits around the fence, not in it', () => {
  const fence = `\`\`\`\n${Array.from({ length: 30 }, (_, i) => `line ${i} ${'y'.repeat(40)}`).join('\n')}\n\`\`\``
  const items = Array.from({ length: 10 }, (_, i) => `- item ${i}: ${'x'.repeat(60)}`)
  const text = `${items.join('\n')}\n${fence}\n${items.join('\n')}`
  expect(slice(text).some(b => b.includes(fence))).toBe(true)
})

test('numbered: ¶ marks before each block, separators kept; a block opening with a fence gets its mark on its own line', () => {
  const text = `${para('alpha', 20)}\n\n${para('beta', 20)}`
  expect(numbered(text)).toBe(`¶1 ${para('alpha', 20)}\n\n¶2 ${para('beta', 20)}`)
  const code = '```bash\necho one two three four five six seven eight nine ten eleven twelve thirteen fourteen\n```'
  expect(numbered(`${code}\n\n${para('beta', 20)}`)).toBe(`¶1\n${code}\n\n¶2 ${para('beta', 20)}`)
})

test('paragraphed: an entry of two blocks or more numbered on a line of its own; one block, or none, as it is', () => {
  const text = `${para('alpha', 20)}\n\n${para('beta', 20)}`
  expect(paragraphed(text)).toBe(`\n${numbered(text)}`)
  expect(paragraphed(para('alpha', 20))).toBe(para('alpha', 20))
  expect(paragraphed('short\n\nlines')).toBe('short\n\nlines')
  expect(paragraphed('')).toBe('')
})

const four = [para('alpha', 20), para('beta', 20), para('gamma', 20), para('delta', 20)]
const text4 = four.join('\n\n')

test('excerptOf: kept blocks verbatim, a gap line wherever blocks are skipped, at either end too', () => {
  expect(excerptOf(text4, [1])).toBe([GAP, four[1], GAP].join('\n\n'))
  expect(excerptOf(text4, [0, 1, 3])).toBe([`${four[0]}\n\n${four[1]}`, GAP, four[3]].join('\n\n'))
  expect(excerptOf(text4, [3, 0])).toBe([four[0], GAP, four[3]].join('\n\n'))
  expect(excerptOf(text4, [0, 1, 2, 3])).toBe(text4)
  expect(excerptOf(text4, [])).toBe(text4)
  expect([excerptOf(text4, [1]), excerptOf(text4, [0, 2]), excerptOf(text4, [3])].map(isExcerpt)).toEqual([true, true, true])
  expect(isExcerpt(text4)).toBe(false)
  expect(isExcerpt('see [beta] […] inline')).toBe(false)
})

test('chosenBlocks: a list of numbers and ranges picks those blocks; anything else, every block', () => {
  expect(chosenBlocks(text4, ' ¶2,4 ')).toEqual([1, 3])
  expect(chosenBlocks(text4, '¶2-3, ¶1')).toEqual([0, 1, 2])
  expect(chosenBlocks(text4, '2–3')).toEqual([1, 2])
  // Out of range: dropped, and none left is the whole entry.
  expect(chosenBlocks(text4, '¶3, 9')).toEqual([2])
  for (const field of ['¶9', '¶3-1', '¶0', '', ' all ', '¶2 ¶4', '(¶2)', '¶2-oops', 'paragraphs 2 to 3', '-844'])
    expect(chosenBlocks(text4, field)).toEqual([0, 1, 2, 3])
  // An entry the history did not number: its pointers count for nothing.
  expect(chosenBlocks(para('alpha', 20), '¶2')).toEqual([0])
})
