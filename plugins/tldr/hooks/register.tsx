// tldr as a mod: each long assistant text block is drawn as a one-sentence
// Haiku TL;DR with [ more ] to expand it. Only the drawing changes; the stored
// transcript and the model's context are untouched. /tldr toggles it globally.
import { atom, read, update, memberOf } from 'claude-code'
import type { EngineInterface, Register } from 'claude-code'
import { GUIDELINES, isLong, lineFor, parseArgs, wordCount } from './lib'

const isOn = atom({ plugin: 'tldr', key: 'isOn' } as const, true)
const line = atom({ plugin: 'tldr', key: 'line' } as const, '')
const isExpanded = atom({ plugin: 'tldr', key: 'isExpanded' } as const, false)
const isLearned = atom({ plugin: 'tldr', key: 'isLearned' } as const, false)
const isFinal = atom({ plugin: 'tldr', key: 'isFinal' } as const, false)

const FAIL = '!fail:'
const RETRY_MS = 60_000 // a failed summary is tried again on a redraw this long after
const MAX_RUNNING = 3
const KEEP_PER_SESSION = 200
const KEEP_SESSIONS = 20

// Module state starts over on every reload; $.state and $.store survive it.
const started = new Set<string>() // ids queued, in flight or summarized this load
const order: string[] = [] // long blocks in the order first drawn, for /tldr more|less
const queue: (() => Promise<void>)[] = []
let running = 0
let saving: Promise<void> = Promise.resolve()

function pump() {
  while (running < MAX_RUNNING && queue.length) {
    const job = queue.shift()!
    running++
    job().finally(() => {
      running--
      pump()
    })
  }
}

async function save($: EngineInterface, id: string, t: string) {
  const sid = await $.session.id() // looked up per save: /clear starts a new session
  const saved = ((await $.store.get('s/' + sid)) ?? {}) as Record<string, string>
  saved[id] = t
  const keys = Object.keys(saved)
  for (const k of keys.slice(0, Math.max(0, keys.length - KEEP_PER_SESSION))) delete saved[k]
  await $.store.set('s/' + sid, saved)
  const index = ((await $.store.get('sessions')) ?? []) as string[]
  if (!index.includes(sid)) {
    index.push(sid)
    while (index.length > KEEP_SESSIONS) await $.store.delete('s/' + index.shift())
    await $.store.set('sessions', index)
  }
}

async function summarize($: EngineInterface, id: string, text: string) {
  let t: string
  try {
    const r = await $.model.complete({
      model: 'haiku',
      system: GUIDELINES,
      prompt: '<message>\n' + text + '\n</message>',
      effort: 'low',
      timeoutMs: 30000,
    })
    t = r.isAnswered ? lineFor(r.text) : FAIL + (r.reason ?? 'no answer')
  } catch (err) {
    t = FAIL + 'refused: ' + String(err) // the engine refused to send it (a blocked model, say)
  }
  await update($, memberOf(line, { requestId: id }), () => t)
  await $.ui.log(`tldr ${id}: ${t}`, { to: 'debug' })
  if (t.startsWith(FAIL)) {
    $.clock.after(RETRY_MS, () => started.delete(id))
    return
  }
  // Serialized read-modify-writes; a failed save must not stall the ones after it.
  saving = saving.then(() => save($, id, t)).catch(() => {})
  await saving
}

/** Summarize a block's visible text, drawn after its row was stored (isFinal): only then is it whole. */
function start($: EngineInterface, id: string, text: string) {
  if (started.has(id)) return
  started.add(id)
  queue.push(() => summarize($, id, text).catch(() => {}))
  pump()
}

async function setOn($: EngineInterface, value: boolean) {
  await $.store.set('enabled', value)
  await update($, isOn, () => value)
}

/** The person has used [ more ], [ less ] or /tldr: the inline hint retires for good. */
async function learn($: EngineInterface) {
  if (await read($, isLearned)) return
  await $.store.set('learned', true)
  await update($, isLearned, () => true)
}

