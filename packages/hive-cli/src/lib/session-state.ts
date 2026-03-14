import { readFile, stat } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { findRawSessions } from './extraction';

export const SESSION_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h
export const CONSENT_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h

export interface UploadedEntry {
  sessionId: string;
  rawMtime: string;
  uploadedAt: string;
}

export interface DiscoveredSession {
  sessionId: string;
  path: string;
  mtime: Date;
}

/** Discover non-agent sessions from transcript directories, with parallel stat() calls. */
export async function discoverSessions(transcriptsDirs: Array<string>): Promise<Array<DiscoveredSession>> {
  const dirResults = await Promise.all(
    transcriptsDirs.map((dir) => findRawSessions(dir).catch(() => [])),
  );

  const statPromises: Array<Promise<DiscoveredSession | null>> = [];
  for (const rawSessions of dirResults) {
    for (const s of rawSessions) {
      if (s.agentId) continue;
      const sessionId = basename(s.path, '.jsonl');
      statPromises.push(
        stat(s.path)
          .then((fileStat) => ({ sessionId, path: s.path, mtime: fileStat.mtime }))
          .catch(() => null),
      );
    }
  }

  const results = await Promise.all(statPromises);
  return results.filter((r): r is DiscoveredSession => r !== null);
}

export async function loadUploadedSessions(stateDir: string): Promise<Map<string, UploadedEntry>> {
  const map = new Map<string, UploadedEntry>();
  try {
    const content = await readFile(join(stateDir, 'uploaded-sessions'), 'utf-8');
    for (const line of content.split('\n')) {
      if (!line.trim()) continue;
      try {
        const entry = JSON.parse(line) as UploadedEntry;
        map.set(entry.sessionId, entry);
      } catch {
        // skip malformed lines
      }
    }
  } catch {
    // file doesn't exist
  }
  return map;
}

export async function loadExcludedSessions(stateDir: string): Promise<Set<string>> {
  const set = new Set<string>();
  try {
    const content = await readFile(join(stateDir, 'excluded-sessions'), 'utf-8');
    for (const line of content.split('\n')) {
      const trimmed = line.trim();
      if (trimmed) set.add(trimmed);
    }
  } catch {
    // file doesn't exist
  }
  return set;
}

export type IneligibleReason = 'excluded' | 'already uploaded' | 'pending review' | 'consent review period';

export function checkSessionEligibility(
  session: DiscoveredSession,
  uploadedMap: Map<string, UploadedEntry>,
  excludedSet: Set<string>,
  consentMtime: number,
): { eligible: true } | { eligible: false; reason: IneligibleReason } {
  if (excludedSet.has(session.sessionId)) {
    return { eligible: false, reason: 'excluded' };
  }

  const uploaded = uploadedMap.get(session.sessionId);
  if (uploaded && uploaded.rawMtime === session.mtime.toISOString()) {
    return { eligible: false, reason: 'already uploaded' };
  }

  const now = Date.now();
  const mtimeMs = session.mtime.getTime();

  if (now < mtimeMs + SESSION_REVIEW_PERIOD_MS) {
    return { eligible: false, reason: 'pending review' };
  }

  if (now < consentMtime + CONSENT_REVIEW_PERIOD_MS) {
    return { eligible: false, reason: 'consent review period' };
  }

  return { eligible: true };
}

