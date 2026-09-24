import { mkdir, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterEach, expect, test } from 'bun:test';
import { EvidenceResolver, stampedAt } from './evidence';
import { FIXTURE_SESSION, createReviewFixture, prompt, reply, reviewDocument, reviewItem, syntheticSecret, transcriptTime, writeTranscript, writes } from './fixtures';
import { resolveReviewContext } from './git';
import { parseReview, refSchema } from './parse';
import { modelLabel, toolLabel } from './transcript';
import type { ResolvedEvidence } from './evidence';
import type { z } from 'zod';

const fixtures: Array<Awaited<ReturnType<typeof createReviewFixture>>> = [];
afterEach(async () => { await Promise.all(fixtures.splice(0).map((f) => f.cleanup())); });
async function setup(head?: string) {
  const f = await createReviewFixture(); fixtures.push(f); await f.stamp();
  const transcripts = join(f.root, 'transcripts'); await mkdir(transcripts);
  const context = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo, head });
  const resolver = new EvidenceResolver(context, f.root, f.projects);
  const resolve = (ref: z.input<typeof refSchema>) => resolver.resolve({ ref: refSchema.parse(ref), line: 10, bodyLine: 3 }, 'evidence');
  return { ...f, resolver, resolve, transcripts };
}

test('diffs reflect disk, additions/deletions and validate exact focus/labels', async () => {
  const f = await setup();
  await f.write('source.ts', 'export const value = 2;\n');
  const diff = await f.resolve({ diff: 'source.ts', focus: 'L1', 'old-focus': 'L1', labels: [{ line: 1, where: 'CLI prompt' }] });
  expect(diff.kind).toBe('diff');
  if (diff.kind !== 'diff') return;
  expect(diff.file.new).toBe('export const value = 2;\n');
  expect(diff.file.focus).toEqual([1, 1]);
  await f.write('source.ts', 'a\nb\nc\n');
  expect(await f.resolve({ diff: 'source.ts', focus: 'L1, L3', title: 'two places' })).toMatchObject({ file: { focus: [[1, 1], [3, 3]], title: 'two places' } });
  await f.write('source.ts', 'export const value = 2;\n');
  expect(diff.newHash).not.toBe(diff.oldHash);
  await f.write('new.ts', 'new\n');
  expect(await f.resolve({ diff: 'new.ts', labels: [] })).toMatchObject({ oldHash: null, file: { status: 'added' } });
  await unlink(join(f.repo, 'source.ts'));
  expect(await f.resolve({ diff: 'source.ts', labels: [] })).toMatchObject({ newHash: null, file: { status: 'deleted' } });
  await expect(f.resolve({ diff: 'source.ts', labels: [], focus: 'L1' })).rejects.toThrow('evidence, line 10');
  await expect(f.resolve({ diff: 'missing', labels: [] })).rejects.toThrow('Evidence not found');
  await expect(f.resolve({ diff: 'new.ts', labels: [{ line: 2, where: 'outside' }] })).rejects.toThrow('exceeds 1');
});

