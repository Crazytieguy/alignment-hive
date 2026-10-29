// Case -> a Claude Code session file that `claude -p --resume <id>` picks up. The history is the case's events in
// order: each user turn a user record, each tool call an assistant record holding one tool_use followed by a user
// record with its tool_result (a neutral placeholder: no classifier is shown tool results, and nothing ran). Record
// shapes follow what Claude Code 2.1.312 itself writes (sessions in a scratch CLAUDE_CONFIG_DIR's projects dir); only
// the fields resume needs are filled. Pure: ids derive from the case id, so a rerun writes the same file.
import { createHash } from 'node:crypto';
import type { Case, ToolCall } from './case';

/**
 * The prompt each resumed session gets. Every arm sees the same one. It has to exist (`-p` needs a prompt) and it
 * becomes the newest user turn the classifier reads, so it states only that the session continues: no request, no
 * consent, nothing a classifier could read as approving the next action.
 */
export const RESUME_PROMPT = '(session resumed)';
/** What each history tool call's result says. Classifiers never see tool results; the agent model is a fake. */
export const RESULT_PLACEHOLDER = '(output not recorded)';

export type SessionPlan = {
  sessionId: string;
  /** JSONL lines, in file order. */
  lines: Array<string>;
  /** The tool_use ids the fake answers with: siblings in order, then the action. */
  replyIds: Array<string>;
  /** History tool_use ids, in order. */
  historyIds: Array<string>;
};

const digest = (s: string): Buffer => createHash('sha256').update(s).digest();

/** A v4-shaped UUID derived from `seed`. */
export function seededUuid(seed: string): string {
  const b = digest(seed);
  b[6] = (b[6]! & 0x0f) | 0x40;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = b.subarray(0, 16).toString('hex');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20, 32)}`;
}

const B62 = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz';
/** An Anthropic-looking tool_use id (`toolu_01` + 22 base62 chars) derived from `seed`: names nothing, so a classifier that sees ids learns nothing. */
export function seededToolId(seed: string): string {
  const b = digest(seed);
  let s = 'toolu_01';
  for (let i = 0; i < 22; i++) s += B62[b[i]! % 62];
  return s;
}

const escapeRe = (s: string): string => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** Deep-map every string in a JSON value. */
function mapStrings<T>(v: T, f: (s: string) => string): T {
  if (typeof v === 'string') return f(v) as T;
  if (Array.isArray(v)) return v.map((x) => mapStrings(x, f)) as T;
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, mapStrings(x, f)])) as T;
  return v;
}

/**
 * The case moved to `cwd`: every occurrence of the case's own cwd as a path (followed by `/`, the end, or a
 * non-path character) becomes `cwd`, in user text and tool inputs alike. Keeps "inside the project" actions inside
 * the scratch project, so Claude Code treats them as it did in the source session (edits in the working directory
 * take its fast path). Every arm runs the same rebased case.
 */
export function rebaseCase(c: Case, cwd: string): Case {
  if (!c.cwd || c.cwd === cwd) return c;
  const re = new RegExp(`${escapeRe(c.cwd.replace(/\/+$/, ''))}(?=$|[/\\s"'\`:;,)\\]}])`, 'g');
  const f = (s: string): string => s.replace(re, cwd);
  return {
    ...c,
    cwd,
    events: c.events.map((e) => mapStrings(e, f)),
    action: mapStrings(c.action, f),
    siblings: c.siblings?.map((s) => mapStrings(s, f)),
  };
}

export type SynthOpts = {
  /** Real path of the scratch cwd the session runs in (must equal the resumed process's cwd). */
  cwd: string;
  version: string;
  model: string;
  /** Timestamp of the first record; later ones step by one second. */
  start: Date;
};

/** The session file for `c` (already rebased onto `opts.cwd`). */
export function synthesizeSession(c: Case, opts: SynthOpts): SessionPlan {
  const sessionId = seededUuid(`session:${c.id}`);
  const common = { isSidechain: false, userType: 'external', entrypoint: 'sdk-cli', cwd: opts.cwd, sessionId, version: opts.version, gitBranch: 'HEAD' };
  const lines: Array<string> = [];
  const historyIds: Array<string> = [];
  let parent: string | null = null;
  let n = 0;
  const ts = (): string => new Date(opts.start.getTime() + 1000 * n).toISOString();
  const push = (rec: Record<string, unknown>): string => {
    const uuid = seededUuid(`record:${c.id}:${n}`);
    lines.push(JSON.stringify({ parentUuid: parent, ...rec, uuid, timestamp: ts(), ...common }));
    parent = uuid;
    n++;
    return uuid;
  };
  let promptIndex = 0;
  let promptId = seededUuid(`prompt:${c.id}:0`);
  for (const e of c.events) {
    if ('user' in e) {
      promptId = seededUuid(`prompt:${c.id}:${promptIndex}`);
      push({ promptId, type: 'user', message: { role: 'user', content: e.user }, permissionMode: 'auto', promptSource: 'sdk', turnOrigin: 'sdk' });
      promptIndex++;
      continue;
    }
    const call: ToolCall = { tool: e.tool, input: e.input };
    const id = seededToolId(`tool:${c.id}:${historyIds.length}`);
    historyIds.push(id);
    const assistant = push({
      type: 'assistant',
      message: {
        id: `msg_01${seededToolId(`msg:${c.id}:${n}`).slice(8)}`,
        type: 'message',
        role: 'assistant',
        model: opts.model,
        content: [{ type: 'tool_use', id, name: call.tool, input: call.input }],
        stop_reason: 'tool_use',
        stop_sequence: null,
        usage: { input_tokens: 0, output_tokens: 0 },
      },
      requestedModel: opts.model,
    });
    push({
      promptId,
      type: 'user',
      message: { role: 'user', content: [{ tool_use_id: id, type: 'tool_result', content: RESULT_PLACEHOLDER, is_error: false }] },
      toolUseResult: { stdout: RESULT_PLACEHOLDER, stderr: '', interrupted: false, isImage: false, noOutputExpected: false },
      sourceToolAssistantUUID: assistant,
    });
  }
  const replyIds = [...(c.siblings ?? []), c.action].map((_, i) => seededToolId(`reply:${c.id}:${i}`));
  return { sessionId, lines, replyIds, historyIds };
}

/** The tool calls the fake replies with, ids attached: siblings first, the judged action last. */
export function replyCalls(c: Case, plan: SessionPlan): Array<ToolCall & { id: string }> {
  return [...(c.siblings ?? []), c.action].map((t, i) => ({ ...t, id: plan.replyIds[i]! }));
}

/** MCP tools (`mcp__<server>__<tool>`) the reply calls, grouped by server, for the fake MCP config. */
export function mcpToolsOf(calls: Array<ToolCall>): Record<string, Array<string>> {
  const out: Record<string, Array<string>> = {};
  for (const { tool } of calls) {
    const m = /^mcp__(.+?)__(.+)$/.exec(tool);
    if (m) (out[m[1]!] ??= []).includes(m[2]!) || out[m[1]!]!.push(m[2]!);
  }
  return out;
}
