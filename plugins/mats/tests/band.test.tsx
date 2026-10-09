import type { On } from 'claude-code'
import { describe, expect, test } from 'claude-code/testing'

const BAND = {
  plugin: 'mats',
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isWorking: false, maxRows: 20, bodyColumns: 140, scroll: { offset: 0, bodyRows: 19 }, view: {} },
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
  on('classic.SessionStart', async () => ({}))
  on('command.list', async () => ({ value: commands.map(name => ({ name, description: '' })) }) as never)
  on('process.run', async () => ({
    value: { exitCode: 0, stdout: NOT_INSTALLED, stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
  }))
  on('ui.render', () => ({ type: 'Box', props: {}, children: [] }) as never)
}

describe('mats install row', () => {
  for (const [label, commands, isShown] of [
    ['shown without the hive plugin', ['mats:lit-review'], true],
    ['left to hive when the hive plugin is enabled', ['mats:lit-review', 'hive:align'], false],
  ] as const) {
    test(label, async ($, on) => {
      fake(on, [...commands])
      await $.classic.SessionStart({ source: 'startup', cwd: '/tmp/project' })
      for (const surface of ['terminal', 'desktop'] as const) {
        const ui = await $.ui.mount({ ...BAND, surface })
        expect((await ui.find({ key: 'mats:install:install' })) !== undefined).toBe(isShown)
        await ui.unmount()
      }
    })
  }
})