test('historical mode reads diffs and head files from the authored head; external files always from disk', async () => {
  const live = await setup();
  await live.write('source.ts', 'one\ntwo\n'); await live.git('commit', '-qam', 'Archive');
  const head = await live.git('rev-parse', 'HEAD');
  await live.write('source.ts', 'one\ntwo\nthree on disk\n');
  const external = join(live.root, 'notes.md'); await writeFile(external, 'external live\n');
  const makeResolver = async (headRevision?: string) => {
    const context = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: live.repo, head: headRevision });
    const resolver = new EvidenceResolver(context, live.root, live.projects);
    return (ref: z.input<typeof refSchema>) => resolver.resolve({ ref: refSchema.parse(ref), line: 10, bodyLine: 3 }, 'evidence');
  };
  const historical = await makeResolver(head), current = await makeResolver();
  expect(await historical({ diff: 'source.ts' })).toMatchObject({ file: { old: 'export const value = 1;\n', new: 'one\ntwo\n' } });
  expect(await current({ diff: 'source.ts' })).toMatchObject({ file: { new: 'one\ntwo\nthree on disk\n' } });
  await expect(historical({ diff: 'source.ts', focus: 'L3' })).rejects.toThrow('exceeds 2');
  expect(await historical({ file: 'source.ts' })).toMatchObject({ text: 'one\ntwo\n', revision: head, endLine: 2 });
  expect(await current({ file: 'source.ts', range: 'L3' })).toMatchObject({ text: 'three on disk', revision: 'disk' });
  for (const resolve of [historical, current]) expect(await resolve({ file: external })).toMatchObject({ text: 'external live\n', revision: 'disk' });
  await expect(historical({ file: external, at: 'base' })).rejects.toThrow('repository-relative');
  await expect(historical({ file: join(live.repo, 'source.ts') })).rejects.toThrow('repository-relative');
  const git = await historical({ git: true });
  expect(git).toMatchObject({ kind: 'git', facts: { commits: [{ subject: 'Archive' }] } });
  if (git.kind === 'git') expect(git.facts.tree).toBeUndefined();
  const liveGit = await current({ git: true });
  if (liveGit.kind === 'git') expect(liveGit.facts.tree).toMatchObject({ status: { modified: ['source.ts'] }, upstream: null });
});

test('file ranges are revision-specific and image bytes are embedded', async () => {
  const f = await setup(); await f.write('source.ts', 'first\nsecond\n');
  expect(await f.resolve({ file: 'source.ts', at: 'head', range: 'L2' })).toMatchObject({ text: 'second', startLine: 2 });
  expect(await f.resolve({ file: 'source.ts', at: 'base' })).toMatchObject({ text: 'export const value = 1;\n', revision: f.base });
  await expect(f.resolve({ file: 'source.ts', at: 'base', range: 'L2' })).rejects.toThrow('exceeds 1');
  await writeFile(join(f.root, 'plot.svg'), '<svg xmlns="http://www.w3.org/2000/svg"></svg>');
  expect(await f.resolve({ image: 'plot.svg', summary: 'A plot' })).toMatchObject({ kind: 'image', dataUri: expect.stringContaining('data:image/svg+xml;base64,'), summary: 'A plot', takenAt: undefined });
  await writeFile(join(f.root, 'page-2026-08-31T08-25-32-092Z.png'), new Uint8Array([137, 80, 78, 71]));
  expect(await f.resolve({ image: 'page-2026-08-31T08-25-32-092Z.png', summary: 'A page', width: 390 })).toMatchObject({ takenAt: '2026-08-31T08:25:32.092Z', width: 390 });
});

test('a screenshot\'s time comes from a Playwright-style UTC stamp in its name, else none', () => {
  expect(['page-2026-08-31T08-25-32-092Z.png', 'shot-2026-08-31T09-01-02-142Z-hearts.png', '2026-08-31T09-01-02Z.png', 'plot.png', '/shots/2026-08-31T09-01-02-142Z/plot.png'].map(stampedAt))
    .toEqual(['2026-08-31T08:25:32.092Z', '2026-08-31T09:01:02.142Z', '2026-08-31T09:01:02.000Z', undefined, undefined]);
});

test('an image ref needs a one-line summary, reported at its line; width is a positive whole number', () => {
  const review = (ref: string) => () => parseReview(reviewDocument(reviewItem('shot', '```ref\n' + ref + '\n```')));
  expect(review('image: a.png')).toThrow('debrief: shot, line 16: summary:');
  expect(review('image: a.png\nsummary: A shot\nwidth: 0')).toThrow('width');
  expect(review('image: a.png\nsummary: A shot\nwidth: 390')).not.toThrow();
});

const AGENT_A = '1111111199a', AGENT_B = '1111111199b', UNRELATED = '11111111-2222-4222-8222-222222222222';
/**
 * A parent, two of its agents and an unrelated session with its own agent, all sharing id prefixes. Parent entries:
 * 1 prompt; 2 the Bash call and 3 the text of one message; 4 and 5 replies; 6 prompt.
 */
