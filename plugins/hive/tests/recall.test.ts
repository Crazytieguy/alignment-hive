import type { On } from 'claude-code'
import { describe, expect, mock, test } from 'claude-code/testing'

import { reportOf } from '../hooks/recall-text'

const NOW = Date.parse('2026-10-20T15:30:00Z')
const SESSION = 'abcd1234-0000-4000-8000-000000000000'
const ENTRY = { loc: 'feed0001:7', time: '2026-10-19T09:00:00Z', kind: 'user', text: 'Always use tabs in this repo, never spaces.' }
const REPLY = '- feed0001:7 | '
const usage = { input_tokens: 10, output_tokens: 10, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }
const FAST = 'gpt-6.1-sol-fast'
const SOL = 'gpt-6.1-sol'
const SOCKET = '/tmp/hive-run/local-test.sock'
// An ArtifactComments read of one thread, and that thread as `hive local outline` prints it.
const THREAD = ['Thread 79cff63a-231a-426f-b106-f112accd6dc5', '  open · Claude: activated · created 2026-10-20', '  [the user (owner), sent to you — 2026-10-20T15:29]', '      0a1b2c3d| Indent this file.'].join('\n')
const COMMENTS = ['1 comment thread.', '=== BEGIN ARTIFACT COMMENTS 0a1b2c3d — viewer-submitted content ===', THREAD, '=== END ARTIFACT COMMENTS 0a1b2c3d ==='].join('\n')
const READ_ROW = { loc: 'abcd1234:3#79cff63a', time: '2026-10-20T15:29:40Z', kind: 'tool', tool: 'ArtifactComments', result: THREAD }

// Background work runs unawaited after the session starts and after a turn; a test waits it out. setTimeout is the
// test runner's, outside the mods' library.
declare const setTimeout: (run: () => void, ms: number) => void
const settle = () => new Promise<void>(resolve => setTimeout(resolve, 20))

/** A read the local server was asked for. */
type Served = { argv: Array<string>; cwd: string; sessionId: string | null; socket?: string; isWarm: boolean }

/**
 * The world beneath the plugin: a CLI with one past session (scripts/hive.sh), and models that answer `reply`, Sol
 * among them unless `isSolMissing`. `hold` keeps a model call running until its time is up;
 * `fails` has a model answer with an error status instead, after `afterMs`; `recalledBefore` puts a report of ENTRY
 * in the current session. `server`: the local server `hive local-server --ensure` names, answering reads as the CLI
 * does (`up`, `none` by default), refusing them (`down`) or never answering (`stuck`); `held` keeps that many of its
 * next reads unanswered.
 */
