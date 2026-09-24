import { isHumanMessage } from './noise';
import type { Transcript } from './transcript';

const MAX_SUMMARY = 100;

/**
 * A session's name in upload listings: its title, else the first line of its first human
 * message (for an agent, the message that started it), clipped to 100 characters.
 */
export function sessionSummary(t: Transcript): string | undefined {
  if (t.title) return t.title;
  const first = t.entries.find((e) => isHumanMessage(e));
  const line = first?.kind === 'user' ? first.text.trim().split('\n')[0].trim() : '';
  if (!line) return undefined;
  return line.length > MAX_SUMMARY ? `${line.slice(0, MAX_SUMMARY - 3)}...` : line;
}