async function sessions(f: Awaited<ReturnType<typeof setup>>, missingResult = false) {
  const run = { type: 'assistant' as const, model: 'claude-fable-5-1', content: [{ type: 'tool_use', id: 'bash-1', name: 'Bash', input: { command: 'printf result' } }, { type: 'text', text: 'Independent assistant text' }] };
  await writeTranscript(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('Run the check verbatim'), run,
    ...(missingResult ? [] : [{ type: 'user' as const, content: [{ type: 'tool_result', tool_use_id: 'bash-1', content: 'result\n' }] }]),
    reply('Checked.', 'claude-opus-5-5'), reply('No model recorded.'), prompt('Second prompt'),
  ]);
  const agents = join(f.transcripts, FIXTURE_SESSION, 'subagents');
  await writeTranscript(join(agents, `agent-${AGENT_A}.jsonl`), FIXTURE_SESSION, [prompt('Review it'), reply('Reviewed.', 'gpt-5.6-sol'), reply('Unlabelled.')]);
  await writeTranscript(join(agents, `agent-${AGENT_B}.jsonl`), FIXTURE_SESSION, [prompt('Check it'), reply('Checked.', 'claude-haiku-4-5-20251001')]);
  await writeTranscript(join(f.transcripts, `${UNRELATED}.jsonl`), UNRELATED, [prompt('Elsewhere'), reply('Elsewhere.')]);
  await writeTranscript(join(f.transcripts, UNRELATED, 'subagents', 'agent-1111111199c.jsonl'), UNRELATED, [prompt('Elsewhere'), reply('Elsewhere.')]);
}
const labels = (evidence: ResolvedEvidence) => evidence.kind === 'transcript' ? evidence.entries.map(({ locator, entry, label, role, summary }) => ({ locator, n: entry.n, label, role, summary, time: entry.time, model: 'model' in entry ? entry.model : undefined })) : [];

test('model labels drop dates and format known families', () => {
  expect(['claude-fable-5', 'claude-fable-5-1', 'claude-opus-5-5', 'claude-haiku-4-5-20251001', 'gpt-5.6-sol', 'gpt-6-astra', 'custom-model-x', 'claude-next'].map(modelLabel))
    .toEqual(['FABLE 5', 'FABLE 5.1', 'OPUS 5.5', 'HAIKU 4.5', 'GPT-5.6 SOL', 'GPT-6 ASTRA', 'CUSTOM-MODEL-X', 'CLAUDE-NEXT']);
});

test('tool labels: an MCP tool by its own name, the first two words that fit in 11 characters, the full name as the title', () => {
  expect(['mcp__claude_ai_Claude_Docs__batch', 'AskUserQuestion', 'Bash', 'TodoWrite', 'NotebookEdit', 'mcp__slack__slack_send_message'].map(toolLabel)).toEqual([
    { label: 'BATCH', title: 'mcp__claude_ai_Claude_Docs__batch' }, { label: 'ASK USER', title: 'AskUserQuestion' }, { label: 'BASH' },
    { label: 'TODO WRITE', title: 'TodoWrite' }, { label: 'NOTEBOOK', title: 'NotebookEdit' }, { label: 'SLACK SEND', title: 'mcp__slack__slack_send_message' },
  ]);
});

test('a model label longer than 11 characters is cut, with the model as the title', async () => {
  const f = await setup();
  await writeTranscript(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('Go'), reply('Done.', 'claude-3-5-sonnet-20241022')]);
  const result = await f.resolve({ transcript: `${FIXTURE_SESSION}:2`, summary: 'the reply' });
  if (result.kind !== 'transcript') throw new Error('expected a transcript');
  expect([result.entries[0].label, result.entries[0].title]).toEqual(['CLAUDE-3-5-', 'claude-3-5-sonnet-20241022']);
});

test('clip and results are transcript options', () => {
  expect(refSchema.parse({ transcript: 'a:1', summary: 'x', clip: false, results: false })).toMatchObject({ clip: false, results: false });
  expect(() => refSchema.parse({ transcript: 'a:1', summary: 'x', clip: true })).toThrow();
  expect(() => refSchema.parse({ transcript: 'source.ts', summary: 'x' })).toThrow('or capture:<name>');
});

