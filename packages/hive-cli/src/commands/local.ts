import { existsSync, statSync } from 'node:fs';
import { realpath } from 'node:fs/promises';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { ArgsError, parseFlags } from '../lib/args';
import {
  claudeProjectsRoot,
  getClaudeProjectDir,
  getStateDir,
  legacyStateDir,
  listWorktreePaths,
  loadTranscriptsDirs,
} from '../lib/config';
import { LocatorError, parseLocator, parseRange, projectDirs } from '../lib/locators';
import { localErrors, localHelp, localNotes, localUsage } from '../lib/messages';
import { makeProjectSessionFilter } from '../lib/session-state';
import { writeLine } from '../lib/stdout';
import { parseTimeSpec } from '../lib/time-filter';
import { extractCwdFromFile, projectScanData, resolveTranscriptDirs } from '../lib/transcript-discovery';
import type { FlagSpec, ParsedArgs } from '../lib/args';
import type { Locator } from '../lib/locators';

/** A project's transcripts: its dirs and those of its worktrees. */
export interface ProjectTranscripts {
  dirs: Array<string>;
  /** False for a session file of another project whose dir name collides with this one's. */
  belongs: (path: string) => boolean;
  /** The project's main worktree, when the path is inside a git repository. */
  root?: string;
}

/** Where `hive local` finds transcripts; the tests substitute a directory of fixtures. */
export interface LocalEnv {
  /** Every project's transcript dir lives here (~/.claude/projects). */
  root: string;
  project: (path: string) => Promise<ProjectTranscripts>;
}

export function localEnv(): LocalEnv {
  const root = claudeProjectsRoot();
  return {
    root,
    project: async (path) => {
      const cwd = await realpath(path).catch(() => path);
      const worktrees = listWorktreePaths(cwd);
      // A folder outside git is exactly that folder: the sessions started in it, and no others.
      if (!worktrees.length)
        return { dirs: [getClaudeProjectDir(cwd)], belongs: (file) => (extractCwdFromFile(file) ?? cwd) === cwd };
      const main = worktrees[0];
      // The registry, and the one the plugin kept before its rename, can name dirs under another
      // config root (CLAUDE_CONFIG_DIR); listing keeps to the root locators resolve in, so every
      // listed session resolves.
      const registered = await Promise.all([getStateDir(main), legacyStateDir(main)].map(loadTranscriptsDirs));
      const registry = [...new Set(registered.flat())].filter((dir) => dirname(resolve(dir)) === root);
      const scan = projectScanData(root, main, worktrees);
      const dirs = resolveTranscriptDirs(main, scan, registry);
      return {
        dirs: dirs.length ? dirs : [getClaudeProjectDir(cwd)],
        belongs: makeProjectSessionFilter(main),
        root: main,
      };
    },
  };
}

/** A user error: exit 2 with the message, plus the usage lines when the call itself was malformed. */
export class LocalError extends Error {
  constructor(
    message: string,
    public usage = false,
  ) {
    super(message);
  }
}

export const emit = (row: unknown): void => writeLine(JSON.stringify(row));
/** One JSON object per stderr line, so that `2>&1 | jq` still parses; never colored (console.error is, under FORCE_COLOR). */
const stderrRow = (row: Record<string, string>): void => void process.stderr.write(`${JSON.stringify(row)}\n`);
export const note = (text: string): void => stderrRow({ note: text });
export const warn = (text: string): void => stderrRow({ warning: text });

const SCOPE = ['--project', '--since', '--until', '--clip'];
const SPECS: Record<string, FlagSpec> = {
  sessions: { bool: ['--all-projects'], value: ['-n', ...SCOPE] },
  outline: { bool: ['--all-entries'], value: ['--clip'] },
  show: { bool: ['--all-entries'], value: ['--clip'] },
  grep: {
    bool: ['-i', '-F', '-E', '-c', '-l', '--agents', '--all-entries', '--all-projects'],
    value: ['-m', ...SCOPE],
  },
};

export type Args = ParsedArgs;

export function parseArgs(verb: string, args: Array<string>): Args {
  return parseFlags(SPECS[verb], args, { ...localErrors, unknownFlag: (flag) => localErrors.unknownFlag(flag, verb) });
}

export function numberFlag(args: Args, name: string, fallback: number): number {
  const v = args.flags.get(name);
  if (v === undefined) return fallback;
  if (typeof v !== 'string' || !/^\d+$/.test(v)) throw new LocalError(localErrors.badNumber(name, String(v)));
  return Number(v);
}

/** Entry times to keep: from `since` (inclusive) to `until` (exclusive), in ms. */
export interface Window {
  since?: number;
  until?: number;
}

