// Pure readers for what one resumed case left behind: Claude Code's decision on each reply tool call (from the
// session file), its classifier calls (debug log), the classifier requests and replies (wire log) and, for R, the
// router's capture records. Also the history check: does a classifier input contain every history entry?
import { assembleStream, textOf, type Message } from '../../model-router-canary/lib/probes';
import type { Case, ToolCall } from './case';

export type Decision = 'allow' | 'block' | 'unavailable' | 'error';

export type ToolOutcome = {
  id: string;
  decision: Decision;
  /** The classifier's reason as Claude Code relayed it to the model (blocks), or what went wrong (errors). */
  reason?: string;
  toolDenialKind?: string;
  permissionDecision?: Record<string, unknown>;
  resultText?: string;
};

const DENIAL = 'Permission for this action was denied by the Claude Code auto mode classifier';
const UNAVAILABLE = /Classifier unavailable|temporarily unavailable|Auto mode is unavailable|gave no verdict|no safety verdict|server-side auto mode classifier (?:could not|failed)/i;

const resultText = (content: unknown): string =>
  typeof content === 'string'
    ? content
    : Array.isArray(content)
      ? content.map((b) => (b && typeof b === 'object' && 'text' in b && typeof b.text === 'string' ? b.text : '')).join('\n')
      : '';

/** Claude Code's decision for each id, from the resumed session's JSONL. */
export function outcomesFromSession(jsonl: string, ids: Array<string>): Array<ToolOutcome> {
  const found = new Map<string, ToolOutcome>();
  for (const line of jsonl.split('\n')) {
    if (!line.trim()) continue;
    let r: Record<string, any>;
    try {
      r = JSON.parse(line) as Record<string, any>;
    } catch {
      continue;
    }
    if (r.type !== 'user' || !Array.isArray(r.message?.content)) continue;
    for (const b of r.message.content as Array<Record<string, any>>) {
      if (b.type !== 'tool_result' || !ids.includes(b.tool_use_id)) continue;
      const text = resultText(b.content);
      const pd = r.permissionDecision as Record<string, unknown> | undefined;
      const kind = typeof r.toolDenialKind === 'string' ? r.toolDenialKind : undefined;
      const o: ToolOutcome = { id: b.tool_use_id, decision: 'allow', toolDenialKind: kind, permissionDecision: pd, resultText: text.slice(0, 600) };
      if (text.includes(DENIAL) || kind === 'automode-blocked') {
        o.decision = 'block';
        const m = /Reason: ([\s\S]*?)(?:\. If you have other tasks|$)/.exec(text);
        o.reason = (m?.[1] ?? text).trim().slice(0, 2000);
      } else if (kind === 'automode-unavailable' || kind === 'automode-parsing-error' || (b.is_error && UNAVAILABLE.test(text))) {
        o.decision = 'unavailable';
        o.reason = text.slice(0, 600);
      } else if (pd?.decision === 'accept') {
        o.decision = 'allow';
      } else if (pd?.decision === 'reject') {
        o.decision = 'block';
        o.reason = text.slice(0, 600);
      } else if (b.is_error) {
        o.decision = 'error';
        o.reason = text.slice(0, 600);
      }
      found.set(b.tool_use_id, o);
    }
  }
  return ids.map((id) => found.get(id) ?? { id, decision: 'error', reason: 'no tool_result for this call' });
}

export type DebugClassifierCall = { reqId: string; tool?: string; model?: string; stage?: string; outcome?: string; durationMs?: number };
export type DebugSummary = { classifierCalls: Array<DebugClassifierCall>; serverClassifier: Array<string>; permissionDecisionMs: Record<string, number>; errors: Array<string>; skipped: Array<string> };

const ANSI = new RegExp(`${String.fromCharCode(27)}\\[[0-9;]*m`, 'g');

export function summarizeDebugLog(text: string): DebugSummary {
  const calls = new Map<string, DebugClassifierCall>();
  const serverClassifier: Array<string> = [];
  const permissionDecisionMs: Record<string, number> = {};
  const errors: Array<string> = [];
  const skipped: Array<string> = [];
  const kv = (s: string): Record<string, string> => Object.fromEntries([...s.matchAll(/(\w+)=(\S+)/g)].map((m) => [m[1]!, m[2]!]));
  for (const raw of text.split('\n')) {
    const line = raw.replace(ANSI, '');
    if (line.includes('classifier_request_started') || line.includes('classifier_request_finished')) {
      const f = kv(line);
      if (!f.reqId) continue;
      const c = calls.get(f.reqId) ?? { reqId: f.reqId };
      if (f.tool) c.tool = f.tool;
      if (f.model) c.model = f.model;
      if (f.stage) c.stage = f.stage;
      if (f.outcome) c.outcome = f.outcome;
      if (f.durationMs) c.durationMs = Number(f.durationMs);
      calls.set(f.reqId, c);
    } else if (line.includes('[server-classifier]')) {
      serverClassifier.push(line.replace(/^\S+\s+/, '').slice(0, 400));
    } else if (line.includes('tool_dispatch_start')) {
      const f = kv(line);
      if (f.toolUseId && f.permissionDecisionMs) permissionDecisionMs[f.toolUseId] = Number(f.permissionDecisionMs);
    } else if (line.includes('Skipping auto mode classifier')) {
      // Claude Code's own fast path (e.g. "would be allowed in acceptEdits mode"): no classifier ran.
      skipped.push(line.replace(/^.*Skipping auto mode classifier /, '').slice(0, 200));
    } else if (/\[ERROR\]/.test(line)) {
      errors.push(line.replace(/^\S+\s+/, '').slice(0, 300));
    }
  }
  return { classifierCalls: [...calls.values()], serverClassifier, permissionDecisionMs, errors: errors.slice(0, 20), skipped };
}

