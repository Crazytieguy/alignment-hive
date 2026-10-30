// hive as a mod: its rows in the band above the prompt. `hive notices` (through scripts/hive.sh)
// decides them for this session and is asked again only when they can have changed; the buttons
// copy a terminal command, run a slash command, or run the CLI's upload commands.
import { atom, read, update } from 'claude-code'
import type { EngineInterface, Register, RenderSurface } from 'claude-code'

import { cli, hive } from './cli'
import { noticeBand, noticeLabel } from './notice-band'
import type { Notice, NoticeAction } from './notice-band'
import { drawRecalled, onPromptStored, submitWithRecall, warm } from './recall'
import type { HiveBusy, HiveFolded, HiveNotices } from '../types'

// `when`: which phase of the session shows the row (packages/hive-cli/src/commands/notices.ts).
type HiveNotice = Notice & { when?: 'start' | 'working' }

const PLUGIN = 'hive'
const notices = atom({ plugin: 'hive', key: 'notices' } as const, null as HiveNotices | null)
const dismissed = atom({ plugin: 'hive', key: 'dismissed' } as const, [] as string[])
// The session the person has sent a message in; a /clear starts a new one, back at its start.
const promptedSession = atom({ plugin: 'hive', key: 'promptedSession' } as const, null as string | null)
// A row whose button's command is running: its buttons give way to the label until it settles.
const busy = atom({ plugin: 'hive', key: 'busy' } as const, null as HiveBusy | null)
// The review page's server is running: it serves until stopped, so Review sessions waits for it.
const isReviewing = atom({ plugin: 'hive', key: 'isReviewing' } as const, false)
// On the desktop, this session's info rows show in the band through its first turn, then fold
// into a label in the prompt footer; a press on the label opens them again until Hide. Set once
// they folded in this session (a /clear starts a new one, open again).
const folded = atom({ plugin: 'hive', key: 'folded' } as const, null as HiveFolded | null)

// Each `hive notices` costs a few seconds of CPU and a backend call, so a turn refreshes only when
// something can have changed: a new session id, a session the CLI has not discovered yet, or rows
// older than this (the pending count and its countdown drift slowly).
const REFRESH_MS = 15 * 60 * 1000

let running: Promise<void> | null = null
// A refresh asked for while one runs: the running one goes again, so its awaiters see fresh rows.
let isRefreshDue = false
// The session a stored reply already asked for: the reply hook asks once per session.
let askedOnReply = ''

/** Fetches this session's rows once; drops the answer if the session changed (a /clear) meanwhile. */
async function fetchNotices($: EngineInterface): Promise<void> {
  const [sessionId, prompted] = await Promise.all([$.session.id(), read($, promptedSession)])
  let found: { notices: unknown[]; isSessionKnown: boolean } = { notices: [], isSessionKnown: false }
  try {
    // Past the first message only this session's row shows: the CLI skips the project-wide scan,
    // most of a full call's time.
    const { exitCode, stdout } = await hive($, ['notices', sessionId, ...(prompted === sessionId ? ['--session-only'] : [])])
    if (exitCode === 0 && stdout.trim()) found = JSON.parse(stdout) as typeof found
  } catch {
    // A check that fails shows nothing rather than a wrong row, and is asked again next turn.
  }
  const fetchedAt = await $.clock.now()
  if ((await $.session.id()) !== sessionId) return
  await update($, notices, (): HiveNotices => ({ sessionId, rows: found.notices, isSessionKnown: found.isSessionKnown, fetchedAt }))
}

/** Refreshes the rows; resolves once they reflect everything asked before it returned. */
function refresh($: EngineInterface): Promise<void> {
  if (running) {
    isRefreshDue = true
    return running
  }
  running = loop($).finally(() => {
    running = null
  })
  return running
}

async function loop($: EngineInterface): Promise<void> {
  do {
    isRefreshDue = false
    await fetchNotices($)
  } while (isRefreshDue)
}

/**
 * While the review page's server runs: it serves until stopped (in /tasks), so its row shows it
 * open and the button comes back once the process is gone. pgrep reads, nothing more.
 */
async function watchReview($: EngineInterface): Promise<void> {
  await update($, isReviewing, () => true)
  try {
    await $.clock.sleep(5_000)
    for (;;) {
      const { exitCode } = await $.process.run(['pgrep', '-f', '/hive upload review$'])
      if (exitCode !== 0) return
      await $.clock.sleep(10_000)
    }
  } catch {
    // Not knowing whether it runs, the button comes back.
  } finally {
    await update($, isReviewing, () => false)
  }
}

