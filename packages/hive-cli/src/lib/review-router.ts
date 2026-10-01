import { stat } from 'node:fs/promises';
import { initTRPC } from '@trpc/server';
import { z } from 'zod';
import { getOrCreateCheckoutId, loadTranscriptsDirs } from './config';
import { resolveProjectConsent } from './convex';
import { errors, hive } from './messages';
import { buildSessionMeta } from './session-format';
import { excludeSessionChecked, findAgentsForParent } from './session-state';
import { getSnoozeUntil, setSnooze } from './snooze';
import { parseDuration } from './time-filter';
import { withUploadLock } from './upload-lock';
import {
  discoverWorkflowRuns,
  loadConsentWindows,
  loadSessionStateWithMigrations,
  readAndSanitizeSession,
  readSessionCwds,
  sessionStatusFields,
  summarizeSessions,
  uploadOneSession,
} from './upload-session';
import type { DiscoveredSession, DiscoveryCache, StatusContext } from './session-state';
import type { SessionReadResult, SummaryCache } from './upload-session';

const t = initTRPC.create();

const sessionInput = z.object({ sessionId: z.string() });

const MAX_CACHED_READS = 4;

export function createReviewRouter(stateDir: string, cwd: string) {
  // Per-process caches, revalidated by file mtime (and size); statuses are always computed fresh.
  const discoveryCache: DiscoveryCache = new Map();
  const summaryCache: SummaryCache = new Map();
  // The last few sanitized transcripts, so a refetch or moving between a parent and its agents
  // does not sanitize again. The excluded count (the file only grows) revalidates them too: an
  // exclusion changes what other sessions' copies of it show.
  const reads = new Map<string, { size: number; mtimeMs: number; excluded: number; read: SessionReadResult }>();
  const readSanitized = async (session: DiscoveredSession, excludedSet: Set<string>): Promise<SessionReadResult> => {
    const { path } = session;
    const { size, mtimeMs } = await stat(path);
    const hit = reads.get(path);
    reads.delete(path);
    const read =
      hit && hit.size === size && hit.mtimeMs === mtimeMs && hit.excluded === excludedSet.size
        ? hit.read
        : await readAndSanitizeSession(session, excludedSet);
    reads.set(path, { size, mtimeMs, excluded: excludedSet.size, read });
    if (reads.size > MAX_CACHED_READS) reads.delete(reads.keys().next().value!);
    return read;
  };

  // Backfill-aware state everywhere: a reopened session must gate as pending (excludable), not
  // as uploaded.
  const loadState = async () => {
    const transcriptsDirs = await loadTranscriptsDirs(stateDir);
    const state = await loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd, discoveryCache);
    return { ...state, transcriptsDirs };
  };
  type State = Awaited<ReturnType<typeof loadState>>;

  type StatusInputs = Pick<StatusContext, 'consentMtime' | 'snoozeUntil' | 'consentWindows'>;

  /**
   * Status inputs beyond the state. Without the backend's consent time, every session not yet
   * uploaded or excluded shows as pending, never as more uploadable than it may be.
   */
  const statusInputs = async (): Promise<StatusInputs & { consentError?: string }> => {
    const [consent, snoozeUntil] = await Promise.all([
      resolveProjectConsent(cwd)
        .then(async ({ consentMtime, ids }) => ({ consentMtime, consentWindows: await loadConsentWindows(ids) }))
        .catch((err: unknown) => ({
          consentMtime: Date.now(),
          consentError: err instanceof Error ? err.message : String(err),
        })),
      getSnoozeUntil(stateDir),
    ]);
    return { ...consent, snoozeUntil };
  };

  const statusOf = (
    session: DiscoveredSession,
    state: State,
    { consentMtime, snoozeUntil, consentWindows }: StatusInputs,
  ) => sessionStatusFields(session, state, { ...state, consentMtime, snoozeUntil, consentWindows });

  const findSession = (state: State, sessionId: string): DiscoveredSession => {
    const session = state.sessionById.get(sessionId);
    if (!session) throw new Error(errors.sessionNotFound(sessionId));
    return session;
  };

  const findParent = (state: State, sessionId: string): DiscoveredSession => {
    const session = findSession(state, sessionId);
    if (session.agentId) throw new Error(errors.sessionNotFound(sessionId));
    return session;
  };

  /** Exclude one parent through the single exclusion path; denials throw. Updates `state`. */
  const excludeOne = async (state: State, sessionId: string) => {
    const session = findSession(state, sessionId);
    if (session.agentId) throw new Error(hive.upload.agentCannotExclude);
    const id = session.sessionId.slice(0, 8);
    const outcome = await excludeSessionChecked(stateDir, state, session);
    switch (outcome.result) {
      case 'denied-uploaded':
        throw new Error(hive.upload.cannotExcludeUploaded(id));
      case 'denied-partial':
        throw new Error(hive.upload.cannotExcludePartial(id));
      case 'already-excluded':
      case 'excluded':
        state.excludedSet.add(session.sessionId);
        // Excluded is final whatever the consent time, so the status needs no backend call.
        return {
          alreadyExcluded: outcome.result === 'already-excluded',
          hadPriorUpload: outcome.hadPriorUpload,
          ...statusOf(session, state, { consentMtime: 0, snoozeUntil: null }),
        };
    }
  };

  return t.router({
    sessions: t.router({
      list: t.procedure.query(async () => {
        const [state, inputs] = await Promise.all([loadState(), statusInputs()]);
        const { consentError, ...statusCtx } = inputs;
        const rows = await summarizeSessions(state, { ...state, ...statusCtx }, summaryCache);
        const sessions = rows.map(({ session, status, partialUpload, statusLabel, summary }) => ({
          sessionId: session.sessionId,
          date: session.mtime.toISOString(),
          status,
          partialUpload,
          statusLabel,
          summary: summary.slice(0, 120),
        }));
        return { sessions, snoozeUntil: inputs.snoozeUntil, ...(consentError !== undefined && { consentError }) };
      }),

      /** The session's own entries plus its agents and runs as metadata; their content loads separately. */
      content: t.procedure.input(sessionInput).query(async ({ input }) => {
        const [state, inputs] = await Promise.all([loadState(), statusInputs()]);
        const session = findSession(state, input.sessionId);

        const sessionRead = await readSanitized(session, state.excludedSet);
        const agents = session.agentId
          ? []
          : await findAgentsForParent(session, state.agentsByParent, state.transcriptsDirs, sessionRead.cwds);
        // Discovery is best-effort.
        const workflowRuns = session.agentId
          ? []
          : await discoverWorkflowRuns(session, sessionRead.cwds)
              .then((rs) => rs.map((r) => ({ ...r.row, runId: r.row.workflowRunId })))
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
          ...statusOf(session, state, inputs),
          agents: agents.map((agent) => ({
            sessionId: agent.sessionId,
            agentId: agent.agentId!,
            ...(agent.agentType && { agentType: agent.agentType }),
            ...(agent.workflowRunId && { workflowRunId: agent.workflowRunId }),
          })),
          workflowRuns,
        };
      }),

      agentContent: t.procedure
        .input(z.object({ sessionId: z.string(), agentId: z.string() }))
        .query(async ({ input }) => {
          const state = await loadState();
          const parent = findParent(state, input.sessionId);
          const byId = (agents: Array<DiscoveredSession>) => agents.find((a) => a.agentId === input.agentId);
          // Agents found in place need no read of the parent; worktree agents need its cwds.
          const agent =
            byId(state.agentsByParent.get(parent.sessionId) ?? []) ??
            byId(
              await findAgentsForParent(
                parent,
                state.agentsByParent,
                state.transcriptsDirs,
                await readSessionCwds(parent.path),
              ),
            );
          if (!agent) throw new Error(errors.sessionNotFound(`${input.sessionId}/agent-${input.agentId}`));
          const { sanitizedEntries } = await readSanitized(agent, state.excludedSet);
          return { entries: sanitizedEntries, messageCount: sanitizedEntries.length };
        }),

      /** The full sanitized run blob, exactly what upload sends. */
      workflowRun: t.procedure
        .input(z.object({ sessionId: z.string(), runId: z.string() }))
        .query(async ({ input }) => {
          const parent = findParent(await loadState(), input.sessionId);
          const run = (await discoverWorkflowRuns(parent, await readSessionCwds(parent.path), input.runId)).at(0);
          if (!run) throw new Error(errors.sessionNotFound(`${input.sessionId}/${input.runId}`));
          return { blob: run.blob };
        }),

      exclude: t.procedure.input(sessionInput).mutation(async ({ input }) => {
        return excludeOne(await loadState(), input.sessionId);
      }),

      excludeMany: t.procedure.input(z.object({ sessionIds: z.array(z.string()) })).mutation(async ({ input }) => {
        const state = await loadState();
        const results: Array<
          { sessionId: string; ok: boolean; error?: string } & Partial<Awaited<ReturnType<typeof excludeOne>>>
        > = [];
        for (const sessionId of input.sessionIds) {
          try {
            results.push({ sessionId, ok: true, ...(await excludeOne(state, sessionId)) });
          } catch (err) {
            results.push({ sessionId, ok: false, error: err instanceof Error ? err.message : String(err) });
          }
        }
        return { results };
      }),

      upload: t.procedure.input(sessionInput).mutation(async ({ input }) => {
        const [state, checkoutId, { consentMtime, ids }] = await Promise.all([
          loadState(),
          getOrCreateCheckoutId(stateDir),
          resolveProjectConsent(cwd),
        ]);

        const session = findSession(state, input.sessionId);
        if (session.agentId) throw new Error(hive.upload.agentCannotUpload);

        const consentWindows = await loadConsentWindows(ids);
        // The same lock as `hive upload send`, so this never runs alongside the scheduled upload.
        const result = await withUploadLock(stateDir, () =>
          uploadOneSession({
            session,
            state,
            statusCtx: { ...state, consentMtime, snoozeUntil: null, consentWindows },
            transcriptsDirs: state.transcriptsDirs,
            checkoutId,
            ids,
            stateDir,
          }),
        );
        if (!result) throw new Error(hive.upload.uploadInProgress);
        if (!result.ok) throw new Error(result.error);

        const [fresh, snoozeUntil] = await Promise.all([loadState(), getSnoozeUntil(stateDir)]);
        return {
          ...result,
          ...statusOf(findSession(fresh, session.sessionId), fresh, { consentMtime, snoozeUntil, consentWindows }),
        };
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