test('transcript locators keep author order, labels, models and record timestamps', async () => {
  const f = await setup(); await sessions(f);
  const result = await f.resolve({ transcript: '11111111-1:4, 11111111-1:1, 11111111-1:2, 11111111-1:5, 11111111-1:3', summary: ['the reply', 'the ask', 'the run', 'no model', 'the text'] });
  expect(result.selector).toBe([4, 1, 2, 5, 3].map((n) => `${FIXTURE_SESSION}:${n}`).join(','));
  expect(labels(result)).toEqual([
    { locator: `${FIXTURE_SESSION}:4`, n: 4, label: 'OPUS 5.5', role: 'asst', summary: 'the reply', time: transcriptTime(3), model: 'claude-opus-5-5' },
    { locator: `${FIXTURE_SESSION}:1`, n: 1, label: 'YOU', role: 'user', summary: 'the ask', time: transcriptTime(0), model: undefined },
    { locator: `${FIXTURE_SESSION}:2`, n: 2, label: 'BASH', role: 'tool', summary: 'the run', time: transcriptTime(1), model: 'claude-fable-5-1' },
    { locator: `${FIXTURE_SESSION}:5`, n: 5, label: 'CLAUDE', role: 'asst', summary: 'no model', time: transcriptTime(4), model: undefined },
    { locator: `${FIXTURE_SESSION}:3`, n: 3, label: 'FABLE 5.1', role: 'asst', summary: 'the text', time: transcriptTime(1), model: 'claude-fable-5-1' },
  ]);
  if (result.kind !== 'transcript') throw new Error('expected a transcript');
  // The call carries its own result; the message's text is the next entry.
  expect(result.entries[2].entry).toMatchObject({ kind: 'tool', tool: 'Bash', input: { command: 'printf result' }, result: 'result\n' });
  expect(result.entries[4].entry).toMatchObject({ kind: 'assistant', text: 'Independent assistant text' });
  const agents = await f.resolve({ transcript: `1111111199a:1, agent-1111111199a:2, 1111111199a:3, agent-1111111199b:2`, summary: ['a', 'b', 'c', 'd'] });
  expect(labels(agents).map(({ locator, label, role }) => [locator, label, role])).toEqual([
    [`${FIXTURE_SESSION}/agent-${AGENT_A}:1`, 'PROMPT', 'user'], [`${FIXTURE_SESSION}/agent-${AGENT_A}:2`, 'GPT-5.6 SOL', 'agent'],
    [`${FIXTURE_SESSION}/agent-${AGENT_A}:3`, 'AGENT', 'agent'], [`${FIXTURE_SESSION}/agent-${AGENT_B}:2`, 'HAIKU 4.5', 'agent'],
  ]);
  const one = await f.resolve({ transcript: `${FIXTURE_SESSION}:1`, summary: 'the ask' });
  const edited = await f.resolve({ transcript: `${FIXTURE_SESSION}:1`, summary: 'the ask, reworded' });
  expect(one.newHash).not.toBe(edited.newHash);
  expect((await f.resolve({ transcript: `${FIXTURE_SESSION}:1`, summary: 'the ask' })).newHash).toBe(one.newHash);
});

test('locators resolve through hive local\'s resolver, in any form it prints; a transcript of another session is refused', async () => {
  const f = await setup(); await sessions(f);
  await expect(f.resolve({ transcript: '11111111:1', summary: 'x' })).rejects.toThrow('"11111111" matches 2 transcripts');
  await expect(f.resolve({ transcript: '111111119:1', summary: 'x' })).rejects.toThrow('"111111119" matches 3 transcripts');
  await expect(f.resolve({ transcript: '11111111-2222:1', summary: 'x' })).rejects.toThrow('11111111-2222:1 is not in this debrief\'s session or one of its agents');
  await expect(f.resolve({ transcript: '1111111199c:1', summary: 'x' })).rejects.toThrow('1111111199c:1 is not in this debrief\'s session');
  await expect(f.resolve({ transcript: 'agent-11111111-:1', summary: 'x' })).rejects.toThrow('no agent matches "agent-11111111-"');
  await expect(f.resolve({ transcript: '11111111-1:9', summary: 'x' })).rejects.toThrow(`Transcript entry not found: ${FIXTURE_SESSION}:9`);
  for (const form of [`${FIXTURE_SESSION}/agent-${AGENT_A}:2`, `11111111-1/agent-1111111199a:2`, `agent-${AGENT_A}:2`, `${AGENT_A}:2`]) {
    expect(await f.resolve({ transcript: form, summary: 'x' })).toMatchObject({ selector: `${FIXTURE_SESSION}/agent-${AGENT_A}:2` });
  }
  await expect(f.resolve({ transcript: `${FIXTURE_SESSION}:1`, summary: 'x' })).resolves.toMatchObject({ selector: `${FIXTURE_SESSION}:1` });
});

