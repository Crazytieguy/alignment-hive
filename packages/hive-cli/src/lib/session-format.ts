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

type SessionEntry = Exclude<ReturnType<typeof parseKnownEntry>['data'], null>;

export function transformEntry(rawEntry: unknown): { entry: SessionEntry | null; error?: string } {
  const result = parseKnownEntry(rawEntry);
  if (result.error) return { entry: null, error: result.error };
  if (!result.data) return { entry: null };

  const type = result.data.type;
  if (type === 'user' || type === 'assistant' || type === 'summary' || type === 'system') {
    return { entry: result.data };
  }
  return { entry: null };
}