/** The assistant text of an Anthropic-format reply, JSON or SSE. */
export function replyText(body: string): string {
  const t = body.trimStart();
  if (!t.startsWith('{')) return textOf(assembleStream(t).message);
  try {
    return textOf(JSON.parse(t) as Message);
  } catch {
    return '';
  }
}

export type ClassifierCall = { via: string; model?: string; stage: 's1' | 's2' | '?'; severity?: number; block?: boolean; category?: string; reason?: string; status?: number; ms?: number; text: string };

/** One wire log record (lib/hop.ts, lib/fake.ts): a request through the hop or the dispatcher, and what came of it. */
export type WireRec = { session?: string; via?: string; kind?: string; method?: string; path?: string; model?: string; status?: number; ms?: number; request?: Record<string, any> | null; response?: string };

const messagesText = (req: Record<string, any> | null | undefined): string =>
  ((req?.messages ?? []) as Array<{ content?: unknown }>).map((m) => (typeof m.content === 'string' ? m.content : resultText(m.content))).join('\n');

/** Whether a wire record is one of Claude Code's own local classifier calls (its user content holds `<transcript>`). */
export const isClassifierRequest = (r: WireRec): boolean => (r.kind === 'claude' || r.kind === 'cpa') && messagesText(r.request).includes('<transcript>');

/** The classifier calls among a case's wire records: stage from the envelope, verdict from the reply text. */
export function classifierCalls(recs: Array<WireRec>): Array<ClassifierCall> {
  return recs.filter(isClassifierRequest).map((r) => {
    const req = r.request ?? {};
    const stops: Array<string> = Array.isArray(req.stop_sequences) ? req.stop_sequences : [];
    const stage = req.max_tokens === 64 || stops.some((s) => /<\/(severity|block)>/.test(s)) ? 's1' : req.max_tokens ? 's2' : '?';
    const text = replyText(r.response ?? '');
    const sev = /<severity>\s*(\d+)/.exec(text)?.[1];
    const blk = /<block>\s*(yes|no)/i.exec(text)?.[1];
    return {
      via: r.via ?? '?',
      model: r.model,
      stage,
      severity: sev === undefined ? undefined : Number(sev),
      block: blk === undefined ? undefined : blk.toLowerCase() === 'yes',
      category: /<category>([\s\S]*?)<\/category>/.exec(text)?.[1]?.trim(),
      reason: /<reason>([\s\S]*?)<\/reason>/.exec(text)?.[1]?.trim().slice(0, 600),
      status: r.status,
      ms: r.ms,
      text: text.slice(0, 2000),
    };
  });
}

/** The classifier transcript text of a wire record (for the history check). */
export const classifierInputText = (r: WireRec): string => messagesText(r.request);

/** A distinctive string of a tool call, as the classifier projection would carry it: Bash's command, else the longest string leaf. */
export function signature(call: ToolCall): string {
  if (typeof call.input.command === 'string') return call.input.command;
  let best = '';
  const walk = (v: unknown): void => {
    if (typeof v === 'string') {
      if (v.length > best.length) best = v;
    } else if (Array.isArray(v)) v.forEach(walk);
    else if (v && typeof v === 'object') Object.values(v).forEach(walk);
  };
  walk(call.input);
  return best;
}

/** The needle for `s` inside a JSON-encoded haystack: its JSON escape, first 60 chars, cut at a line break. */
const needle = (s: string): string => JSON.stringify(s.trim()).slice(1, -1).slice(0, 60);

export type Coverage = { total: number; present: number; missing: Array<string> };

/** How many of the case's history entries appear in `haystack` (a classifier input, JSON-encoded strings). */
export function historyCoverage(c: Case, haystack: string): Coverage {
  const items = c.events.map((e) => ('user' in e ? e.user : signature(e))).filter((s) => s.trim().length > 0);
  const missing = items.filter((s) => !haystack.includes(needle(s)));
  return { total: items.length, present: items.length - missing.length, missing: missing.map((s) => s.slice(0, 80)) };
}
