import { parseKnownEntry } from '@alignment-hive/session-data';
import type { KnownEntry, SessionMeta } from '@alignment-hive/session-data';

export const SESSION_FORMAT_VERSION = '0.1' as const;

export type ReadSessionResult = { meta: SessionMeta; entries: Array<KnownEntry> } | { error: string } | null;

export function* parseJsonl(content: string) {
  for (const line of content.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      yield JSON.parse(trimmed) as unknown;
    } catch (error) {
      if (process.env.DEBUG) {
        console.warn('Skipping malformed JSONL line:', error);
      }
    }
  }
}

const KEPT_ENTRY_TYPES = new Set(['user', 'assistant', 'summary', 'system']);

/** The entry types that are read, uploaded and formatted; everything else (snapshots, queue ops, unknown) is null. */
export function transformEntry(rawEntry: unknown): KnownEntry | null {
  const entry = parseKnownEntry(rawEntry);
  if (entry) return KEPT_ENTRY_TYPES.has(entry.type) ? entry : null;
  const type = (rawEntry as { type?: unknown } | null)?.type;
  if (process.env.DEBUG && typeof type === 'string' && KEPT_ENTRY_TYPES.has(type)) {
    console.warn(`Skipping ${type} entry that failed the schema`);
  }
  return null;
}

export function parseEntries(content: string): Array<KnownEntry> {
  const entries: Array<KnownEntry> = [];
  for (const raw of parseJsonl(content)) {
    const entry = transformEntry(raw);
    if (entry) entries.push(entry);
  }
  return entries;
}

/** The one session-meta shape, used by uploads, local reads and the review preview. */
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
