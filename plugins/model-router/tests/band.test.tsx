import type { On } from 'claude-code'
import { describe, expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'

import { fitsToast } from '../hooks/register'

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
  mock.env(on, { HOME: '/home/me' })
  mock.store(on, {})
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
      windows: { 'gpt-6.1-sol': 475000 },
    })
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:new-model:add-models' })
    expect(settingsCalls).toEqual([['settings', 'models', '--apply', '--project-dir', '/proj']])
    expect(toasts[0]).toBe('Added to /model; restart Claude Code to pick. Compacts gpt-6.1-sol at 475K.')
    expect(await ui.find({ type: 'Button' })).toBeUndefined()
    await ui.unmount()
  })

  test('Migrate has the binary move the windows and says at what window', async ($, on) => {
    const windowsRow = {
      id: 'windows',
      severity: 'action',
      text: 'context windows are still set the old way',
      actions: [{ id: 'windows', label: 'Migrate', kind: 'plugin' }, customizeAction],
    }
    const { toasts, settingsCalls } = await startWithSettings($, on, [windowsRow], {
      applied: true,
      file: '/home/me/.claude/settings.json',
      declared: 1000000,
      windows: { 'gpt-6-astra': 258400, 'gpt-6-luna': 258400, 'grok-4.7': 500000 },
    })
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:windows:windows' })
    expect(settingsCalls).toEqual([['settings', 'windows', '--apply', '--project-dir', '/proj']])
    expect(toasts[0]).toBe('Restart Claude Code to compact gpt-6-astra, gpt-6-luna at 258K; grok-4.7 at 500K.')
    expect(await ui.find({ type: 'Button' })).toBeUndefined()
    await ui.unmount()
  })

  test('Migrate names the Muse windows at the 1M cap', async ($, on) => {
    const windowsRow = {
      id: 'windows',
      severity: 'action',
      text: 'no context window set for muse-wonder-1.0, muse-wonder-full-1.0',
      actions: [{ id: 'windows', label: 'Migrate', kind: 'plugin' }, customizeAction],
    }
    const { toasts } = await startWithSettings($, on, [windowsRow], {
      applied: true,
      file: '/home/me/.claude/settings.json',
      declared: null,
      windows: { 'muse-wonder-1.0': 1000000, 'muse-wonder-full-1.0': 1000000 },
    })
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:windows:windows' })
    expect(toasts[0]).toBe(
      'Restart Claude Code to compact muse-wonder-1.0, muse-wonder-full-1.0 at 1000K (in ~/.claude/settings.json).',
    )
    expect(fitsToast(toasts[0]!)).toBe(true)
    await ui.unmount()
  })

  test('Update has the binary raise a window Codex raised and names the new one', async ($, on) => {
    const windowsRow = {
      id: 'windows-update',
      severity: 'action',
      text: 'Codex raised the default context window of gpt-6.1-sol',
      actions: [{ id: 'windows', label: 'Update', kind: 'plugin' }, customizeAction],
    }
    const { toasts, settingsCalls } = await startWithSettings($, on, [windowsRow], {
      applied: true,
      file: '/home/me/.claude/settings.json',
      declared: null,
      windows: { 'gpt-6.1-sol': 475000 },
    })
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:windows-update:windows' })
    expect(settingsCalls).toEqual([['settings', 'windows', '--apply', '--project-dir', '/proj']])
    expect(toasts[0]).toBe('Restart Claude Code to compact gpt-6.1-sol at 475K (in ~/.claude/settings.json).')
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
    expect(toasts[0]).toBe('Bypassed the router. Restart Claude Code to get back online, then run /model-router:setup.')
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
    expect(toasts[0]).toBe(
      `In shared .claude/settings.json, remove the router's wiring (and a GPT "model"), then restart Claude Code.`,
    )
    await ui.unmount()
  })

  // The longest toasts these buttons raise: every model the binary ships, the other outcomes.
  const longest: [string, typeof addAction, unknown][] = [
    [
      'every shipped model added',
      addAction,
      { applied: true, added: ['gpt-6-astra', 'gpt-6-luna', 'gpt-6.1-sol'], windows: { 'gpt-6-astra': 258400, 'gpt-6-luna': 258400, 'gpt-6.1-sol': 475000 } },
    ],
    [
      'every window set',
      { id: 'windows', label: 'Migrate', kind: 'plugin' },
      { applied: true, file: '/proj/.claude/settings.local.json', windows: { 'gpt-6-astra': 258400, 'gpt-6-luna': 258400, 'gpt-6.1-sol': 475000, 'grok-4.7': 500000 } },
    ],
    [
      'every window set, Muse included',
      { id: 'windows', label: 'Migrate', kind: 'plugin' },
      {
        applied: true,
        file: '/proj/.claude/settings.local.json',
        windows: {
          'gpt-6-astra': 258400,
          'gpt-6-luna': 258400,
          'gpt-6.1-sol': 475000,
          'grok-4.7': 500000,
          'muse-wonder-1.0': 1000000,
          'muse-wonder-full-1.0': 1000000,
        },
      },
    ],
    [
      'the Muse windows with the GPT ones',
      { id: 'windows', label: 'Migrate', kind: 'plugin' },
      {
        applied: true,
        windows: { 'gpt-6-astra': 258400, 'gpt-6-luna': 258400, 'gpt-6.1-sol': 475000, 'muse-wonder-1.0': 1000000, 'muse-wonder-full-1.0': 1000000 },
      },
    ],
    [
      'the Muse rows added',
      addAction,
      { applied: true, added: ['muse-wonder-1.0', 'muse-wonder-full-1.0'], windows: { 'muse-wonder-1.0': 1000000, 'muse-wonder-full-1.0': 1000000 } },
    ],
    ['a refusal', addAction, { applied: false, reason: 'Claude Code 2.1.200 predates per-model windows (2.1.288); update it first' }],
    ['a changed bypass', bypassAction, { status: 'changed', files: ['/home/me/.claude/settings.json', '/proj/.claude/settings.local.json'] }],
    ['a bypass with nothing to edit', bypassAction, { status: 'not-found' }],
  ]
  test('a button pressed after another session made the same edit says it is already done', async ($, on) => {
    const { toasts } = await startWithSettings($, on, [newModel], { applied: true, unchanged: true })
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:new-model:add-models' })
    expect(toasts).toEqual(['Already in /model; restart Claude Code to pick.'])
    await ui.unmount()
  })

  test('a changed bypass names the files it did change when they fit', async ($, on) => {
    const { toasts } = await startWithSettings(
      $,
      on,
      [{ id: 'down', severity: 'urgent', text: 'down', actions: [bypassAction, retryAction] }],
      { status: 'changed', files: ['/home/me/.claude/settings.json'] },
    )
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'model-router:down:bypass' })
    expect(toasts).toEqual([
      'Settings changed mid-edit, so Bypass router is not finished: try again. Changed so far: ~/.claude/settings.json',
    ])
    await ui.unmount()
  })

  test('windows too many to name still say to restart', async ($, on) => {
    const [, action, settings] = longest.find(([name]) => name === 'every window set, Muse included')!
    const { toasts } = await startWithSettings($, on, [{ id: 'row', severity: 'action', text: 'row', actions: [action] }], settings)
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: `model-router:row:${action.id}` })
    expect(toasts).toEqual(['Windows set; restart Claude Code to use them.'])
    await ui.unmount()
  })

  for (const [name, action, settings] of longest) {
    test(`the toast for ${name} shows whole`, async ($, on) => {
      const { toasts } = await startWithSettings($, on, [{ id: 'row', severity: 'action', text: 'row', actions: [action] }], settings)
      const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
      await ui.press({ key: `model-router:row:${action.id}` })
      expect(toasts).toHaveLength(1)
      expect(fitsToast(toasts[0]!)).toBe(true)
      await ui.unmount()
    })
  }
})