/** Whether the rows can say more about this session: none yet, another session's, or undiscovered. */
function isUnsettled(current: HiveNotices | null, sessionId: string): boolean {
  return current?.sessionId !== sessionId || !current.isSessionKnown
}

/** Whether a turn's end should refresh: see REFRESH_MS. */
async function isRefreshNeeded($: EngineInterface): Promise<boolean> {
  const [current, sessionId, now] = await Promise.all([read($, notices), $.session.id(), $.clock.now()])
  return isUnsettled(current, sessionId) || now - (current?.fetchedAt ?? 0) > REFRESH_MS
}

/**
 * Runs a CLI command for a row's button, showing `label` in the row's place until the rows are
 * fresh again; the CLI's words go to a toast only when it fails (the row shows success).
 * Resolves whether it succeeded.
 */
async function runForRow($: EngineInterface, row: string, label: string, args: string[]): Promise<boolean> {
  await update($, busy, (): HiveBusy => ({ row, label }))
  try {
    const { exitCode, stdout, stderr } = await hive($, args)
    if (exitCode !== 0) $.ui.toast((stderr || stdout).trim() || `hive ${args.join(' ')} failed`)
    await refresh($)
    return exitCode === 0
  } finally {
    await update($, busy, () => null)
  }
}

/** Work nobody awaits: an unload (a reload, the session's end) aborts it, which is no failure. */
function inBackground(work: Promise<unknown>): void {
  work.catch(() => undefined)
}

const isInfo = (notice: Notice) => notice.severity === 'info'

/**
 * This session's rows, split between the band and the footer label as `surface` shows them. On the
 * desktop, once the person works, the info rows can fold (`canFold`: the band gives them Hide);
 * folded, they are the footer's until opened again (see `folded`). `hasFolded`: they folded once
 * in this session. Null while the rows are another session's.
 */
async function visibleRows(
  $: EngineInterface,
  surface: RenderSurface,
): Promise<{ band: Notice[]; footer: Notice[]; canFold: boolean; hasFolded: boolean; sessionId: string } | null> {
  const [current, hidden, prompted, busyRow, reviewing, foldedIn, sessionId] = await Promise.all([
    read($, notices),
    read($, dismissed),
    read($, promptedSession),
    read($, busy),
    read($, isReviewing),
    read($, folded),
    $.session.id(),
  ])
  if (current?.sessionId !== sessionId) return null

  const isWorking = prompted === sessionId
  const phase = isWorking ? 'working' : 'start'
  const rows = (current.rows as HiveNotice[])
    .filter(notice => !hidden.includes(notice.id) && (notice.when === undefined || notice.when === phase))
    .map(notice => {
      if (busyRow?.row === notice.id) return { ...notice, busy: busyRow.label }
      if (reviewing && notice.actions?.some(action => action.id === 'review')) {
        return { ...notice, busy: 'review page open, stop it in /tasks' }
      }
      return notice
    })

  const canFold = surface === 'desktop' && isWorking
  const hasFolded = foldedIn?.sessionId === sessionId
  const isFolded = canFold && hasFolded && !foldedIn.isOpen
  return {
    band: isFolded ? rows.filter(notice => !isInfo(notice)) : rows,
    footer: isFolded ? rows.filter(isInfo) : [],
    canFold,
    hasFolded,
    sessionId,
  }
}

/** Opens or folds this session's info rows on the desktop (see `folded`). */
function fold($: EngineInterface, sessionId: string, isOpen: boolean): Promise<unknown> {
  return update($, folded, (): HiveFolded => ({ sessionId, isOpen }))
}

