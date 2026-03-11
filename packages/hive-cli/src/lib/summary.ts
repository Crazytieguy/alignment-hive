import type { KnownEntry } from '@alignment-hive/shared';

const META_XML_TAGS = ['<command-name>', '<local-command-', '<system-reminder>'];

function isMetaXml(text: string): boolean {
  const trimmed = text.trim();
  return META_XML_TAGS.some((tag) => trimmed.startsWith(tag));
}

function isGarbageSummary(summary: string): boolean {
  const trimmed = summary.trim();
  return isMetaXml(trimmed) || trimmed.startsWith('Caveat:');
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
    if (s.leafUuid && uuids.has(s.leafUuid) && !isGarbageSummary(s.summary)) {
      return s.summary;
    }
  }

  const lastSummary = summaries.at(-1)?.summary;
  return lastSummary && !isGarbageSummary(lastSummary) ? lastSummary : undefined;
}

function findFirstUserPrompt(entries: Array<KnownEntry>): string | undefined {
  for (const entry of entries) {
    if (entry.type !== 'user') continue;
    const content = entry.message.content;
    if (!content) continue;

    let text: string | undefined;
    if (typeof content === 'string') {
      text = content;
    } else if (Array.isArray(content)) {
      for (const block of content) {
        if (block.type === 'text' && 'text' in block && typeof block.text === 'string') {
          text = block.text;
          break;
        }
      }
    }

    if (text) {
      const trimmed = text.trim();
      if (isMetaXml(trimmed)) continue;
      const firstLine = trimmed.split('\n')[0].trim();
      if (firstLine) {
        return firstLine.length > 100 ? `${firstLine.slice(0, 97)}...` : firstLine;
      }
    }
  }
  return undefined;
}

export function extractSessionSummary(entries: Array<KnownEntry>): string | undefined {
  return findSummaryEntry(entries) || findFirstUserPrompt(entries);
}