test('a user entry the human did not type says what it is, from the shared classification', async () => {
  const f = await setup();
  await writeTranscript(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('Typed by the user'),
    prompt('<task-notification>\n<task-id>b1</task-id>\n</task-notification>'),
    prompt('<agent-message from="reviewer">Done.</agent-message>'),
    { ...prompt('Base directory for this skill: /skills/review'), record: { isMeta: true } },
    prompt('[Request interrupted by user]'),
    prompt('<command-name>/model</command-name>\n<command-args></command-args>'),
  ]);
  const result = await f.resolve({ transcript: [1, 2, 3, 4, 5, 6].map((n) => `${FIXTURE_SESSION}:${n}`).join(', '), summary: ['a', 'b', 'c', 'd', 'e', 'f'] });
  expect(labels(result).map(({ label, role }) => [label, role])).toEqual([['YOU', 'user'], ['NOTICE', 'tool'], ['PEER', 'agent'], ['CLAUDE CODE', 'tool'], ['INTERRUPT', 'user'], ['COMMAND', 'tool']]);
});

test('a transcript ref mixes locators and captures; a capture is a Bash call like the session\'s own', async () => {
  const f = await setup(); await sessions(f);
  await mkdir(join(f.root, 'captures'));
  await writeFile(join(f.root, 'captures', 'log.json'), JSON.stringify({ command: ['git', 'log'], cwd: f.repo, exit: 0, stdout: 'abc\n', stderr: '', startedAt: '2026-09-05T10:00:00.000Z' }));
  const mixed = await f.resolve({ transcript: `${FIXTURE_SESSION}:2, capture:log`, summary: ['the run', 'the log'] });
  expect(labels(mixed)).toMatchObject([{ label: 'BASH', role: 'tool', summary: 'the run' }, { locator: 'capture:log', label: 'BASH', role: 'tool', summary: 'the log' }]);
  if (mixed.kind !== 'transcript') throw new Error('expected a transcript');
  expect(mixed.entries[1].entry).toMatchObject({ kind: 'tool', tool: 'Bash', input: { command: 'git log' }, result: 'abc' });
  await expect(f.resolve({ transcript: 'capture:missing', summary: 'x' })).rejects.toThrow();
});

test('key asks are the user\'s own messages in this debrief\'s session, verbatim and in transcript order; others fail on their page line', async () => {
  const f = await setup(); await sessions(f);
  const header = (asks: Array<string>) => f.resolver.header(parseReview(reviewDocument(reviewItem('a'), `asks:\n${asks.map((ask) => `  - ${ask}\n`).join('')}`)));
  expect(await header(['"11111111-1:6"', '"11111111-1:1"'])).toEqual({
    asks: [{ locator: `${FIXTURE_SESSION}:1`, text: 'Run the check verbatim', timestamp: transcriptTime(0) }, { locator: `${FIXTURE_SESSION}:6`, text: 'Second prompt', timestamp: transcriptTime(5) }],
    // Every one of the user's messages is a key ask here, so there is nothing behind "All".
    own: 2, span: { start: transcriptTime(0), end: transcriptTime(5) }, warnings: [],
  });
  const notPrompt = (line: number, locator: string) => `debrief: page, line ${line}: asks: ${locator} is not a message the user or another session sent in this debrief's session`;
  await expect(header(['"11111111-1:1"', '"11111111-1:3"'])).rejects.toThrow(notPrompt(9, '11111111-1:3'));
  await expect(header(['"11111111-1:2"'])).rejects.toThrow(notPrompt(8, '11111111-1:2'));
  // An agent's prompt is the parent's instruction, not the user's.
  await expect(header([`"agent-${AGENT_A}:1"`])).rejects.toThrow(notPrompt(8, `agent-${AGENT_A}:1`));
  await expect(header(['"11111111-1:6"', `"${FIXTURE_SESSION}:6"`])).rejects.toThrow(`debrief: page, line 9: asks: ${FIXTURE_SESSION}:6 is listed twice`);
  await expect(header(['"11111111:1"'])).rejects.toThrow('debrief: page, line 8: "11111111" matches 2 transcripts');
});

