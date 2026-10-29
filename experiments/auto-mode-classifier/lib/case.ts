// One benchmark case: what a classifier sees before judging a tool call, plus the label. Builders write these as
// JSONL; every arm reads the same file. Nothing in a case is ever executed.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

/** A tool call as the agent issued it: Claude Code tool name and input object. */
export type ToolCall = { tool: string; input: Record<string, unknown> };

/** One transcript entry before the judged action, in order. */
export type CaseEvent = { user: string } | ({ id?: string } & ToolCall);

export type Label = 'block' | 'allow' | 'unknown';

export type Case = {
  /** Stable, unique across datasets: `<dataset>:<source id>[:<step>]`. */
  id: string;
  dataset: 'corpus' | 'monitoringbench' | 'mbhonest' | 'ampermbench' | 'sleight' | 'floor' | 'multitool';
  /** Unit for train/test splitting and bootstrap resampling: the source trajectory, task or session. */
  group: string;
  label: Label;
  /** Where the label comes from, e.g. `attack_step`, `oracle:must_preserve`, `historical:local-classifier`. */
  labelSource: string;
  /** Dataset-specific risk category, if any (free text; analysis groups by it). */
  category?: string;
  /** Working directory the agent ran in, as the classifier should be told. */
  cwd: string;
  /** Earlier user turns and tool calls, oldest first. Tool results are deliberately absent. */
  events: Array<CaseEvent>;
  /** The tool call under judgement. */
  action: ToolCall;
  /** Earlier tool calls in the same model reply as `action` (multi-tool replies only), in order. */
  siblings?: Array<ToolCall>;
  /** Named subsets the analysis reports separately, e.g. `deny-rule`, `trigger`, `gpt-issued`, `hidden-owner`. */
  slices?: Array<string>;
  /** Free-form provenance for adjudication (never shown to a classifier). */
  notes?: string;
};

/**
 * A hash of everything a classifier sees in a case (cwd, events, siblings, action). Results carry it, so a dataset
 * rebuilt under the same ids never mixes old results with new cases: resume reruns, analysis drops the stale ones.
 */
export function caseHash(c: Case): string {
  return new Bun.CryptoHasher('sha256').update(JSON.stringify([c.cwd, c.events, c.siblings ?? [], c.action])).digest('hex').slice(0, 16);
}

/** Cases from comma-separated JSONL files (a bare name is in `dataDir`), each with its file and 1-based line. */
export function readCaseLines(files: string, dataDir: string): Array<{ c: Case; file: string; line: number }> {
  const out: Array<{ c: Case; file: string; line: number }> = [];
  for (const f of files.split(',')) {
    const file = f.includes('/') ? f : join(dataDir, f);
    readFileSync(file, 'utf8')
      .split('\n')
      .forEach((l, i) => {
        if (l.trim()) out.push({ c: JSON.parse(l) as Case, file, line: i + 1 });
      });
  }
  return out;
}

export const readCases = (files: string, dataDir: string): Array<Case> => readCaseLines(files, dataDir).map((x) => x.c);
