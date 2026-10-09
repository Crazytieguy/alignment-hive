import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import { noticeRows } from './notice-band'
import type { Notice, NoticeAction } from './notice-band'

const PLUGIN = 'mats'
const notices = atom({ plugin: 'mats', key: 'notices' } as const, [] as Notice[])
const dismissed = atom({ plugin: 'mats', key: 'dismissed' } as const, [] as string[])

/** Runs the plugin's session-start check and keeps its rows; none when it fails. */
async function check($: EngineInterface, projectDir: string): Promise<void> {
  try {
    // The hive plugin draws the same install row, so mats leaves it to hive. No API lists
    // plugins; the hive plugin's commands are its mark.
    if ((await $.command.list()).some(command => command.name.startsWith('hive:'))) return
    const { exitCode, stdout } = await $.process.run(['bash', `${$.plugin.root}/scripts/check.sh`], {
      env: { CLAUDE_PLUGIN_ROOT: $.plugin.root, CLAUDE_PROJECT_DIR: projectDir },
    })
    const found = exitCode === 0 && stdout.trim() ? (JSON.parse(stdout) as { notices: Notice[] }).notices : []
    await update($, notices, () => found)
  } catch {
    // A check that fails shows nothing rather than a wrong row.
  }
}

export const register: Register = on => {
  // The SessionStart hook's lifecycle: startup and /clear, awaited.
  on('classic.SessionStart', async ($, e, next) => {
    if (e.source === 'startup' || e.source === 'clear') await check($, e.cwd)
    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const [below, found, hidden] = await Promise.all([next(e), read($, notices), read($, dismissed)])
    if (e.props.hasSurvey) return below

    const rows = (found as Notice[]).filter(notice => !hidden.includes(notice.id))
    if (rows.length === 0) return below

    const onAction = async (_notice: Notice, action: NoticeAction) => {
      // The install script asks questions, so it runs in a terminal of the person's own.
      if (action.kind !== 'copy' || !action.command) return
      const copied = await $.ui.copy({ text: action.command, surface: e.surface })
      $.ui.toast(copied.isCopied ? 'Copied: paste it into a terminal' : `Run in a terminal: ${action.command}`)
    }
    const onDismiss = (notice: Notice) => update($, dismissed, list => [...list, notice.id])

    const ui = $.ui.resolve(e)
    const drawn = noticeRows(ui, PLUGIN, rows, { onAction, onDismiss })

    return (
      <ui.Box flexDirection="column">
        {drawn}
        {below}
      </ui.Box>
    )
  })
}