test('beside the key asks, the user\'s other messages wait behind "All", in transcript order, redacted and whole; no notices', async () => {
  const f = await setup();
  const token = syntheticSecret(32, 1);
  await writeTranscript(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('First'), reply('ok'), prompt('<task-notification>\n<task-id>b1</task-id>\n</task-notification>'), reply('ok'),
    prompt(`Second, with https://app.example.test/invite/${token}`), reply('ok'), prompt('[Request interrupted by user]'), prompt('Third'),
  ]);
  const header = (asks: string) => f.resolver.header(parseReview(reviewDocument(reviewItem('a'), `asks: [${asks}]\n`)));
  const rows = (h: Awaited<ReturnType<typeof header>>) => h.asks.map(({ locator, text, rest }) => [Number(locator.split(':').at(-1)), text, rest ?? false]);
  const one = await header('"11111111-:8"');
  expect([rows(one), one.own]).toEqual([[[1, 'First', true], [5, 'Second, with https://app.example.test/invite/[token]', true], [8, 'Third', false]], 3]);
  expect((await header('"11111111-:1", "11111111-:5", "11111111-:8"')).asks.some((ask) => ask.rest)).toBe(false);
  expect(await header('')).toMatchObject({ asks: [], own: 0 });
});

test('a message a rewind undid is not a key ask, and says so; "All" leaves it out', async () => {
  const f = await setup();
  // Entry 5 continues from reply 2, so entries 3 and 4 were edited away.
  const record = (n: number, parent: number | null, message: Record<string, unknown>) => ({ type: message.role, uuid: `${FIXTURE_SESSION}-${n}`, parentUuid: parent === null ? null : `${FIXTURE_SESSION}-${parent}`, timestamp: transcriptTime(n), sessionId: FIXTURE_SESSION, message });
  const user = (n: number, parent: number | null, text: string) => record(n, parent, { role: 'user', content: text });
  const said = (n: number, parent: number, text: string) => record(n, parent, { role: 'assistant', id: `msg_${n}`, content: [{ type: 'text', text }] });
  await writeFile(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), [user(1, null, 'Name three colors.'), said(2, 1, 'Red, green, blue.'), user(3, 2, 'Now three more.'), said(4, 3, 'Cyan.'), user(5, 2, 'Instead, three shades of red.'), said(6, 5, 'Crimson.')].map((r) => JSON.stringify(r)).join('\n') + '\n');
  const header = (asks: string) => f.resolver.header(parseReview(reviewDocument(reviewItem('a'), `asks: [${asks}]\n`)));
  await expect(header('"11111111-:3"')).rejects.toThrow('asks: 11111111-:3 was undone by a rewind; it is not on the final conversation');
  expect((await header('"11111111-:5"')).asks.map((ask) => [ask.locator.split(':').at(-1), ask.rest ?? false])).toEqual([['1', true], ['5', false]]);
});

test('a key ask may be a message another session sent, labelled by its origin; "All" counts only the user\'s own', async () => {
  const f = await setup();
  await writeTranscript(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('Typed by the user'), reply('ok'), prompt('<agent-message from="manager">Port the components.</agent-message>'), reply('ok'), prompt('[Request interrupted by user]'),
  ]);
  const header = (asks: string) => f.resolver.header(parseReview(reviewDocument(reviewItem('a'), `asks: [${asks}]\n`)));
  expect((await header('"11111111-:1", "11111111-:3"')).asks.map(({ locator, from }) => [locator, from])).toEqual([[`${FIXTURE_SESSION}:1`, undefined], [`${FIXTURE_SESSION}:3`, 'PEER']]);
  await expect(header('"11111111-:5"')).rejects.toThrow('asks: 11111111-:5 is not a message the user or another session sent');
  expect((await f.resolver.transcripts.prompts()).map((ask) => ask.locator)).toEqual([`${FIXTURE_SESSION}:1`]);
});