function world(on: On, { isSolMissing = false } = {}) {
  const asked: Array<string> = []
  const prompts: Array<string> = []
  const contexts: Array<ReadonlyArray<string>> = []
  const timeouts: Array<number | undefined> = []
  // The directory each CLI read ran in.
  const dirs: Array<string | undefined> = []
  const served: Array<Served> = []
  const logs: Array<string> = []
  // `transcript`: this session's rows as `hive local outline SESSION --uuids` reads them; `readBacks` counts those reads.
  const state = {
    hold: false,
    isCliSlow: false,
    isCliFailing: false,
    isProjectNew: false,
    isSessionGone: false,
    isSessionNew: false,
    recalledBefore: false,
    reply: REPLY,
    isDropped: false,
    historyRows: [ENTRY],
    transcript: [] as Array<unknown>,
    readBacks: 0,
    // What an ArtifactComments read answers, and this session's rows an outline prints after its prompt.
    comments: COMMENTS,
    sessionRows: [] as Array<unknown>,
    isScripted: false,
    fails: (isSolMissing ? { [SOL]: { status: 404 } } : {}) as Record<string, { status: number; afterMs?: number }>,
    server: 'none' as 'none' | 'up' | 'down' | 'stuck',
    held: 0,
    ensures: 0,
  }
  const clock = mock.clock(on, { now: NOW })
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('turn.complete', async () => ({ text: '' }) as never)
  on('session.id', async () => ({ value: SESSION }))
  on('session.root', async () => ({ value: '/tmp/project' }))
  on('tool.call', { tool: 'ArtifactComments' }, async () => ({ result: state.comments, text: state.comments }) as never)
  on('prompt.submit', async (_$, e) => {
    contexts.push(e.context ?? [])
    return (state.isDropped ? { drop: 'a settings hook refused it' } : { text: e.text }) as never
  })
  const cli = (args: ReadonlyArray<string>): Array<unknown> => {
    if (args[0] === 'notices') return [{ notices: [], isSessionKnown: true }]
    // A project with no transcripts yet lists none (and the CLI exits 0); a session gone since the listing fails its
    // batch; a new one has nothing to outline yet.
    if (args[1] === 'sessions' && state.isProjectNew) return []
    if (args[1] === 'sessions')
      return [
        { loc: 'abcd1234', end: '2026-10-20T15:29:00Z', ...(state.isScripted ? { scripted: true } : {}) },
        ...(state.isSessionNew ? [{ loc: 'beef0001', end: '2026-10-20T15:00:00Z' }] : []),
        ...(state.isSessionGone ? [{ loc: 'dead0001', end: '2026-10-19T12:00:00Z' }] : []),
        { loc: 'feed0001', end: '2026-10-19T09:00:00Z' },
      ]
    if (args[1] !== 'outline' || isFailing(args)) return []
    if (args.includes('--uuids')) {
      state.readBacks++
      return state.transcript
    }
    const report = { loc: 'abcd1234:2', time: '2026-10-20T15:29:30Z', kind: 'user', origin: 'recall', recall: true, text: '[hive recall] Entries.\n\n> [user] Always use tabs in this repo, never spaces. (feed0001:7, 2026-10-19)' }
    return [
      ...(args.includes('feed0001') ? state.historyRows : []),
      ...(args.includes('abcd1234') ? [{ loc: 'abcd1234:1', time: '2026-10-20T15:29:00Z', kind: 'user', text: 'Format the file.' }, ...(state.recalledBefore ? [report] : []), ...state.sessionRows] : []),
    ]
  }
  const isFailing = (args: ReadonlyArray<string>) => state.isCliFailing || (state.isSessionGone && args.includes('dead0001'))
  on('process.run', async (_$, e) => {
    const args = e.argv.slice(2)
    if (args[0] === 'local-server') state.ensures++
    const stdout = args[0] === 'local-server' ? JSON.stringify(state.server === 'none' ? {} : { socket: SOCKET }) : cli(args).map(r => JSON.stringify(r)).join('\n')
    return { value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } } as never
  })
  on('http.fetch', async (_$, e) => {
    // `argv`: what follows `hive local`, its only verbs served.
    const { argv, cwd, sessionId } = JSON.parse(e.init?.body ?? '{}') as { argv: Array<string>; cwd: string; sessionId: string | null }
    const isWarm = e.init?.headers?.['x-hive-warm'] === '1'
    served.push({ argv, cwd, sessionId, socket: e.init?.socketPath, isWarm })
    if (state.server === 'stuck' || (state.held > 0 && state.held--)) await clock.sleep(600_000)
    if (state.server === 'down') throw new Error('connect ECONNREFUSED')
    if (!['sessions', 'outline'].includes(argv[0]!)) return { value: { status: 400, ok: false, headers: {}, text: '' } } as never
    const code = isFailing(['local', ...argv]) ? 2 : 0
    const stdout = cli(['local', ...argv]).map(r => JSON.stringify(r)).join('\n')
    return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify({ code, stdout, stderr: '' }) } } as never
  })
  // Streamed in pieces that cut lines, as a pipe does.
  on('process.spawn', async function* (_$, e) {
    dirs.push(e.cwd)
    if (state.isCliSlow) await clock.sleep(60_000)
    const stdout = cli(e.argv.slice(2)).map(r => JSON.stringify(r)).join('\n')
    for (let i = 0; i < stdout.length; i += 7) yield { stream: 'stdout' as const, text: stdout.slice(i, i + 7) }
    return { value: { code: isFailing(e.argv.slice(2)) ? 2 : 0, signal: null } }
  } as never)
  on('model.complete', async (_$, e) => {
    asked.push(e.model)
    prompts.push(JSON.stringify(e.prompt))
    timeouts.push(e.timeoutMs)
    if (state.hold) {
      await clock.sleep(e.timeoutMs ?? 0)
      return { value: { isAnswered: false, reason: 'aborted', usage } } as never
    }
    const fails = state.fails[e.model]
    if (fails?.afterMs) await clock.sleep(fails.afterMs)
    if (fails) return { value: { isAnswered: false, reason: 'api-error', status: fails.status, error: fails.status === 429 ? 'rate_limit' : 'not_found_error', usage } } as never
    return { value: { isAnswered: true, text: state.reply, usage } } as never
  })
  on('ui.status', async () => ({ value: undefined }) as never)
  on('ui.log', async (_$, e) => {
    logs.push(e.text)
    return { value: undefined } as never
  })
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
  return { asked, contexts, clock, dirs, logs, prompts, served, state, timeouts }
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
    expect(asked).toEqual([SOL])
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
    expect(asked).toEqual([SOL, 'sonnet', 'sonnet'])
    expect(contexts[1]?.join('')).toContain('Always use tabs')
  })

  test('on, auto without model-router: a 4xx from Sonnet fails only that prompt, and Sonnet is asked again', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts, state } = world(on, { isSolMissing: true })
    await start($)
    state.fails.sonnet = { status: 400 }
    await $.prompt.submit(typed('Format the file.'))
    delete state.fails.sonnet
    await $.prompt.submit(typed('And the other one.'))
    await $.prompt.submit(typed('And a third.'))
    expect(asked).toEqual([SOL, 'sonnet', 'sonnet', 'sonnet'])
    const found = expect.stringContaining('Always use tabs')
    expect(contexts.map(c => c.join(''))).toEqual([expect.stringContaining('did not finish'), found, found])
  })

  test('on, Sonnet chosen by name: a 4xx fails only that prompt', { options: { recall: true, recallModel: 'sonnet' } }, async ($, on) => {
    const { asked, contexts, state } = world(on)
    await start($)
    state.fails.sonnet = { status: 400 }
    await $.prompt.submit(typed('Format the file.'))
    delete state.fails.sonnet
    await $.prompt.submit(typed('And the other one.'))
    expect(asked).toEqual(['sonnet', 'sonnet'])
    expect(contexts[1]?.join('')).toContain('Always use tabs')
  })

  test('on, auto: Sonnet, after Sol turned out missing, gets its own history and the time left', { options: { recall: true } }, async ($, on) => {
    const { asked, clock, prompts, state, timeouts } = world(on)
    // A row just ahead of the first attempt's clock tells a history built at the second attempt's clock.
    const later = 'A newer entry in the same outline.'
    state.historyRows.push({ ...ENTRY, loc: 'feed0001:8', text: later, time: new Date(NOW + 1_000).toISOString() })
    state.fails[SOL] = { status: 404, afterMs: 5_000 }
    await start($)
    const submitted = $.prompt.submit(typed('Format the file.'))
    await clock.advance(5_000)
    await submitted
    expect(asked).toEqual([SOL, 'sonnet'])
    expect(timeouts).toEqual([30_000, 25_000])
    expect(prompts[0]).not.toContain(later)
    expect(prompts[1]).toContain(later)
  })

  test('on, auto: when Sol fails, the prompt goes on without recall, not on Sonnet', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts, state } = world(on)
    state.fails[SOL] = { status: 429 }
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    await $.prompt.submit(typed('And the other one.'))
    expect(asked).toEqual([SOL, SOL])
    expect(contexts.map(c => c.join(''))).toEqual([expect.stringContaining('did not finish'), expect.stringContaining('did not finish')])
  })

  test('on, a model chosen by name: only it is asked, and once missing, not again', { options: { recall: true, recallModel: FAST } }, async ($, on) => {
    const { asked, contexts, state } = world(on)
    await start($)
    state.fails[FAST] = { status: 429 }
    await $.prompt.submit(typed('Format the file.'))
    state.fails[FAST] = { status: 404 }
    await $.prompt.submit(typed('And the other one.'))
    await $.prompt.submit(typed('And a third.'))
    expect(asked).toEqual([FAST, FAST])
    expect(contexts.map(c => c.join(''))).toEqual([1, 2, 3].map(() => expect.stringContaining('did not finish')))
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

  test('on: a CLI read past 30 seconds holds the prompt no longer, and no model call starts after', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts, clock, state } = world(on)
    await start($)
    state.isCliSlow = true
    const submitted = $.prompt.submit(typed('Format the file.'))
    await clock.advance(31_000)
    await submitted
    expect(contexts[0]?.join('')).toContain('did not finish')
    await clock.advance(60_000)
    expect(asked).toEqual([])
  })

  test('on: the model call gets the time left of the 30 seconds', { options: { recall: true } }, async ($, on) => {
    const { timeouts } = world(on)
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    expect(timeouts).toEqual([30_000])
  })

  for (const server of ['none', 'up'] as const) {
    const through = server === 'up' ? 'the local server' : 'the CLI'

    test(`on: a session gone since the listing costs only itself; the rest of its batch is read (through ${through})`, { options: { recall: true } }, async ($, on) => {
      const { contexts, dirs, state } = world(on)
      state.server = server
      await start($)
      state.isSessionGone = true
      await $.prompt.submit(typed('Format the file.'))
      expect(contexts[0]?.join('')).toContain('> [user] Always use tabs in this repo, never spaces. (feed0001:7, 2026-10-19)')
      expect(dirs.length > 0).toBe(server === 'none')
    })

    test(`on: a CLI read that fails partway is no history at all (through ${through})`, { options: { recall: true } }, async ($, on) => {
      const { asked, contexts, dirs, state } = world(on)
      state.server = server
      await start($)
      state.isCliFailing = true
      await $.prompt.submit(typed('Format the file.'))
      expect(asked).toEqual([])
      expect(contexts[0]?.join('')).toContain('did not finish')
      expect(dirs.length > 0).toBe(server === 'none')
    })
  }

  test('on: a project with no transcripts yet finds nothing', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts, state } = world(on)
    await start($)
    state.isProjectNew = true
    await $.prompt.submit(typed('Format the file.'))
    expect(asked).toEqual([])
    expect(contexts).toEqual([[]])
  })

  test('on: the CLI reads the project the session started in, wherever a shell cd took it', { options: { recall: true } }, async ($, on) => {
    const { contexts, dirs } = world(on)
    on('session.cwd', async () => ({ value: '/tmp/project/notes' }))
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    expect(contexts[0]?.join('')).toContain('> [user] Always use tabs in this repo, never spaces. (feed0001:7, 2026-10-19)')
    expect(dirs.length).toBeGreaterThan(0)
    expect(new Set(dirs)).toEqual(new Set(['/tmp/project']))
  })
})

