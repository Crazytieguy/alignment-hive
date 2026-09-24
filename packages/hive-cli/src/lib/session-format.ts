import type { SessionMeta } from '@alignment-hive/session-data';

export const SESSION_FORMAT_VERSION = '0.1' as const;

/** The one session-meta shape, used by uploads and the review preview. */
export function buildSessionMeta(m: Omit<SessionMeta, '_type' | 'version'>): SessionMeta {
  const { agentId, parentSessionId, agentType, workflowRunId, ...rest } = m;
  return {
    _type: 'session-meta',
    version: SESSION_FORMAT_VERSION,
    ...rest,
    ...(agentId && { agentId }),
    ...(parentSessionId && { parentSessionId }),
    ...(agentType && { agentType }),
    ...(workflowRunId && { workflowRunId }),
  };
}