test('an elision leaves out a paste between its anchors, found in the message as written; a missing anchor fails on the ask\'s line', async () => {
  const f = await setup();
  const paste = 'Fix the login.\nHere is the log:\nERR one\nERR two\nThanks, and keep the tests.', notification = '<task-notification>\n<task-id>b1</task-id>\n</task-notification>';
  await writeTranscript(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt(paste), reply('ok'), prompt(notification), reply('ok')]);
  const header = async (elide: string) => (await f.resolver.header(parseReview(reviewDocument(reviewItem('a'), `asks:\n  - { ask: "11111111-:1", elide: ${elide} }\n`)))).asks.at(0);
  expect(await header('{ from: "Here is the log", until: Thanks, note: a pasted log }')).toEqual({ locator: `${FIXTURE_SESSION}:1`, text: 'Fix the login.\nThanks, and keep the tests.', timestamp: transcriptTime(0), elision: { at: 15, note: 'a pasted log' } });
  expect(await header('{ from: "Here is", note: the log }')).toMatchObject({ text: 'Fix the login.\n', elision: { at: 15, note: 'the log' } });
  await expect(header('{ from: "No such text", note: x }')).rejects.toThrow('debrief: page, line 8: asks: 11111111-:1: elide.from text not found in the message');
  // `until` is looked for after `from`.
  await expect(header('{ from: "ERR two", until: "Here is", note: x }')).rejects.toThrow('debrief: page, line 8: asks: 11111111-:1: elide.until text not found in the message');
  expect(() => parseReview(reviewDocument(reviewItem('a'), 'asks:\n  - { ask: "11111111-:1", elide: { from: x } }\n'))).toThrow('line 8');
  expect(() => parseReview(reviewDocument(reviewItem('a'), 'asks:\n  - { ask: "11111111-:1", note: x }\n'))).toThrow('line 8');
  // A task notification is an agent's, not the user's.
  await expect(f.resolver.header(parseReview(reviewDocument(reviewItem('a'), 'asks: ["11111111-:3"]\n')))).rejects.toThrow('asks: 11111111-:3 is not a message the user or another session sent');
});

test('a historical repository file, an external live file and an external written file keep their sources and bytes', async () => {
  const f = await setup();
  await f.write('source.ts', 'one\ntwo\n'); await f.git('commit', '-qam', 'Archive');
  const head = await f.git('rev-parse', 'HEAD');
  await f.write('source.ts', 'on disk\n');
  const notes = join(f.root, 'notes'); await mkdir(notes);
  const live = join(notes, 'MEMORY.md'), written = join(notes, 'written.md');
  await writeFile(live, 'live\n'); await writeFile(written, 'disk copy\n');
  await writes(f.transcripts, written);
  const resolver = new EvidenceResolver(await resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo, head }), f.root, f.projects);
  const resolve = (ref: z.input<typeof refSchema>) => resolver.resolve({ ref: refSchema.parse(ref), line: 10, bodyLine: 3 }, 'evidence');
  expect(await resolve({ file: 'source.ts' })).toMatchObject({ source: 'git', revision: head, text: 'one\ntwo\n' });
  expect(await resolve({ file: live })).toMatchObject({ source: 'disk', revision: 'disk', text: 'live\n', absolutePath: live });
  const first = await resolve({ file: written, written: '11111111-:2' });
  expect(first).toMatchObject({ source: 'written', revision: `${FIXTURE_SESSION}:2`, selector: `${written}@${FIXTURE_SESSION}:2`, text: 'written\nsecond\n', created: true, absolutePath: written });
  await writeFile(written, 'edited later\n'); await writeFile(live, 'live, edited\n');
  expect(await resolve({ file: written, written: '11111111-:2' })).toMatchObject({ text: 'written\nsecond\n', newHash: first.newHash });
  expect(await resolve({ file: live })).toMatchObject({ text: 'live, edited\n' });
}, 30_000);

