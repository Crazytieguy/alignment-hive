// model-router as a mod: scripts/check.sh's rows (router down, settings to migrate, not set up,
// wrong platform variant) in the band above the prompt, checked at startup and /clear. Buttons
// retry the check, run setup, or have the binary edit the settings, saying what it did in a toast.
import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'

import { noticeBand } from './notice-band'
import type { Notice, NoticeAction } from './notice-band'

const PLUGIN = 'model-router'
const notices = atom({ plugin: 'model-router', key: 'notices' } as const, [] as Notice[])
// Rows dismissed this session; a versioned row stays dismissed in the store until its version changes.
const dismissed = atom({ plugin: 'model-router', key: 'dismissed' } as const, [] as string[])
// A row whose Try again is running: its buttons give way to "restarting…" until the check settles.
const retrying = atom({ plugin: 'model-router', key: 'retrying' } as const, null as string | null)

// The directory the session started in, for the check's CLAUDE_PROJECT_DIR and project settings.
let projectDir = ''

/**
 * Runs the plugin's check and keeps its rows; none when it fails. The check reads the wiring
 * (ANTHROPIC_BASE_URL, or HTTPS_PROXY) from the environment Claude Code passes on, settings'
 * `env` included, and restarts a stopped router. It is told the picker rows' dismissals and the
 * permanent Fast-row decline: while a picker row shows, the windows row leaves out its adds.
 */
async function check($: EngineInterface): Promise<void> {
  try {
    const [newModelDismissed, fastModelsDismissed] = await Promise.all([
      $.store.get('dismissed:new-model'),
      $.store.get('dismissed:fast-models'),
    ])
    const { exitCode, stdout } = await $.process.run(['bash', `${$.plugin.root}/scripts/check.sh`], {
      env: {
        CLAUDE_PLUGIN_ROOT: $.plugin.root,
        CLAUDE_PROJECT_DIR: projectDir,
        NEW_MODEL_DISMISSED: typeof newModelDismissed === 'string' ? newModelDismissed : '',
        FAST_MODELS_DISMISSED: typeof fastModelsDismissed === 'string' ? fastModelsDismissed : '',
      },
    })
    const found = exitCode === 0 && stdout.trim() ? (JSON.parse(stdout) as { notices: Notice[] }).notices : []
    await update($, notices, () => found)
  } catch {
    // A check that fails shows nothing rather than a wrong row.
  }
}

/** Try again on `row`: runs the check once more (one more restart attempt), the row busy meanwhile. */
async function retry($: EngineInterface, row: string): Promise<void> {
  await update($, retrying, () => row)
  try {
    await check($)
  } finally {
    await update($, retrying, () => null)
  }
}

/** "gpt-6-astra, gpt-6-luna at 258K; grok-4.7 at 500K": routes grouped by compaction window. */
function compactsAt(windows: Record<string, number>): string {
  const byWindow = new Map<number, string[]>()
  for (const [id, window] of Object.entries(windows)) {
    byWindow.set(window, [...(byWindow.get(window) ?? []), id])
  }
  return [...byWindow].map(([window, ids]) => `${ids.join(', ')} at ${Math.round(window / 1000)}K`).join('; ')
}

/**
 * Whether a toast shows whole on the terminal: Claude Code 2.1.299 word-wraps it at 40 columns
 * (a 44-column box less its frame) and draws 3 lines, the third cut at its end.
 */
export function fitsToast(text: string): boolean {
  const lines: string[] = []
  for (const word of text.split(' ')) {
    const last = lines.at(-1)
    if (last !== undefined && last.length + 1 + word.length <= 40) lines[lines.length - 1] = `${last} ${word}`
    else for (let at = 0; at < Math.max(word.length, 1); at += 40) lines.push(word.slice(at, at + 40))
  }
  return lines.length <= 3
}

/** The first of `texts` that fits in a toast, else the last. */
const firstFitting = (...texts: string[]): string => texts.find(fitsToast) ?? texts.at(-1)!

const SETTINGS_ARGS = new Map<string, readonly string[]>([
  ['add-models', ['models', '--apply']],
  ['add-fast-models', ['models', '--fast', '--apply']],
  ['windows', ['windows', '--apply']],
  ['bypass', ['bypass']],
])

/**
 * A settings edit by the binary (`model-router settings`, the setup skill's own), as the toast
 * that says what it did. The edits take effect when Claude Code next starts.
 *
 * A toast shows 3 lines of 40 columns and cuts the rest, so what to do comes first, and the files
 * edited only where they fit.
 */
