import { atom, memberOf, read, update } from 'claude-code'
import type { EngineInterface, Hook, RenderElement, RenderInput } from 'claude-code'

import { CliFailed, hiveRows } from './cli'
import { FAILED, INSTRUCTIONS, budgetFor, entries, history, promptBlocks, recalledIn, reportOf, sessionsFor } from './recall-text'
import type { Listed, Row } from './recall-text'
import type { HiveRecalled } from '../types'

// Automatic recall (beta, opt-in through the `recall` option): before the main agent starts on a prompt the user
// typed, one model call over the project's recent sessions picks the entries that bear on it, and their text comes
// with the prompt.

const SOL = 'gpt-6.1-sol'
const FALLBACK = 'sonnet'
// The whole step, CLI reads included; past it the prompt goes on with a note that recall did not finish.
const TIMEOUT_MS = 30_000

// `auto` tries Sol (through model-router) first; a process without it uses FALLBACK from then on.
let isSolMissing = false

// What recall gave each prompt, by its transcript row, shown under the prompt in the ctrl+o transcript.
const recalled = atom({ plugin: 'hive', key: 'recalled' } as const, null as HiveRecalled)
// Off the terminal, by row: the person opened what recall gave the prompt.
const isRecallOpen = atom({ plugin: 'hive', key: 'isRecallOpen' } as const, false)

type Shown = Exclude<HiveRecalled, null>
// What recall gave prompts not stored yet: the prompt's row (`session.append`) names the row it shows under.
const unstored: Array<{ prompt: string; shown: Shown }> = []
// The sessions whose earlier prompts were read back from the transcript in this process (see restore).
const restored = new Set<string>()

/**
 * What recall gave this session's prompts before this process held them (a resumed session, a restarted one), read
 * back from the reports its transcript keeps. What this process recalled stands. Recall that found nothing leaves no
 * report, so those prompts stay unknown.
 */
async function restore($: EngineInterface, sessionId: string) {
  const rows = (await hiveRows($, ['local', 'outline', sessionId, '--uuids', '--no-agents', '--clip', '0'])) as Array<Row>
  await Promise.all(recalledIn(rows).map(([requestId, shown]) => update($, memberOf(recalled, { requestId }), was => was ?? shown)))
}

/** What recall gave a prompt: the report the main agent reads with it (none when nothing was found), and what the person sees. */
type Outcome = { context?: string; shown: Shown }
const NOTHING: Outcome = { shown: 'none' }
const FAILURE: Outcome = { context: FAILED, shown: 'failed' }

/** Recall for `prompt`, until `deadline` (the clock's time): past it, `signal` aborts and nothing new starts. */
async function recall($: EngineInterface, choice: string, prompt: string, deadline: number, signal: AbortSignal): Promise<Outcome> {
  const session = (await $.session.id()).slice(0, 8)
  const models = choice === 'auto' ? (isSolMissing ? [FALLBACK] : [SOL, FALLBACK]) : [choice]
  const listed = (await hiveRows($, ['local', 'sessions', '-n', '500', '--brief'], signal)) as Array<Listed>
  const outlineOf = (locs: Array<string>) =>
    hiveRows($, ['local', 'outline', ...locs, '--replies', '--no-agents', '--clip', '0'], signal) as Promise<Array<Row>>
  // A session gone since the listing fails its whole batch before a row prints: the batch is read again one session
  // at a time, without the ones that fail.
  const outline = (locs: Array<string>) =>
    outlineOf(locs).catch(async (error: unknown) => {
      if (!(error instanceof CliFailed) || error.hasRows || locs.length === 1) throw error
      const rows: Array<Row> = []
      for (const loc of locs) rows.push(...(await outlineOf([loc]).catch((e: unknown) => (e instanceof CliFailed && !e.hasRows ? [] : Promise.reject(e)))))
      return rows
    })
  const sessions = await sessionsFor(listed, outline, budgetFor(models[0]!))
  for (const model of models) {
    const now = await $.clock.now()
    const h = history(sessions, now, budgetFor(model), session)
    // Nothing the main agent lacks (a new project, say): no entry could come of a call.
    if (![...h.seen.keys()].some(h.isNew)) return NOTHING
    // A call with under a second left could not answer.
    const left = Math.ceil(deadline - now)
    if (left < 1000) return FAILURE
    const r = await $.model
      .complete(
        { model, system: INSTRUCTIONS, prompt: promptBlocks(h, session, prompt, model.startsWith('gpt-')), effort: 'low', maxTokens: 4000, timeoutMs: left },
        { signal },
      )
      .catch(() => undefined)
    if (r?.isAnswered) {
      const found = entries(r.text, h)
      return found.length ? { context: reportOf(found, h.from), shown: { entries: found, from: h.from } } : NOTHING
    }
    // Not there to ask (Sol without model-router): the request was refused, or a 4xx other than a rate limit.
    const isMissing = !r || (r.reason === 'api-error' && r.status !== null && r.status >= 400 && r.status < 500 && r.status !== 429)
    if (!isMissing) return FAILURE
    if (model === SOL) isSolMissing = true
  }
  return FAILURE
}

type Submit = Parameters<Hook<'prompt.submit'>>

