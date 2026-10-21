import type { On } from 'claude-code'
import { expect, test } from 'claude-code/testing'

const VIEW = {
  readmes: [{ path: 'README.md', text: 'The project.' }],
  messages: ['Make the change'],
  top: { title: 'A page', heading: 'It changed.', story: 'I changed it.', stats: [], foryou: [] },
  items: [
    { id: 'main', section: 'What changed', heading: 'The change', lede: 'It works.', isJudgementCall: false, isLowerPriority: false, pieces: ['First.', 'Second.'] },
    { id: 'empty', section: 'What changed', heading: 'Nothing to open', lede: 'All said.', isJudgementCall: false, isLowerPriority: false, pieces: [] },
  ],
}
const INPUT = { session: '11111111-1111-4111-8111-111111111111', round: 1, data: '/data', cwd: '/work' }
const reply = (questions: unknown) => ({ isAnswered: true, text: JSON.stringify({ questions }) })

/** The engine beneath the reader: `hive debrief view` prints VIEW; model calls are answered by `answer` in call order. */
function fakeEngine(on: On, answer: (prompt: string, call: number) => { isAnswered: boolean; text?: string; reason?: string }) {
  const prompts: Array<{ prompt: string; model: string; firstCached: boolean }> = []
  on('session.start', async (_$, e) => ({ cwd: e.cwd }) as never)
  on('tool.register', async () => ({ value: undefined }) as never)
  on('process.run', async () => ({ value: { exitCode: 0, stdout: JSON.stringify(VIEW), stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }) as never)
  on('model.complete', async (_$, e) => {
    prompts.push({ prompt: e.prompt, model: e.model, firstCached: e.promptBlocks?.[0]?.cache === true })
    return { value: { ...answer(e.prompt, prompts.length), usage: { input_tokens: 1, output_tokens: 1, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 } } } as never
  })
  return prompts
}
const call = async ($: Parameters<Parameters<typeof test>[1]>[0], extra: Record<string, unknown> = {}) => {
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true } as never)
  return (await $.tool.call({ tool: 'mcp__debrief__read', ...INPUT, ...extra } as never)) as unknown as { result?: string; isError?: boolean }
}

test('one call for the top and list, then one per item with a body; questions come back by item, ids named', async ($, on) => {
  const prompts = fakeEngine(on, (prompt) => prompt.includes('<opened_item')
    ? reply([{ item: '1', piece: 2, quote: 'Second.', question: 'Second of what?', severity: 'nags' }])
    : reply([{ item: 0, piece: 0, quote: 'I changed it.', question: 'Changed what?', severity: 'blocks' }]))
  const out = await call($)
  expect(prompts.length).toBe(2)
  expect(prompts.every((p) => p.firstCached && p.model === 'sonnet')).toBe(true)
  expect(out.result).toBe('- top of the page, blocks: "I changed it.": Changed what?\n- item 1 (main), nags: "Second.": Second of what?')
})

test('a malformed entry makes the reply unreadable: retried once, then reported as a passage not read', async ($, on) => {
  const prompts = fakeEngine(on, (prompt) => prompt.includes('<opened_item')
    ? reply([{ item: 'one', question: 'What?' }])
    : reply([]))
  const out = await call($)
  expect(prompts.length).toBe(3)
  expect(out.isError).toBeUndefined()
  expect(out.result).toBe('The readers raised no questions.\n\nIncomplete: 1 of 2 reader calls gave no usable answer (item 1: unreadable reply); those passages were not read.')
})

test('when every call fails the tool reports an error', async ($, on) => {
  fakeEngine(on, () => ({ isAnswered: false, reason: 'api-error' }))
  const out = await call($)
  expect(out.isError).toBe(true)
  expect(out.result).toBe('Every reader call failed: top of the page: api-error; item 1: api-error')
})

test('a model named in the call reads in place of the default', async ($, on) => {
  const prompts = fakeEngine(on, () => reply([]))
  await call($, { model: 'claude-opus-4-8' })
  expect(prompts.map((p) => p.model)).toEqual(['claude-opus-4-8', 'claude-opus-4-8'])
})
