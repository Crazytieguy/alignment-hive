import { mkdir, mkdtemp, rm, utimes, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { SessionMetaSchema, parseTranscript, sessionSummary } from '@alignment-hive/session-data';
import { buildSessionMeta } from '../lib/session-format';
import { statePaths } from '../lib/config';
import { hive } from '../lib/messages';
import {
  discoverWorkflowRuns,
  readAndSanitizeSession,
  readParseableRunBlobs,
  readSessionSummary,
  summarizeSessions,
  uploadOneSession,
} from '../lib/upload-session';
import type { DiscoveredSession } from '../lib/session-state';
import type { SummaryCache } from '../lib/upload-session';

const SID = 'parent-session-xyz';
const HOME = homedir();

let root: string;
let parent: DiscoveredSession;

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'hive-wfruns-'));
  const workflowsDir = join(root, SID, 'workflows');
  await mkdir(workflowsDir, { recursive: true });

  // A full run with a home-path leak in script/scriptPath and a valid-zero scalar.
  await writeFile(
    join(workflowsDir, 'wf_run1.json'),
    JSON.stringify({
      runId: 'wf_run1',
      workflowName: 'review-changes',
      summary: 'Reviewed the diff',
      status: 'completed',
      totalTokens: 0, // valid zero — must be preserved
      totalToolCalls: 7,
      agentCount: 3,
      durationMs: 1200,
      scriptPath: `${HOME}/projects/app/.claude/scripts/wf_run1.js`,
      script: `// generated at ${HOME}/projects/app\nconsole.log("hi")`,
      result: {
        findings: [`see ${HOME}/projects/app/file.ts`],
        sibling: `${HOME}extra/leak.ts`, // HOME is a prefix but not a path boundary — must stay intact
        pathMap: { [`${HOME}/secrets`]: 1 }, // a home path as an object KEY
      },
    }),
  );

  // A minimal run (only runId).
  await writeFile(join(workflowsDir, 'wf_run2.json'), JSON.stringify({ runId: 'wf_run2' }));

  // Over-long indexed scalars — must be capped so saveWorkflowRuns can never be failed by them.
  await writeFile(
    join(workflowsDir, 'wf_run3.json'),
    JSON.stringify({
      runId: 'wf_run3',
      workflowName: 'n'.repeat(600),
      status: 's'.repeat(600),
      summary: 'x'.repeat(2001),
    }),
  );

  // Non-run files / dirs that must be ignored.
  await writeFile(join(workflowsDir, 'notes.json'), JSON.stringify({ runId: 'nope' }));
  await mkdir(join(workflowsDir, 'scripts'), { recursive: true });
  await writeFile(join(workflowsDir, 'scripts', 'wf_run1.js'), 'export const meta = {}');

  // Malformed JSON — must never count as a parseable run (backfill loop-safety).
  await writeFile(join(workflowsDir, 'wf_broken.json'), '{ truncated');

  parent = { sessionId: SID, path: join(root, `${SID}.jsonl`), mtime: new Date() };
});

afterAll(async () => {
  await rm(root, { recursive: true, force: true });
});

describe('discoverWorkflowRuns', () => {
  test('discovers wf_*.json runs, ignores other files + the scripts/ dir', async () => {
    const runs = await discoverWorkflowRuns(parent, new Set());
    const byId = new Map(runs.map((r) => [r.row.workflowRunId, r]));

    expect(runs.length).toBe(3);
    expect(byId.has('wf_run1')).toBe(true);
    expect(byId.has('wf_run2')).toBe(true);
    expect(byId.has('wf_run3')).toBe(true);
    expect(byId.has('notes')).toBe(false); // notes.json not matched (no wf_ prefix)
    expect(byId.has('wf_broken')).toBe(false); // malformed JSON gated out
  });

  test('over-long indexed scalars are capped (workflowName, status, summary)', async () => {
    const runs = await discoverWorkflowRuns(parent, new Set());
    const run3 = runs.find((r) => r.row.workflowRunId === 'wf_run3')!.row;
    expect(run3.workflowName!.length).toBe(501); // 500 + ellipsis
    expect(run3.workflowName!.endsWith('…')).toBe(true);
    expect(run3.status!.length).toBe(501);
    expect(run3.summary!.length).toBe(2001);
    // The full values remain in the blob.
    const blob = runs.find((r) => r.row.workflowRunId === 'wf_run3')!.blob as Record<string, unknown>;
    expect(String(blob.workflowName).length).toBe(600);
  });

  test('row scalars are extracted, including totalTokens: 0', async () => {
    const runs = await discoverWorkflowRuns(parent, new Set());
    const run1 = runs.find((r) => r.row.workflowRunId === 'wf_run1')!.row;
    expect(run1).toMatchObject({
      workflowRunId: 'wf_run1',
      runId: 'wf_run1',
      workflowName: 'review-changes',
      summary: 'Reviewed the diff',
      status: 'completed',
      totalTokens: 0,
      totalToolCalls: 7,
      agentCount: 3,
      durationMs: 1200,
    });
  });

  test('home paths are redacted to ~ across keys + values, boundary-aware', async () => {
    const runs = await discoverWorkflowRuns(parent, new Set());
    const blob = runs.find((r) => r.row.workflowRunId === 'wf_run1')!.blob as Record<string, unknown>;

    expect(blob.scriptPath).toBe('~/projects/app/.claude/scripts/wf_run1.js');
    expect(String(blob.script)).toContain('~/projects/app');

    const result = blob.result as Record<string, unknown>;
    expect(JSON.stringify(result.findings)).toContain('~/projects/app/file.ts');
    // Object KEYS are redacted, not just values.
    expect(result.pathMap).toEqual({ '~/secrets': 1 });
    // Boundary: a sibling path that merely has HOME as a (non-boundary) prefix is left intact.
    expect(result.sibling).toBe(`${HOME}extra/leak.ts`);

    // No exact home-dir path (HOME + '/') survives anywhere in the blob.
    expect(JSON.stringify(blob)).not.toContain(`${HOME}/`);
  });
});

