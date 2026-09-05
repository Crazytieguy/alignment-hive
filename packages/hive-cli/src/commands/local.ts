import { getClaudeProjectDir, getStateDir, loadTranscriptsDirs } from '../lib/config';
import { localCmd } from '../lib/messages';
import { printError } from '../lib/output';
import { readRawSession } from '../lib/session-io';
import { discoverSessions } from '../lib/session-state';
import type { ReadSessionResult } from '../lib/session-format';
import type { DiscoveredSession } from '../lib/session-io';

/** How the local commands see sessions; the tests substitute an in-memory one. */
export interface SessionSource {
  /** Session file paths; readSession accepts exactly these. */
  listSessionFiles: (cwd: string) => Promise<Array<string>>;
  readSession: (path: string) => Promise<ReadSessionResult>;
}

function createRawSessionSource(): SessionSource {
  // Discovered records carry agent metadata (parentSessionId/agentType/workflowRunId) that is
  // not recoverable from the path alone, so readSession looks the record up by path.
  let byPath = new Map<string, DiscoveredSession>();
  return {
    async listSessionFiles(cwd) {
      const dirs = await loadTranscriptsDirs(getStateDir(cwd));
      const sessions = await discoverSessions(dirs.length > 0 ? dirs : [getClaudeProjectDir(cwd)], cwd);
      byPath = new Map(sessions.map((s) => [s.path, s]));
      return sessions.map((s) => s.path);
    },
    async readSession(path) {
      const session = byPath.get(path);
      return session ? readRawSession(session) : null;
    },
  };
}

export async function local(): Promise<number> {
  const subcommand = process.argv[3];

  if (!subcommand || subcommand === '--help' || subcommand === '-h') {
    console.log(localCmd.usage);
    return subcommand ? 0 : 1;
  }

  const source = createRawSessionSource();
  const args = process.argv.slice(4);

  switch (subcommand) {
    case 'search':
      return (await import('./search')).searchCore(source, args);
    case 'read':
      return (await import('./read')).readCore(source, args);
    case 'index':
      return (await import('./index')).indexCore(source, args);
    default:
      printError(localCmd.unknownCommand(subcommand));
      console.log(localCmd.usage);
      return 1;
  }
}
