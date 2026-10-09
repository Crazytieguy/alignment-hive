import type { On } from 'claude-code'
import { describe, expect, test } from 'claude-code/testing'

const BAND = {
  plugin: 'remote-kernels',
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 140, scroll: { offset: 0, bodyRows: 19 }, view: {} },
}

const NOT_CONFIGURED = JSON.stringify({
  notices: [
    {
      id: 'setup',
      severity: 'action',
      text: 'not configured',
      actions: [{ id: 'setup', label: 'Run setup', kind: 'command', command: '/remote-kernels:setup' }],
    },
  ],
})

/** The session-start script's output, and the commands the band runs. */
function fakeSession(on: On, stdout: string) {
  const ran: string[] = []
  on('classic.SessionStart', async () => ({}))
  on('process.run', async () => ({
    value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
  }))
  on('command.run', async (_$, e) => {
    ran.push(e.command)
    return { text: '' }
  })
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
  return ran
}

describe('remote-kernels band', () => {
  test('not configured: Run setup runs the setup command and dismisses the row', async ($, on) => {
    const ran = fakeSession(on, NOT_CONFIGURED)
    await $.classic.SessionStart({ source: 'startup', cwd: '/tmp/project' })

    for (const surface of ['terminal', 'desktop'] as const) {
      const ui = await $.ui.mount({ ...BAND, surface })
      expect(await ui.find({ key: 'remote-kernels:setup:setup' })).toBeDefined()
      await ui.unmount()
    }

    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    await ui.press({ key: 'remote-kernels:setup:setup' })
    expect(ran).toEqual(['remote-kernels:setup'])
    expect(await ui.find({ key: 'remote-kernels:setup:setup' })).toBeUndefined()
    await ui.unmount()
  })

  test('a configured project draws nothing', async ($, on) => {
    fakeSession(on, '')
    await $.classic.SessionStart({ source: 'startup', cwd: '/tmp/project' })
    const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
    expect(await ui.find({ type: 'Button' })).toBeUndefined()
    await ui.unmount()
  })
})