describe('readParseableRunBlobs', () => {
  test('returns parseable run ids only — malformed files are gated out', async () => {
    const ids = [...(await readParseableRunBlobs(parent, new Set())).keys()];
    expect(ids.sort()).toEqual(['wf_run1', 'wf_run2', 'wf_run3']);
  });
});

describe('readAndSanitizeSession', () => {
  test('upload records keep titles, queued messages and chain stubs, redact secrets, and number like the local file', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'hive-fields-'));
    try {
      const token = 'ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs';
      const path = join(dir, 's.jsonl');
      await writeFile(
        path,
        [
          {
            type: 'user',
            uuid: 'u',
            parentUuid: null,
            timestamp: 't',
            message: { role: 'user', content: 'report' },
            toolUseResult: { status: 'completed', agentId: 'a', description: 'review', prompt: `inspect ${token}` },
          },
          {
            type: 'system',
            subtype: 'compact_boundary',
            compactMetadata: { trigger: 'auto', preTokens: 42, messagesSummarized: 3 },
            isCompactSummary: false,
          },
          { type: 'fork-context-ref', parentSessionId: 'parent', parentLastUuid: 'last', context: token },
          { type: 'continued-in', sessionId: 'next', context: token },
          { type: 'custom-title', customTitle: 'kept since titles name sessions' },
          { type: 'attachment', uuid: 'q', attachment: { type: 'queued_command', prompt: `also ${token}`, cwd: '/x' } },
          { type: 'attachment', uuid: 'h', attachment: { type: 'hook_success', stdout: 'local only' } },
        ]
          .map((entry) => JSON.stringify(entry))
          .join('\n'),
      );
      const { sanitizedEntries, lineCount } = await readAndSanitizeSession({ sessionId: 's', path }, new Set());
      const meta = buildSessionMeta({
        sessionId: 's',
        checkoutId: 'c',
        rawMtime: 't',
        messageCount: sanitizedEntries.length,
      });
      expect(SessionMetaSchema.safeParse(meta).success).toBe(true);
      expect(sanitizedEntries.map((entry) => entry.type)).toEqual([
        'user',
        'system',
        'fork-context-ref',
        'continued-in',
        'custom-title',
        'attachment',
        'attachment',
      ]);
      expect(sanitizedEntries[0]).toMatchObject({
        toolUseResult: { status: 'completed', agentId: 'a', description: 'review' },
      });
      expect(sanitizedEntries[5]).toEqual({
        type: 'attachment',
        uuid: 'q',
        attachment: { type: 'queued_command', prompt: expect.stringContaining('also [REDACTED:') },
      });
      expect(sanitizedEntries[6]).toEqual({ type: 'attachment', uuid: 'h' });
      // The web's Lines column: every record but the title and the hook's id-only stub.
      expect(lineCount).toBe(5);
      const uploaded = parseTranscript(sanitizedEntries.map((entry) => JSON.stringify(entry)).join('\n'));
      const local = parseTranscript(await Bun.file(path).text());
      expect(uploaded.entries.map((e) => [e.n, e.kind, e.uuid])).toEqual(
        local.entries.map((e) => [e.n, e.kind, e.uuid]),
      );
      const text = JSON.stringify(sanitizedEntries);
      expect(text).not.toContain(token);
      expect(text).toContain('[REDACTED:');
      expect(JSON.stringify(sanitizedEntries[0])).toContain('inspect [REDACTED:');
      for (const entry of sanitizedEntries.slice(2, 4)) expect(JSON.stringify(entry)).toContain('[REDACTED:');
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
  test('redacts entries and summary and collects cwds', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'hive-read-'));
    const path = join(dir, 's.jsonl');
    const TOKEN = 'ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs';
    await writeFile(
      path,
      [
        JSON.stringify({ type: 'summary', summary: `found ${TOKEN} here`, leafUuid: 'u1' }),
        JSON.stringify({
          type: 'user',
          uuid: 'u1',
          parentUuid: null,
          timestamp: 't',
          cwd: '/proj',
          message: { role: 'user', content: `here ${TOKEN}` },
        }),
        JSON.stringify({
          type: 'assistant',
          uuid: 'a1',
          parentUuid: 'u1',
          timestamp: 't',
          message: { role: 'assistant', content: 'ok' },
        }),
      ].join('\n'),
    );
    const r = await readAndSanitizeSession({ sessionId: 's', path }, new Set());
    const text = JSON.stringify(r.sanitizedEntries) + r.summary;
    expect(text).not.toContain('ghp_');
    expect(text).toContain('[REDACTED:');
    expect(r.cwds).toEqual(new Set(['/proj']));
    await rm(dir, { recursive: true, force: true });
  });
});