describe('recall on comment reads', () => {
  const readComments = ($: Parameters<typeof start>[0], more: Record<string, unknown> = {}) =>
    $.tool.call({ tool: 'ArtifactComments', action: 'read', url: 'https://claude.ai/artifact/abc', ...more } as never)

  test('off: a read comes back as the tool answered', async ($, on) => {
    const { asked } = world(on)
    await start($)
    const read = await readComments($)
    expect(asked).toEqual([])
    expect(read.context).toBeUndefined()
    expect(read.text).toBe(COMMENTS)
  })

  test('on: a thread new to the session comes with the report on it; once the session has read it, nothing more', { options: { recall: true } }, async ($, on) => {
    const { asked, prompts, state } = world(on)
    await start($)
    const read = await readComments($)
    expect(asked).toEqual([SOL])
    expect(prompts[0]).toContain('read these comments the user left on a page, new to it:\\n\\n<new_comments>\\nThread 79cff63a')
    expect(prompts[0]).not.toContain('BEGIN ARTIFACT COMMENTS')
    expect(read.text).toBe(COMMENTS)
    const report = read.context?.join('') ?? ''
    expect(report).toStartWith('[hive recall] On the comments just read: entries')
    expect(report).toContain('> [user] Always use tabs in this repo, never spaces. (feed0001:7, 2026-10-19)')

    state.sessionRows = [READ_ROW]
    const again = await readComments($)
    expect(asked).toEqual([SOL])
    expect(again.context).toBeUndefined()
  })

  test('on: a thread a recall ran on this turn gets none again in it, changed or not; in the next turn it does', { options: { recall: true } }, async ($, on) => {
    const { asked, state } = world(on)
    await start($)
    await readComments($)
    state.sessionRows = [READ_ROW]
    // Read whole after a listing elided a comment: other text, the same thread.
    state.comments = COMMENTS.replace('Indent this file.', 'Indent this file.\n  [the user (owner) — 2026-10-20T15:29]\n      0a1b2c3d| With tabs.')
    expect((await readComments($)).context).toBeUndefined()
    expect(asked).toEqual([SOL])
    // A subagent's turn ending is not the main agent's.
    await $.turn.complete({ reason: 'end', agentId: 'child' } as never)
    expect((await readComments($)).context).toBeUndefined()
    await $.turn.complete({ reason: 'end' } as never)
    expect((await readComments($)).context?.join('')).toContain('On the comments just read')
    expect(asked).toEqual([SOL, SOL])
  })

  test('on: a subagent read, another action, a session a script ran, and a recall that found nothing or did not finish add nothing', { options: { recall: true } }, async ($, on) => {
    const { asked, clock, state } = world(on)
    await start($)
    expect((await readComments($, { agentId: 'a1' })).context).toBeUndefined()
    expect((await readComments($, { action: 'reply', thread_id: '79cff63a', text: 'Done.' })).context).toBeUndefined()
    state.isScripted = true
    expect((await readComments($)).context).toBeUndefined()
    state.isScripted = false
    expect(asked).toEqual([])

    state.reply = 'none'
    expect((await readComments($)).context).toBeUndefined()
    await $.turn.complete({ reason: 'end' } as never)
    state.reply = REPLY
    state.hold = true
    const held = readComments($)
    await clock.advance(31_000)
    expect((await held).context).toBeUndefined()
    expect(asked).toEqual([SOL, SOL])
  })
})

