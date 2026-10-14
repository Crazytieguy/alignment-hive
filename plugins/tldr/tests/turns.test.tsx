import type { On } from 'claude-code'
import { expect, test } from 'claude-code/testing'

// A summary starts unawaited once a block is drawn, and is drawn when it comes back; a test
// waits it out. setTimeout is the test runner's, outside the mods' library.
declare const setTimeout: (run: () => void, ms: number) => void
const settle = (ms = 20) => new Promise<void>(resolve => setTimeout(resolve, ms))
async function eventually<T>(find: () => Promise<T | undefined>) {
  for (let i = 0; i < 100; i++) {
    const found = await find()
    if (found !== undefined) return found
    await settle()
  }
  return undefined
}

const LONG = Array(120).fill('word').join(' ') + '\nsecond line'
const SUMMARY = /A short summary\./

/** The engine beneath tldr; every summary request is answered with one sentence. */
function fakeEngine(on: On) {
  const store = new Map<string, unknown>()
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('session.id', async () => ({ value: 'abc' }))
  on('store.get', async (_$, e) => ({ value: store.get(e.key) }) as never)
  on('store.set', async (_$, e) => {
    store.set(e.key, e.value)
    return { value: undefined } as never
  })
  on('command.register', async () => ({ value: undefined }) as never)
  on('ui.log', async () => ({ value: undefined }) as never)
  on('turn.start', async (_$, e) => ({ turnId: e.turnId }) as never)
  on('turn.complete', async () => ({ text: '' }) as never)
  on('ui.render', async (_$, e) => ({ type: 'Text', props: {}, children: [(e.props as { text: string }).text] }) as never)
  on('model.complete', async () => ({ value: { isAnswered: true, text: 'A short summary.', usage: { input_tokens: 1, output_tokens: 1 } } }) as never)
}

const draw = (requestId: string) => ({
  plugin: 'tldr',
  surface: 'terminal' as const,
  component: 'AssistantMessage' as const,
  props: { text: LONG, isFirstOfReply: true } as never,
  requestId,
})

test("a subagent's turn starting after the main turn ended does not hold back a summary", async ($, on) => {
  fakeEngine(on)
  await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true } as never)
  await $.turn.start({ text: 'hi', turnId: 't1' } as never)
  await $.turn.complete({ reason: 'answer', answer: 'done', durationMs: 1, isAborted: false, turnId: 't1' } as never)
  await $.turn.start({ text: 'background work', agentId: 'a1', turnId: 't2' } as never)

  const ui = await $.ui.mount(draw('r1'))
  expect(await eventually(() => ui.find({ type: 'Text', text: SUMMARY }))).toBeDefined()
  await ui.unmount()
})

test("a subagent's turn is not this load's first turn: blocks drawn before the main loop's are left alone", async ($, on) => {
  fakeEngine(on)
  await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true } as never)
  await $.turn.start({ text: 'background work', agentId: 'a1', turnId: 't3' } as never)

  const ui = await $.ui.mount(draw('r2'))
  await $.turn.start({ text: 'hi', turnId: 't4' } as never)
  await $.turn.complete({ reason: 'answer', answer: 'done', durationMs: 1, isAborted: false, turnId: 't4' } as never)
  await settle(500)
  expect(await ui.find({ type: 'Text', text: SUMMARY })).toBeUndefined()
  expect(await ui.find({ type: 'Text', text: /summarizing/ })).toBeUndefined()
  await ui.unmount()
})
