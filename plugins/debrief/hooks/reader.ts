// The reader check as a tool: reads a rendered debrief round the way its user meets it and returns the questions
// each passage leaves them with. `hive debrief view` gives what the reader sees; one call reads the top of the page
// and the collapsed list, which writes that shared opening to the prompt cache, then one call per item reads the
// opening from the cache plus the item's body, up to 16 at a time.
import type { Register } from 'claude-code'

const MODEL = 'sonnet'
const MAX_IN_FLIGHT = 16

const BRIEF = `You are the person who delegated a task to an AI agent, reading the report it wrote for you. You run the project, but you did not watch the agent work and skipped most of its messages. What you knew before opening the report is under <known>.

You read the page in a browser, top to bottom: first the top of the page, then the list of items, each collapsed to its heading and a one-line summary, then you open items and read their bodies. You can't use what comes later to understand what comes earlier.

A question worth noting is one that stops you from understanding what a passage says, or from judging whether what it reports is right or what you would have decided: a name, label or term you can't place (quote it); a reference to something you haven't seen; a claim whose reason you would need to judge it; a consequence for you that isn't clear. Skip curiosity, what only someone auditing the evidence would ask, and what the next sentence or two answers. Lines like [Folded diff: ...] are collapsed evidence: you see only their label, and you need them only to audit, not to follow the prose. Most passages raise nothing; don't pad.

Mark each question "blocks" (you can't follow or judge the passage without it) or "nags" (you can follow, but it bothers you). Reply with JSON only: {"questions": [{"item": <item number, 0 for the top of the page>, "piece": <0 for the item's heading and summary, else the body piece number>, "quote": "<the words that raised it>", "question": "<your question>", "severity": "blocks" | "nags"}]}, or {"questions": []} when nothing gets in your way.`

interface Question { item: number; piece: number; quote: string; question: string; severity: string }
interface View {
  readmes: Array<{ path: string; text: string }>
  messages: Array<string>
  top: { title: string; heading: string; story: string; stats: Array<string>; foryou: Array<string> }
  items: Array<{ id: string; section: string; heading: string; lede?: string; isJudgementCall: boolean; isLowerPriority: boolean; pieces: Array<string> }>
}

/** The reply's questions, or null when it is not the JSON asked for (cut off at the token cap, say). */
function parseQuestions(text: string): Array<Question> | null {
  const start = text.indexOf('{'), end = text.lastIndexOf('}')
  if (start < 0 || end < start) return null
  let questions: unknown
  try { questions = JSON.parse(text.slice(start, end + 1)).questions } catch { return null }
  if (!Array.isArray(questions)) return null
  // A number written as a string is still that number; any other malformed entry makes the whole reply unreadable,
  // so a raised question is never dropped as if none were asked.
  const read: Array<Question> = []
  for (const q of questions as Array<Record<string, unknown> | null>) {
    const item = Number(q?.item), piece = Number(q?.piece ?? 0)
    if (!q || typeof q.question !== 'string' || !Number.isInteger(item) || !Number.isInteger(piece)) return null
    read.push({ item, piece, quote: String(q.quote ?? ''), question: q.question, severity: String(q.severity ?? 'nags') })
  }
  return read
}