/** A shell word: the path in single quotes. */
function quoted(text: string): string {
  return `'${text.replaceAll("'", `'\\''`)}'`
}

/** Has the local server read what the next recall will (recall.tsx's warm), from a timer: outside the hook's dispatch. */
function warmLater($: EngineInterface, recallModel: string): void {
  $.clock.after(0, () => void warm($, recallModel))
}

export const register: Register = (on, options) => {
  const recallModel = String(options.recallModel ?? 'auto')

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    cli.projectDir = await $.session.root()
    // While recall is on, a warm follows the notices: its CLI call waits for theirs, which may download the CLI first.
    inBackground(refresh($).finally(() => options.recall === true && warmLater($, recallModel)))
    return started
  })

  on('prompt.submit', async ($, e, next) => {
    const [sessionId, prompted] = await Promise.all([$.session.id(), read($, promptedSession)])
    if (prompted !== sessionId) await update($, promptedSession, () => sessionId)
    return options.recall === true ? submitWithRecall($, e, next, recallModel) : next(e)
  })

  // What recall gave a prompt shows under it (recall.tsx); the prompt's stored row names it.
  if (options.recall === true) {
    on('session.append', { door: ['prompt', 'delivery'] }, onPromptStored)
    on('ui.render', { component: 'UserMessage' }, drawRecalled)
  }

  // The CLI discovers a session once it holds an assistant reply: the first reply row stored in
  // the main conversation of a session it does not know yet is the moment to ask again, once per
  // session, so this session's row comes before the turn ends. The turn's end asks again if
  // that answer came too soon.
  on('session.append', { door: 'response' }, async ($, e, next) => {
    const stored = await next(e)
    if (e.agentId === undefined) {
      const [current, sessionId] = await Promise.all([read($, notices), $.session.id()])
      if (askedOnReply !== sessionId && isUnsettled(current, sessionId)) {
        askedOnReply = sessionId
        inBackground(refresh($))
      }
    }
    return stored
  })

  // A new session is not discovered until its first reply, and a /clear changes the session id
  // without a session.start. The end of the first main turn that showed info rows folds them
  // (the desktop's fold: the terminal never reads it); a subagent's end is not the person's turn.
  on('turn.complete', async ($, e, next) => {
    if (await isRefreshNeeded($)) inBackground(refresh($))
    // The person may type next.
    if (options.recall === true && e.agentId === undefined) warmLater($, recallModel)
    const visible = e.agentId === undefined ? await visibleRows($, 'desktop') : null
    if (visible?.canFold && !visible.hasFolded && visible.band.some(isInfo)) await fold($, visible.sessionId, false)
    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const [below, visible] = await Promise.all([next(e), visibleRows($, e.surface)])
    if (e.props.hasSurvey || visible === null || visible.band.length === 0) return below

    const { band: rows, sessionId } = visible

    const onAction = async (notice: Notice, action: NoticeAction) => {
      if (action.kind === 'copy' && action.command) {
        // The install script and `hive login` ask questions, so they run in a terminal of the
        // person's own.
        const copied = await $.ui.copy({ text: action.command, surface: e.surface })
        $.ui.toast(copied.isCopied ? 'Copied: paste it into a terminal' : `Run in a terminal: ${action.command}`)
        return
      }
      if (action.kind === 'command' && action.command) {
        await update($, dismissed, list => [...list, notice.id])
        await $.command.run({ command: action.command.slice(1) }).catch(() => $.ui.toast(`Could not run ${action.command}`))
        return
      }
      if (action.id === 'keep-private') {
        // Done, the row folds back on the desktop: its label then says the new state.
        const isKept = await runForRow($, notice.id, 'keeping private…', ['upload', 'exclude', sessionId])
        if (isKept && visible.canFold) await fold($, sessionId, false)
        return
      }
      if (action.id === 'snooze') {
        await runForRow($, notice.id, 'snoozing…', ['upload', 'snooze', '24h'])
        return
      }
      if (action.id === 'review') {
        // A background shell task of the session (listed in /tasks, stopped there): a local
        // server that opens the review page in the browser.
        const root = $.plugin.root
        const started = await $.tool
          .call({
            tool: 'Bash',
            command: `CLAUDE_PLUGIN_ROOT=${quoted(root)} CLAUDE_PROJECT_DIR=${quoted(cli.projectDir)} bash ${quoted(`${root}/scripts/hive.sh`)} upload review`,
            description: 'Open the hive session review page',
            run_in_background: true,
          })
          .catch(() => null)
        // A denied or failed start leaves the button as it was.
        if (started?.result) inBackground(watchReview($))
        else $.ui.toast('Could not start the review page')
      }
    }
    const onDismiss = (notice: Notice) => update($, dismissed, list => [...list, notice.id])

    const ui = $.ui.resolve(e)
    const onHide = visible.canFold ? () => fold($, sessionId, false) : undefined
    return noticeBand(ui, e.surface, PLUGIN, rows, { onAction, onDismiss, onHide }, below)
  })

  // The desktop's footer: this session's folded info rows as a label while the person works.
  on('ui.render', { component: 'SessionMode' }, async ($, e, next) => {
    if (e.surface !== 'desktop') return next(e)
    const [below, visible] = await Promise.all([next(e), visibleRows($, e.surface)])
    if (!visible?.footer.length) return below

    const ui = $.ui.resolve(e)
    const label = noticeLabel(ui, PLUGIN, visible.footer, () => fold($, visible.sessionId, true))
    return (
      <ui.Box flexDirection="row" alignItems="center" columnGap={1}>
        {label}
        {below}
      </ui.Box>
    )
  })
}
