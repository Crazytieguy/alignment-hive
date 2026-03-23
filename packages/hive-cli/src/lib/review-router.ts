import { initTRPC } from '@trpc/server';
import { z } from 'zod';
import {
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  loadTranscriptsDirs,
} from './config';
import { formatDisplayStatus, getDisplayStatus, getProjectConsentMtime, getSessionSummary } from './session-display';
import {
  checkSessionEligibility,
  findAgentsForParent,
  isSessionUploaded,
  loadSessionState,
  recordExcludedSession,
} from './session-state';
import { clearSnooze, getSnoozeUntil, parseDuration, setSnooze } from './snooze';
import { readAndSanitizeSession, uploadParentWithAgents } from './upload-session';

const t = initTRPC.create();

export function createReviewRouter(stateDir: string, cwd: string) {
  return t.router({
    sessions: t.router({
      list: t.procedure.query(async () => {
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        const { parentSessions, agentsByParent, uploadedMap, excludedSet, migrationTimestamp } = await loadSessionState(stateDir, transcriptsDirs);
        const consentMtime = await getProjectConsentMtime(cwd);
        const snoozeUntil = await getSnoozeUntil(stateDir);

        const sessions = await Promise.all(
          parentSessions
            .sort((a, b) => b.mtime.getTime() - a.mtime.getTime())
            .map(async (session) => {
              const result = checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime ?? Date.now(), migrationTimestamp);
              // Use findAgentsForParent for accurate count (includes worktree agents)
              const agents = await findAgentsForParent(session, agentsByParent, transcriptsDirs);
              const agentCount = agents.length;
              const uploadedEntry = uploadedMap.get(session.sessionId);
              const status = getDisplayStatus(result, session, consentMtime, snoozeUntil, { uploadedEntry, agentCount });
              const summary = await getSessionSummary(session.path);

              return {
                sessionId: session.sessionId,
                date: session.mtime.toISOString(),
                status,
                statusLabel: formatDisplayStatus(status),
                summary: summary.slice(0, 120),
                agentCount,
              };
            }),
        );

        return { sessions, snoozeUntil };
      }),

      content: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .query(async ({ input }) => {
          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          const { sessionById, agentsByParent } = await loadSessionState(stateDir, transcriptsDirs);

          const session = sessionById.get(input.sessionId);
          if (!session) {
            throw new Error(`No session matching "${input.sessionId}"`);
          }

          // Use the same read+sanitize pipeline as the upload flow
          const { sanitizedEntries } = await readAndSanitizeSession(session.path);

          // Find ALL agents (including worktree agents) — same code path as upload
          const allAgents = session.agentId
            ? [] // Agent sessions don't have their own agents
            : await findAgentsForParent(session, agentsByParent, transcriptsDirs);

          // Read and sanitize each agent's content
          const agents = await Promise.all(
            allAgents.map(async (agent) => {
              const agentRead = await readAndSanitizeSession(agent.path);
              return {
                sessionId: agent.sessionId,
                agentId: agent.agentId,
                entries: agentRead.sanitizedEntries,
                messageCount: agentRead.sanitizedEntries.length,
              };
            }),
          );

          return {
            meta: {
              _type: 'session-meta' as const,
              version: '0.1' as const,
              sessionId: session.sessionId,
              rawMtime: session.mtime.toISOString(),
              messageCount: sanitizedEntries.length,
              ...(session.agentId && { agentId: session.agentId }),
              ...(session.parentSessionId && { parentSessionId: session.parentSessionId }),
            },
            entries: sanitizedEntries,
            agents,
          };
        }),

      exclude: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .mutation(async ({ input }) => {
          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          const { sessionById, excludedSet, uploadedMap } = await loadSessionState(stateDir, transcriptsDirs);

          const session = sessionById.get(input.sessionId);
          if (!session) {
            throw new Error(`No session matching "${input.sessionId}"`);
          }

          if (session.agentId) {
            throw new Error('Agent sessions cannot be excluded individually. Exclude the parent session instead.');
          }

          if (excludedSet.has(session.sessionId)) {
            return { alreadyExcluded: true };
          }

          if (isSessionUploaded(session, uploadedMap)) {
            throw new Error('Cannot exclude an already uploaded session');
          }

          await recordExcludedSession(stateDir, session.sessionId);
          return { alreadyExcluded: false };
        }),

      upload: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .mutation(async ({ input }) => {
          const checkoutId = await getOrCreateCheckoutId(stateDir);
          const ids = getProjectIdentifiers(cwd);

          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          const { sessionById, agentsByParent, uploadedMap } = await loadSessionState(stateDir, transcriptsDirs);

          const session = sessionById.get(input.sessionId);
          if (!session) {
            throw new Error(`No session matching "${input.sessionId}"`);
          }

          if (session.agentId) {
            throw new Error('Agent sessions cannot be uploaded individually. Upload the parent session instead.');
          }

          const agents = await findAgentsForParent(session, agentsByParent, transcriptsDirs);
          return uploadParentWithAgents({ parent: session, agents, checkoutId, ids, stateDir, uploadedMap });
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
        const { parentSessions, uploadedMap, excludedSet, migrationTimestamp } = await loadSessionState(stateDir, transcriptsDirs);

        let pending = 0;
        let ready = 0;
        let uploaded = 0;
        let excluded = 0;
        for (const session of parentSessions) {
          const result = checkSessionEligibility(session, uploadedMap, excludedSet, Date.now(), migrationTimestamp);
          if (result.eligible) ready++;
          else if (result.reason === 'excluded') excluded++;
          else if (result.reason === 'already uploaded') uploaded++;
          else pending++;
        }

        return { snoozeUntil, counts: { pending, ready, uploaded, excluded, total: parentSessions.length } };
      }),
    }),
  });
}

export type AppRouter = ReturnType<typeof createReviewRouter>;
