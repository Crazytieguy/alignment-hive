// The round-4 corpus for the recall layer: corpus.ts's human messages, answers and assistant replies, plus the tool
// calls that carry decisions (files written or edited, shell commands, agents started; inputs only, no results), and
// --bg conversations, which corpus.ts dropped with every sdk-cli transcript.
// Usage: bun corpus4.ts > $BENCH/corpus4.jsonl; CORPUS_CLIP=0 for unclipped text (corpus4-full.jsonl, for the clip sweep)
import { readdirSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { inOutline, isHumanMessage, readRecords, readTranscript } from '@alignment-hive/session-data';
import type { Entry } from '@alignment-hive/session-data';

const root = join(homedir(), '.claude/projects');
const CLIP = Number(process.env.CORPUS_CLIP ?? 3000);
const clip = (s: string) => (CLIP && s.length > CLIP ? `${s.slice(0, CLIP)} [+${s.length - CLIP} chars]` : s);
const str = (v: unknown) => (typeof v === 'string' ? v : v === undefined ? '' : JSON.stringify(v));

/** A tool call's text for the corpus: what it did, from its input. */
function toolText(e: Entry & { kind: 'tool' }): string | undefined {
  const i = (e.input ?? {}) as Record<string, unknown>;
  switch (e.tool) {
    case 'Write':
      return `${str(i.file_path)}\n${str(i.content)}`;
    case 'Edit':
      return `${str(i.file_path)}\n${str(i.new_string)}`;
    case 'MultiEdit':
      return `${str(i.file_path)}\n${((i.edits as Array<Record<string, unknown>>) ?? []).map((x) => str(x.new_string)).join('\n')}`;
    case 'Bash':
      return str(i.command);
    case 'Agent':
    case 'Task':
      return `${str(i.subagent_type)} ${str(i.description)}\n${str(i.prompt)}`;
    default:
      return undefined;
  }
}

for (const dir of readdirSync(root).filter((d) => d.startsWith('-Users-yoav-projects-alignment-hive'))) {
  for (const f of readdirSync(join(root, dir))) {
    if (!f.endsWith('.jsonl')) continue;
    const session = f.slice(0, 8);
    let es: Array<Entry>;
    let entrypoint: unknown;
    try {
      const { records } = readRecords(readFileSync(join(root, dir, f), 'utf8'));
      entrypoint = records.find((r) => typeof r.data.entrypoint === 'string')?.data.entrypoint;
      es = readTranscript(records).entries;
    } catch {
      continue;
    }
    const human = es.filter((e) => isHumanMessage(e) && e.kind === 'user' && !e.branch) as Array<Entry & { kind: 'user' }>;
    // sdk-cli is claude -p and --bg alike: keep conversations (3+ human messages) that aren't a scripted pipeline.
    if (entrypoint === 'sdk-cli' && (human.length < 3 || /^Read review-run\//.test(human[0]!.text))) continue;
    for (const e of es) {
      if (e.branch || !e.time) continue;
      let kind: string | undefined;
      let text: string | undefined;
      let tool: string | undefined;
      if (isHumanMessage(e) && e.kind === 'user') [kind, text] = ['user', e.text];
      else if (e.kind === 'assistant') [kind, text] = ['assistant', e.text];
      else if (e.kind === 'tool' && (e.tool === 'AskUserQuestion' || e.tool === 'ArtifactComments') && inOutline(e, false, false) && e.result)
        [kind, text] = ['answer', e.result];
      else if (e.kind === 'tool') [kind, text, tool] = ['tool', toolText(e), e.tool];
      if (!kind || !text || text.length < 8) continue;
      console.log(JSON.stringify({ loc: `${session}:${e.n}`, session, time: e.time, kind, ...(tool ? { tool } : {}), text: clip(text) }));
    }
  }
}
