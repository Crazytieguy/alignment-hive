import { collectToolResults, formatEntry, getTimestamp } from './format';
import { computeUniformLimit, countWords } from './truncation';
import type { ReadFieldFilter } from './field-filter';
import type { LogicalEntry, ToolResultInfo } from './format';
import type { KnownEntry } from './schemas';

const DEFAULT_TARGET_WORDS = 2000;

export interface RangeFormatOptions {
  redact?: boolean;
  targetWords?: number;
  skipWords?: number;
  fieldFilter?: ReadFieldFilter;
  allEntries: Array<KnownEntry>;
}

export function formatRangeEntries(
  rangeEntries: Array<LogicalEntry>,
  options: RangeFormatOptions,
): string {
  const { redact = false, targetWords = DEFAULT_TARGET_WORDS, skipWords = 0, fieldFilter, allEntries } = options;
  const toolResults = collectToolResults(allEntries);

  let wordLimit: number | undefined;
  if (redact) {
    const wordCounts = collectWordCounts(rangeEntries, skipWords);
    wordLimit = computeUniformLimit(wordCounts, targetWords) ?? undefined;
  }

  const results: Array<string> = [];
  let prevDate: string | undefined;

  for (let i = 0; i < rangeEntries.length; i++) {
    const { lineNumber, entry } = rangeEntries[i];

    const timestamp = getTimestamp(entry);
    const currentDate = timestamp ? timestamp.slice(0, 10) : undefined;
    const isFirst = i === 0;

    const formatted = formatEntry(entry, {
      lineNumber,
      toolResults,
      redact,
      prevDate,
      isFirst,
      wordLimit,
      skipWords,
      fieldFilter,
    });

    if (formatted) {
      results.push(formatted);
    }

    if (currentDate) {
      prevDate = currentDate;
    }
  }

  if (redact && wordLimit !== undefined) {
    results.push(`[Limited to ${wordLimit} words per field. Use --skip ${wordLimit} for more.]`);
  }

  const separator = redact ? '\n' : '\n\n';
  return results.join(separator);
}

function collectWordCounts(entries: Array<LogicalEntry>, skipWords: number): Array<number> {
  const counts: Array<number> = [];

  function addCount(text: string): void {
    const words = countWords(text);
    const afterSkip = Math.max(0, words - skipWords);
    if (afterSkip > 0) {
      counts.push(afterSkip);
    }
  }

  for (const { entry } of entries) {
    if (entry.type === 'user') {
      const content = getUserMessageContent(entry.message.content);
      addCount(content);
    } else if (entry.type === 'assistant') {
      const blocks = entry.message.content;
      if (typeof blocks === 'string') {
        addCount(blocks);
      } else if (Array.isArray(blocks)) {
        for (const block of blocks) {
          if (block.type === 'text' && 'text' in block) {
            addCount(block.text);
          } else if (block.type === 'thinking' && 'thinking' in block) {
            addCount(block.thinking);
          }
        }
      }
    } else if (entry.type === 'summary') {
      addCount(entry.summary);
    } else if (entry.type === 'system') {
      addCount(entry.content || '');
    }
  }

  return counts;
}

function getUserMessageContent(content: string | Array<{ type: string; text?: string }> | undefined): string {
  if (!content) return '';
  if (typeof content === 'string') return content;

  const textParts: Array<string> = [];
  for (const block of content) {
    if (block.type === 'tool_result') continue;
    if (block.type === 'text' && 'text' in block && block.text) {
      textParts.push(block.text);
    }
  }

  return textParts.join('\n');
}
