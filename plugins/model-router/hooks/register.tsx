import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import { noticeRows } from './notice-band'
import type { Notice, NoticeAction } from './notice-band'

const PLUGIN = 'model-router'
const notices = atom({ plugin: 'model-router', key: 'notices' } as const, [] as Notice[])
// Rows dismissed this session; a row with a `version` stays dismissed in the store until the next.
const dismissed = atom({ plugin: 'model-router', key: 'dismissed' } as const, [] as string[])
// A row whose Try again is running: its buttons give way to "restarting…" until the check settles.
const retrying = atom({ plugin: 'model-router', key: 'retrying' } as const, null as string | null)

// The directory the session started in, for the check's CLAUDE_PROJECT_DIR and project settings.
let projectDir = ''

/**
 * Runs the plugin's check and keeps its rows; none when it fails. The check reads
 * ANTHROPIC_BASE_URL from the environment Claude Code passes on, settings' `env` included, and
 * restarts a stopped router.
 */
async function check($: EngineInterface): Promise<void> {
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

async function retry($: EngineInterface, row: string): Promise<void> {
  await update($, retrying, () => row)
  try {
    await check($)
  } finally {
    await update($, retrying, () => null)
  }
}

/**
 * A settings edit by the binary (`model-router settings`, the setup skill's own), as the toast
 * that says what it did. The edits take effect when Claude Code next starts.
 */
async function editSettings($: EngineInterface, operation: 'models' | 'bypass'): Promise<string> {
  const args = operation === 'models' ? ['models', '--apply'] : ['bypass']
  const { exitCode, stdout } = await $.process.run(
    ['bash', `${$.plugin.root}/scripts/bootstrap.sh`, 'settings', ...args, '--project-dir', projectDir],
    { env: { CLAUDE_PLUGIN_ROOT: $.plugin.root } },
  )
  if (exitCode !== 0 || !stdout.trim()) return 'Could not edit your settings: run /model-router:setup'
  const result = JSON.parse(stdout) as {
    applied?: boolean
    added?: string[]
    windows?: Record<string, number>
    reason?: string
    status?: 'done' | 'shared-file' | 'not-found' | 'changed'
    files?: string[]
    file?: string
  }

  if (operation === 'models') {
    if (!result.applied) return `${result.reason ?? 'Nothing to add'}: run /model-router:setup`
    const added = (result.added ?? []).map(id => {
      const window = result.windows?.[id]
      return window ? `${id} (compacts at ${Math.round(window / 1000)}K)` : id
    })
    // Only retired rows dropped: nothing added to name.
    if (added.length === 0) return 'Updated /model. Restart Claude Code to see it.'
    return `Added ${added.join(', ')} to /model. Restart Claude Code to pick it; Customize runs setup.`
  }
  if (result.status === 'done') {
    return `Claude Code now talks to Anthropic directly (${(result.files ?? []).join(', ')}). Restart it to get back online, then run /model-router:setup.`
  }
  // Another writer kept changing a file: its edit is not done, nothing of theirs was overwritten.
  if (result.status === 'changed') {
    const done = result.files?.length ? ` (changed so far: ${result.files.join(', ')})` : ''
    return `Your settings changed while editing; Bypass router is not finished${done}. Try again.`
  }
  if (result.status === 'shared-file') {
    const done = result.files?.length ? ` (changed so far: ${result.files.join(', ')})` : ''
    return `${result.file} routes through model-router and collaborators share it: remove ANTHROPIC_BASE_URL (and a GPT "model") there, then restart Claude Code${done}`
  }
  return 'ANTHROPIC_BASE_URL is not in your settings files: unset it where you set it, then restart Claude Code'
}

export const register: Register = on => {
  // The SessionStart hook's lifecycle, startup and /clear, awaited: a stopped router's restart
  // finishes before the session's first request.
  on('classic.SessionStart', async ($, e, next) => {
    if (e.source === 'startup' || e.source === 'clear') {
      projectDir = e.cwd
      await check($)
    }
    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const [below, found, hidden, running] = await Promise.all([
      next(e),
      read($, notices),
      read($, dismissed),
      read($, retrying),
    ])
    if (e.props.hasSurvey) return below

    const candidates = (found as Notice[]).filter(notice => !hidden.includes(notice.id))
    const stored = await Promise.all(
      candidates.map(notice => (notice.version === undefined ? undefined : $.store.get(`dismissed:${notice.id}`))),
    )
    const rows = candidates
      .filter((notice, index) => notice.version === undefined || stored[index] !== notice.version)
      .map(notice => (running === notice.id ? { ...notice, busy: 'restarting…' } : notice))
    if (rows.length === 0) return below

    const dismiss = (notice: Notice) => update($, dismissed, list => [...list, notice.id])
    const onAction = async (notice: Notice, action: NoticeAction) => {
      if (action.id === 'retry') {
        await retry($, notice.id)
        return
      }
      // The settings edits take effect at the next start: the row's work is done this session.
      if (action.id === 'add-models' || action.id === 'bypass') {
        const said = await editSettings($, action.id === 'add-models' ? 'models' : 'bypass').catch(
          () => 'Could not edit your settings: run /model-router:setup',
        )
        await dismiss(notice)
        $.ui.toast(said)
        return
      }
      // Running setup (or Customize) dismisses the row for this session; the next session's
      // check decides whether it comes back.
      await dismiss(notice)
      if (action.kind === 'command' && action.command) {
        await $.command.run({ command: action.command.slice(1) }).catch(() => $.ui.toast(`Could not run ${action.command}`))
      }
    }
    const onDismiss = async (notice: Notice) => {
      await dismiss(notice)
      if (notice.version !== undefined) await $.store.set(`dismissed:${notice.id}`, notice.version)
    }

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
