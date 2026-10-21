import type { EngineInterface, Hook } from 'claude-code'

import { hiveRows } from './cli'
import { FAILED, INSTRUCTIONS, budgetFor, history, promptBlocks, report, sessionsFor } from './recall-text'
import type { Listed, Row } from './recall-text'

// Automatic recall (beta, opt-in through the `recall` option): before the main agent starts on a prompt the user
// typed, one model call over the project's recent sessions picks the entries that bear on it, and their text comes
// with the prompt.

const SOL = 'gpt-6.1-sol'
const FALLBACK = 'sonnet'
// The whole step, CLI reads included; past it the prompt goes on with a note that recall did not finish.
const TIMEOUT_MS = 30_000

// `auto` tries Sol (through model-router) first; a process without it uses FALLBACK from then on.
let isSolMissing = false

/** The report for `prompt`; undefined when recall found nothing, FAILED when it could not run. */
async function recall($: EngineInterface, choice: string, prompt: string): Promise<string | undefined> {
  const session = (await $.session.id()).slice(0, 8)
  const models = choice === 'auto' ? (isSolMissing ? [FALLBACK] : [SOL, FALLBACK]) : [choice]
  const listed = (await hiveRows($, ['local', 'sessions', '-n', '500'])) as Array<Listed>
  const outline = (locs: Array<string>) => hiveRows($, ['local', 'outline', ...locs, '--replies', '--clip', '0']) as Promise<Array<Row>>
  const sessions = await sessionsFor(listed, outline, budgetFor(models[0]!))
  for (const model of models) {
    const h = history(sessions, await $.clock.now(), budgetFor(model), session)
    // Nothing the main agent lacks (a new project, say): no entry could come of a call.
    if (![...h.seen.keys()].some(h.isNew)) return undefined
    const r = await $.model
      .complete({ model, system: INSTRUCTIONS, prompt: promptBlocks(h, session, prompt), effort: 'low', maxTokens: 4000, timeoutMs: TIMEOUT_MS })
      .catch(() => undefined)
    if (r?.isAnswered) return report(r.text, h)
    // Not there to ask (Sol without model-router): the request was refused, or a 4xx other than a rate limit.
    const isMissing = !r || (r.reason === 'api-error' && r.status !== null && r.status >= 400 && r.status < 500 && r.status !== 429)
    if (!isMissing) return FAILED
    if (model === SOL) isSolMissing = true
  }
  return FAILED
}

type Submit = Parameters<Hook<'prompt.submit'>>

/** While recall is on, register.tsx's prompt.submit hook ends here: the prompt goes on with recall's report. */
export async function submitWithRecall($: EngineInterface, e: Submit[1], next: Submit[2], choice: string) {
  // Only what the user typed, at the terminal or through Remote Control; never a slash command.
  if ((e.origin.kind !== 'composer' && e.origin.kind !== 'bridge') || e.command) return next(e)
  $.ui.status('recalling past sessions…')
  // A slow CLI read or model call past the limit keeps running, but the prompt goes on.
  const text = await Promise.race([recall($, choice, e.text), $.clock.sleep(TIMEOUT_MS).then(() => FAILED)]).catch(() => FAILED)
  $.ui.status(undefined)
  return next(text ? { ...e, context: [...(e.context ?? []), text] } : e)
}
