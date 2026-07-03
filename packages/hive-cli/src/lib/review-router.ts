import { initTRPC } from '@trpc/server';
import { z } from 'zod';
import {
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  loadTranscriptsDirs,
} from './config';
import { resolveProjectConsent } from './convex';
import { hive } from './messages';
import {
  computeSessionStatus,
  excludeSessionChecked,
  findAgentsForParent,
  formatSessionStatus,
  hasIncompleteUpload,
  isEligibleForAutoUpload,
  loadSessionState,
} from './session-state';
import { clearSnooze, getSnoozeUntil, parseDuration, setSnooze } from './snooze';
import { discoverWorkflowRuns, isInConsentWindows, loadConsentWindows, loadSessionStateWithAgentMigration, readAndSanitizeSession, readSessionSummary, uploadParentWithAgents } from './upload-session';

const t = initTRPC.create();

export function createReviewRouter(stateDir: string, cwd: string) {
  return t.router({
    sessions: t.router({
      list: t.procedure.query(async () => {
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        const { parentSessions, uploadedMap, excludedSet, startedMap, migrationTimestamp } =
          await loadSessionStateWithAgentMigration(stateDir, transcriptsDirs);
        const consentResult = await resolveProjectConsent(cwd);
        if ('error' in consentResult) {
          throw new Error(`Consent not available: ${consentResult.error}`);
        }
        const { consentMtime } = consentResult;
        const snoozeUntil = await getSnoozeUntil(stateDir);

        const sorted = parentSessions.sort((a, b) => b.mtime.getTime() - a.mtime.getTime());

        // Compute statuses (no file reads), then read summaries in batches
        const statusCtx = { uploadedMap, excludedSet, consentMtime, snoozeUntil, migrationTimestamp };
        const sessionMetas = sorted.map((session) => {
          const status = computeSessionStatus(session, statusCtx);
          return { session, status };
        });

        const SUMMARY_BATCH = 10;
        const sessions = [];
        for (let i = 0; i < sessionMetas.length; i += SUMMARY_BATCH) {
          const batch = sessionMetas.slice(i, i + SUMMARY_BATCH);
          const summaries = await Promise.all(
            batch.map(async ({ session }) => {
              try { return await readSessionSummary(session.path); }
              catch { return ''; }
            }),
          );
          for (let j = 0; j < batch.length; j++) {
            const { session, status } = batch[j];
            const partialUpload = hasIncompleteUpload(session.sessionId, uploadedMap, startedMap);
            sessions.push({
              sessionId: session.sessionId,
              date: session.mtime.toISOString(),
              status,
              partialUpload,
              statusLabel: formatSessionStatus(status, partialUpload),
              summary: summaries[j].slice(0, 120),
            });
          }
        }

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
          const sessionRead = await readAndSanitizeSession(session.path);

          // Find ALL agents (including worktree agents) — same code path as upload
          const allAgents = session.agentId
            ? [] // Agent sessions don't have their own agents
            : await findAgentsForParent(session, agentsByParent, transcriptsDirs, sessionRead.cwds);

          // Read and sanitize each agent's content in batches
          const AGENT_READ_BATCH = 10;
          const agents = [];
          for (let i = 0; i < allAgents.length; i += AGENT_READ_BATCH) {
            const batch = allAgents.slice(i, i + AGENT_READ_BATCH);
            const batchResults = await Promise.all(
              batch.map(async (agent) => {
                const agentRead = await readAndSanitizeSession(agent.path);
                return {
                  sessionId: agent.sessionId,
                  agentId: agent.agentId,
                  ...(agent.agentType && { agentType: agent.agentType }),
                  ...(agent.workflowRunId && { workflowRunId: agent.workflowRunId }),
                  entries: agentRead.sanitizedEntries,
                  messageCount: agentRead.sanitizedEntries.length,
                };
              }),
            );
            agents.push(...batchResults);
          }

          // Workflow run metadata (indexed scalars) for the parent — same sanitize pipeline as upload.
          // Best-effort: never fail the whole content query if run discovery hiccups.
          const workflowRuns = session.agentId
            ? []
            : await discoverWorkflowRuns(session, sessionRead.cwds)
                .then((rs) => rs.map((r) => r.row))
                .catch(() => []);

          return {
            meta: {
              _type: 'session-meta' as const,
              version: '0.1' as const,
              sessionId: session.sessionId,
              rawMtime: session.mtime.toISOString(),
              messageCount: sessionRead.sanitizedEntries.length,
              ...(session.agentId && { agentId: session.agentId }),
              ...(session.parentSessionId && { parentSessionId: session.parentSessionId }),
            },
            entries: sessionRead.sanitizedEntries,
            agents,
            workflowRuns,
          };
        }),

      exclude: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .mutation(async ({ input }) => {
          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          // Backfill-aware state, same as list — a reopened session must gate as pending
          // (excludable), not as uploaded.
          const { sessionById, uploadedMap, excludedSet, startedMap, migrationTimestamp } =
            await loadSessionStateWithAgentMigration(stateDir, transcriptsDirs);

          const session = sessionById.get(input.sessionId);
          if (!session) {
            throw new Error(`No session matching "${input.sessionId}"`);
          }

          if (session.agentId) {
            throw new Error(hive.upload.agentCannotExclude);
          }

          // consentMtime/snooze don't affect excludability — see uploadExclude.
          const status = computeSessionStatus(session, { uploadedMap, excludedSet, consentMtime: 0, snoozeUntil: null, migrationTimestamp });
          const partial = hasIncompleteUpload(session.sessionId, uploadedMap, startedMap);
          const id = session.sessionId.slice(0, 8);

          switch (await excludeSessionChecked(stateDir, session.sessionId, status, partial)) {
            case 'already-excluded':
              return { alreadyExcluded: true };
            case 'denied-uploaded':
              throw new Error(hive.upload.cannotExcludeUploaded(id));
            case 'denied-partial':
              throw new Error(hive.upload.cannotExcludePartial(id));
            case 'excluded':
              return { alreadyExcluded: false };
          }
        }),

      upload: t.procedure
        .input(z.object({ sessionId: z.string() }))
        .mutation(async ({ input }) => {
          const checkoutId = await getOrCreateCheckoutId(stateDir);
          const ids = getProjectIdentifiers(cwd);

          const transcriptsDirs = await loadTranscriptsDirs(stateDir);
          const { sessionById, agentsByParent } = await loadSessionState(stateDir, transcriptsDirs);

          const session = sessionById.get(input.sessionId);
          if (!session) {
            throw new Error(`No session matching "${input.sessionId}"`);
          }

          if (session.agentId) {
            throw new Error(hive.upload.agentCannotUpload);
          }

          // Check consent windows before uploading
          const consentWindows = await loadConsentWindows(ids);
          if (consentWindows && !isInConsentWindows(session.mtime.getTime(), consentWindows)) {
            throw new Error(hive.upload.outsideConsentWindow);
          }

          const parentRead = await readAndSanitizeSession(session.path);
          const agents = await findAgentsForParent(session, agentsByParent, transcriptsDirs, parentRead.cwds);
          return uploadParentWithAgents({ parent: session, parentRead, agents, checkoutId, ids, stateDir });
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
        const { parentSessions, uploadedMap, excludedSet, migrationTimestamp } =
          await loadSessionStateWithAgentMigration(stateDir, transcriptsDirs);

        const consentResult = await resolveProjectConsent(cwd);
        if ('error' in consentResult) {
          throw new Error(`Consent not available: ${consentResult.error}`);
        }
        const { consentMtime } = consentResult;

        let pending = 0;
        let ready = 0;
        let uploaded = 0;
        let excluded = 0;
        const statusCtx = { uploadedMap, excludedSet, consentMtime, snoozeUntil, migrationTimestamp };
        for (const session of parentSessions) {
          const status = computeSessionStatus(session, statusCtx);
          if (isEligibleForAutoUpload(status)) ready++;
          else if (status.type === 'excluded') excluded++;
          else if (status.type === 'uploaded') uploaded++;
          else pending++;
        }

        return { snoozeUntil, counts: { pending, ready, uploaded, excluded, total: parentSessions.length } };
      }),
    }),
  });
}

export type AppRouter = ReturnType<typeof createReviewRouter>;
