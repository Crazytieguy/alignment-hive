import { sanitizerStamp } from './sanitizer-stamp' with { type: 'macro' };
import type { SessionMeta } from '@alignment-hive/session-data';

export const SESSION_FORMAT_VERSION = '0.1' as const;
/** Which sanitizer redacted an upload, so uploads made before a rule change can be found and re-sanitized. */
export const SANITIZER_STAMP = sanitizerStamp();

/** The one session-meta shape, used by uploads and the review preview. */
export function buildSessionMeta(m: Omit<SessionMeta, '_type' | 'version' | 'sanitizer'>): SessionMeta {
  const { agentId, parentSessionId, agentType, workflowRunId, ...rest } = m;
  return {
    _type: 'session-meta',
    version: SESSION_FORMAT_VERSION,
    sanitizer: SANITIZER_STAMP,
    ...rest,
    ...(agentId && { agentId }),
    ...(parentSessionId && { parentSessionId }),
    ...(agentType && { agentType }),
    ...(workflowRunId && { workflowRunId }),
  };
}
