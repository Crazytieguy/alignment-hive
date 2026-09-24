import { createHash } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { expect } from 'bun:test';
import { runCommand } from '../lib/spawn';

/** Keep page structure readable while pinning embedded asset contents. */
export function snapshotHtml(html: string): string {
  return html.replace(/(<(style|script)\b[^>]*>)([\s\S]*?)(<\/\2>)/g, (whole, open: string, _tag: string, body: string, close: string) =>
    !body || /\btype="application\/json"/.test(open) ? whole : `${open}sha256:${createHash('sha256').update(body).digest('hex')}${close}`);
}

/** Compares with `__snapshots__/<name>`; UPDATE_SNAPSHOTS=1 rewrites it. */
export async function expectSnapshot(name: string, text: string): Promise<void> {
  const path = join(import.meta.dir, '__snapshots__', name);
  if (process.env.UPDATE_SNAPSHOTS === '1') { await mkdir(dirname(path), { recursive: true }); await writeFile(path, text); }
  expect(text).toBe(await readFile(path, 'utf8'));
}

/** A random-looking secret for tests, a digit in every fifth place, built at run time so no committed file holds one. */
export function syntheticSecret(length: number, seed: number): string {
  const letters = 'abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ', digits = '23456789';
  let x = seed, out = '';
  // The high bits: a power-of-two LCG's low bits cycle quickly.
  for (let i = 0; i < length; i++) { x = (x * 1103515245 + 12345) % 2147483648; const set = i % 5 === 2 ? digits : letters; out += set[Math.floor(x / 65536) % set.length]; }
  return out;
}
export const FIXTURE_SESSION = '11111111-1111-4111-8111-111111111111';
export async function createReviewFixture() {
  const root = await mkdtemp(join(tmpdir(), 'hive-review-'));
  const repo = join(root, 'repo');
  await mkdir(repo);
  const git = async (...args: Array<string>) => {
    const { stdout, stderr, exit } = await runCommand(['git', '-C', repo, ...args], { env: { ...process.env, GIT_AUTHOR_DATE: '2026-09-16T00:00:00Z', GIT_COMMITTER_DATE: '2026-09-16T00:00:00Z' } });
    if (exit) throw new Error(stderr.toString());
    return stdout.toString().trim();
  };
  const write = async (path: string, text: string | Uint8Array) => {
    const absolute = join(repo, path);
    await mkdir(dirname(absolute), { recursive: true });
    await writeFile(absolute, text);
  };
  await git('init', '--initial-branch=main');
  await git('config', 'user.name', 'Review fixture');
  await git('config', 'user.email', 'fixture@example.invalid');
  await git('config', 'commit.gpgsign', 'false');
  await write('source.ts', 'export const value = 1;\n');
  await git('add', '.');
  await git('commit', '-m', 'Fixture base');
  const base = await git('rev-parse', 'HEAD');
  const stateDir = join(repo, '.claude', 'hive');
  const stamp = async () => {
    await mkdir(stateDir, { recursive: true });
    await writeFile(join(stateDir, '.gitignore'), '*\n');
    await writeFile(join(stateDir, `${FIXTURE_SESSION}-commit.txt`), base + '\n');
  };
  // Transcripts under the fixture root only, which stands for ~/.claude/projects with one project dir, `transcripts`.
  return { root, repo, git, write, base, stateDir, stamp, projects: root, cleanup: () => rm(root, { recursive: true, force: true }) };
}

export function reviewDocument(items: string, front = ''): string {
  return `---\ntitle: Fixture review\nheading: Review the change\nsession: ${FIXTURE_SESSION}\nstory: The change, reviewed.\nsections: [{ id: main, title: Main }]\n${front}---\n${items}`;
}
export function reviewItem(id: string, body = '', extra = ''): string {
  return `## Changed ${id}\n\n\`\`\`yaml\nid: ${id}\nsection: main\nlede: Preserve compatibility\n${extra}\`\`\`\n${body}\n`;
}

