import { readFileSync } from 'node:fs';
import { basename, dirname } from 'node:path';
import { parseTranscript } from '@alignment-hive/session-data';
import { preferred, printer, sessionFiles } from '../lib/locators';
import { clipToFirstLine, localTime } from '../lib/local-rows';
import { localErrors, localNotes } from '../lib/messages';
import { firstHumanMessage, sessionFacts, sessionTitle } from '../lib/session-facts';
import { extractCwd, extractCwdFromFile } from '../lib/transcript-discovery';
import { LocalError, emit, inWindow, note, numberFlag, scopeOf, spanInWindow, timeWindow } from './local';
import type { SessionFacts } from '../lib/session-facts';
import type { TranscriptRef } from '../lib/locators';
import type { Args, LocalEnv, Scope } from './local';

/** Each session of a scope once: a session that ran in several dirs keeps the file with the conversation. */
async function sessionsIn(scope: Scope): Promise<Array<TranscriptRef>> {
  const files = (await sessionFiles(scope.dirs)).filter((ref) => scope.belongs(ref.path));
  return Promise.all([...Map.groupBy(files, (ref) => ref.session).values()].map(preferred));
}

/** Sessions by last activity, newest first; files with no entry time are left out. */
export async function sessionsByActivity(scope: Scope): Promise<Array<SessionFacts>> {
  return (await sessionsIn(scope))
    .map(sessionFacts)
    .filter((f) => f.start !== undefined || f.end !== undefined)
    .sort((a, b) => (b.end ?? 0) - (a.end ?? 0) || (b.start ?? 0) - (a.start ?? 0));
}

export async function run(env: LocalEnv, args: Args): Promise<number> {
  if (args.positional.length) throw new LocalError(localErrors.sessionsTakesNoSession(args.positional[0]), true);
  const limit = numberFlag(args, '-n', 20);
  const clip = clipToFirstLine(numberFlag(args, '--clip', 150));
  const scope = await scopeOf(env, args);
  const w = timeWindow(args);
  // A session is in the window when one of its entries is. Its first or last entry decides unless
  // it spans the whole window, when it is read to look for an entry inside.
  const active = (await sessionsByActivity(scope)).filter(
    (f) =>
      spanInWindow(f, w) &&
      (inWindow(f.start, w) ||
        inWindow(f.end, w) ||
        parseTranscript(readFileSync(f.ref.path, 'utf8')).entries.some((e) =>
          inWindow(e.time ? Date.parse(e.time) : undefined, w),
        )),
  );
  // Sessions with no human message and no reply are left out. A file with an assistant record near
  // either end has a reply; only the rest are parsed to check.
  const listed = active
    .map((f) => ({ f, human: f.hasAssistant ? undefined : firstHumanMessage(f.ref, f.size) }))
    .filter(({ human }) => !human?.empty);
  const print = await printer(env.root);
  // A file whose first cwd lies past the head read takes its project dir's cwd, from another file there.
  const dirCwd = new Map<string, string | null>();
  const projectOf = (path: string) => {
    const own = extractCwdFromFile(path);
    if (own) return own;
    const dir = dirname(path);
    if (!dirCwd.has(dir)) dirCwd.set(dir, extractCwd(dir));
    return dirCwd.get(dir) ?? basename(dir);
  };
  for (const { f, human } of listed.slice(0, limit)) {
    const row: Record<string, unknown> = { loc: print.transcript(f.ref) };
    if (scope.all) row.project = projectOf(f.ref.path);
    const start = localTime(f.start);
    const end = localTime(f.end);
    if (start) row.start = start;
    if (end) row.end = end;
    if (f.branch) row.branch = f.branch;
    const title = sessionTitle(f.ref);
    if (title) row.title = clip('title', title);
    const first = (human ?? firstHumanMessage(f.ref, f.size)).entry;
    if (first?.kind === 'user') row.first = clip('first', first.text);
    emit(row);
  }
  const windowed = w.since !== undefined || w.until !== undefined;
  note(localNotes.sessions(Math.min(limit, listed.length), listed.length, scope.all, windowed));
  return 0;
}