async function setExpanded($: EngineInterface, id: string, value: boolean) {
  await update($, memberOf(isExpanded, { requestId: id }), () => value)
  await learn($)
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const enabled = await $.store.get('enabled')
    await update($, isOn, () => enabled !== false)
    const learned = (await $.store.get('learned')) === true
    await update($, isLearned, () => learned)
    await $.command.register({ name: 'tldr', description: 'Collapse long replies to one sentence: /tldr toggles; on, off, more, less' })
    const saved = ((await $.store.get('s/' + (await $.session.id()))) ?? {}) as Record<string, string>
    for (const [id, t] of Object.entries(saved)) await update($, memberOf(line, { requestId: id }), () => t)
    return next(e)
  })

  on('session.append', { door: 'response' }, async ($, e, next) => {
    const r = await next(e)
    // The row is stored, so its blocks are whole. Marking it redraws a block already drawn (it may
    // have been drawn while streaming), and that drawing starts the summary from its visible text.
    if (!e.agentId && r.uuid && r.message?.type === 'assistant') {
      await update($, memberOf(isFinal, { requestId: r.uuid }), () => true)
    }
    return r
  })

  on('command.run', { command: 'tldr' }, async ($, e) => {
    const what = parseArgs(e.args)
    await learn($)
    if (what === 'help') return { text: 'Usage: /tldr (toggle), /tldr on, /tldr off, /tldr more, /tldr less' }
    if (what === 'toggle' || what === 'on' || what === 'off') {
      const value = what === 'toggle' ? !(await read($, isOn)) : what === 'on'
      await setOn($, value)
      return { text: value ? 'tl;dr on: long replies collapse to one sentence; [ more ] expands one.' : 'tl;dr off: replies show in full.' }
    }
    if (!(await read($, isOn))) return { text: 'tl;dr is off; /tldr turns it on.' }
    const wantOpen = what === 'more'
    for (let i = order.length - 1; i >= 0; i--) {
      const me = { requestId: order[i]! }
      const t = await read($, memberOf(line, me))
      if (!t || t.startsWith(FAIL)) continue // drawn in full anyway
      if ((await read($, memberOf(isExpanded, me))) !== wantOpen) {
        await update($, memberOf(isExpanded, me), () => wantOpen)
        return { text: wantOpen ? 'Expanded the latest collapsed reply.' : 'Collapsed the latest expanded reply.' }
      }
    }
    return { text: wantOpen ? 'No collapsed reply to expand.' : 'No expanded reply to collapse.' }
  })

  on('ui.render', { component: 'AssistantMessage' }, async ($, e, next) => {
    const text = e.props.text
    if (e.props.isSummary || !isLong(text) || !(await read($, isOn))) return next(e)
    const id = e.requestId
    if (!order.includes(id)) order.push(id)
    const t = await read($, memberOf(line, e))
    const fin = await read($, memberOf(isFinal, e))
    if (fin && (!t || t.startsWith(FAIL))) start($, id, text)

    // No summary and none coming (not stored yet, so maybe streaming, or stored before this
    // session's mod started): the engine's own drawing, untouched.
    if (!t && !fin) return next(e)

    const { Box, Text, Button } = $.ui.resolve(e)
    const indent = e.surface === 'terminal' ? 2 : 0 // under the message text, past the bullet
    if (t.startsWith(FAIL)) {
      return (
        <Box flexDirection="column">
          {await next(e)}
          <Box paddingLeft={indent}>
            <Text dimColor>tl;dr unavailable: {t.slice(FAIL.length)}</Text>
          </Box>
        </Box>
      )
    }
    if (await read($, memberOf(isExpanded, e))) {
      return (
        <Box flexDirection="column">
          {await next(e)}
          <Box marginTop={1} paddingLeft={indent}>
            <Button key="less" label="less" dimColor onPress={() => setExpanded($, id, false)} />
          </Box>
        </Box>
      )
    }
    const bullet = e.surface === 'terminal' ? (e.props.isFirstOfReply ? '⏺ ' : '  ') : ''
    const learned = await read($, isLearned)
    return (
      <Box flexDirection="column" marginTop={1}>
        <Box flexDirection="row">
          {bullet ? <Text>{bullet}</Text> : null}
          <Box flexGrow={1} flexShrink={1}>
            {t ? <Text>{t}</Text> : <Text dimColor italic>summarizing…</Text>}
          </Box>
        </Box>
        <Box marginTop={1} paddingLeft={indent}>
          <Button key="more" label={`more · ${wordCount(text)} words`} dimColor onPress={() => setExpanded($, id, true)} />
          {learned ? null : <Text dimColor>  /tldr turns collapsing off</Text>}
        </Box>
      </Box>
    )
  })
}
