import type { On } from 'claude-code'
import { describe, expect, mock, test } from 'claude-code/testing'

const NOW = Date.parse('2026-10-20T15:30:00Z')
const SESSION = 'abcd1234-0000-4000-8000-000000000000'
const ENTRY = { loc: 'feed0001:7', time: '2026-10-19T09:00:00Z', kind: 'user', text: 'Always use tabs in this repo, never spaces.' }
const REPLY = '- feed0001:7 | '
const usage = { input_tokens: 10, output_tokens: 10, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }

/**
 * The world beneath the plugin: a CLI with one past session (scripts/hive.sh), and models that answer `reply`, Sol
 * among them unless `isSolMissing`. `hold` keeps a model call running until its time is up; `recalledBefore` puts
 * a report of ENTRY in the current session.
 */
function world(on: On, { isSolMissing = false } = {}) {
  const asked: Array<string> = []
  const contexts: Array<ReadonlyArray<string>> = []
  const state = { hold: false, isCliSlow: false, recalledBefore: false, reply: REPLY, isDropped: false }
  const clock = mock.clock(on, { now: NOW })
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('session.id', async () => ({ value: SESSION }))
  on('prompt.submit', async (_$, e) => {
    contexts.push(e.context ?? [])
    return (state.isDropped ? { drop: 'a settings hook refused it' } : { text: e.text }) as never
  })
  const cli = (args: ReadonlyArray<string>): Array<unknown> => {
    if (args[0] === 'notices') return [{ notices: [], isSessionKnown: true }]
    if (args[1] === 'sessions') return [{ loc: 'abcd1234', end: '2026-10-20T15:29:00Z' }, { loc: 'feed0001', end: '2026-10-19T09:00:00Z' }]
    if (args[1] !== 'outline') return []
    const report = { loc: 'abcd1234:2', time: '2026-10-20T15:29:30Z', kind: 'user', origin: 'recall', recall: true, text: '[hive recall] Entries.\n\n> [user] Always use tabs in this repo, never spaces. (feed0001:7, 2026-10-19)' }
    return [ENTRY, { loc: 'abcd1234:1', time: '2026-10-20T15:29:00Z', kind: 'user', text: 'Format the file.' }, ...(state.recalledBefore ? [report] : [])]
  }
  on('process.run', async (_$, e) => {
    const stdout = cli(e.argv.slice(2)).map(r => JSON.stringify(r)).join('\n')
    return { value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } } as never
  })
  // Streamed in pieces that cut lines, as a pipe does.
  on('process.spawn', async function* (_$, e) {
    if (state.isCliSlow) await clock.sleep(60_000)
    const stdout = cli(e.argv.slice(2)).map(r => JSON.stringify(r)).join('\n')
    for (let i = 0; i < stdout.length; i += 7) yield { stream: 'stdout' as const, text: stdout.slice(i, i + 7) }
    return { value: { exitCode: 0, signal: null } }
  } as never)
  on('model.complete', async (_$, e) => {
    asked.push(e.model)
    if (state.hold) {
      await clock.sleep(e.timeoutMs ?? 0)
      return { value: { isAnswered: false, reason: 'aborted', usage } } as never
    }
    if (isSolMissing && e.model === 'gpt-6.1-sol') return { value: { isAnswered: false, reason: 'api-error', status: 404, error: 'not_found_error', usage } } as never
    return { value: { isAnswered: true, text: state.reply, usage } } as never
  })
  on('ui.status', async () => ({ value: undefined }) as never)
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
  return { asked, contexts, clock, state }
}

const start = ($: Parameters<Parameters<typeof test>[1]>[0]) => $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
const typed = (text: string) => ({ text, origin: { kind: 'composer' } }) as never

