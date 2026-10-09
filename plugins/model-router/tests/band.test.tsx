import type { On } from 'claude-code'
import { describe, expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'

const BAND = {
  plugin: 'model-router',
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 140, scroll: { offset: 0, bodyRows: 19 }, view: {} },
}

const setup = [{ id: 'setup', label: 'Run setup', kind: 'command', command: '/model-router:setup' }]
const retryAction = { id: 'retry', label: 'Try again', kind: 'plugin' }
const bypassAction = { id: 'bypass', label: 'Bypass router', kind: 'plugin' }
const addAction = { id: 'add-models', label: 'Add to /model', kind: 'plugin' }

const customizeAction = { id: 'customize', label: 'Customize', kind: 'command', command: '/model-router:setup' }

// A pressed Try again holds the check open; a test waits a moment to see the row meanwhile.
// setTimeout is the test runner's, outside the mods' library.
declare const setTimeout: (run: () => void, ms: number) => void
const settle = () => new Promise<void>(resolve => setTimeout(resolve, 20))

/** Starts a session whose check prints these rows. */
async function start($: Engine, on: On, rows: unknown[], stored: Record<string, unknown> = {}) {
  mock.store(on, stored)
  on('classic.SessionStart', async () => ({}))
  on('process.run', async () => ({
    value: {
      exitCode: 0,
      stdout: JSON.stringify({ notices: rows }),
      stderr: '',
      isStdoutTruncated: false,
      isStderrTruncated: false,
    },
  }))
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
  await $.classic.SessionStart({ source: 'startup', cwd: '/tmp/project' })
}

/**
 * A session whose check prints `rows`; `settings` answers the binary's `settings` subcommand
 * (bootstrap.sh settings ...), whose argv the test reads back.
 */