/** --since and --until: 2h, 7d, a local date (midnight; --until includes the whole day), or a date and time. */
export function timeWindow(args: Args): Window {
  const bound = (flag: '--since' | '--until'): number | undefined => {
    const v = args.flags.get(flag);
    if (typeof v !== 'string') return undefined;
    const t = parseTimeSpec(v);
    if (!t) throw new LocalError(localErrors.badTime(flag, v));
    const wholeDay = flag === '--until' && /^\d{4}-\d{2}-\d{2}$/.test(v);
    return wholeDay ? new Date(t.getFullYear(), t.getMonth(), t.getDate() + 1).getTime() : t.getTime();
  };
  return { since: bound('--since'), until: bound('--until') };
}

export const inWindow = (t: number | undefined, w: Window): boolean =>
  (w.since === undefined && w.until === undefined) ||
  (t !== undefined && (w.since === undefined || t >= w.since) && (w.until === undefined || t < w.until));

/** A session whose entries span `start` to `end` has entries in the window. */
export const spanInWindow = (span: { start?: number; end?: number }, w: Window): boolean =>
  (w.since === undefined || (span.end ?? span.start ?? 0) >= w.since) &&
  (w.until === undefined || (span.start ?? span.end ?? 0) < w.until);

export interface Scope extends ProjectTranscripts {
  label: string;
  all: boolean;
}

/** The transcript dirs `sessions` and `grep` search: this project and its worktrees, --project DIR, or every project. */
export async function scopeOf(env: LocalEnv, args: Args): Promise<Scope> {
  const all = args.flags.get('--all-projects') === true;
  const project = args.flags.get('--project');
  if (all && project !== undefined) throw new LocalError(localErrors.scopeConflict, true);
  if (all) return { dirs: await projectDirs(env.root), belongs: () => true, label: localNotes.allProjects, all };
  const path =
    typeof project === 'string'
      ? project.startsWith('~/')
        ? join(homedir(), project.slice(2))
        : resolve(project)
      : process.cwd();
  if (statSync(path, { throwIfNoEntry: false })?.isDirectory() === false)
    throw new LocalError(localErrors.notADirectory(path));
  const transcripts = await env.project(path);
  const dirs = transcripts.dirs.filter((dir) => existsSync(dir));
  if (!dirs.length) throw new LocalError(localErrors.noProject(path, project === undefined));
  const label = transcripts.root ? localNotes.projectLabel(transcripts.root) : localNotes.folderLabel(path);
  return { dirs, belongs: transcripts.belongs, label, all };
}

/**
 * show's arguments, in the order given: SESSION[:RANGE] first, then SESSION:RANGE of any transcript
 * or bare RANGEs of the transcript named last. A SESSION without a range stands for the whole
 * transcript only when no bare RANGE follows.
 */
export function sessionArguments(args: Args, verb: string): Array<Locator> {
  if (!args.positional.length) throw new LocalError(localErrors.needsSession(verb), true);
  const [first, ...rest] = args.positional;
  const head = parseLocator(first);
  const locators = [head];
  let named = head;
  for (const text of rest) {
    if (text.includes(':')) {
      named = parseLocator(text);
      locators.push(named);
      continue;
    }
    let range;
    try {
      range = parseRange(text);
    } catch (error) {
      if (error instanceof LocatorError) throw new LocalError(localErrors.rangeAfterSession(error.message));
      throw error;
    }
    // SESSION RANGE: the range replaces the whole transcript.
    if (named === head && !head.range && locators[0] === head) locators.shift();
    locators.push({ ...named, range });
  }
  return locators;
}

const VERBS: Record<string, () => Promise<{ run: (env: LocalEnv, args: Args) => Promise<number> }>> = {
  sessions: () => import('./sessions'),
  outline: () => import('./show').then((m) => ({ run: m.outline })),
  show: () => import('./show'),
  grep: () => import('./grep'),
};

export async function localCore(env: LocalEnv, argv: Array<string>): Promise<number> {
  const [verb, ...rest] = argv;
  if (!argv.length || ['--help', '-h', 'help'].includes(verb)) {
    writeLine(localHelp);
    return 0;
  }
  try {
    const load = VERBS[verb] as (typeof VERBS)[string] | undefined;
    if (!load) throw new LocalError(localErrors.unknownCommand(verb), true);
    const args = parseArgs(verb, rest);
    if (args.help) {
      writeLine(localHelp);
      return 0;
    }
    return await (await load()).run(env, args);
  } catch (error) {
    if (!(error instanceof LocalError || error instanceof LocatorError || error instanceof ArgsError)) throw error;
    const usage = error instanceof ArgsError || (error instanceof LocalError && error.usage);
    stderrRow(usage ? { error: error.message, usage: localErrors.usage(localUsage) } : { error: error.message });
    return 2;
  }
}

/** Any other failure (a filesystem error, a bug) is an error row too, so stderr stays JSON. */
export async function local(): Promise<number> {
  try {
    return await localCore(localEnv(), process.argv.slice(3));
  } catch (error) {
    stderrRow({ error: error instanceof Error ? error.message : String(error) });
    return 2;
  }
}