describe('automatic recall', () => {
  test('off: a prompt goes on as typed', async ($, on) => {
    const { asked, contexts } = world(on)
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    expect(asked).toEqual([])
    expect(contexts).toEqual([[]])
  })

  test('on: a typed prompt goes on with the cited entry, its loc and date', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts } = world(on)
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    expect(asked).toEqual(['gpt-6.1-sol'])
    expect(contexts[0]?.join('')).toContain('> [user] Always use tabs in this repo, never spaces. (feed0001:7, 2026-10-19)')
    expect(contexts[0]?.join('')).toContain('hive:retrieval')

    // A slash command and a message from another session start nothing.
    await $.prompt.submit({ text: '/model', command: { name: 'model', args: '' }, origin: { kind: 'composer' } } as never)
    await $.prompt.submit({ text: 'hi', origin: { kind: 'peer' } } as never)
    expect(asked).toHaveLength(1)
  })

  test('on: nothing found, or only what an earlier recall in this session reported, adds nothing', { options: { recall: true } }, async ($, on) => {
    const { contexts, state } = world(on)
    await start($)
    state.reply = 'none'
    await $.prompt.submit(typed('Format the file.'))
    state.reply = REPLY
    state.recalledBefore = true
    await $.prompt.submit(typed('And the other one.'))
    expect(contexts).toEqual([[], []])
  })

  test('on, auto without model-router: Sonnet answers, and Sol is not asked again', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts } = world(on, { isSolMissing: true })
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    await $.prompt.submit(typed('And the other one.'))
    expect(asked).toEqual(['gpt-6.1-sol', 'sonnet', 'sonnet'])
    expect(contexts[1]?.join('')).toContain('Always use tabs')
  })

  test('on: past 30 seconds the prompt goes on, saying recall did not finish', { options: { recall: true } }, async ($, on) => {
    const { contexts, clock, state } = world(on)
    await start($)
    state.hold = true
    const submitted = $.prompt.submit(typed('Format the file.'))
    await clock.advance(31_000)
    await submitted
    expect(contexts[0]?.join('')).toContain('did not finish')
  })

  test('on: a CLI read past 30 seconds holds the prompt no longer', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts, clock, state } = world(on)
    await start($)
    state.isCliSlow = true
    const submitted = $.prompt.submit(typed('Format the file.'))
    await clock.advance(31_000)
    await submitted
    expect(contexts[0]?.join('')).toContain('did not finish')
    expect(asked).toEqual([])
  })
})

/** The prompt's row as the session stores it, under `uuid`. */
const stored = ($: Parameters<Parameters<typeof test>[1]>[0], text: string, uuid: string) =>
  $.session.append({ message: { type: 'user', role: 'user', content: [{ type: 'text', text }] }, door: 'prompt', origin: { kind: 'composer' }, uuid } as never)

/** The prompt's row on `surface`: in the ctrl+o transcript (`isExpanded`), and on a fullscreen terminal. */
const row = (surface: 'terminal' | 'desktop', requestId: string, { isExpanded = false, isFullscreen = false } = {}) =>
  ({
    plugin: 'hive',
    surface,
    component: 'UserMessage',
    requestId,
    viewport: { columns: 150, rows: 50, isFullscreen },
    props: { text: 'Format the file.', origin: { kind: 'composer' }, isExpanded },
  }) as never

const LINE = /hive recalled 1 memory/

