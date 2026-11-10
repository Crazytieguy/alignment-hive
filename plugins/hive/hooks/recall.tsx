import { atom, memberOf, read, update } from 'claude-code'
import type { EngineInterface, Hook, MatchedHook, RenderElement, RenderInput } from 'claude-code'

import { CliFailed, localReads } from './cli'
import type { Read } from './cli'
import { FAILED, INSTRUCTIONS, TOOL_FAILED, TOOL_FOUND_NOTHING, TOOL_REFUSED, budgetFor, entries, history, newThreads, noteText, promptBlocks, recalledIn, reportOf, sessionsFor, withRewind } from './recall-text'
import type { Ask, Held, Listed, RECALL_TOOL, Row, Session } from './recall-text'
import type { HiveRecalled } from '../types'

// Automatic recall (beta, opt-in through the `recall` option): before the main agent starts on a prompt the user
// typed, one model call over the project's recent sessions picks the entries that bear on it, and their text comes
// with the prompt; likewise for comment threads new to the main agent, with the read that brought them; and for what
// the main agent describes through the recall tool, as the tool's result.

const SOL = 'gpt-6.1-sol'
const FALLBACK = 'sonnet'
// The whole step, CLI reads included; past it the prompt goes on with a note that recall did not finish.
const TIMEOUT_MS = 30_000

// The models this process found missing, not asked again: Sol without model-router, a model chosen by name that the
// router does not serve.
const missing = new Set<string>()

/** The models a recall asks, in turn: `auto` tries Sol, then FALLBACK. */
const modelsFor = (choice: string) => (choice === 'auto' ? [SOL, FALLBACK] : [choice]).filter(model => !missing.has(model))

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
  const rows = (await localReads($)(['local', 'outline', sessionId, '--uuids', '--no-agents', '--clip', '0'])) as Array<Row>
  await Promise.all(recalledIn(rows).map(([requestId, shown]) => update($, memberOf(recalled, { requestId }), was => was ?? shown)))
}

const LISTING = ['local', 'sessions', '-n', '500', '--brief']

/**
 * Sessions' outlines, read in one call. A session gone since the listing fails its whole batch before a row prints: the
 * batch is read again one session at a time, without the ones that fail.
 */
function outlinesOf(read: Read) {
  const outlineOf = (locs: Array<string>) => read(['local', 'outline', ...locs, '--replies', '--no-agents', '--clip', '0']) as Promise<Array<Row>>
  return (locs: Array<string>) =>
    outlineOf(locs).catch(async (error: unknown) => {
      if (!(error instanceof CliFailed) || error.hasRows || locs.length === 1) throw error
      const rows: Array<Row> = []
      for (const loc of locs) rows.push(...(await outlineOf([loc]).catch((e: unknown) => (e instanceof CliFailed && !e.hasRows ? [] : Promise.reject(e)))))
      return rows
    })
}

/** What recall reads: the project's sessions newest first, each with its outline, while they can reach a window of `budget`. */
async function readRecallSessions(read: Read, budget: number): Promise<Array<Session>> {
  return sessionsFor((await read(LISTING)) as Array<Listed>, outlinesOf(read), budget)
}

// The warm running, which a recall stops; and the recalls running, during which no warm starts.
let warming: AbortController | null = null
let recalls = 0

/**
 * Fills the local server with what the next recall reads, so that recall finds it parsed (register.tsx: once the
 * session starts and after each of its turns, while recall is on). It reads as recall does, as warm reads: behind
 * prompts' reads. One at a time and never during a recall, which stops one running; never through the CLI, as a CLI
 * run keeps nothing.
 */
export async function warm($: EngineInterface, choice: string) {
  const model = modelsFor(choice)[0]
  if (warming || recalls > 0 || !model) return
  const stop = (warming = new AbortController())
  try {
    await readRecallSessions(localReads($, stop.signal, true), budgetFor(model))
  } catch {
    // Stopped, or the server did not serve: the next recall reads on its own.
  } finally {
    warming = null
  }
}

/** What a recall came to: its report, when it found entries, and what the person sees under a prompt. */
type Outcome = { report?: string; shown: Shown }
const NOTHING: Outcome = { shown: 'none' }
const FAILURE: Outcome = { shown: 'failed' }

type Completed = Awaited<ReturnType<EngineInterface['model']['complete']>> | undefined

/** How a model call ended, for the debug log. */
const endOf = (r: Completed) => (!r ? 'refused' : r.isAnswered ? 'answered' : r.reason === 'api-error' ? `api-error ${r.status}` : r.reason)

