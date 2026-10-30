import type { EngineInterface } from 'claude-code'

// The directory the session started in, which hive.sh reads for the dev binary; register.tsx sets it at the start.
export const cli = { projectDir: '' }

function argv($: EngineInterface, args: string[]) {
  return ['bash', `${$.plugin.root}/scripts/hive.sh`, ...args]
}

// The CLI reads the project from its working directory, which must not follow a shell `cd`: a session started in ~
// keeps every `cd` beneath it. Before session.start sets projectDir (a resumed session draws its rows first), the
// session's root is still the directory it started in. NO_COLOR: the CLI's words reach a toast, not a terminal.
async function init($: EngineInterface) {
  const dir = cli.projectDir || (await $.session.root())
  return { cwd: dir, env: { CLAUDE_PLUGIN_ROOT: $.plugin.root, CLAUDE_PROJECT_DIR: dir, NO_COLOR: '1' } }
}

/** Runs the CLI this plugin pins (scripts/hive.sh) in the directory the session started in. */
export async function hive($: EngineInterface, args: string[]) {
  return $.process.run(argv($, args), { ...(await init($)), timeoutMs: 60_000 })
}

/** The CLI exited unsuccessfully; `hasRows`: it printed rows first. */
export class CliFailed extends Error {
  constructor(
    message: string,
    readonly hasRows: boolean,
  ) {
    super(message)
  }
}

type Rows = Array<Record<string, unknown>>

/** Adds a line of the CLI's JSON Lines output to `rows`; a line that does not parse is skipped. */
function take(rows: Rows, line: string) {
  try {
    rows.push(JSON.parse(line) as Record<string, unknown>)
  } catch {
    // a note or a blank line
  }
}

/** A read whose CLI exited unsuccessfully, by its exit code. */
const failed = (args: string[], code: unknown, rows: Rows) => new CliFailed(`hive ${args.slice(0, 2).join(' ')}: exit ${code}`, rows.length > 0)

/** Rejects with `signal`'s reason once it aborts; never without one. */
const abortOf = (signal?: AbortSignal) =>
  new Promise<never>((_, reject) => signal?.addEventListener('abort', () => reject(signal.reason), { once: true }))

/**
 * The CLI's JSON Lines rows, read as they stream: `$.process.run` keeps only the first 4 MiB of output, which a
 * project's outlines can pass. Rejects with CliFailed when the CLI exits unsuccessfully, so a read that failed partway
 * never passes for a whole one, and at once when `signal` aborts; the CLI is then ended at its next write, as a pending
 * read cannot be cut short, or finishes on its own.
 */
async function hiveRows($: EngineInterface, args: string[], signal?: AbortSignal): Promise<Rows> {
  const rows: Rows = []
  signal?.throwIfAborted()
  const stopped = abortOf(signal)
  const child = $.process.spawn({ argv: argv($, args), ...(await init($)) })
  // It rejects when the child is ended early; the loop below reads how it ended.
  child.result.catch(() => undefined)
  let rest = ''
  try {
    for (;;) {
      const piece = await Promise.race([child.next(), stopped])
      if (piece.done) {
        take(rows, rest)
        if (piece.value.code === 0) return rows
        throw failed(args, piece.value.code ?? piece.value.signal, rows)
      }
      if (piece.value.stream !== 'stdout') continue
      const lines = (rest + piece.value.text).split('\n')
      rest = lines.pop()!
      for (const line of lines) take(rows, line)
    }
  } finally {
    void child.return(undefined as never).catch(() => undefined)
  }
}

// The local server (`hive local-server`): one background process for all of the user's sessions that keeps what it
// read of their transcripts in memory and answers `hive local sessions` and `outline` as the CLI would, so a read finds
// most files parsed. Its socket, once `--ensure` named one, and that call while it runs.
const server: { socket?: string; ensuring?: Promise<string | undefined> } = {}
// `--ensure` starts the server when none runs, then waits up to 2 s for it to answer.
const ENSURE_MS = 3_000
// A read the server has not answered by then (busy with other sessions' reads, or stuck) goes to the CLI.
const READ_MS = 5_000

/**
 * The server's socket: the one known, else what `hive local-server --ensure` names, starting the server when none
 * runs; undefined when none can serve (a CLI without it, a cutoff set). `failed`: a socket that just failed,
 * forgotten first. One `--ensure` at a time.
 */