export const transcriptTime = (minute: number) => `2026-09-16T03:${String(minute).padStart(2, '0')}:00.000Z`;
/** `record` adds record-level fields, such as `isMeta`. */
export type TranscriptMessage = { type: 'user' | 'assistant'; content: unknown; model?: string; timestamp?: string; record?: Record<string, unknown> };
export const prompt = (text: string, timestamp?: string): TranscriptMessage => ({ type: 'user', content: text, timestamp });
export const reply = (text: string, model?: string): TranscriptMessage => ({ type: 'assistant', content: [{ type: 'text', text }], model });
/** One record per message, minute i unless it has its own time, chained by uuid, after a noise record. */
export async function writeTranscript(path: string, session: string, messages: Array<TranscriptMessage>) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, [{ type: 'progress', data: { type: 'hook_progress' } }, ...messages.map(({ type, content, model, timestamp, record }, i) =>
    ({ ...record, type, uuid: `${session}-${i}`, parentUuid: i ? `${session}-${i - 1}` : null, timestamp: timestamp ?? transcriptTime(i), sessionId: session, message: { role: type, content, ...(model && { model }) } }))].map((r) => JSON.stringify(r)).join('\n'));
}
/** Parent entries: 1 prompt; 2 Write created; 3 Write updated; 4 and 5 two Writes of one message; 6 Bash; 7 Write without content; 8 Write with an unrecognised result. */
export async function writes(transcripts: string, path: string) {
  const write = (id: string, content?: string) => ({ type: 'tool_use', id, name: 'Write', input: { file_path: path, ...(content !== undefined && { content }) } });
  const call = (...uses: Array<unknown>): TranscriptMessage => ({ type: 'assistant', model: 'claude-fable-5-1', content: uses });
  const result = (id: string, text: string): TranscriptMessage => ({ type: 'user', content: [{ type: 'tool_result', tool_use_id: id, content: text }] });
  await writeTranscript(join(transcripts, `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('Write the notes'),
    call(write('w1', 'written\nsecond\n')), result('w1', `File created successfully at: ${path} (file state is current)`),
    call(write('w2', 'rewritten\n')), result('w2', `The file ${path} has been updated successfully.`),
    call(write('w3', 'a\n'), write('w4', 'b\n')), result('w3', 'ok'), result('w4', 'ok'),
    call({ type: 'tool_use', id: 'b1', name: 'Bash', input: { command: 'true' } }), result('b1', 'done'),
    call(write('w5')), result('w5', 'ok'),
    call(write('w6', 'unconfirmed\n')), result('w6', '<tool_use_error>denied</tool_use_error>'),
  ]);
}

/* ---------- the kitchen sink: one page with every component, for the invariant gates ---------- */

const KITCHEN_IMAGES = ['page-2026-09-16T03-04-05-123Z.png', 'shot-2026-09-16T03-10-00-000Z-thumb.png'];

const codeLines = (n: number, edit: (i: number) => string | undefined = () => undefined) => Array.from({ length: n }, (_, i) => edit(i + 1) ?? `export const line${i + 1} = ${i + 1};`).join('\n') + '\n';
const guide = (changed: boolean) => [
  '---', 'title: Guide', 'owner: docs', '---', '', '# The guide', '',
  ...Array.from({ length: 14 }, (_, i) => `Paragraph ${i + 1} explains one part of the tool${changed && (i === 2 || i === 11) ? ', now in fewer words' : ' in plain words'}.\n`),
  '- first point', '- second point', '',
].join('\n');

/** A key ask too long for one line at any width. */
const KITCHEN_ASK = 'Now make the guide match the code, and keep every heading a reader links to, since other pages point at them by anchor; where a heading has to change, leave the old anchor working and say so in the guide.';
/**
 * Writes the kitchen sink's repository and transcript into a debrief fixture and returns its debrief file:
 * two story sections, a judgement call with alternatives, a plain row, a focused diff that leaves changes out,
 * a new file with a focus inside it, a Markdown diff and file view, a commit, transcript entries with a long
 * output, and the fixed sections.
 */
export async function kitchenSink(f: Awaited<ReturnType<typeof createReviewFixture>>): Promise<string> {
  await f.write('src/app.ts', codeLines(80));
  await f.write('docs/guide.md', guide(false));
  await f.git('add', '.'); await f.git('commit', '-qm', 'Base');
  await f.stamp();
  await writeFile(join(f.stateDir, `${FIXTURE_SESSION}-commit.txt`), await f.git('rev-parse', 'HEAD') + '\n');
  await f.write('src/app.ts', codeLines(80, (i) => (i === 10 || i === 60 ? `export const changed${i} = 0;` : undefined)));
  await f.write('src/new.ts', codeLines(40));
  await f.write('docs/guide.md', guide(true));
  await f.git('add', '.'); await f.git('commit', '-qm', 'Change the app and the guide', '-m', 'Two edits, a new module and a shorter guide.');
  const output = Array.from({ length: 40 }, (_, i) => `step ${i + 1} ok`).join('\n');
  for (const name of KITCHEN_IMAGES) await copyFile(join(import.meta.dir, 'fixtures/images', name), join(f.root, name));
  await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('Make the app smaller and tidy the guide.'),
    { type: 'assistant', model: 'claude-fable-5-1', content: [{ type: 'tool_use', id: 'b1', name: 'Bash', input: { command: 'bun test', description: 'Run the tests' } }] },
    { type: 'user', content: [{ type: 'tool_result', tool_use_id: 'b1', content: output }] },
    reply('The tests pass.', 'claude-fable-5-1'),
    reply(Array.from({ length: 40 }, (_, i) => `Point ${i + 1} of a long reply, one line each.`).join('\n\n'), 'claude-fable-5'),
    { type: 'assistant', model: 'claude-fable-5-1', content: [{ type: 'tool_use', id: 'a1', name: 'Agent', input: { subagent_type: 'code-reviewer', description: 'Review the change', prompt: Array.from({ length: 35 }, (_, i) => `Check **part ${i + 1}** of the change.`).join('\n\n') } }] },
    { type: 'user', content: [{ type: 'tool_result', tool_use_id: 'a1', content: 'No findings.' }] },
    { type: 'assistant', model: 'claude-fable-5-1', content: [{ type: 'tool_use', id: 'b2', name: 'Bash', input: { command: 'git push' } }] },
    { type: 'user', content: [{ type: 'tool_result', tool_use_id: 'b2', content: 'Permission for this action was denied by the Claude Code auto mode classifier. The push was not asked for.' }] },
    prompt(`${KITCHEN_ASK}\nLinter output:\n${Array.from({ length: 12 }, (_, i) => `docs/guide.md:${i + 1}: heading level skipped`).join('\n')}\nThat is all from the linter.`),
    reply('Done.', 'claude-fable-5-1'),
    prompt('Ship it.', '2026-09-17T09:30:00.000Z'),
    prompt('And check it in the dark theme.', '2026-09-17T09:45:00.000Z'),
  ]);
  const ref = (yaml: string) => '```ref\n' + yaml + '\n```';
  const item = (id: string, section: string, heading: string, body: string, extra = '') => `## ${heading}\n\n\`\`\`yaml\nid: ${id}\nsection: ${section}\nlede: The ${id.replace(/-/g, ' ')} item, in one line\n${extra}\`\`\`\n${body}\n\n`;
  const items = [
    item('alt', 'built', 'The app keeps one export per line, not a table', `Why it matters.\n\n${ref('diff: src/app.ts\nfocus: L10\ntitle: the first edit')}`, 'judgement-call: true\nalternatives: ["A lookup table: fewer lines, one more indirection"]\n'),
    item('plain', 'built', 'A plain row with nothing behind it', ''),
    item('new-module', 'built', 'A new module whose middle is the point', ref('diff: src/new.ts\nfocus: L20-L22')),
    item('guide', 'design', 'The guide lost two phrases', `${ref('diff: docs/guide.md')}\n\n${ref('file: docs/guide.md')}`),
    item('tests', 'checked', 'The tests pass', ref('transcript: 11111111:1, 11111111:2, 11111111:3\nsummary: [The ask, The run, The reply]')),
    item('commit', 'checked', 'One commit', ref('git: true')),
    item('long-reply', 'design', 'The long reply, clipped and whole', `${ref('transcript: 11111111:4\nsummary: The reply')}\n\n${ref('transcript: 11111111:4\nsummary: The reply, whole\nclip: false')}`),
    item('review-agent', 'checked', 'A review agent read the change', ref('transcript: 11111111:5\nsummary: The brief\nresults: false')),
    item('push', 'side-effects', 'A push the classifier denied', ref('transcript: 11111111:6\nsummary: The push')),
    item('quiet-note', 'built', 'A lower-priority note on naming', 'The names follow the existing module.', 'lower-priority: true\nnav: Naming\n'),
    item('quiet-code', 'built', 'The `line` exports keep their order', 'Nothing moved.', 'lower-priority: true\n'),
    item('bump', 'landing', 'The version stays as it is', 'No release in this change.', 'lower-priority: true\n'),
    item('screens', 'design', 'The page, as captured', `${ref(`image: ${KITCHEN_IMAGES[0]}\nsummary: The page, wide`)}\n\n${ref(`image: ${KITCHEN_IMAGES[1]}\nsummary: A thumbnail\nwidth: 160`)}`),
    item('not-run', 'unverified', 'Not run: the app on a phone', 'Worth a look on a small screen.', 'judgement-call: true\n'),
    item('side', 'side-effects', 'Wrote a scratch file outside the repository', 'Nothing to undo.'),
  ].join('');
  const front = `title: Kitchen sink\nheading: Every component on one page\nsession: ${FIXTURE_SESSION}\nasks: ["11111111:1", { ask: "11111111:7", elide: { from: "Linter output", until: "That is all", note: twelve linter lines } }, "11111111:9"]\nstats: [{ n: 2, label: edits, item: alt }, { n: 1, label: new module, item: new-module }]\nstory: The app got smaller and the guide shorter.\nforyou: ["Look at [the alternative](#item-alt)."]\nsections: [{ id: built, title: What was built }, { id: design, title: Design }]\n`;
  return `---\n${front}---\n${items}`;
}