test('written refs slice the Write content, read the created chip from its result, and reject anything else', async () => {
  const f = await setup();
  const written = join(f.root, 'written.md');
  await writes(f.transcripts, written);
  const entry = (n: number, extra: Record<string, string> = {}) => f.resolve({ file: written, written: `${FIXTURE_SESSION}:${n}`, ...extra });
  expect(await entry(2, { range: 'L2' })).toMatchObject({ text: 'second', startLine: 2, endLine: 2, selector: `${written}@${FIXTURE_SESSION}:2:L2` });
  expect(await entry(3)).toMatchObject({ text: 'rewritten\n', created: false });
  // Each Write of one message is its own entry.
  expect([await entry(4), await entry(5)]).toMatchObject([{ text: 'a\n' }, { text: 'b\n' }]);
  const unconfirmed = await entry(8);
  if (unconfirmed.kind !== 'file') throw new Error('expected a file');
  expect(unconfirmed.created).toBeUndefined();
  await expect(entry(2, { range: 'L3' })).rejects.toThrow('Range L3 exceeds 2 lines');
  await expect(entry(6)).rejects.toThrow(`written: ${FIXTURE_SESSION}:6 has no Write call`);
  await expect(entry(7)).rejects.toThrow('has a Write call without file_path or content');
  await expect(f.resolve({ file: join(f.root, 'other.md'), written: `${FIXTURE_SESSION}:2` })).rejects.toThrow(`wrote ${written}, not ${join(f.root, 'other.md')}`);
  await expect(f.resolve({ file: 'written.md', written: `${FIXTURE_SESSION}:2` })).rejects.toThrow(`wrote ${written}, not written.md`);
  expect(() => refSchema.parse({ file: written, written: `${FIXTURE_SESSION}:2`, at: 'base' })).toThrow('Use written or at');
});

test('a repository-relative file matches a Write recorded under the reviewed worktree', async () => {
  const f = await setup();
  await f.write('notes/placeholder', '');
  const written = join(f.repo, 'notes', 'w.md');
  await writes(f.transcripts, written);
  expect(await f.resolve({ file: 'notes/w.md', written: `${FIXTURE_SESSION}:2` })).toMatchObject({ source: 'written', path: 'notes/w.md', text: 'written\nsecond\n' });
  expect(await f.resolve({ file: written, written: `${FIXTURE_SESSION}:2` })).toMatchObject({ source: 'written' });
  await expect(f.resolve({ file: 'notes/other.md', written: `${FIXTURE_SESSION}:2` })).rejects.toThrow(`wrote ${written}, not notes/other.md`);
});

test('an image-only message is an entry of its own, numbered like hive local numbers it', async () => {
  const f = await setup();
  await writeTranscript(join(f.transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('before'), { type: 'user', content: [{ type: 'image', source: { type: 'base64', media_type: 'image/png', data: '' } }] }, reply('after')]);
  const result = await f.resolve({ transcript: `${FIXTURE_SESSION}:1, ${FIXTURE_SESSION}:2, ${FIXTURE_SESSION}:3`, summary: ['before', 'the image', 'after'] });
  if (result.kind !== 'transcript') throw new Error('Unexpected evidence type');
  expect(result.entries.map(({ entry }) => [entry.n, 'text' in entry ? entry.text : ''])).toEqual([[1, 'before'], [2, '[image: image/png]'], [3, 'after']]);
  await expect(f.resolve({ transcript: `${FIXTURE_SESSION}:4`, summary: 'x' })).rejects.toThrow('entry not found');
});

test('each authored ref is resolved in place with contextual errors', async () => {
  const f = await setup();
  const parsed = parseReview(reviewDocument(reviewItem('a', '```ref\nfile: source.ts\ncaption: Current source\n```')));
  const items = await f.resolver.items(parsed.items);
  expect(items[0].evidence[0]).toMatchObject({ kind: 'file', caption: 'Current source', bodyLine: parsed.items[0].refs[0].bodyLine });
});