function ensureServer($: EngineInterface, failed?: string): Promise<string | undefined> {
  if (failed !== undefined && server.socket === failed) server.socket = undefined
  if (server.socket !== undefined) return Promise.resolve(server.socket)
  server.ensuring ??= (async () => {
    const { exitCode, stdout } = await $.process.run(argv($, ['local-server', '--ensure']), { ...(await init($)), timeoutMs: ENSURE_MS })
    // One JSON line; one without a socket (`{}`, or bootstrap's install notice) is none.
    const said = exitCode === 0 ? (JSON.parse(stdout.trim().split('\n').pop()!) as { socket?: unknown } | null) : null
    server.socket = typeof said?.socket === 'string' ? said.socket : undefined
    return server.socket
  })()
    .catch(() => undefined)
    .finally(() => (server.ensuring = undefined))
  return server.ensuring
}

type Reply = { code: number; stdout: string }

/** The server's reply, whole: an exit code and the CLI's output. */
function replyOf(text: string): Reply | undefined {
  try {
    const reply = JSON.parse(text) as { code?: unknown; stdout?: unknown; stderr?: unknown } | null
    if (!Number.isInteger(reply?.code) || typeof reply!.stdout !== 'string' || typeof reply!.stderr !== 'string') return undefined
    return { code: reply!.code as number, stdout: reply!.stdout }
  } catch {
    // cut short at `$.http.fetch`'s 4 MiB, say
    return undefined
  }
}

/**
 * One read asked of the server: its reply, `late` when none came in READ_MS, else undefined (unreachable, an error
 * status, a reply cut short or malformed). Rejects at once when `signal` aborts: `$.http.fetch` takes no signal, and
 * the server finishes the read on its own.
 */
async function ask($: EngineInterface, socket: string, args: string[], isWarm: boolean, signal?: AbortSignal) {
  const { cwd } = await init($)
  // The session (for a `current` locator, which recall's reads never use); none for a warm read, so that the server
  // takes the same warm read from several sessions as one.
  const sessionId = isWarm ? null : await $.session.id()
  // The server runs `hive local`'s verbs: the arguments after `local`.
  const body = JSON.stringify({ argv: args.slice(1), cwd, sessionId })
  const headers: Record<string, string> = { 'content-type': 'application/json', ...(isWarm && { 'x-hive-warm': '1' }) }
  const timer = new AbortController()
  const answered = $.http
    .fetch('http://hive/local', { method: 'POST', headers, body, socketPath: socket })
    .then(({ status, text }) => (status === 200 ? replyOf(text) : undefined), () => undefined)
  const late = $.clock.sleep(READ_MS, { signal: timer.signal }).then(() => 'late' as const)
  try {
    return await Promise.race([answered, late, abortOf(signal)])
  } finally {
    timer.abort()
  }
}

/** One task's reads (a recall's, a warm's): `hive local` arguments to their rows. */
export type Read = (args: string[]) => Promise<Rows>

/**
 * A task's reads through the server, with the rows and the CliFailed the CLI's own would give. A read that fails
 * there is asked once more of the server ensured again; once that fails too, or the server gave no answer in time or
 * none runs, the task's later reads skip it, each going to the CLI (hiveRows), or rejecting when `isWarm`. A server
 * late for a recall is forgotten, so the next task ensures it again. `isWarm`: reads that fill the server for a later
 * recall, queued behind prompts' reads and never through the CLI, as a CLI run keeps nothing.
 */
export function localReads($: EngineInterface, signal?: AbortSignal, isWarm = false): Read {
  const fallback: Read | undefined = isWarm ? undefined : args => hiveRows($, args, signal)
  let isOff = false
  const viaServer = async (args: string[]): Promise<Rows | string> => {
    let socket = await ensureServer($)
    if (socket === undefined) return 'none runs'
    for (let isRetry = false; ; isRetry = true) {
      const reply = await ask($, socket, args, isWarm, signal)
      if (reply === 'late') {
        if (!isWarm && server.socket === socket) server.socket = undefined
        return `no answer in ${READ_MS} ms`
      }
      if (reply) {
        const rows: Rows = []
        for (const line of reply.stdout.split('\n')) take(rows, line)
        if (reply.code === 0) return rows
        throw failed(args, reply.code, rows)
      }
      if (isRetry) return 'failed twice'
      socket = await ensureServer($, socket)
      if (socket === undefined) return 'failed, and none runs'
    }
  }
  return async args => {
    signal?.throwIfAborted()
    const served = isOff ? undefined : await viaServer(args)
    if (typeof served === 'object') return served
    if (served) $.ui.log(`hive local server: ${served}; ${fallback ? 'reading through the CLI' : 'warm stopped'}`, { to: 'debug' })
    isOff = true
    if (!fallback) throw new Error(`hive local server: ${served ?? 'off'}`)
    return fallback(args)
  }
}