/**
 * What recall is for, made of the current session's rows as recall read them: undefined rows when it read none of the
 * session (a script ran it), and no ask when there is nothing to recall for.
 */
type AskOf = (rows: ReadonlyArray<Row> | undefined) => Ask | undefined

/** What the main agent holds of its conversation; undefined when the engine does not say. */
async function heldOf($: EngineInterface): Promise<Held | undefined> {
  const messages = await $.session.messages({ as: 'api' }).catch(() => undefined)
  if (!messages) return undefined
  const replies = messages.flatMap(m => (m.role === 'assistant' ? m.content.flatMap(b => (b.type === 'text' && typeof b.text === 'string' ? [b.text] : [])) : []))
  return { messages: messages.length, replies }
}

/**
 * Recall for what `askOf` makes of the current session (no ask: no call, and nothing found), until `deadline` (the
 * clock's time): past it, `signal` aborts and nothing new starts. `mayRewind`: a prompt sent while the session was idle,
 * after a rewind the transcript may not show yet (withRewind).
 */
async function recall($: EngineInterface, choice: string, askOf: AskOf, deadline: number, signal: AbortSignal, mayRewind: boolean): Promise<Outcome> {
  const session = (await $.session.id()).slice(0, 8)
  const models = modelsFor(choice)
  // A model chosen by name and found missing: no call could answer.
  if (!models.length) return FAILURE
  const started = await $.clock.now()
  const [read, held] = await Promise.all([readRecallSessions(localReads($, signal), budgetFor(models[0]!)), mayRewind ? heldOf($) : undefined])
  const sessions = read.map(s => (s.loc === session && held ? { ...s, rows: withRewind(s.rows, held) } : s))
  $.ui.log(`hive recall: read ${sessions.length} sessions in ${(await $.clock.now()) - started} ms`, { to: 'debug' })
  const ask = askOf(sessions.find(s => s.loc === session)?.rows)
  if (!ask) return NOTHING
  for (const model of models) {
    const now = await $.clock.now()
    const h = history(sessions, now, budgetFor(model), session)
    // Nothing the main agent lacks (a new project, say): no entry could come of a call.
    if (![...h.seen.keys()].some(h.isNew)) return NOTHING
    // A call with under a second left could not answer.
    const left = Math.ceil(deadline - now)
    if (left < 1000) return FAILURE
    const asked = await $.clock.now()
    const r: Completed = await $.model
      .complete(
        { model, system: INSTRUCTIONS, prompt: promptBlocks(h, session, ask, model.startsWith('gpt-')), effort: 'low', maxTokens: 4000, timeoutMs: left },
        { signal },
      )
      .catch(() => undefined)
    $.ui.log(`hive recall: ${model} ${endOf(r)} in ${(await $.clock.now()) - asked} ms, after ${asked - now} ms of history`, { to: 'debug' })
    if (r?.isAnswered) {
      const found = entries(r.text, h)
      return found.length ? { report: reportOf(found, h.from, ask), shown: { entries: found, from: h.from } } : NOTHING
    }
    // Not there to ask (the request was refused, or a 4xx other than a rate limit): the next model, if any. Any other
    // failure (a rate limit, a server error, no answer in time) fails this recall: FALLBACK is only for a process
    // without Sol, and is never marked missing.
    const isMissing = !r || (r.reason === 'api-error' && r.status !== null && r.status >= 400 && r.status < 500 && r.status !== 429)
    if (!isMissing) return FAILURE
    if (model !== FALLBACK) missing.add(model)
  }
  return FAILURE
}

/** Recall within TIMEOUT_MS, said on the status line meanwhile: past it, the CLI reads and model call still running stop. */
async function recallInTime($: EngineInterface, choice: string, askOf: AskOf, mayRewind = false): Promise<Outcome> {
  $.ui.status('recalling past sessions…')
  const stop = new AbortController()
  const deadline = (await $.clock.now()) + TIMEOUT_MS
  const expired = $.clock.sleep(TIMEOUT_MS, { signal: stop.signal }).then(() => FAILURE)
  // Recall's reads go before a warm's.
  recalls++
  warming?.abort()
  const outcome = await Promise.race([recall($, choice, askOf, deadline, stop.signal, mayRewind), expired])
    .catch(() => FAILURE)
    .finally(() => recalls--)
  stop.abort()
  $.ui.status(undefined)
  return outcome
}

type Submit = Parameters<Hook<'prompt.submit'>>

