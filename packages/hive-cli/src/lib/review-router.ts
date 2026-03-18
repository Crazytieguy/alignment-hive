import { readFile } from 'node:fs/promises';
import { initTRPC } from '@trpc/server';
import { z } from 'zod';
import {
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  loadTranscriptsDirs,
} from './config';
import { parseJsonl, transformEntry } from './extraction';
import { sanitizeDeep } from './sanitize';
import { getDisplayStatus, getProjectConsentMtime, getSessionSummary } from './session-display';
import { lookupRawSession } from './session-lookup';
import {
  checkSessionEligibility,
  isSessionUploaded,
  loadSessionState,
  recordExcludedSession,
  recordUploadedSession,
} from './session-state';
import { clearSnooze, getSnoozeUntil, parseDuration, setSnooze } from './snooze';
import { uploadSingleSession } from './upload-session';
import type { KnownEntry } from '@alignment-hive/session-data';

const t = initTRPC.create();

export function createReviewRouter(stateDir: string, cwd: string) {
  return t.router({
    sessions: t.router({
      list: t.procedure.query(async () => {
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        const { sessions: allSessions, uploadedMap, excludedSet } = await loadSessionState(stateDir, transcriptsDirs);
        const consentMtime = await getProjectConsentMtime(cwd);
        const snoozeUntil = await getSnoozeUntil(stateDir);

        const sessions = await Promise.all(
          allSessions
            .sort((a, b) => b.mtime.getTime() - a.mtime.getTime())
            .map(async (session) => {
              const result = checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime ?? Date.now());
              const status = getDisplayStatus(result, session, consentMtime, snoozeUntil);
              const summary = await getSessionSummary(session.path);

              return {
                sessionId: session.sessionId,
                date: session.mtime.toISOString(),
                status,
                summary: summary.slice(0, 120),
              };
            }),
        );

        return { sessions, snoozeUntil };
      }),

      content: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .query(async ({ input }) => {
          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          const { sessions: allSessions } = await loadSessionState(stateDir, transcriptsDirs);
          const result = lookupRawSession(allSessions, input.sessionId);
          if (!result.found) {
            throw new Error(result.error);
          }

          const rawContent = await readFile(result.session.path, 'utf-8');
          const entries: Array<KnownEntry> = [];
          for (const rawEntry of parseJsonl(rawContent)) {
            const { entry } = transformEntry(rawEntry);
            if (entry) entries.push(entry as KnownEntry);
          }

          const sanitizedEntries = entries.map((e) => sanitizeDeep(e));

          return {
            meta: {
              _type: 'session-meta' as const,
              version: '0.1',
              sessionId: result.session.sessionId,
              rawMtime: result.session.mtime.toISOString(),
              messageCount: sanitizedEntries.length,
            },
            entries: sanitizedEntries,
          };
        }),

      exclude: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .mutation(async ({ input }) => {
          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          const { sessions: allSessions, excludedSet, uploadedMap } = await loadSessionState(stateDir, transcriptsDirs);
          const result = lookupRawSession(allSessions, input.sessionId);
          if (!result.found) {
            throw new Error(result.error);
          }

          if (excludedSet.has(result.session.sessionId)) {
            return { alreadyExcluded: true };
          }

          if (isSessionUploaded(result.session, uploadedMap)) {
            throw new Error('Cannot exclude an already uploaded session');
          }

          await recordExcludedSession(stateDir, result.session.sessionId);
          return { alreadyExcluded: false };
        }),

      upload: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .mutation(async ({ input }) => {
          const checkoutId = await getOrCreateCheckoutId(stateDir);
          const ids = getProjectIdentifiers(cwd);

          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          const { sessions: allSessions } = await loadSessionState(stateDir, transcriptsDirs);
          const result = lookupRawSession(allSessions, input.sessionId);
          if (!result.found) {
            throw new Error(result.error);
          }

          const session = result.session;
          const rawMtime = session.mtime.toISOString();
          const uploadResult = await uploadSingleSession(session.path, session.sessionId, checkoutId, rawMtime, ids);

          if (uploadResult.success) {
            await recordUploadedSession(stateDir, session.sessionId, rawMtime);
          }

          return uploadResult;
        }),
    }),

    upload: t.router({
      snooze: t.procedure
        .input(z.object({ duration: z.string() }))
        .mutation(async ({ input }) => {
          const durationMs = parseDuration(input.duration);
          if (!durationMs) {
            throw new Error(`Invalid duration: ${input.duration}`);
          }
          const until = await setSnooze(stateDir, durationMs);
          return { until };
        }),

      clearSnooze: t.procedure.mutation(async () => {
        await clearSnooze(stateDir);
        return { cleared: true };
      }),

      status: t.procedure.query(async () => {
        const snoozeUntil = await getSnoozeUntil(stateDir);
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        const { sessions: allSessions, uploadedMap, excludedSet } = await loadSessionState(stateDir, transcriptsDirs);

        let pending = 0;
        let ready = 0;
        let uploaded = 0;
        let excluded = 0;
        for (const session of allSessions) {
          const result = checkSessionEligibility(session, uploadedMap, excludedSet, Date.now());
          if (result.eligible) ready++;
          else if (result.reason === 'excluded') excluded++;
          else if (result.reason === 'already uploaded') uploaded++;
          else pending++;
        }

        return { snoozeUntil, counts: { pending, ready, uploaded, excluded, total: allSessions.length } };
      }),
    }),
  });
}

export type AppRouter = ReturnType<typeof createReviewRouter>;