/** While recall is on, register.tsx's prompt.submit hook ends here: the prompt goes on with recall's report. */
export async function submitWithRecall($: EngineInterface, e: Submit[1], next: Submit[2], choice: string) {
  // Only what the user typed, at the terminal or through Remote Control; never a slash command.
  if ((e.origin.kind !== 'composer' && e.origin.kind !== 'bridge') || e.command) return next(e)
  $.ui.status('recalling past sessions…')
  // Past the limit the prompt goes on, and the CLI reads and model call still running are stopped.
  const stop = new AbortController()
  const deadline = (await $.clock.now()) + TIMEOUT_MS
  const expired = $.clock.sleep(TIMEOUT_MS, { signal: stop.signal }).then(() => FAILURE)
  const { context, shown } = await Promise.race([recall($, choice, e.text, deadline, stop.signal), expired]).catch(() => FAILURE)
  stop.abort()
  $.ui.status(undefined)
  const waiting = { prompt: e.text, shown }
  unstored.push(waiting)
  // One a prompt never claimed is not kept for long.
  if (unstored.length > 5) unstored.shift()
  // A prompt that did not enter as typed (dropped, or its text rewritten beneath) leaves no row to claim it, and an
  // identical prompt later must not take its place.
  const forget = () => unstored.includes(waiting) && unstored.splice(unstored.indexOf(waiting), 1)
  try {
    const entered = await next(context ? { ...e, context: [...(e.context ?? []), context] } : e)
    if ('drop' in entered || entered.text !== e.text) forget()
    return entered
  } catch (error) {
    forget()
    throw error
  }
}

type Append = Parameters<Hook<'session.append'>>

/** register.tsx's hook on a prompt's row as it is stored: what recall gave the prompt is kept under the row's id. */
export async function onPromptStored($: EngineInterface, e: Append[1], next: Append[2]) {
  const stored = await next(e)
  const i = unstored.findIndex(r => e.message.content.some(block => block.type === 'text' && block.text === r.prompt))
  if (e.agentId === undefined && i >= 0) {
    const { shown } = unstored.splice(i, 1)[0]!
    await update($, memberOf(recalled, { requestId: e.uuid }), () => shown)
  }
  return stored
}

type UserMessage = RenderInput<'UserMessage'>

// Who wrote an entry, by its label in the history.
const WHO: Record<string, string> = { user: 'You', assistant: 'Claude', 'user answer': 'Your answer' }

/**
 * What recall gave a prompt, under it: a dim line counting the entries it found, drawn like the transcript's own
 * folded tool calls, that a press opens and closes (ctrl+o opens it on a terminal that takes no presses); the
 * terminal's ctrl+o transcript shows the entries, and when recall found none or did not finish, says so. `next` is
 * typed by hand: `Hook<'ui.render'>` does not narrow to one component.
 */
export async function drawRecalled($: EngineInterface, e: UserMessage, next: (e: UserMessage) => Promise<RenderElement>) {
  const isTerminal = e.surface === 'terminal'
  const isFull = isTerminal && e.props.isExpanded
  const shown = await read($, memberOf(recalled, e))
  // A row this process knows nothing of: the session's earlier rows are read back, once (a failed read, an older CLI's
  // say, is not asked again), and the write draws this row again. A drawing writes no state, so a timer does it.
  if (shown === null) {
    const sessionId = await $.session.id()
    if (!restored.has(sessionId)) {
      restored.add(sessionId)
      $.clock.after(0, () => void restore($, sessionId).catch(() => undefined))
    }
  }
  const found = typeof shown === 'object' ? shown : null
  if (shown === null || (!found && !isFull)) return next(e)

  // Presses reach the transcript off the terminal, and on it in the fullscreen layout.
  const canPress = !!found && !isFull && (!isTerminal || e.viewport?.isFullscreen === true)
  const isOpen = isFull || (canPress && (await read($, memberOf(isRecallOpen, e))))
  const { Box, Text, Button, Markdown } = $.ui.resolve(e)
  const n = found?.entries.length ?? 0
  const words = found
    ? ['hive recalled ', <Text key="n" bold>{n}</Text>, n === 1 ? ' memory' : ' memories']
    : [shown === 'failed' ? 'hive recall did not finish' : 'hive recall found nothing relevant']
  const toggle = () => update($, memberOf(isRecallOpen, e), open => !open)
  const line = canPress ? (
    <Button key="hive:recall" plain dimColor onPress={toggle}>
      {isTerminal ? '' : isOpen ? '▾ ' : '▸ '}
      {words}
    </Button>
  ) : (
    <Text dimColor>
      {words}
      {isFull ? '' : ' (ctrl+o to expand)'}
    </Text>
  )
  const below = await next(e)
  // Each entry under who wrote it and when, so it reads as quoted history, not as Claude's reply.
  const body =
    isOpen && found ? (
      <Box flexDirection="column" rowGap={1}>
        {found.entries.map(entry => (
          <Box key={entry.loc} flexDirection="column">
            <Text dimColor>
              {[WHO[entry.label] ?? entry.label, entry.date, entry.loc.split(':')[0]].filter(Boolean).join(' · ')}
              {entry.context ? ` · ${entry.context}` : ''}
            </Text>
            <Markdown text={entry.text} />
          </Box>
        ))}
        <Text dimColor>Recall read this project's messages and replies since {found.from}.</Text>
      </Box>
    ) : null
  // On the terminal a blank line comes before the line, as before the transcript's own folded tool lines (`Read 2
  // files`). Off the terminal the line sits under the person's message, which it is about, and the entries open full width,
  // framed. The desktop keeps a row under the message for its hover actions, so the line sits below that row (pulled
  // into it, the actions cover it), and a row under the line balances the space above it.
  return (
    <Box flexDirection="column" marginBottom={isTerminal ? 0 : 1}>
      {below}
      <Box flexDirection="column" alignItems={isTerminal ? 'flex-start' : 'flex-end'} paddingLeft={isTerminal ? 2 : 0} marginTop={isTerminal ? 1 : 0}>
        {line}
      </Box>
      {body &&
        (isTerminal ? (
          <Box paddingLeft={4}>{body}</Box>
        ) : (
          <Box borderStyle="round" borderDimColor paddingX={1}>
            {body}
          </Box>
        ))}
    </Box>
  )
}