async function editSettings($: EngineInterface, args: readonly string[]): Promise<string> {
  const operation = args[0]
  const { exitCode, stdout } = await $.process.run(
    ['bash', `${$.plugin.root}/scripts/bootstrap.sh`, 'settings', ...args, '--project-dir', projectDir],
    { env: { CLAUDE_PLUGIN_ROOT: $.plugin.root } },
  )
  if (exitCode !== 0 || !stdout.trim()) return 'Could not edit your settings: run /model-router:setup'
  const result = JSON.parse(stdout) as {
    applied?: boolean
    // Already as the edit would leave it: another session, or setup, made it since this one started.
    unchanged?: boolean
    added?: string[]
    windows?: Record<string, number>
    reason?: string
    status?: 'done' | 'shared-file' | 'not-found' | 'changed'
    files?: string[]
    file?: string
  }
  // A settings file as the person knows it: relative to the project, or under ~.
  const home = await $.env.get('HOME')
  const shown = (file: string) => {
    if (file.startsWith(`${projectDir}/`)) return file.slice(projectDir.length + 1)
    return home && file.startsWith(`${home}/`) ? `~${file.slice(home.length)}` : file
  }

  if (operation === 'models') {
    if (!result.applied) return `Not added, run /model-router:setup: ${result.reason ?? 'no reason given'}`
    if (result.unchanged) return 'Already in /model; restart Claude Code to pick.'
    const added = Object.fromEntries(
      (result.added ?? []).flatMap(id => (result.windows?.[id] ? [[id, result.windows[id]]] : [])),
    )
    // Only retired rows dropped: nothing added to name.
    if (!result.added?.length) return 'Updated /model. Restart Claude Code to see it.'
    const compacts = Object.keys(added).length ? ` Compacts ${compactsAt(added)}.` : ''
    const withCompacts = `Added to /model; restart Claude Code to pick.${compacts}`
    return firstFitting(withCompacts, 'Added to /model; restart Claude Code to pick.')
  }
  if (operation === 'windows') {
    if (!result.applied) return `Not changed, run /model-router:setup: ${result.reason ?? 'no reason given'}`
    if (result.unchanged) return 'Windows already set; restart Claude Code to use them.'
    const set = 'Windows set; restart Claude Code to use them.'
    if (!Object.keys(result.windows ?? {}).length) return set
    const compact = `Restart Claude Code to compact ${compactsAt(result.windows!)}`
    // Too many routes to name (every family enabled at once): what to do still shows.
    return firstFitting(...(result.file ? [`${compact} (in ${shown(result.file)}).`] : []), `${compact}.`, set)
  }
  if (result.status === 'done') {
    return 'Bypassed the router. Restart Claude Code to get back online, then run /model-router:setup.'
  }
  // Another writer kept changing a file: its edit is not done, nothing of theirs was overwritten.
  if (result.status === 'changed') {
    const unfinished = 'Settings changed mid-edit, so Bypass router is not finished: try again.'
    const done = result.files?.length ? [`${unfinished} Changed so far: ${result.files.map(shown).join(', ')}`] : []
    return firstFitting(...done, unfinished)
  }
  if (result.status === 'shared-file') {
    const file = shown(result.file ?? '.claude/settings.json')
    return `In shared ${file}, remove the router's wiring (and a GPT "model"), then restart Claude Code.`
  }
  return 'Not wired in your settings: unset ANTHROPIC_BASE_URL or HTTPS_PROXY where you set it, then restart Claude Code.'
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
      // One edit can do another row's work (adding a route to /model sets its window), so the
      // rows are checked again after it.
      const args = SETTINGS_ARGS.get(action.id)
      let said: string
      if (args) {
        said = await editSettings($, args).catch(() => 'Could not edit your settings: run /model-router:setup')
      } else if (action.id === 'decline-fast-models') {
        await $.store.set('dismissed:fast-models', 'declined')
        said = 'Not added to /model; /model-router:setup can add them later.'
      } else {
        // Running setup (or Customize) dismisses the row for this session; the next session's
        // check decides whether it comes back.
        await dismiss(notice)
        if (action.kind === 'command' && action.command) {
          await $.command.run({ command: action.command.slice(1) }).catch(() => $.ui.toast(`Could not run ${action.command}`))
        }
        return
      }
      await dismiss(notice)
      $.ui.toast(said)
      if (args?.[0] !== 'bypass') await check($)
    }
    const onDismiss = async (notice: Notice) => {
      await dismiss(notice)
      if (notice.version !== undefined) await $.store.set(`dismissed:${notice.id}`, notice.version)
      // The windows row takes back the routes the picker row would have added.
      if (notice.id === 'new-model' || notice.id === 'fast-models') await check($)
    }

    const ui = $.ui.resolve(e)
    return noticeBand(ui, e.surface, PLUGIN, rows, { onAction, onDismiss }, below)
  })
}
