import { readFileSync, statSync } from 'node:fs';
import { basename, dirname } from 'node:path';
import { isHumanMessage } from '@alignment-hive/session-data';
import { cutoffTime, transcriptAsOf } from '../lib/as-of';
import { preferred, printer, sessionFiles } from '../lib/locators';
import { clipToFirstLine, localTime } from '../lib/local-rows';
import { localErrors, localNotes } from '../lib/messages';
import { firstHumanMessage, sessionFacts, sessionPresence, sessionTitle, someEntry } from '../lib/session-facts';
import { extractCwd, extractCwdFromFile } from '../lib/transcript-discovery';
import { LocalError, emit, inWindow, note, numberFlag, scopeOf, spanInWindow, timeWindow } from './local';
import type { SessionFacts, SessionPresence } from '../lib/session-facts';
import type { TranscriptRef } from '../lib/locators';
import type { Transcript } from '@alignment-hive/session-data';
import type { Args, LocalEnv, Scope } from './local';

/** Each session of a scope once: a session that ran in several dirs keeps the file with the conversation. */
export async function sessionsIn(scope: Scope): Promise<Array<TranscriptRef>> {
  const files = (await sessionFiles(scope.dirs)).filter((ref) => scope.belongs(ref.path));
  return Promise.all([...Map.groupBy(files, (ref) => ref.session).values()].map(preferred));
}

/** A session's facts; `asOf` holds its entries before HIVE_LOCAL_CUTOFF when it ran past it. */
type Facts = SessionFacts & { asOf?: Transcript };

/** Facts as of HIVE_LOCAL_CUTOFF: a session still running then reads from its records before it, untitled. */
function factsAsOf(f: SessionFacts): Facts {
  const cutoff = cutoffTime();
  if (cutoff === undefined || (f.end !== undefined && f.end < cutoff)) return f;
  const asOf = transcriptAsOf(readFileSync(f.ref.path)).transcript;
  const end = asOf.entries.findLast((e) => e.time !== undefined)?.time;
  return { ...f, end: end ? Date.parse(end) : undefined, branch: asOf.branch, asOf };
}

/** Newest first: by last entry time, then first. */
const byActivity = (a: SessionFacts, b: SessionFacts): number =>
  (b.end ?? 0) - (a.end ?? 0) || (b.start ?? 0) - (a.start ?? 0);

/**
 * Sessions by last activity, newest first; files with no entry time are left out. Each `start` is
 * read on use (see sessionFacts): its one caller, grep, needs few of them.
 */
export function sessionsByActivity(refs: Array<TranscriptRef>): Array<Facts> {
  return refs
    .map((ref) => factsAsOf(sessionFacts(ref, true)))
    .filter((f) => f.end !== undefined || f.start !== undefined)
    .sort(byActivity);
}

/**
 * Sessions with no human message and no reply are left out. A file with an assistant record near
 * either end has a reply; only the rest are parsed to check.
 */
const humanOf = (f: SessionPresence) => (f.hasAssistant ? undefined : firstHumanMessage(f.ref, f.size));

/** `hive local sessions`: sessions by last activity, newest first, each read from its file's ends (session-facts.ts). */
export async function run(env: LocalEnv, args: Args): Promise<number> {
  if (args.positional.length) throw new LocalError(localErrors.sessionsTakesNoSession(args.positional[0]), true);
  const limit = numberFlag(args, '-n', 20);
  const clip = clipToFirstLine(numberFlag(args, '--clip', 150));
  const scope = await scopeOf(env, args);
  const w = timeWindow(args);
  const windowed = w.since !== undefined || w.until !== undefined;
  // A file's mtime bounds its last entry time (a backdated copy, restore or import can drop or
  // misorder a session), so files are read newest-written first: one last written before --since
  // has no entry in the window, and once `limit` listed sessions ended after the next file was last
  // written, no later file is shown. Without a window those are only counted.
  const files = (await sessionsIn(scope))
    .map((ref, order) => ({ ref, order, written: statSync(ref.path).mtimeMs }))
    .filter((file) => w.since === undefined || file.written >= w.since)
    .sort((x, y) => y.written - x.written);
  const listed: Array<{ f: Facts; order: number; human?: ReturnType<typeof humanOf> }> = [];
  let counted = 0;
  /** The `limit` latest ends among the listed sessions read so far, latest first. */
  const latest: Array<number> = [];
  for (const { ref, order, written } of files) {
    if (!windowed && (limit === 0 || (latest.length >= limit && latest[limit - 1] > written))) {
      const p = sessionPresence(ref);
      if (p.timed && !humanOf(p)?.empty) counted++;
      continue;
    }
    const f = factsAsOf(sessionFacts(ref));
    if (f.start === undefined && f.end === undefined) continue;
    // A session is in the window when one of its entries is. Its first or last entry decides unless
    // it spans the whole window, when it is read to look for an entry inside.
    const active =
      spanInWindow(f, w) &&
      (inWindow(f.start, w) ||
        inWindow(f.end, w) ||
        (f.asOf
          ? f.asOf.entries.some((e) => inWindow(e.time ? Date.parse(e.time) : undefined, w))
          : someEntry(f.ref, (e) => inWindow(e.time ? Date.parse(e.time) : undefined, w))));
    if (!active) continue;
    const human = humanOf(f);
    if (human?.empty) continue;
    listed.push({ f, order, human });
    const end = f.end ?? 0;
    if (latest.length < limit || end > latest[limit - 1]) {
      let at = 0;
      for (let hi = latest.length; at < hi; ) {
        const mid = (at + hi) >> 1;
        if (latest[mid] >= end) at = mid + 1;
        else hi = mid;
      }
      latest.splice(at, 0, end);
      latest.length = Math.min(latest.length, limit);
    }
  }
  // In the order a sort of every session gives.
  listed.sort((x, y) => byActivity(x.f, y.f) || x.order - y.order);
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
    const title = f.asOf ? undefined : sessionTitle(f.ref);
    if (title) row.title = clip('title', title);
    const first = f.asOf ? f.asOf.entries.find(isHumanMessage) : (human ?? firstHumanMessage(f.ref, f.size)).entry;
    if (first?.kind === 'user') row.first = clip('first', first.text);
    emit(row);
  }
  const total = listed.length + counted;
  note(localNotes.sessions(Math.min(limit, total), total, scope.all, windowed));
  return 0;
}