async function startWithSettings($: Engine, on: On, rows: unknown[], settings: unknown) {
  const toasts: string[] = []
  const settingsCalls: string[][] = []
  const commands: string[] = []
  on('classic.SessionStart', async () => ({}))
  on('process.run', async (_$, e) => {
    const isSettings = e.argv[2] === 'settings'
    if (isSettings) settingsCalls.push(e.argv.slice(2))
    const stdout = JSON.stringify(isSettings ? settings : { notices: rows })
    return { value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('command.run', async (_$, e) => {
    commands.push(e.command)
    return { text: '' }
  })
  on('ui.toast', async (_$, e) => {
    toasts.push(e.text)
    return { value: undefined } as never
  })
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
  await $.classic.SessionStart({ source: 'startup', cwd: '/proj' })
  return { toasts, settingsCalls, commands }
}

describe('model-router settings buttons', () => {
  const newModel = {
    id: 'new-model',
    severity: 'action',
    text: 'GPT-6.1 Sol can be added to /model',
    actions: [addAction, customizeAction],
  }

  test('Add to /model has the binary apply the migration and says what it added, at what window', async ($, on) => {
    const { toasts, settingsCalls } = await startWithSettings($, on, [newModel], {
      applied: true,
      added: ['gpt-6.1-sol'],
      windows: { 'gpt-6.1-sol': 258400 },
    })
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:new-model:add-models' })
    expect(settingsCalls).toEqual([['settings', 'models', '--apply', '--project-dir', '/proj']])
    expect(toasts[0]).toBe('Added gpt-6.1-sol (compacts at 258K) to /model. Restart Claude Code to pick it; Customize runs setup.')
    expect(await ui.find({ type: 'Button' })).toBeUndefined()
    await ui.unmount()
  })

  test('Customize runs setup instead', async ($, on) => {
    const { commands, settingsCalls } = await startWithSettings($, on, [newModel], {})
    const ui = await $.ui.mount({ ...BAND, surface: 'desktop' })
    await ui.press({ key: 'model-router:new-model:customize' })
    expect(commands).toEqual(['model-router:setup'])
    expect(settingsCalls).toEqual([])
    await ui.unmount()
  })

  test('Bypass router has the binary edit the settings and says what to do next', async ($, on) => {
    const { toasts, settingsCalls } = await startWithSettings(
      $,
      on,
      [{ id: 'down', severity: 'urgent', text: 'down', actions: [bypassAction, retryAction] }],
      { status: 'done', files: ['/home/me/.claude/settings.json'] },
    )
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:down:bypass' })
    expect(settingsCalls).toEqual([['settings', 'bypass', '--project-dir', '/proj']])
    expect(toasts[0]).toMatch(/talks to Anthropic directly \(\/home\/me\/\.claude\/settings\.json\)\. Restart it/)
    await ui.unmount()
  })

  test('Bypass router passes on a shared project file it left alone', async ($, on) => {
    const { toasts } = await startWithSettings(
      $,
      on,
      [{ id: 'down', severity: 'urgent', text: 'down', actions: [bypassAction, retryAction] }],
      { status: 'shared-file', file: '/proj/.claude/settings.json' },
    )
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:down:bypass' })
    expect(toasts[0]).toMatch(/^\/proj\/\.claude\/settings\.json routes through model-router and collaborators share it/)
    await ui.unmount()
  })
})

describe('model-router check', () => {
  test('runs, awaited, at startup and again at /clear, not at a resume', async ($, on) => {
    let runs = 0
    on('classic.SessionStart', async () => ({}))
    on('process.run', async () => {
      runs++
      return { value: { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
    })
    await $.classic.SessionStart({ source: 'startup', cwd: '/tmp/project' })
    expect(runs).toBe(1)
    await $.classic.SessionStart({ source: 'resume', cwd: '/tmp/project' })
    expect(runs).toBe(1)
    await $.classic.SessionStart({ source: 'clear', cwd: '/tmp/project' })
    expect(runs).toBe(2)
  })
})

describe('model-router band', () => {
  const newModel = { id: 'new-model', severity: 'action', text: 'GPT-6.1 Sol is available', version: '0.1.36', actions: setup }

  test('a new-model row dismissed for this plugin version stays hidden', async ($, on) => {
    await start($, on, [newModel], { 'dismissed:new-model': '0.1.36' })
    const ui = await $.ui.mount({ ...BAND, surface: 'desktop' })
    expect(await ui.find({ key: 'model-router:new-model:setup' })).toBeUndefined()
    await ui.unmount()
  })

  test('a dismissal from an earlier version shows the row again; a new one holds', async ($, on) => {
    await start($, on, [newModel], { 'dismissed:new-model': '0.1.35' })
    const ui = await $.ui.mount({ ...BAND, surface: 'desktop' })
    expect(await ui.find({ key: 'model-router:new-model:setup' })).toBeDefined()
    await ui.press({ key: 'model-router:new-model:dismiss' })
    await ui.unmount()
    const again = await $.ui.mount({ ...BAND, surface: 'desktop' })
    expect(await again.find({ key: 'model-router:new-model:setup' })).toBeUndefined()
    await again.unmount()
  })

  test('the down row offers Bypass router and Try again, in a narrow band too', async ($, on) => {
    await start($, on, [{ id: 'down', severity: 'urgent', text: 'not running and could not be restarted, so requests fail', actions: [bypassAction, retryAction] }])
    for (const surface of ['terminal', 'desktop'] as const) {
      const ui = await $.ui.mount({ ...BAND, props: { ...BAND.props, bodyColumns: 60 }, surface })
      expect((await ui.findAll({ type: 'Button' })).map(button => button.key)).toEqual([
        'model-router:down:bypass',
        'model-router:down:retry',
        'model-router:down:dismiss',
      ])
      await ui.unmount()
    }
  })

  test('Try again says it is restarting, runs the check again, and a recovered router clears the row', async ($, on) => {
    let runs = 0
    const gate: { release?: () => void } = {}
    on('classic.SessionStart', async () => ({}))
    on('process.run', async () => {
      runs++
      if (runs === 2) await new Promise<void>(resolve => (gate.release = resolve))
      const rows = runs === 1 ? [{ id: 'down', severity: 'urgent', text: 'not running', actions: [retryAction] }] : []
      return { value: { exitCode: 0, stdout: JSON.stringify({ notices: rows }), stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
    })
    on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
    await $.classic.SessionStart({ source: 'startup', cwd: '/tmp/project' })

    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    const pressed = ui.press({ key: 'model-router:down:retry' })
    await settle()
    expect(await ui.find({ type: 'Text', text: /restarting…/ })).toBeDefined()
    expect(await ui.find({ key: 'model-router:down:retry' })).toBeUndefined()
    gate.release?.()
    await pressed
    expect(runs).toBe(2)
    expect(await ui.find({ type: 'Button' })).toBeUndefined()
    await ui.unmount()
  })
})