/** While recall is on, register.tsx's prompt.submit hook ends here: the prompt goes on with recall's report. */
export async function submitWithRecall($: EngineInterface, e: Submit[1], next: Submit[2], choice: string) {
  // Only what the user typed, at the terminal or through Remote Control; never a slash command.
  if ((e.origin.kind !== 'composer' && e.origin.kind !== 'bridge') || e.command) return next(e)
  // Past the limit the prompt goes on. Only a prompt sent while no turn runs can follow a rewind.
  const { report, shown } = await recallInTime($, choice, () => ({ prompt: e.text }), e.turnId === undefined)
  // Only a prompt says when recall did not finish: it is where the main agent would have had the entries.
  const context = report ?? (shown === 'failed' ? FAILED : undefined)
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

type Call = Parameters<MatchedHook<'tool.call', { tool: 'ArtifactComments' }>>

// The comment threads a recall ran on in the main agent's current turn, by name: a thread read again in the turn
// (whole, after a listing elided some of its comments, say) needs no recall of its own. Cleared as each turn ends.
const recalledThisTurn = new Set<string>()

/** register.tsx's turn.complete hook, for the main agent: the next turn's reads start afresh. */
export const endRecallTurn = () => recalledThisTurn.clear()

/**
 * While recall is on, register.tsx's hook on ArtifactComments ends here: a read by the main agent that brings comment
 * threads new to it comes with recall's report on them, as the person's comments are much like their prompts. A
 * recall that found nothing, or did not finish, adds nothing. A session a script ran gets none, as its prompts do not.
 */
export async function readWithRecall($: EngineInterface, e: Call[1], next: Call[2], choice: string) {
  const read = await next(e)
  if (e.agentId !== undefined || e.action !== 'read' || read.deny !== undefined || read.isError || !read.text) return read
  const text = read.text
  const unrecalled = (rows: ReadonlyArray<Row>) => newThreads(text, rows).filter(t => !recalledThisTurn.has(t.id))
  // No thread, or none this turn has not had: no history to read.
  if (!unrecalled([]).length) return read
  const { report } = await recallInTime($, choice, rows => {
    const threads = rows ? unrecalled(rows) : []
    for (const t of threads) if (t.id) recalledThisTurn.add(t.id)
    return threads.length ? { threads: threads.map(t => t.text) } : undefined
  })
  return report ? { ...read, context: [...(read.context ?? []), report] } : read
}

type Asked = Parameters<MatchedHook<'tool.call', { tool: typeof RECALL_TOOL }>>[1]

/**
 * While recall is on, register.tsx's hook on the recall tool ends here: recall for what the main agent describes.
 * Unlike a prompt or a comments read, a session a script ran gets one too, since its agent asked. A subagent is refused:
 * a registered tool can't be hidden from it, and its context may be limited on purpose.
 */
export async function answerRecallTool($: EngineInterface, e: Asked, choice: string) {
  if (e.agentId !== undefined) return { deny: TOOL_REFUSED }
  const { report, shown } = await recallInTime($, choice, () => ({ about: e.about as string }))
  return { result: report ?? (shown === 'failed' ? TOOL_FAILED : TOOL_FOUND_NOTHING) }
}

/** What the typeahead says of `/remember <note>`, which register.tsx's session.start registers. */
export const REMEMBER_DESCRIPTION = 'Save a note for automatic recall in later sessions, without starting a turn'

/**
 * `/remember <note>`: the note joins the session as a user row the main agent reads with its next request, and starts
 * no turn; typed mid-turn it joins the running one. Recall reads it in later sessions as the user's note (recall-text's
 * label). The command's own row keeps no arguments, so its output repeats the note for the person.
 */
export async function remember($: EngineInterface, words: string, isRecallOn: boolean): Promise<{ text: string }> {
  const note = words.trim()
  if (!note) return { text: 'Usage: /remember <note>. Saves the note for automatic recall in later sessions, without starting a turn.' }
  // A plugin above refuses with `{ deny }`; the engine's own refusal rejects.
  const added = await $.session
    .append({ message: { type: 'user', content: [{ type: 'text', text: noteText(note) }] } })
    .catch((error: unknown) => ({ deny: error instanceof Error ? error.message : String(error) }))
  if (added.deny !== undefined) return { text: `Not saved: ${added.deny}` }
  return { text: isRecallOn ? 'Saved for recall' : 'Saved. Automatic recall is off, so only the hive:retrieval agent will find it; /config turns recall on.' }
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
const WHO: Record<string, string> = { user: 'You', 'user note': 'Your note', assistant: 'Claude', 'user answer': 'Your answer', comments: 'Page comments' }

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