describe('what recall gave a prompt', () => {
  test('terminal: a line under the prompt counts the entries; ctrl+o shows them', { options: { recall: true } }, async ($, on) => {
    world(on)
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    await stored($, 'Format the file.', 'p1')

    // On the main screen no press reaches the transcript: ctrl+o is the way in.
    const compact = await $.ui.mount(row('terminal', 'p1'))
    expect((await compact.find({ type: 'Text', text: LINE }))?.text).toBe('hive recalled 1 memory (ctrl+o to expand)')
    expect(await compact.find({ type: 'Markdown' })).toBeUndefined()
    await compact.unmount()

    const expanded = await $.ui.mount(row('terminal', 'p1', { isExpanded: true }))
    expect((await expanded.find({ type: 'Text', text: LINE }))?.text).toBe('hive recalled 1 memory')
    // Each entry under who wrote it and when, then how far back recall read.
    expect(await expanded.find({ type: 'Text', text: /You · 2026-10-19 · feed0001/ })).toBeDefined()
    expect((await expanded.find({ type: 'Markdown', text: /Always use tabs/ }))?.text).toBe('Always use tabs in this repo, never spaces.')
    expect(await expanded.find({ type: 'Text', text: /since 2026-10-19/ })).toBeDefined()
    await expanded.unmount()

    // Another prompt's row, and one recall did not see, show nothing of it.
    await stored($, 'Something else.', 'p2')
    const other = await $.ui.mount(row('terminal', 'p2', { isExpanded: true }))
    expect(await other.find({ type: 'Text', text: /hive recall/ })).toBeUndefined()
    await other.unmount()
  })

  test('terminal: only ctrl+o says when recall found nothing', { options: { recall: true } }, async ($, on) => {
    const { state } = world(on)
    await start($)
    state.reply = 'none'
    await $.prompt.submit(typed('Format the file.'))
    await stored($, 'Format the file.', 'p1')
    const compact = await $.ui.mount(row('terminal', 'p1'))
    expect(await compact.find({ type: 'Text', text: /hive recall/ })).toBeUndefined()
    await compact.unmount()
    const expanded = await $.ui.mount(row('terminal', 'p1', { isExpanded: true }))
    expect(await expanded.find({ type: 'Text', text: /hive recall found nothing relevant/ })).toBeDefined()
    await expanded.unmount()
  })

  test('a dropped prompt leaves nothing for the same prompt typed again', { options: { recall: true } }, async ($, on) => {
    const { state } = world(on)
    await start($)
    state.isDropped = true
    await $.prompt.submit(typed('Format the file.'))
    state.isDropped = false
    state.reply = 'none'
    await $.prompt.submit(typed('Format the file.'))
    await stored($, 'Format the file.', 'p1')
    const expanded = await $.ui.mount(row('terminal', 'p1', { isExpanded: true }))
    expect(await expanded.find({ type: 'Text', text: /hive recall found nothing relevant/ })).toBeDefined()
    await expanded.unmount()
  })

  test('a press on the line opens and closes the entries: fullscreen terminal and desktop', { options: { recall: true } }, async ($, on) => {
    world(on)
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    await stored($, 'Format the file.', 'p1')

    for (const [surface, closed, open] of [
      ['terminal', 'hive recalled 1 memory', 'hive recalled 1 memory'],
      ['desktop', '▸ hive recalled 1 memory', '▾ hive recalled 1 memory'],
    ] as const) {
      const ui = await $.ui.mount(row(surface, 'p1', { isExpanded: surface === 'desktop', isFullscreen: true }))
      expect((await ui.find({ key: 'hive:recall' }))?.text).toBe(closed)
      expect(await ui.find({ type: 'Markdown', text: /Always use tabs/ })).toBeUndefined()
      await ui.press({ key: 'hive:recall' })
      expect((await ui.find({ key: 'hive:recall' }))?.text).toBe(open)
      expect(await ui.find({ type: 'Markdown', text: /Always use tabs/ })).toBeDefined()
      await ui.press({ key: 'hive:recall' })
      expect(await ui.find({ type: 'Markdown', text: /Always use tabs/ })).toBeUndefined()
      await ui.unmount()
    }
  })

  test('desktop: no line where recall found nothing', { options: { recall: true } }, async ($, on) => {
    const { state } = world(on)
    await start($)
    state.reply = 'none'
    await $.prompt.submit(typed('Format the file.'))
    await stored($, 'Format the file.', 'p1')
    const ui = await $.ui.mount(row('desktop', 'p1', { isExpanded: true }))
    expect(await ui.find({ type: 'Text', text: /hive recall/ })).toBeUndefined()
    expect(await ui.find({ key: 'hive:recall' })).toBeUndefined()
    await ui.unmount()
  })
})