describe('model-router check', () => {
  test('runs, awaited, at startup and again at /clear, not at a resume', async ($, on) => {
    let runs = 0
    mock.store(on, {})
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
    mock.store(on, {})
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

describe('model-router rows that overlap', () => {
  const newModel = { id: 'new-model', severity: 'action', text: 'GPT-6.1 Terra can be added to /model', version: '0.1.43', actions: [addAction] }
  const windowsRow = {
    id: 'windows',
    severity: 'action',
    text: 'no context window set for gpt-6.1-terra',
    version: '0.1.43',
    actions: [{ id: 'windows', label: 'Set', kind: 'plugin' }],
  }

  /** A session whose check prints what `rows` returns for its env; each check's env is kept. */
  async function startChecking($: Engine, on: On, rows: (env: Record<string, string>, edited: boolean) => unknown[]) {
    const checks: Record<string, string>[] = []
    let edited = false
    mock.store(on, {})
    on('classic.SessionStart', async () => ({}))
    on('process.run', async (_$, e) => {
      const isSettings = e.argv[2] === 'settings'
      if (isSettings) edited = true
      else checks.push(e.init?.env ?? {})
      const stdout = JSON.stringify(
        isSettings ? { applied: true, added: ['gpt-6.1-terra'], windows: { 'gpt-6.1-terra': 475000 } } : { notices: rows(e.init?.env ?? {}, edited) },
      )
      return { value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
    })
    on('ui.toast', async () => ({ value: undefined }) as never)
    on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
    await $.classic.SessionStart({ source: 'startup', cwd: '/proj' })
    return checks
  }

  test('an edit checks the rows again, so a row the edit did the work of goes', async ($, on) => {
    const checks = await startChecking($, on, (_env, edited) => (edited ? [] : [newModel, windowsRow]))
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    expect(await ui.find({ key: 'model-router:windows:windows' })).toBeDefined()
    await ui.press({ key: 'model-router:new-model:add-models' })
    expect(checks).toHaveLength(2)
    expect(await ui.find({ key: 'model-router:windows:windows' })).toBeUndefined()
    await ui.unmount()
  })

  test('dismissing the new-model row tells the next check, which gives its routes to the windows row', async ($, on) => {
    const checks = await startChecking($, on, env => (env.NEW_MODEL_DISMISSED === '0.1.43' ? [newModel, windowsRow] : [newModel]))
    expect(checks[0]?.NEW_MODEL_DISMISSED).toBe('')
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    expect(await ui.find({ key: 'model-router:windows:windows' })).toBeUndefined()
    await ui.press({ key: 'model-router:new-model:dismiss' })
    expect(checks.map(env => env.NEW_MODEL_DISMISSED)).toEqual(['', '0.1.43'])
    expect(await ui.find({ key: 'model-router:new-model:add-models' })).toBeUndefined()
    expect(await ui.find({ key: 'model-router:windows:windows' })).toBeDefined()
    await ui.unmount()
  })
})
