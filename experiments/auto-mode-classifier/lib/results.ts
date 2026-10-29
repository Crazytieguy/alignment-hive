// An arm's results.jsonl: one line per case run, appended; reruns append again, so the last line per id wins.
import { existsSync, readFileSync } from 'node:fs';

export type CaseResult = Record<string, unknown> & { id: string; decision: string };

/** Results on disk by case id (the last line per id wins), each with its 1-based line number. */
export function loadResultLines(file: string): Map<string, { r: CaseResult; line: number }> {
  const m = new Map<string, { r: CaseResult; line: number }>();
  if (!existsSync(file)) return m;
  readFileSync(file, 'utf8')
    .split('\n')
    .forEach((l, i) => {
      if (!l.trim()) return;
      try {
        const r = JSON.parse(l) as CaseResult;
        m.set(r.id, { r, line: i + 1 });
      } catch {
        // torn last line
      }
    });
  return m;
}

/** Results already on disk for an arm, by case id (the last line per id wins). */
export function loadResults(file: string): Map<string, CaseResult> {
  return new Map([...loadResultLines(file)].map(([id, { r }]) => [id, r]));
}
