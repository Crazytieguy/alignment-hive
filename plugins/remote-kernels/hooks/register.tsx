import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import { noticeRows } from './notice-band'
import type { Notice, NoticeAction } from './notice-band'

const PLUGIN = 'remote-kernels'
const notices = atom({ plugin: 'remote-kernels', key: 'notices' } as const, [] as Notice[])
// Rows dismissed this session; the script itself shows `updated` once per binary version.
const dismissed = atom({ plugin: 'remote-kernels', key: 'dismissed' } as const, [] as string[])

/** Runs the plugin's session-start check and keeps its rows; none when it fails. */
async function check($: EngineInterface, projectDir: string): Promise<void> {
  try {
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

    const onAction = async (notice: Notice, action: NoticeAction) => {
      // Running setup dismisses the row for this session; the next session's check
      // decides whether it comes back.
      await update($, dismissed, list => [...list, notice.id])
      if (action.kind === 'command' && action.command) {
        await $.command.run({ command: action.command.slice(1) }).catch(() => $.ui.toast(`Could not run ${action.command}`))
      }
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
