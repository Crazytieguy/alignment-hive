import type { On } from 'claude-code'
import { describe, expect, mock, test } from 'claude-code/testing'

const BAND = {
  plugin: 'hive',
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 140, scroll: { offset: 0, bodyRows: 19 }, view: {} },
}

// The check runs unawaited after session start; a test waits it out. setTimeout is the
// test runner's, outside the mods' library.
declare const setTimeout: (run: () => void, ms: number) => void
const settle = () => new Promise<void>(resolve => setTimeout(resolve, 20))

const plugin = (id: string, label: string) => ({ id, label, kind: 'plugin' })

/**
 * A fake CLI behind scripts/hive.sh: `notices` answers from what `upload exclude` and
 * `upload snooze` have done, and knows the session from the `knownFrom`th call on (a new session
 * is discovered after its first reply). `hold` keeps exclude and snooze running until released.
 */
function fakeHive(on: On, knownFrom = 1) {
  const calls: string[][] = []
  const noticesCalls = () => calls.filter(call => call[0] === 'notices').length
  const tools: Array<{ tool: string; command?: string; run_in_background?: boolean }> = []
  const toasts: string[] = []
  let isExcluded = false
  let isSnoozed = false
  let release: (() => void) | null = null
  const state = { hold: false, isReviewRunning: true }
  const clock = mock.clock(on)
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('session.id', async () => ({ value: 'abc' }))
  on('turn.complete', async () => ({ text: '' }) as never)
  on('prompt.submit', async (_$, e) => ({ text: e.text }) as never)
  on('ui.toast', async (_$, e) => {
    toasts.push(e.text)
    return { value: undefined } as never
  })
  on('process.run', async (_$, e) => {
    const ok = (exitCode: number, stdout: string) => ({
      value: { exitCode, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
    })
    if (e.argv[0] === 'pgrep') return ok(state.isReviewRunning ? 0 : 1, '')
    const args = e.argv.slice(2)
    calls.push(args)
    if (args[0] === 'upload' && state.hold) await new Promise<void>(resolve => (release = resolve))
    if (args[1] === 'exclude') isExcluded = true
    if (args[1] === 'snooze') isSnoozed = true
    const isSessionKnown = noticesCalls() >= knownFrom
    const rows = [
      isSnoozed
        ? { id: 'snoozed', severity: 'info', text: '4 sessions ready to upload to alignment-hive · uploads snoozed', actions: [plugin('review', 'Review sessions')], when: 'start' }
        : { id: 'uploading', severity: 'info', text: 'uploading 4 sessions to alignment-hive in 9m', actions: [plugin('snooze', 'Snooze 24h')], when: 'start' },
      { id: 'pending', severity: 'info', text: '19 sessions pending upload to alignment-hive · next in 1h 13m', actions: [plugin('review', 'Review sessions')], when: 'start' },
      // The CLI says nothing about a session it has not discovered.
      ...(!isSessionKnown
        ? []
        : isExcluded
          ? [{ id: 'session', severity: 'info', text: 'this session: excluded', actions: [], when: 'working' }]
          : [{ id: 'session', severity: 'info', text: 'this session uploads to alignment-hive after 24h idle', actions: [plugin('keep-private', 'Keep private')], when: 'working' }]),
    ]
    return ok(0, args[0] === 'notices' ? JSON.stringify({ notices: rows, isSessionKnown }) : 'done')
  })
  on('tool.call', async (_$, e) => {
    tools.push(e as never)
    return { result: { text: '' } } as never
  })
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
  return { calls, tools, toasts, clock, state, noticesCalls, release: () => release?.() }
}

const keys = async (ui: { findAll: (q: { type: 'Button' }) => Promise<Array<{ key?: string }>> }) =>
  (await ui.findAll({ type: 'Button' })).map(button => button.key)

describe('hive band', () => {
  test('the upload decisions show at the start; after the first message, this session and Keep private', async ($, on) => {
    fakeHive(on)
    await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
    await settle()

    for (const surface of ['terminal', 'desktop'] as const) {
      const ui = await $.ui.mount({ ...BAND, surface })
      expect(await keys(ui)).toEqual(['hive:uploading:snooze', 'hive:pending:review'])
      await ui.unmount()
    }

    await $.prompt.submit({ text: 'hi' } as never)
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    expect(await keys(ui)).toEqual(['hive:session:keep-private'])
    await ui.unmount()
  })

  test('Keep private says it is working, then the row shows the session excluded', async ($, on) => {
    const { calls, toasts, state, release } = fakeHive(on)
    await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
    await settle()
    await $.prompt.submit({ text: 'hi' } as never)

    state.hold = true
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    const pressed = ui.press({ key: 'hive:session:keep-private' })
    await settle()
    expect(await ui.find({ type: 'Text', text: /keeping private…/ })).toBeDefined()
    expect(await keys(ui)).toEqual([])

    release()
    await pressed
    expect(calls).toContainEqual(['upload', 'exclude', 'abc'])
    expect(await ui.find({ type: 'Text', text: /this session: excluded/ })).toBeDefined()
    expect(toasts).toEqual([])
    await ui.unmount()
  })

  test('Snooze snoozes uploads for a day and the row says so', async ($, on) => {
    const { calls } = fakeHive(on)
    await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
    await settle()
    const ui = await $.ui.mount({ ...BAND, surface: 'desktop' })
    await ui.press({ key: 'hive:uploading:snooze' })
    expect(calls).toContainEqual(['upload', 'snooze', '24h'])
    expect(await ui.find({ type: 'Text', text: /uploads snoozed/ })).toBeDefined()
    await ui.unmount()
  })

  test('Review sessions opens the page as a background task; the button returns once it stops', async ($, on) => {
    const { tools, clock, state } = fakeHive(on)
    await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
    await settle()
    const ui = await $.ui.mount({ ...BAND, surface: 'desktop' })
    await ui.press({ key: 'hive:pending:review' })
    await settle()
    expect(tools).toHaveLength(1)
    expect(tools[0]?.tool).toBe('Bash')
    expect(tools[0]?.run_in_background).toBe(true)
    expect(tools[0]?.command).toMatch(/scripts\/hive\.sh' upload review$/)
    expect(await ui.find({ type: 'Text', text: /review page open, stop it in \/tasks/ })).toBeDefined()
    expect(await ui.find({ key: 'hive:pending:review' })).toBeUndefined()

    await clock.advance(5_000) // still serving
    await settle()
    expect(await ui.find({ key: 'hive:pending:review' })).toBeUndefined()

    state.isReviewRunning = false // stopped in /tasks
    await clock.advance(10_000)
    await settle()
    expect(await ui.find({ key: 'hive:pending:review' })).toBeDefined()
    await ui.unmount()
  })

  test("this session's row comes once the first reply is stored, before the turn ends", async ($, on) => {
    const { calls, noticesCalls } = fakeHive(on, 2)
    await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
    await settle()
    await $.prompt.submit({ text: 'hi' } as never)
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    expect(await keys(ui)).toEqual([])

    const reply = (uuid: string) =>
      $.session.append({
        door: 'response',
        uuid,
        origin: { kind: 'model' },
        message: { type: 'assistant', role: 'assistant', content: [{ type: 'text', text: 'hello' }] },
      } as never)
    await reply('r1') // the first reply is on disk: the CLI knows the session
    await settle()
    expect(await keys(ui)).toEqual(['hive:session:keep-private'])

    await reply('r2') // known now: later replies ask nothing
    await settle()
    expect(noticesCalls()).toBe(2)
    // At the start the whole project, after the first message this session alone.
    expect(calls.filter(call => call[0] === 'notices').map(call => call.includes('--session-only'))).toEqual([false, true])
    await ui.unmount()
  })

  test('a turn refreshes until the CLI knows the session, then only every 15 minutes', async ($, on) => {
    const { clock, noticesCalls } = fakeHive(on, 2)
    const turn = async () => {
      await $.turn.complete({ reason: 'end' } as never)
      await settle()
    }
    await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
    await settle()
    expect(noticesCalls()).toBe(1)

    await turn() // not discovered at start: asks again, and now the CLI knows it
    expect(noticesCalls()).toBe(2)
    await turn()
    await turn()
    expect(noticesCalls()).toBe(2)

    await clock.advance(16 * 60 * 1000)
    await turn()
    expect(noticesCalls()).toBe(3)
  })
})
