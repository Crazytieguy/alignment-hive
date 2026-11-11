import type { On } from 'claude-code'
import { describe, expect, mock, test } from 'claude-code/testing'

const BAND = {
  plugin: 'lit-review',
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isCollapsed: false, isWorking: false, maxRows: 20, bodyColumns: 140, scroll: { offset: 0, bodyRows: 19 }, view: {} },
}

const NOT_INSTALLED = JSON.stringify({
  notices: [
    {
      id: 'install',
      severity: 'action',
      text: 'alignment-hive CLI not installed',
      actions: [{ id: 'install', label: 'Copy install command', kind: 'copy', command: 'curl -fsSL https://alignment-hive.com/install.sh | bash' }],
    },
  ],
})

/** A session with these slash commands whose check reports the CLI missing. */
function fake(on: On, commands: string[]) {
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('command.list', async () => ({ value: commands.map(name => ({ name, description: '' })) }) as never)
  on('process.run', async () => ({
    value: { exitCode: 0, stdout: NOT_INSTALLED, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
  }))
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
}

describe('lit-review install row', () => {
  for (const [label, commands, isShown] of [
    ['shown without the hive plugin', ['lit-review:lit-review'], true],
    ['left to hive when the hive plugin is enabled', ['lit-review:lit-review', 'hive:align'], false],
  ] as const) {
    test(label, async ($, on) => {
      fake(on, [...commands])
      await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
      for (const surface of ['terminal', 'desktop'] as const) {
        const ui = await $.ui.mount({ ...BAND, surface })
        expect((await ui.find({ key: 'lit-review:install:install' })) !== undefined).toBe(isShown)
        await ui.unmount()
      }
    })
  }
})

describe('lit-review check', () => {
  test('runs at startup and again after /clear, in the directory the session then runs in, not at other ends', async ($, on) => {
    const dirs: string[] = []
    const clock = mock.clock(on)
    on('session.start', async (_$, e) => ({ cwd: e.cwd }))
    on('session.end', async (_$, e) => ({ sessionId: e.sessionId }))
    on('session.cwd', async () => ({ value: '/tmp/moved' }))
    on('command.list', async () => ({ value: [] }) as never)
    on('process.run', async (_$, e) => {
      dirs.push(e.init?.env?.CLAUDE_PROJECT_DIR ?? '')
      return { value: { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
    })
    await $.session.start({ cwd: '/tmp/project', surface: 'terminal', isInteractive: true })
    await $.session.end({ reason: 'resume', sessionId: 'a', resume: { id: 'a' } })
    await clock.settle()
    expect(dirs).toEqual(['/tmp/project'])
    await $.session.end({ reason: 'clear', sessionId: 'a', resume: { id: 'a' } })
    await clock.settle()
    expect(dirs).toEqual(['/tmp/project', '/tmp/moved'])
  })
})
