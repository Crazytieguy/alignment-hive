import { readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { loadAuthData } from './auth';
import { getHiveMindSessionsDir, readExtractedMeta } from './extraction';
import type { HiveMindMeta } from './schemas';

const REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000;

function decodeJwtPayload(token: string): Record<string, unknown> | null {
  try {
    const parts = token.split('.');
    if (parts.length !== 3) return null;

    let payload = parts[1];
    const padding = 4 - (payload.length % 4);
    if (padding < 4) {
      payload += '='.repeat(padding);
    }

    return JSON.parse(atob(payload));
  } catch {
    return null;
  }
}

export async function getAuthIssuedAt(): Promise<number | null> {
  const authData = await loadAuthData();
  if (!authData?.access_token) return null;

  const payload = decodeJwtPayload(authData.access_token);
  if (!payload || typeof payload.iat !== 'number') return null;

  return payload.iat * 1000;
}

export interface SessionEligibility {
  sessionId: string;
  meta: HiveMindMeta;
  eligible: boolean;
  excluded: boolean;
  eligibleAt: number | null; // When it becomes eligible (if not yet)
  reason: string;
}

export function checkSessionEligibility(
  meta: HiveMindMeta,
  authIssuedAt: number | null
): SessionEligibility {
  const sessionId = meta.sessionId;

  if (meta.excluded) {
    return {
      sessionId,
      meta,
      eligible: false,
      excluded: true,
      eligibleAt: null,
      reason: 'Excluded by user',
    };
  }

  const now = Date.now();
  const rawMtimeMs = new Date(meta.rawMtime).getTime();
  const eligibilityBase = authIssuedAt
    ? Math.max(rawMtimeMs, authIssuedAt)
    : rawMtimeMs;

  const eligibleAt = eligibilityBase + REVIEW_PERIOD_MS;

  if (now < eligibleAt) {
    const remainingMs = eligibleAt - now;
    const remainingHours = Math.ceil(remainingMs / (60 * 60 * 1000));
    return {
      sessionId,
      meta,
      eligible: false,
      excluded: false,
      eligibleAt,
      reason: `Eligible in ${remainingHours}h`,
    };
  }

  return {
    sessionId,
    meta,
    eligible: true,
    excluded: false,
    eligibleAt,
    reason: 'Ready for upload',
  };
}

export async function getAllSessionsEligibility(
  cwd: string
): Promise<Array<SessionEligibility>> {
  const sessionsDir = getHiveMindSessionsDir(cwd);
  const authIssuedAt = await getAuthIssuedAt();

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    return [];
  }

  const results: Array<SessionEligibility> = [];

  for (const file of files) {
    if (!file.endsWith('.jsonl')) continue;

    const meta = await readExtractedMeta(join(sessionsDir, file));
    if (!meta || meta.agentId) continue;

    const eligibility = checkSessionEligibility(meta, authIssuedAt);
    results.push(eligibility);
  }

  return results;
}

export async function getEligibleSessions(
  cwd: string
): Promise<Array<SessionEligibility>> {
  const all = await getAllSessionsEligibility(cwd);
  return all.filter((s) => s.eligible);
}

export async function getPendingSessions(
  cwd: string
): Promise<Array<SessionEligibility>> {
  const all = await getAllSessionsEligibility(cwd);
  return all.filter((s) => !s.eligible && !s.excluded);
}
