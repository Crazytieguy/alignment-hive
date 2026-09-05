import { initTRPC } from '@trpc/server';
import { z } from 'zod';
import { getOrCreateCheckoutId, loadTranscriptsDirs } from './config';
import { resolveProjectConsent } from './convex';
import { errors, hive } from './messages';
import { buildSessionMeta } from './session-format';
import { excludeSessionChecked, findAgentsForParent, loadSessionState } from './session-state';
import { getSnoozeUntil, setSnooze } from './snooze';
import { parseDuration } from './time-filter';
import {
  discoverWorkflowRuns,
  loadConsentWindows,
  loadSessionStateWithMigrations,
  mapBatched,
  readAndSanitizeSession,
  summarizeSessions,
  uploadOneSession,
} from './upload-session';

const t = initTRPC.create();

export function createReviewRouter(stateDir: string, cwd: string) {
  return t.router({
    sessions: t.router({
      list: t.procedure.query(async () => {
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        const state = await loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd);
        const { consentMtime } = await resolveProjectConsent(cwd);
        const snoozeUntil = await getSnoozeUntil(stateDir);
        const rows = await summarizeSessions(state, { ...state, consentMtime, snoozeUntil });
        const sessions = rows.map(({ session, status, partialUpload, statusLabel, summary }) => ({
          sessionId: session.sessionId,
          date: session.mtime.toISOString(),
          status,
          partialUpload,
          statusLabel,
          summary: summary.slice(0, 120),
        }));
        return { sessions, snoozeUntil };
      }),

      content: t.procedure.input(z.object({ sessionId: z.string() })).query(async ({ input }) => {
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        const { sessionById, agentsByParent } = await loadSessionState(stateDir, transcriptsDirs, cwd);

        const session = sessionById.get(input.sessionId);
        if (!session) throw new Error(errors.sessionNotFound(input.sessionId));

        const sessionRead = await readAndSanitizeSession(session.path);
        const allAgents = session.agentId
          ? []
          : await findAgentsForParent(session, agentsByParent, transcriptsDirs, sessionRead.cwds);

        const agents = await mapBatched(allAgents, 10, async (agent) => {
          const agentRead = await readAndSanitizeSession(agent.path);
          return {
            sessionId: agent.sessionId,
            agentId: agent.agentId,
            ...(agent.agentType && { agentType: agent.agentType }),
            ...(agent.workflowRunId && { workflowRunId: agent.workflowRunId }),
            entries: agentRead.sanitizedEntries,
            messageCount: agentRead.sanitizedEntries.length,
          };
        });

        // Full sanitized run blobs are included so the UI shows exactly what upload sends;
        // discovery is best-effort.
        const workflowRuns = session.agentId
          ? []
          : await discoverWorkflowRuns(session, sessionRead.cwds)
              .then((rs) => rs.map((r) => ({ ...r.row, blob: r.blob })))
              .catch(() => []);

        return {
          meta: buildSessionMeta({
            sessionId: session.sessionId,
            checkoutId: 'local',
            rawMtime: session.mtime.toISOString(),
            messageCount: sessionRead.sanitizedEntries.length,
            agentId: session.agentId,
            parentSessionId: session.parentSessionId,
          }),
          entries: sessionRead.sanitizedEntries,
          agents,
          workflowRuns,
        };
      }),

      exclude: t.procedure.input(z.object({ sessionId: z.string() })).mutation(async ({ input }) => {
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        // Backfill-aware state, same as list — a reopened session must gate as pending
        // (excludable), not as uploaded.
        const state = await loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd);

        const session = state.sessionById.get(input.sessionId);
        if (!session) throw new Error(errors.sessionNotFound(input.sessionId));
        if (session.agentId) throw new Error(hive.upload.agentCannotExclude);

        const id = session.sessionId.slice(0, 8);
        const outcome = await excludeSessionChecked(stateDir, state, session);
        switch (outcome.result) {
          case 'already-excluded':
            return { alreadyExcluded: true, hadPriorUpload: outcome.hadPriorUpload };
          case 'denied-uploaded':
            throw new Error(hive.upload.cannotExcludeUploaded(id));
          case 'denied-partial':
            throw new Error(hive.upload.cannotExcludePartial(id));
          case 'excluded':
            return { alreadyExcluded: false, hadPriorUpload: outcome.hadPriorUpload };
        }
      }),

      upload: t.procedure.input(z.object({ sessionId: z.string() })).mutation(async ({ input }) => {
        const transcriptsDirs = await loadTranscriptsDirs(stateDir);
        const [state, checkoutId, { consentMtime, ids }] = await Promise.all([
          loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd),
          getOrCreateCheckoutId(stateDir),
          resolveProjectConsent(cwd),
        ]);

        const session = state.sessionById.get(input.sessionId);
        if (!session) throw new Error(errors.sessionNotFound(input.sessionId));
        if (session.agentId) throw new Error(hive.upload.agentCannotUpload);

        return uploadOneSession({
          session,
          state,
          statusCtx: { ...state, consentMtime, snoozeUntil: null },
          consentWindows: await loadConsentWindows(ids),
          transcriptsDirs,
          checkoutId,
          ids,
          stateDir,
        });
      }),
    }),

    upload: t.router({
      snooze: t.procedure.input(z.object({ duration: z.string() })).mutation(async ({ input }) => {
        const durationMs = parseDuration(input.duration);
        if (!durationMs) throw new Error(hive.upload.invalidDuration(input.duration));
        return { until: await setSnooze(stateDir, durationMs) };
      }),
    }),
  });
}

export type AppRouter = ReturnType<typeof createReviewRouter>;