export const register: Register = (on) => {
  on('session.start', async ($, e, next) => {
    await $.tool.register({
      name: 'read',
      description: 'Reads a rendered debrief round as its user would, knowing only the READMEs as of the session\'s start and their own messages, and returns the questions each passage leaves them with, by item.',
      inputSchema: {
        type: 'object',
        properties: {
          session: { type: 'string', description: 'The full parent session id.' },
          round: { type: 'number' },
          data: { type: 'string', description: 'The debrief plugin\'s data directory.' },
          cwd: { type: 'string', description: 'The directory the work was done in.' },
        },
        required: ['session', 'round', 'data', 'cwd'],
      },
    })
    return next(e)
  })

  on('tool.call', { tool: 'mcp__debrief__read' }, async ($, e) => {
    const input = e as unknown as { session: string; round: number; data: string; cwd: string }
    const ran = await $.process.run(['hive', 'debrief', 'view', '--session', input.session, '--round', String(input.round), '--data', input.data], { cwd: input.cwd, timeoutMs: 120000 })
    if (ran.exitCode !== 0) return { result: `hive debrief view failed: ${ran.stderr.trim() || ran.stdout.trim()}`, isError: true }
    const view = JSON.parse(ran.stdout) as View

    let inFlight = 0, calls = 0
    const waiting: Array<() => void> = []
    const failures: Array<string> = []
    const ask = async (what: string, prompt: Array<{ text: string; cache?: true }>): Promise<Array<Question>> => {
      if (inFlight >= MAX_IN_FLIGHT) await new Promise<void>((resolve) => waiting.push(resolve))
      inFlight++, calls++
      try {
        // One retry: a reply cut off or not the JSON asked for usually comes back whole the second time.
        let reason = ''
        for (let attempt = 0; attempt < 2; attempt++) {
          const r = await $.model.complete({ model: MODEL, prompt, maxTokens: 4000, timeoutMs: 180000 })
          const questions = r.isAnswered ? parseQuestions(r.text) : null
          if (questions !== null) return questions
          reason = r.isAnswered ? 'unreadable reply' : r.reason
        }
        failures.push(`${what}: ${reason}`)
        return []
      } finally { inFlight--; waiting.shift()?.() }
    }

    const known = [
      view.readmes.length ? `The project's READMEs as of when the task began:\n\n${view.readmes.map((d) => `<readme path="${d.path}">\n${d.text.trim()}\n</readme>`).join('\n\n')}` : 'No README.',
      `Your own messages in the session, in order:\n\n${view.messages.map((m) => `<message>\n${m.trim()}\n</message>`).join('\n\n')}`,
    ].join('\n\n')
    const { title, heading, story, stats, foryou } = view.top
    const top = [`# ${title}`, heading, story.trim(), stats.length ? `Stats: ${stats.join(' · ')}` : '', foryou.length ? `Left for you:\n${foryou.map((f) => `- ${f}`).join('\n')}` : ''].filter(Boolean).join('\n\n')
    const collapsed = view.items.map((item, i) => {
      const tags = [item.isJudgementCall && 'JUDGEMENT CALL', item.isLowerPriority && 'lower priority, in a skippable tray'].filter(Boolean)
      return `[Item ${i + 1}]\nSection "${item.section}": ${item.heading}${tags.length ? ` [${tags.join(', ')}]` : ''}${item.lede ? `\n${item.lede}` : ''}`
    })
    // Behind a gateway the cache is found again only while the prompt opens the same way, so the shared text leads it.
    const shared = { text: `${BRIEF}\n\n<known>\n${known}\n</known>\n\n<page_top>\n${top}\n</page_top>\n\n<collapsed_list>\n${collapsed.join('\n\n')}\n</collapsed_list>\n\n`, cache: true as const }
    const questions = await ask('top of the page', [shared, { text: 'You have read the top of the page and skimmed the collapsed list. Note the questions they leave you with (item 0 for the top; piece 0 for an item\'s heading and summary). Questions about bodies come later, when you open the items.' }])
    const bodies = await Promise.all(view.items.map((item, i) => item.pieces.length === 0 ? [] : ask(`item ${i + 1}`, [shared,
      { text: item.pieces.map((piece, j) => `<opened_item number="${i + 1}" piece="${j + 1}">\n${piece}\n</opened_item>\n\n`).join('') },
      { text: `Now you open item ${i + 1} and read its body. Note the questions it leaves you with, each at the piece where it arose, from what you knew by then. Use item ${i + 1}.` }])))
    questions.push(...bodies.flat())

    const lines = questions
      .sort((a, b) => a.item - b.item || (a.piece ?? 0) - (b.piece ?? 0))
      .map((q) => `- ${q.item === 0 ? 'top of the page' : `item ${q.item} (${view.items[q.item - 1]?.id ?? '?'})${q.piece === 0 ? ', heading or lede' : ''}`}, ${q.severity}: "${q.quote}": ${q.question}`)
    if (failures.length === calls) return { result: `Every reader call failed: ${failures.join('; ')}`, isError: true }
    const note = failures.length ? `\n\nIncomplete: ${failures.length} of ${calls} reader calls gave no usable answer (${failures.join('; ')}); those passages were not read.` : ''
    return { result: (lines.length ? lines.join('\n') : 'The readers raised no questions.') + note }
  })
}
