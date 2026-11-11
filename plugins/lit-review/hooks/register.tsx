// lit-review as a mod: scripts/check.sh's install row in the band above the prompt, checked at startup
// and /clear. Left to the hive plugin when that is enabled, since hive draws the same row.
import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import { noticeBand } from './notice-band'
import type { Notice, NoticeAction } from './notice-band'

const PLUGIN = 'lit-review'
const notices = atom({ plugin: 'lit-review', key: 'notices' } as const, [] as Notice[])
const dismissed = atom({ plugin: 'lit-review', key: 'dismissed' } as const, [] as string[])

/** Runs the plugin's session-start check and keeps its rows; none when it fails. */
async function check($: EngineInterface, projectDir: string): Promise<void> {
  try {
    // The hive plugin draws the same install row, so lit-review leaves it to hive. No API lists
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
  // At startup, awaited. Not a classic.SessionStart hook: Claude Code's built-in
  // cc-plugin-sec-default, on for Team and Enterprise organizations and on machines with managed
  // settings, passes every classic event over the plugins a person installs.
  on('session.start', async ($, e, next) => {
    await check($, e.cwd)
    return next(e)
  })

  // `/clear` starts a session without a session.start; the one it ends says so. The end's hooks
  // share a short time bound, so the check runs from a timer set once the end step is done (set
  // before it, the timer runs in the ending session), or once the bound cut it short.
  on('session.end', async ($, e, next) => {
    try {
      return await next(e)
    } finally {
      if (e.reason === 'clear') $.clock.after(0, () => $.session.cwd().then(cwd => check($, cwd), () => undefined))
    }
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
    return noticeBand(ui, e.surface, PLUGIN, rows, { onAction, onDismiss }, below)
  })
}
