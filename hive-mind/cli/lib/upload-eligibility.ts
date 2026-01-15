import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { loadAuthData, saveAuthData } from "./auth";
import { getHiveMindSessionsDir, readExtractedMeta } from "./extraction";
import type { HiveMindMeta } from "./schemas";

const SESSION_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h for session age
const AUTH_REVIEW_PERIOD_MS = 4 * 60 * 60 * 1000; // 4h for auth age

export async function getAuthIssuedAt(): Promise<number | null> {
  const authData = await loadAuthData();
  if (!authData) return null;

  // Migration: if authenticated_at is missing, set it to now
  if (authData.authenticated_at === undefined) {
    const now = Date.now();
    await saveAuthData({ ...authData, authenticated_at: now });
    return now;
  }

  return authData.authenticated_at;
}

export interface SessionEligibility {
  sessionId: string;
  meta: HiveMindMeta;
  eligible: boolean;
  excluded: boolean;
  eligibleAt: number | null;
  reason: string;
}

export function checkSessionEligibility(
  meta: HiveMindMeta,
  authIssuedAt: number | null,
): SessionEligibility {
  const sessionId = meta.sessionId;

  if (meta.excluded) {
    return {
      sessionId,
      meta,
      eligible: false,
      excluded: true,
      eligibleAt: null,
      reason: "Excluded by user",
    };
  }

  const now = Date.now();
  const rawMtimeMs = new Date(meta.rawMtime).getTime();

  // Decoupled review periods: 24h for session age, 4h for auth age
  const sessionEligibleAt = rawMtimeMs + SESSION_REVIEW_PERIOD_MS;
  const authEligibleAt = authIssuedAt ? authIssuedAt + AUTH_REVIEW_PERIOD_MS : 0;
  const eligibleAt = Math.max(sessionEligibleAt, authEligibleAt);

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
    reason: "Ready for upload",
  };
}

export async function getAllSessionsEligibility(
  cwd: string,
): Promise<Array<SessionEligibility>> {
  const sessionsDir = getHiveMindSessionsDir(cwd);
  const authIssuedAt = await getAuthIssuedAt();

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    return [];
  }

  const jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));

  const results = await Promise.all(
    jsonlFiles.map(async (file) => {
      const meta = await readExtractedMeta(join(sessionsDir, file));
      if (!meta || meta.agentId) return null;
      return checkSessionEligibility(meta, authIssuedAt);
    })
  );

  return results.filter((r): r is SessionEligibility => r !== null);
}

export async function getEligibleSessions(
  cwd: string,
): Promise<Array<SessionEligibility>> {
  const all = await getAllSessionsEligibility(cwd);
  return all.filter((s) => s.eligible);
}

export async function getPendingSessions(
  cwd: string,
): Promise<Array<SessionEligibility>> {
  const all = await getAllSessionsEligibility(cwd);
  return all.filter((s) => !s.eligible && !s.excluded);
}
