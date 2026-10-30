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

/**
 * The CLI's JSON Lines rows, read as they stream: `$.process.run` keeps only the first 4 MiB of output, which a
 * project's outlines can pass. A line that does not parse is skipped. Rejects with CliFailed when the CLI exits
 * unsuccessfully, so a read that failed partway never passes for a whole one, and at once when `signal` aborts; the
 * CLI is then ended at its next write, as a pending read cannot be cut short, or finishes on its own.
 */
export async function hiveRows($: EngineInterface, args: string[], signal?: AbortSignal): Promise<Array<Record<string, unknown>>> {
  const rows: Array<Record<string, unknown>> = []
  const take = (line: string) => {
    try {
      rows.push(JSON.parse(line) as Record<string, unknown>)
    } catch {
      // a note or a blank line
    }
  }
  signal?.throwIfAborted()
  const stopped = new Promise<never>((_, reject) => signal?.addEventListener('abort', () => reject(signal.reason), { once: true }))
  const child = $.process.spawn({ argv: argv($, args), ...(await init($)) })
  // It rejects when the child is ended early; the loop below reads how it ended.
  child.result.catch(() => undefined)
  let rest = ''
  try {
    for (;;) {
      const piece = await Promise.race([child.next(), stopped])
      if (piece.done) {
        take(rest)
        if (piece.value.code === 0) return rows
        throw new CliFailed(`hive ${args.slice(0, 2).join(' ')}: exit ${piece.value.code ?? piece.value.signal}`, rows.length > 0)
      }
      if (piece.value.stream !== 'stdout') continue
      const lines = (rest + piece.value.text).split('\n')
      rest = lines.pop()!
      lines.forEach(take)
    }
  } finally {
    void child.return(undefined as never).catch(() => undefined)
  }
}
