import type { EngineInterface } from 'claude-code'

// The directory the session started in, which hive.sh reads for the dev binary; register.tsx sets it at the start.
export const cli = { projectDir: '' }

function argv($: EngineInterface, args: string[]) {
  return ['bash', `${$.plugin.root}/scripts/hive.sh`, ...args]
}

// NO_COLOR: the CLI's words reach a toast, not a terminal.
function env($: EngineInterface) {
  return { CLAUDE_PLUGIN_ROOT: $.plugin.root, CLAUDE_PROJECT_DIR: cli.projectDir, NO_COLOR: '1' }
}

/** Runs the CLI this plugin pins (scripts/hive.sh) in the session's working directory. */
export async function hive($: EngineInterface, args: string[]) {
  return $.process.run(argv($, args), { env: env($), timeoutMs: 60_000 })
}

/**
 * The CLI's JSON Lines rows, read as they stream: `$.process.run` keeps only the first 4 MiB of output, which a
 * project's outlines can pass. A line that does not parse is skipped.
 */
export async function hiveRows($: EngineInterface, args: string[]): Promise<Array<Record<string, unknown>>> {
  const rows: Array<Record<string, unknown>> = []
  const take = (line: string) => {
    try {
      rows.push(JSON.parse(line) as Record<string, unknown>)
    } catch {
      // a note or a blank line
    }
  }
  let rest = ''
  for await (const piece of $.process.spawn({ argv: argv($, args), env: env($) })) {
    if (piece.stream !== 'stdout') continue
    const lines = (rest + piece.text).split('\n')
    rest = lines.pop()!
    lines.forEach(take)
  }
  take(rest)
  return rows
}