describe('uploadOneSession', () => {
  test('a local sharing-disabled marker stops the upload before anything is read or sent', async () => {
    const stateDir = await mkdtemp(join(tmpdir(), 'hive-optout-'));
    await writeFile(statePaths(stateDir).sharingDisabled, '');
    const result = await uploadOneSession({
      session: { sessionId: 's1', path: join(stateDir, 'missing.jsonl'), mtime: new Date(0) },
      state: { agentsByParent: new Map() },
      statusCtx: {
        uploadedMap: new Map(),
        excludedSet: new Set(),
        consentMtime: 0,
        snoozeUntil: null,
        consentWindows: { global: [{ start: 0, end: Infinity }], project: [{ start: 0, end: Infinity }] },
      },
      transcriptsDirs: [],
      checkoutId: 'c',
      ids: { directory: '/proj' },
      stateDir,
    });
    expect(result).toEqual({ ok: false, error: hive.upload.noProjectConsent });
    await rm(stateDir, { recursive: true, force: true });
  });
});

describe('readSessionSummary', () => {
  const user = (content: string, extra: object = {}) =>
    JSON.stringify({ type: 'user', uuid: content, timestamp: 't', message: { role: 'user', content }, ...extra });
  const files: Record<string, Array<string>> = {
    head: [user('<command-name>/clear</command-name>'), user('first line\nsecond'), user('later')],
    latestTitle: [
      JSON.stringify({ type: 'ai-title', aiTitle: 'early' }),
      user('hello'),
      JSON.stringify({ type: 'custom-title', customTitle: 'named' }),
    ],
    secret: [user(`deploy with ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs`)],
    none: [JSON.stringify({ type: 'system', subtype: 'x' })],
    long: [user('x'.repeat(5000))],
  };

  test('matches sessionSummary over the whole parsed file, sanitized', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'hive-summary-'));
    try {
      for (const [name, lines] of Object.entries(files)) {
        const path = join(dir, `${name}.jsonl`);
        const text = lines.join('\n') + '\n';
        await writeFile(path, text);
        const whole = sessionSummary(parseTranscript(text)) ?? '';
        const got = await readSessionSummary({ sessionId: name, path }, new Set());
        if (name === 'secret') {
          expect(got).toStartWith('deploy with [REDACTED:');
        } else {
          expect(got).toBe(whole);
        }
      }
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });

  test('summarizeSessions with a cache rereads a file only when it changes', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'hive-summary-cache-'));
    try {
      const path = join(dir, 's.jsonl');
      await writeFile(path, user('before') + '\n');
      const session: DiscoveredSession = { sessionId: 's', path, mtime: new Date() };
      const state = {
        parentSessions: [session],
        uploadedMap: new Map(),
        excludedSet: new Set<string>(),
        startedMap: new Map(),
      };
      const ctx = { ...state, consentMtime: 0, snoozeUntil: null };
      const cache: SummaryCache = new Map();
      expect((await summarizeSessions(state, ctx, cache))[0].summary).toBe('before');
      cache.set(path, { ...cache.get(path)!, summary: 'cached' });
      expect((await summarizeSessions(state, ctx, cache))[0].summary).toBe('cached');
      await writeFile(path, user('after') + '\n');
      const later = new Date(Date.now() + 60_000);
      await utimes(path, later, later);
      expect((await summarizeSessions(state, ctx, cache))[0].summary).toBe('after');
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  });
});
