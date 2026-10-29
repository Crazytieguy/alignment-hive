// What an hourly tick should do: the live checks when they are due, the newest aged CLIProxyAPI release when it has
// not been evaluated, and whether a run's findings are new enough to start a Claude session. Pure functions over the
// history, the candidate state and the release list, so the decisions are unit-tested.
import type { HistoryRow } from './report';
import type { CandidateVerdict } from './report';
import { pickCandidate } from './upstream';
import type { Release } from './upstream';

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

/** The live checks run at least this often, and on every Claude Code or router version change in between. */
export const LIVE_EVERY_HOURS = 24;
/** The live tier's context-overflow probes (requests of ~1.2M tokens) run this often. */
export const FULL_EVERY_DAYS = 7;
/**
 * A release must be this old before it is a candidate. Upstream fixed every regression found in 90 days of releases
 * within about three days (never by pulling the release), so by then the notes of the releases after it show any fix.
 */
export const MIN_AGE_DAYS = 4;
/** An inconclusive or skipped candidate is tried again after this long, up to MAX_TRIES runs that got to test it. */
export const RETRY_AFTER_HOURS = 6;
export const MAX_TRIES = 3;
/** The same finding starts at most one Claude session in this window. */
export const REPEAT_AFTER_HOURS = 24;

export type CandidateEntry = {
  verdict: CandidateVerdict;
  reason: string;
  at: string;
  tries: number;
  report: string;
  /** Candidate-only failures of the last try, so a repeat settles 'fail'. */
  regressions?: Array<string>;
  firstSkippedAt?: string;
  transientSkip?: boolean;
};
export type CandidateState = Record<string, CandidateEntry>;

export const settled = (e: CandidateEntry | undefined): boolean =>
  !!e && (e.verdict === 'pass' || e.verdict === 'fail' || e.tries >= MAX_TRIES);

/** Whether the live tier is due, and with the overflow probes. */
export function liveDue(
  history: Array<HistoryRow>,
  versions: { claudeCode?: string; router?: string },
  now: number,
): { due: boolean; full: boolean; why: string } {
  const lives = history.filter((h) => h.live);
  const last = lives.at(-1);
  const lastFull = lives.filter((h) => h.live!.full).at(-1);
  const full = !lastFull || now - Date.parse(lastFull.at) >= FULL_EVERY_DAYS * DAY;
  if (!last) return { due: true, full, why: 'no live run yet' };
  if (versions.claudeCode && versions.claudeCode !== last.versions.claudeCode) {
    return { due: true, full, why: `Claude Code ${last.versions.claudeCode ?? '?'} -> ${versions.claudeCode}` };
  }
  if (versions.router && versions.router !== last.versions.router) {
    return { due: true, full, why: `router ${last.versions.router ?? '?'} -> ${versions.router}` };
  }
  const age = now - Date.parse(last.at);
  if (age >= LIVE_EVERY_HOURS * HOUR) return { due: true, full, why: `last live run ${(age / HOUR).toFixed(0)}h ago` };
  return { due: false, full, why: `live checks ran ${(age / HOUR).toFixed(1)}h ago on the same versions` };
}

/**
 * The release to evaluate now, if any: the newest one above the pin that is at least MIN_AGE_DAYS old. Older releases
 * are never evaluated once a newer one has aged (a release still waiting for its retry is dropped then). A settled one is
 * not evaluated again; an unsettled one is retried once RETRY_AFTER_HOURS have passed.
 */
export function nextCandidate(
  releases: Array<Release>,
  state: CandidateState,
  pinned: string,
  now: number,
  minAgeDays = MIN_AGE_DAYS,
): { version?: string; why: string } {
  const aged = pickCandidate(releases, pinned, minAgeDays, now);
  if (!aged) return { why: `no release newer than ${pinned} is at least ${minAgeDays} days old` };
  const seen = state[aged.version];
  if (settled(seen)) return { why: `${aged.version} already evaluated: ${seen!.verdict} (${seen!.at.slice(0, 16)})` };
  if (seen && now - Date.parse(seen.at) < RETRY_AFTER_HOURS * HOUR) {
    return { why: `${aged.version} was ${seen.verdict} at ${seen.at.slice(0, 16)}; retrying after ${RETRY_AFTER_HOURS}h` };
  }
  return { version: aged.version, why: seen ? `retrying ${aged.version} (${seen.verdict})` : `${aged.version} is new` };
}

/** Same major and minor as the pin: upstream ships nearly every change as a patch release. */
export function isPatch(pinned: string, candidate: string): boolean {
  const [a, b] = [pinned, candidate].map((v) => v.split('.').slice(0, 2).join('.'));
  return a === b;
}

/** A finding for the hand-off session, with a key that stays the same while the finding does. */
export type Finding = { key: string; text: string };
/** When each finding key last started a session. */
export type SessionLog = Record<string, string>;

/** The findings no session has seen within REPEAT_AFTER_HOURS; an empty result means the run stays quiet. */
export function newFindings(findings: Array<Finding>, log: SessionLog, now: number): Array<Finding> {
  return findings.filter((f) => !log[f.key] || now - Date.parse(log[f.key]!) >= REPEAT_AFTER_HOURS * HOUR);
}