describe('the local server', () => {
  test('reads for recall when it runs: no CLI starts, and it reads the project the session started in', { options: { recall: true } }, async ($, on) => {
    const { asked, contexts, dirs, logs, served, state } = world(on)
    state.server = 'up'
    on('session.cwd', async () => ({ value: '/tmp/project/notes' }))
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    await $.prompt.submit(typed('And the other one.'))
    expect(asked).toEqual([SOL, SOL])
    expect(contexts[1]?.join('')).toContain('> [user] Always use tabs in this repo, never spaces. (feed0001:7, 2026-10-19)')
    expect(dirs).toEqual([])
    const reads = ['sessions -n 500 --brief', 'outline abcd1234 feed0001 --replies --no-agents --clip 0']
    expect(served.map(s => s.argv.join(' '))).toEqual([...reads, ...reads])
    expect(new Set(served.map(s => `${s.socket} ${s.cwd} ${s.sessionId} ${s.isWarm}`))).toEqual(new Set([`${SOCKET} /tmp/project ${SESSION} false`]))
    // Started once, then known.
    expect(state.ensures).toBe(1)
    expect(logs).toContainEqual(expect.stringMatching(/^hive recall: read 2 sessions in \d+ ms$/))
    expect(logs).toContainEqual(expect.stringMatching(/^hive recall: gpt-6\.1-sol answered in \d+ ms/))
  })

  test('a server that fails is started again and asked once more; then the CLI reads, for the rest of the recall', { options: { recall: true } }, async ($, on) => {
    const { contexts, dirs, served, state } = world(on)
    state.server = 'down'
    await start($)
    await $.prompt.submit(typed('Format the file.'))
    expect(contexts[0]?.join('')).toContain('Always use tabs')
    expect(served.map(s => s.argv[0])).toEqual(['sessions', 'sessions'])
    expect(state.ensures).toBe(2)
    expect(dirs).toHaveLength(2)
  })

  test('a server with no answer in 5 seconds: the CLI reads instead, and the next recall starts it again', { options: { recall: true } }, async ($, on) => {
    const { clock, contexts, dirs, served, state, timeouts } = world(on)
    state.server = 'stuck'
    await start($)
    const submitted = $.prompt.submit(typed('Format the file.'))
    await clock.advance(5_000)
    await submitted
    expect(contexts[0]?.join('')).toContain('Always use tabs')
    expect(timeouts).toEqual([25_000])
    expect(served).toHaveLength(1)
    expect(dirs).toHaveLength(2)

    state.server = 'up'
    await $.prompt.submit(typed('And the other one.'))
    expect(state.ensures).toBe(2)
    expect(served).toHaveLength(3)
    expect(dirs).toHaveLength(2)
  })

  test('warms after the session starts and after each main turn, reading what recall will; never through the CLI', { options: { recall: true } }, async ($, on) => {
    const { clock, dirs, served, state } = world(on)
    state.server = 'up'
    await start($)
    await settle()
    await clock.advance(1)
    await settle()
    // It reads as recall does, as warm reads, which name no session.
    expect(served.map(s => [...s.argv.slice(0, 4), s.isWarm, s.sessionId])).toEqual([
      ['sessions', '-n', '500', '--brief', true, null],
      ['outline', 'abcd1234', 'feed0001', '--replies', true, null],
    ])

    await $.prompt.submit(typed('Format the file.'))
    served.length = 0
    await $.turn.complete({ reason: 'end', agentId: 'child' } as never)
    await settle()
    await clock.advance(1)
    await settle()
    expect(served).toEqual([])

    // A later one reads the listing anew, and the sessions in it now.
    state.isSessionNew = true
    await $.turn.complete({ reason: 'end' } as never)
    await settle()
    await clock.advance(1)
    await settle()
    expect(served.map(s => [...s.argv.slice(0, 4), s.isWarm, s.sessionId])).toEqual([
      ['sessions', '-n', '500', '--brief', true, null],
      ['outline', 'abcd1234', 'beef0001', 'feed0001', true, null],
    ])
    expect(dirs).toEqual([])
  })

  test('a warm read with no answer in 5 seconds stops the warm, and the server stays known', { options: { recall: true } }, async ($, on) => {
    const { asked, clock, logs, served, state } = world(on)
    state.server = 'up'
    state.held = 1
    await start($)
    await settle()
    await clock.advance(1)
    await settle()
    await clock.advance(5_000)
    await settle()
    expect(served).toHaveLength(1)
    expect(logs).toContain('hive local server: no answer in 5000 ms; warm stopped')

    await $.prompt.submit(typed('Format the file.'))
    expect(asked).toEqual([SOL])
    expect(served.map(s => s.argv[0])).toEqual(['sessions', 'sessions', 'outline'])
    expect(state.ensures).toBe(1)
  })

  test('a recall stops a warm at once, and none starts during one', { options: { recall: true } }, async ($, on) => {
    const { clock, contexts, served, state } = world(on)
    state.server = 'up'
    state.held = 1
    await start($)
    await settle()
    await clock.advance(1)
    await settle()
    expect(served).toHaveLength(1)

    // The model call runs to the time limit: a turn's end meanwhile warms nothing.
    state.hold = true
    const submitted = $.prompt.submit(typed('Format the file.'))
    await settle()
    expect(served.map(s => s.argv[0])).toEqual(['sessions', 'sessions', 'outline'])
    await $.turn.complete({ reason: 'end' } as never)
    await settle()
    await clock.advance(1)
    await settle()
    expect(served).toHaveLength(3)
    await clock.advance(31_000)
    await submitted
    expect(contexts[0]?.join('')).toContain('did not finish')
  })

  test('without one, a warm reads nothing', { options: { recall: true } }, async ($, on) => {
    const { clock, dirs, served, state } = world(on)
    await start($)
    await settle()
    await clock.advance(1)
    await settle()
    expect(state.ensures).toBe(1)
    expect(served).toEqual([])
    expect(dirs).toEqual([])
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

  test('a resumed session: earlier prompts show what recall gave them, read back once from the transcript', { options: { recall: true } }, async ($, on) => {
    const { clock, state } = world(on)
    await start($)
    // This process recalled nothing for p1; the transcript, written by another, says otherwise.
    state.reply = 'none'
    await $.prompt.submit(typed('Format the file.'))
    await stored($, 'Format the file.', 'p1')
    const found = [{ loc: 'feed0001:7', label: 'user', text: 'Always use tabs in this repo, never spaces.', date: '2026-10-19', context: '' }]
    const report = (uuid: string) => ({ loc: 'abcd1234:9', uuid, kind: 'user', origin: 'recall', recall: true, text: reportOf(found, '2026-10-17') })
    state.transcript = [
      { loc: 'abcd1234:1', uuid: 'old1', kind: 'user', text: 'Format the file.' },
      report('r1'),
      { loc: 'abcd1234:3', uuid: 'old2', kind: 'user', text: 'Something else.' },
      { loc: 'abcd1234:4', uuid: 'p1', kind: 'user', text: 'Format the file.' },
      report('r2'),
    ]

    const ui = await $.ui.mount(row('terminal', 'old1'))
    expect(await ui.find({ type: 'Text', text: LINE })).toBeUndefined()
    await clock.advance(1)
    expect((await ui.find({ type: 'Text', text: LINE }))?.text).toBe('hive recalled 1 memory (ctrl+o to expand)')
    await ui.unmount()

    // A row the transcript has nothing for reads it no more.
    const other = await $.ui.mount(row('terminal', 'old2', { isExpanded: true }))
    await clock.advance(1)
    expect(await other.find({ type: 'Text', text: /hive recall/ })).toBeUndefined()
    await other.unmount()
    expect(state.readBacks).toBe(1)

    const own = await $.ui.mount(row('terminal', 'p1', { isExpanded: true }))
    expect(await own.find({ type: 'Text', text: /hive recall found nothing relevant/ })).toBeDefined()
    await own.unmount()
  })
})
