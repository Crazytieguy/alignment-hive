import { extractUserText } from './parse';
import type { KnownEntry } from './schemas';

const META_XML_TAGS = ['<command-name>', '<local-command-', '<system-reminder>'];

/** Text that is Claude Code plumbing rather than the user's own words. */
function isGarbageText(text: string): boolean {
  const trimmed = text.trim();
  return META_XML_TAGS.some((tag) => trimmed.startsWith(tag)) || trimmed.startsWith('Caveat:');
}

function findSummaryEntry(entries: Array<KnownEntry>): string | undefined {
  const uuids = new Set<string>();
  const summaries: Array<{ summary: string; leafUuid?: string }> = [];

  for (const entry of entries) {
    if ('uuid' in entry && typeof entry.uuid === 'string') {
      uuids.add(entry.uuid);
    }
    if (entry.type === 'summary') {
      summaries.push({ summary: entry.summary, leafUuid: entry.leafUuid });
    }
  }

  for (const s of summaries) {
    if (s.leafUuid && uuids.has(s.leafUuid) && !isGarbageText(s.summary)) {
      return s.summary;
    }
  }

  const lastSummary = summaries.at(-1)?.summary;
  return lastSummary && !isGarbageText(lastSummary) ? lastSummary : undefined;
}

function findFirstUserPrompt(entries: Array<KnownEntry>): string | undefined {
  for (const entry of entries) {
    if (entry.type !== 'user') continue;
    const trimmed = extractUserText(entry).trim();
    if (!trimmed || isGarbageText(trimmed)) continue;
    const firstLine = trimmed.split('\n')[0].trim();
    if (!firstLine) continue;
    return firstLine.length > 100 ? `${firstLine.slice(0, 97)}...` : firstLine;
  }
  return undefined;
}

export function extractSessionSummary(entries: Array<KnownEntry>): string | undefined {
  return findSummaryEntry(entries) || findFirstUserPrompt(entries);
}
