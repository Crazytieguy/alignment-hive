import { describe, expect, test } from 'bun:test';
import { parseHTML } from 'linkedom';
import { parseFragment } from 'parse5';
import { FIXTURE_SESSION, expectSnapshot } from './fixtures';
import { FileAnchors, evidenceHtml, reflowMessage, timeHtml } from './evidence-html';
import { captureEntry } from './transcript';
import type { DefaultTreeAdapterMap } from 'parse5';
import type { ResolvedEvidence } from './evidence';
import type { PageState } from './evidence-html';
import type { TranscriptEntry } from './transcript';
import type { Entry } from '@alignment-hive/session-data';

type Parsed = { kind: 'user' | 'assistant'; text: string } | { kind: 'tool'; tool: string; id: string; input: Record<string, unknown>; result?: string; error?: true };
let index = 0;
const user = (text: string): Parsed => ({ kind: 'user', text });
const said = (text: string): Parsed => ({ kind: 'assistant', text });
const tool = (name: string, input: Record<string, unknown>, result?: string, error?: true): Parsed => ({ kind: 'tool', tool: name, id: `toolu_${index++}`, input, ...(result !== undefined && { result }), ...(error && { error }) });
/** An entry of the fixture session, at minute `n` unless `time` says otherwise. */
const entry = (n: number, extra: Pick<TranscriptEntry, 'label' | 'role'> & { summary?: string; time?: string; entry: Parsed }): TranscriptEntry => ({
  locator: `${FIXTURE_SESSION}:${n}`, transcript: FIXTURE_SESSION, source: 'parent', summary: extra.summary ?? `Entry ${n}`, label: extra.label, role: extra.role,
  entry: { n, time: 'time' in extra ? extra.time : `2026-09-05T10:${String(n).padStart(2, '0')}:00.000Z`, ...extra.entry } as Entry,
});
const base = { line: 7, bodyLine: 0, oldHash: null, newHash: 'hash', selector: 'selector' };
const page = (): PageState => ({ diffs: [], scope: 'review', files: new FileAnchors([]) });
function render(evidence: ResolvedEvidence) {
  const html = evidenceHtml(evidence, 'item', page());
  const { document } = parseHTML(`<main>${html}</main>`);
  return { html, figure: document.querySelector('figure')! };
}
/** The component as rendered (every fold closed, except an image's), then with every fold opened. */
async function snapshots(name: string, evidence: ResolvedEvidence) {
  const { html, figure } = render(evidence);
  expect([...figure.querySelectorAll('details')].filter((details) => details.hasAttribute('open')).map((details) => details.className)).toEqual(evidence.kind === 'image' ? ['ev-box fv shot'] : []);
  expect([...html].every((character) => character.charCodeAt(0) < 128)).toBe(true);
  await expectSnapshot(`evidence/${name}.closed.html`, html + '\n');
  figure.querySelectorAll('details').forEach((details) => details.toggleAttribute('open', true));
  await expectSnapshot(`evidence/${name}.open.html`, figure.outerHTML + '\n');
  return figure;
}

const command = '\n\tcd /tmp && printf "%s\\n" "a  b" | sort   # tabs\tand  spaces kept\nexit 0';
const output = Array.from({ length: 20 }, (_, i) => `line ${i + 1}`).join('\n') + '\n';
const transcript: ResolvedEvidence = {
  ...base, kind: 'transcript', caption: 'What *ran*, and the reply.', clip: true, results: true,
  entries: [
    entry(1, { label: 'YOU', role: 'user', entry: user('Please **check** <b>this</b>.') }),
    entry(2, { label: 'BASH', role: 'tool', summary: 'The `sort` run', entry: tool('Bash', { command, description: 'Sort the input' }, output) }),
    entry(3, { label: 'WRITE', role: 'tool', entry: tool('Write', { file_path: '/Users/someone/Projects/MixedCase/Run.SH', content: '#!/bin/bash\necho <hi>\n' }, 'File created successfully') }),
    entry(4, { label: 'READ', role: 'tool', entry: tool('Read', { file_path: 'a.md', limit: 2 }, '') }),
    entry(5, { label: 'FABLE 5.1', role: 'asst', time: undefined, entry: said('Done: see [docs](https://example.invalid/) ![x](https://example.invalid/x.png) <script>alert(1)</script> café.') }),
  ],
};
/** One tool call's body, alone in a transcript ref. */
const toolBody = (name: string, input: Record<string, unknown>, result?: string, error?: true) => render({ ...transcript, entries: [entry(9, { label: 'X', role: 'tool', entry: tool(name, input, result, error) })] }).figure.querySelector('.tr-body')!;

describe('transcript entries', () => {
  test('closed and open snapshots', async () => { await snapshots('transcript', transcript); });

  test('one row per entry: chevron, role label, Markdown summary, time; no locator anywhere visible', () => {
    const { figure } = render(transcript);
    const rows = [...figure.querySelectorAll('.ev-box.tr > details.tr-entry')];
    expect(rows.map((row) => [row.querySelector('.tr-role')!.getAttribute('data-role'), row.querySelector('.tr-role')!.textContent])).toEqual([['user', 'YOU'], ['tool', 'BASH'], ['tool', 'WRITE'], ['tool', 'READ'], ['asst', 'FABLE 5.1']]);
    expect(rows[1].querySelector('.tr-sum')!.innerHTML).toBe('The <code>sort</code> run');
    expect(render({ ...transcript, entries: [entry(1, { label: 'BASH', role: 'tool', summary: 'the `<run>`', entry: tool('Bash', {}) })] }).figure.querySelector('.tr-sum')!.innerHTML).toBe('the <code>&lt;run&gt;</code>');
    expect([rows[1].querySelector('time.tr-time')!.getAttribute('datetime'), rows[1].querySelector('time')!.textContent]).toEqual(['2026-09-05T10:02:00.000Z', 'Sep 5 10:02']);
    expect(rows[1].querySelector('summary > .tr-n > .tr-chevron')).not.toBeNull();
    expect(rows[4].querySelector('time')).toBeNull();
    expect(figure.querySelector('figcaption.ev-cap')!.innerHTML).toBe('What <em>ran</em>, and the reply.');
    expect(figure.textContent).not.toContain(FIXTURE_SESSION);
    // The locator is only the fold's state key, never shown: no text and no other attribute carries it.
    expect(rows.map((row) => row.getAttribute('data-key'))).toEqual([1, 2, 3, 4, 5].map((n) => `tr:${FIXTURE_SESSION}:${n}`));
    const shownMarkup = figure.cloneNode(true) as Element;
    shownMarkup.querySelectorAll('[data-key]').forEach((el) => el.removeAttribute('data-key'));
    expect(shownMarkup.outerHTML).not.toContain(FIXTURE_SESSION);
  });

  test('times carry the UTC instant and its UTC text, with an optional class', () => {
    expect(timeHtml('2026-09-05T00:05:00+02:00')).toBe('<time datetime="2026-09-04T22:05:00.000Z">Sep 4 22:05</time>');
    expect(timeHtml('2026-09-05T00:05:00Z', 'tr-time')).toBe('<time class="tr-time" datetime="2026-09-05T00:05:00.000Z">Sep 5 00:05</time>');
  });

  test('a Bash command keeps its bytes, with its description; its result pairs with it', () => {
    const { figure } = render(transcript);
    const body = figure.querySelectorAll('details.tr-entry')[1].querySelectorAll('.tr-body');
    expect(body).toHaveLength(1);
    expect([...body[0].querySelectorAll('.tr-label')].map((label) => label.textContent)).toEqual(['Sort the input', 'result']);
    // A browser's parser drops one newline after <pre> (linkedom does not); the renderer adds it back.
    const pre = parseFragment(/<pre class="tr-cmd">[\s\S]*?<\/pre>/.exec(render(transcript).html)![0]).childNodes[0] as DefaultTreeAdapterMap['element'];
    expect((pre.childNodes[0] as DefaultTreeAdapterMap['textNode']).value).toBe(command);
    const monitor = toolBody('Monitor', { command: 'tail -f log', description: 'Watch the log' });
    expect([monitor.querySelector('.tr-label')!.textContent, monitor.querySelector('pre.tr-cmd')!.textContent]).toEqual(['Watch the log', 'tail -f log']);
  });

  test('output past 30 lines is clipped behind Show all N lines; 30 lines are not', () => {
    const lines = (n: number) => Array.from({ length: n }, (_, i) => `line ${i + 1}`).join('\n') + '\n';
    const run = (n: number) => render({ ...transcript, entries: [entry(2, { label: 'BASH', role: 'tool', entry: tool('Bash', { command: 'seq' }, lines(n)) })] }).figure;
    const out = run(31).querySelector('pre.tr-out')!;
    expect([out.classList.contains('clip'), out.textContent]).toEqual([true, lines(31)]);
    const more = out.parentElement!.querySelector('button.ev-more')!;
    expect([more.textContent, more.getAttribute('data-more'), more.getAttribute('data-less'), more.getAttribute('type')]).toEqual(['Show all 31 lines', 'Show all 31 lines', 'Show less', 'button']);
    expect([run(30).querySelector('pre.tr-out')!.classList.contains('clip'), run(30).querySelector('button')]).toEqual([false, null]);
    expect(render(transcript).figure.querySelectorAll('details.tr-entry')[3].querySelector('pre.tr-out')!.textContent).toBe('(no output)');
  });

  test('a long reply or prompt clips at 1,800 characters or 30 lines behind Show all, unless the ref says clip: false', () => {
    const reply = (text: string, clip = true) => render({ ...transcript, clip, entries: [entry(5, { label: 'FABLE 5', role: 'asst', entry: said(text) })] }).figure;
    const clipbox = (figure: HTMLElement) => figure.querySelector('.tr-body.prose > .tr-wrap > .tr-clipbox.clip');
    expect(clipbox(reply('a'.repeat(1801)))).not.toBeNull();
    expect(clipbox(reply('a'.repeat(1800)))).toBeNull();
    const long = reply(Array.from({ length: 31 }, (_, i) => `Line ${i}.`).join('\n\n'));
    expect(clipbox(long)).not.toBeNull();
    expect([...long.querySelectorAll('.tr-body.prose button.ev-more')].map((b) => [b.textContent, b.getAttribute('data-less')])).toEqual([['Show all', 'Show less']]);
    expect(clipbox(reply('a'.repeat(1801), false))).toBeNull();
  });

  test('an Agent call is its prompt, as prose, labelled by type, its description as written; its report pairs with it, as prose', () => {
    const agent = (input: Record<string, unknown>) => render({ ...transcript, entries: [entry(6, { label: 'AGENT', role: 'tool', entry: tool('Agent', input, 'The **report**.') })] }).figure;
    const body = agent({ subagent_type: 'code-reviewer', description: 'Review <the> diff', prompt: 'Look **hard**.' }).querySelector('.tr-body.prose')!;
    // The type takes the label's capitals; the description keeps its own case.
    expect(body.querySelector('.tr-label')!.innerHTML).toBe('code-reviewer<span class="tr-label-about"> · Review &lt;the&gt; diff</span>');
    expect(agent({ description: 'Only a description', prompt: 'x' }).querySelector('.tr-body.prose > .tr-label')!.innerHTML).toBe('<span class="tr-label-about">Only a description</span>');
    expect([...body.querySelectorAll('strong')].map((s) => s.textContent)).toEqual(['hard', 'report']);
    expect([...body.querySelectorAll('.tr-label')].map((l) => l.textContent)).toEqual(['code-reviewer · Review <the> diff', 'result']);
    expect(agent({ prompt: 'Just a prompt.' }).querySelector('.tr-body.prose > .tr-label')!.textContent).toBe('agent prompt');
    expect(agent({ prompt: 'x'.repeat(1801) }).querySelector('.tr-clipbox.clip')).not.toBeNull();
  });

  test('a denied result says so and wraps; results: false hides every result of the ref', () => {
    const denial = 'Permission for this action was denied by the Claude Code auto mode classifier. ' + 'reason '.repeat(40);
    const denied = render({ ...transcript, entries: [entry(7, { label: 'BASH', role: 'tool', entry: tool('Bash', { command: 'git push' }, denial) })] }).figure;
    expect([...denied.querySelectorAll('.tr-label.tr-denied')].map((l) => l.textContent)).toEqual(['denied']);
    expect(denied.querySelector('pre.tr-out.tr-wrap-lines')!.textContent).toBe(denial);
    const hidden = render({ ...transcript, results: false }).figure;
    expect(hidden.querySelectorAll('.tr-out')).toHaveLength(0);
    expect([...hidden.querySelectorAll('.tr-label')].map((l) => l.textContent)).toEqual(['Sort the input']);
  });

  test('a tool without a view lists its fields: short ones inline, a long string wrapped and clipped past 30 lines, anything nested as JSON', () => {
    const notes = Array.from({ length: 40 }, (_, i) => `line ${i}`).join('\n');
    const custom = toolBody('mcp__x__run', { task_id: 'b1', force: true, path: '/Users/someone/p', notes, nested: { a: 1 } });
    expect([...custom.querySelectorAll('.tr-field')].map((f) => [f.querySelector('.tr-key')!.textContent, f.querySelector('code')!.textContent])).toEqual([['task_id', 'b1'], ['force', 'true'], ['path', '~/p']]);
    expect([...custom.querySelectorAll(':scope > .tr-label')].map((l) => l.textContent)).toEqual(['notes', 'nested']);
    expect([...custom.querySelectorAll('pre.tr-out')].map((pre) => [pre.className, pre.textContent])).toEqual([['tr-out tr-wrap-lines clip', notes], ['tr-out', '{\n  "a": 1\n}']]);
    // A call missing its view's fields shows as fields too.
    expect(toolBody('Edit', { file_path: 'a.ts' }).querySelector('.tr-field .tr-key')!.textContent).toBe('file_path');
  });

  test('Bash commands never clip', () => {
    const script = Array.from({ length: 40 }, (_, i) => `echo ${i}`).join('\n');
    const bash = render({ ...transcript, entries: [entry(2, { label: 'BASH', role: 'tool', entry: tool('Bash', { command: script }) })] }).figure;
    expect(bash.querySelector('pre.tr-cmd.clip')).toBeNull();
  });

  test('a Write shows its path in its own case under the home directory, and its content as code', () => {
    const { figure } = render(transcript);
    const write = figure.querySelectorAll('details.tr-entry')[2];
    const path = write.querySelector('.tr-path')!;
    expect([path.textContent, path.getAttribute('title')]).toEqual(['~/Projects/MixedCase/Run.SH', '/Users/someone/Projects/MixedCase/Run.SH']);
    expect(write.querySelector('pre.tr-cmd > code')!.textContent).toBe('#!/bin/bash\necho <hi>\n');
    expect(write.querySelector('pre.tr-cmd > code')!.getAttribute('class')).toBe('language-bash');
    expect(write.querySelector('.tr-label')!.textContent).toBe('result');
  });

  test('replies and prompts are inert Markdown; thinking stays out', () => {
    const { figure } = render(transcript);
    const [prompt, , , , reply] = figure.querySelectorAll('details.tr-entry');
    expect(prompt.querySelector('.tr-body.prose strong')!.textContent).toBe('check');
    expect(prompt.querySelector('.tr-body.prose b')).toBeNull();
    expect(prompt.querySelector('.tr-body.prose')!.textContent).toContain('<b>this</b>');
    expect(reply.querySelectorAll('.tr-body')).toHaveLength(1);
    expect(reply.textContent).not.toContain('hidden reasoning');
    expect(reply.querySelector('script, img')).toBeNull();
    expect(reply.querySelector('a')!.getAttribute('href')).toBe('https://example.invalid/');
  });

  test('a message names its recipient, another session by its name or process, with its summary; the message is prose; delivery is one line', () => {
    const socket = { to: 'uds:/tmp/cc-socks/50990.sock', summary: 'Decision: (a)', message: 'Go with **(a)**.' };
    const delivered = (message: string) => JSON.stringify({ success: true, message });
    const older = toolBody('SendMessage', socket, delivered('\u201cDecision: (a)\u201d \u2192 uds:/tmp/cc-socks/50990.sock queued there \u2014 a [Cross-session delivery notice] follows if that session holds it'));
    expect([older.className, older.querySelector('.tr-label')!.innerHTML, older.querySelector('strong')!.textContent, older.querySelector('.tr-meta')!.textContent])
      .toEqual(['tr-body prose', 'to another session (50990)<span class="tr-label-about"> \u00b7 Decision: (a)</span>', '(a)', 'queued']);
    const named = toolBody('SendMessage', socket, delivered('\u201cDecision: (a)\u201d \u2192 hive local parser (another Claude session on this machine; queued there \u2014 a notice follows)'));
    expect([named.querySelector('.tr-label')!.textContent, named.querySelector('.tr-meta')!.textContent]).toEqual(['to hive local parser \u00b7 Decision: (a)', 'queued']);
    expect([toolBody('SendMessage', socket, delivered('\u201cDecision: (a)\u201d \u2192 uds:/tmp/cc-socks/50990.sock')), toolBody('SendMessage', socket)].map((body) => body.querySelector('.tr-label')!.textContent))
      .toEqual(['to another session (50990) \u00b7 Decision: (a)', 'to another session (50990) \u00b7 Decision: (a)']);
    // An agent's delivery says what happened to it; the teams-mode fields read the same.
    expect(toolBody('SendMessage', { to: 'a1b2c3d', message: 'Go on.' }, delivered('Resuming agent a1b2c3d')).querySelector('.tr-meta')!.textContent).toBe('Resuming agent a1b2c3d');
    const teams = toolBody('SendMessage', { recipient: 'researcher', content: 'Hi *there*' }, JSON.stringify({ success: false, message: 'No agent named \'researcher\' is reachable.' }));
    expect([teams.querySelector('.tr-label')!.textContent, teams.querySelector('em')!.textContent, teams.querySelector('.tr-meta')!.textContent]).toEqual(['to researcher', 'there', 'No agent named \'researcher\' is reachable.']);
  });

  test('a Read shows its path and lines; an Edit its path and both halves as code', () => {
    const read = (input: Record<string, unknown>) => toolBody('Read', { file_path: '/Users/someone/p/a.md', ...input }, 'text');
    expect([read({ limit: 2 }), read({ offset: 10, limit: 5 }), read({ offset: 10 }), read({})].map((body) => [body.querySelector('.tr-path')!.textContent, body.querySelector('.tr-meta')?.textContent]))
      .toEqual([['~/p/a.md', 'L1\u2013L2'], ['~/p/a.md', 'L10\u2013L14'], ['~/p/a.md', 'from L10'], ['~/p/a.md', undefined]]);
    const edited = toolBody('Edit', { file_path: '/Users/someone/p/a.ts', old_string: 'let a = 1;', new_string: 'const a = 1;', replace_all: true }, 'The file /Users/someone/p/a.ts has been updated successfully. (file state is current in your context)');
    expect([edited.querySelector('.tr-path')!.textContent, [...edited.querySelectorAll('.tr-meta')].map((m) => m.textContent), [...edited.querySelectorAll('.tr-label')].map((l) => l.textContent), [...edited.querySelectorAll('pre.tr-cmd > code.language-typescript')].map((c) => c.textContent)])
      .toEqual(['~/p/a.ts', ['every occurrence'], ['replaced', 'with'], ['let a = 1;', 'const a = 1;']]);
    // Markdown halves wrap, as a written Markdown file does.
    expect(toolBody('Edit', { file_path: 'a.md', old_string: 'a', new_string: 'b' }).querySelectorAll('pre.tr-cmd.tr-prose')).toHaveLength(2);
  });

  test('prose views: a fetch with its question and answer, questions with their options, a plan', () => {
    const fetched = toolBody('WebFetch', { url: 'https://example.invalid/doc', prompt: 'Summarize **it**' }, 'It is *short*.');
    expect([fetched.querySelector('.tr-path')!.textContent, [...fetched.querySelectorAll('.tr-label')].map((l) => l.textContent), fetched.querySelector('strong')!.textContent, fetched.querySelector('em')!.textContent])
      .toEqual(['https://example.invalid/doc', ['asked', 'result'], 'it', 'short']);
    const asked = toolBody('AskUserQuestion', { questions: [{ header: 'Scope', question: 'Which one?', options: [{ label: 'A', description: 'the first' }, { label: 'B' }] }] }, 'User has answered: "Which one?"="A"');
    expect([asked.querySelector('strong')!.textContent, [...asked.querySelectorAll('li')].map((li) => li.textContent), asked.textContent.includes('User has answered')]).toEqual(['Scope', ['A: the first', 'B'], true]);
    expect(toolBody('ExitPlanMode', { plan: '# Plan\n\n1. Do it' }).querySelector('ol li')!.textContent).toBe('Do it');
  });

  test('a todo list marks each item by its status; a skill is its name and arguments', () => {
    const todos = toolBody('TodoWrite', { todos: [{ content: 'Port', status: 'completed' }, { content: 'Test', status: 'in_progress' }, { content: 'Ship', status: 'pending' }, { content: 'Odd', status: 'constructor' }] });
    expect([...todos.querySelectorAll('.tr-todos li')].map((li) => [li.querySelector('.tr-todo')!.getAttribute('title'), li.textContent.trim()]))
      .toEqual([['done', '\u2713 Port'], ['in progress', '\u25b8 Test'], ['to do', '\u25cb Ship'], ['to do', '\u25cb Odd']]);
    expect(toolBody('Skill', { skill: 'review:review', args: 'round 2' }).querySelector('.tr-label')!.textContent).toBe('review:review \u00b7 round 2');
  });

  test.each([
    ['ExitPlanMode', { plan: 'x' }, 'User has approved your plan.', 'User has approved your plan.'],
    ['ToolSearch', { query: 'select:Monitor,TaskStop' }, '[tool_reference: Monitor]\n[tool_reference: TaskStop]', 'loaded Monitor, TaskStop'],
    ['ToolSearch', { query: 'nothing' }, 'No matching deferred tools found', 'No matching deferred tools found'],
    ['TaskStop', { task_id: 'b1' }, JSON.stringify({ message: 'Successfully stopped task: b1 (cat <<EOF\nx\nEOF)', task_id: 'b1' }), 'Successfully stopped task: b1 (cat <<EOF\u2026'],
    ['Edit', { file_path: 'a.ts', old_string: 'a', new_string: 'b' }, 'Updated, with a note.', 'Updated, with a note.'],
    ['SendMessage', { to: 'a', message: 'b' }, '', '(no output)'],
    // Success boilerplate says nothing the call does not.
    ['Edit', { file_path: 'a.ts', old_string: 'a', new_string: 'b' }, 'The file /x/a.ts has been updated successfully.', undefined],
    ['TodoWrite', { todos: [] }, 'Todos have been modified successfully. Ensure that you continue to use the todo list.', undefined],
    ['Skill', { skill: 's' }, 'Launching skill: s', undefined],
  ])('a %s result is at most one line', (name, input, result, line) => {
    expect([...toolBody(name, input, result).querySelectorAll('.tr-meta')].at(-1)?.textContent).toBe(line);
  });

  test('a failed call\'s result shows whole, so a rejection keeps what the user said', () => {
    const rejection = 'The user doesn\'t want to proceed with this tool use. The tool use was rejected.\nTo tell you how to proceed, the user said:\nKeep the old name.';
    const rejected = toolBody('Edit', { file_path: 'a.ts', old_string: 'a', new_string: 'b' }, rejection, true);
    // A message, so it wraps; a command's failed output keeps its lines.
    expect([rejected.querySelectorAll('.tr-meta').length, rejected.querySelector('pre.tr-out')!.className, rejected.querySelector('pre.tr-out')!.textContent]).toEqual([0, 'tr-out tr-wrap-lines', rejection]);
    expect(toolBody('Bash', { command: 'false' }, 'Exit code 1', true).querySelector('pre.tr-out')!.className).toBe('tr-out');
    expect(toolBody('WebFetch', { url: 'https://example.invalid/' }, '<tool_use_error>Fetch *failed*</tool_use_error>', true).querySelector('pre.tr-out')!.textContent).toBe('<tool_use_error>Fetch *failed*</tool_use_error>');
  });

});

describe('file views', () => {
  const markdown: ResolvedEvidence = { ...base, kind: 'file', caption: 'As *written*.', path: '/Users/someone/.claude/memory/Notes.md', absolutePath: '/Users/someone/.claude/memory/Notes.md', source: 'written', revision: `${FIXTURE_SESSION}:9`, text: '---\nname: notes\nmetadata:\n  type: reference\n---\n\n# Notes\n\nSee [other](Other.md).\n', startLine: 1, endLine: 9, created: true };
  const code: ResolvedEvidence = { ...base, kind: 'file', path: 'packages/cli/src/Main.ts', absolutePath: '/repo/packages/cli/src/Main.ts', source: 'git', revision: 'abc', text: 'const a = 1;\nconst b = 2;', startLine: 41, endLine: 42 };

  test('closed and open snapshots', async () => {
    await snapshots('file-markdown', markdown);
    await snapshots('file-code', code);
  });

  test('the header holds the path, the chips and the source toggle; external paths show two segments', () => {
    const view = render(markdown).figure;
    const head = view.querySelector('details.ev-box.fv > summary.fv-head')!;
    expect(head.querySelector('.git-chev')).not.toBeNull();
    expect([head.querySelector('.fv-path')!.innerHTML, head.querySelector('.fv-path')!.getAttribute('title')]).toEqual(['<span class="dir">memory/</span>Notes.md', '/Users/someone/.claude/memory/Notes.md']);
    expect([...head.querySelectorAll('.fv-chip')].map((chip) => chip.textContent)).toEqual(['new', 'L1–L9']);
    expect(head.querySelector('button.fv-toggle')!.textContent).toBe('View source');
    expect([...view.querySelectorAll('.fv-fm dt')].map((dt) => dt.textContent)).toEqual(['name', 'metadata.type']);
    expect(view.querySelector('.fv-md a')).toBeNull();
    expect(view.querySelector('.fv-md')!.textContent).toContain('See other.');
    expect(view.querySelector('.fv-src')!.hasAttribute('hidden')).toBe(true);
  });

  test('source line numbers match the resolved range', () => {
    const view = render(code).figure;
    expect(view.querySelector('.fv-ln')!.textContent).toBe('41\n42');
    expect(view.querySelector('.fv-src code')!.textContent).toBe('const a = 1;\nconst b = 2;');
    expect(view.querySelector('.fv-path')!.innerHTML).toBe('<span class="dir">packages/cli/src/</span>Main.ts');
    expect([view.querySelector('.fv-md'), view.querySelector('.fv-toggle'), view.querySelector('.fv-src')!.hasAttribute('hidden')]).toEqual([null, null, false]);
  });
});

describe('git card', () => {
  const facts = {
    commits: [{ hash: 'abc1234', subject: 'Fix the <thing>', body: 'First line\nwrapped here.\n\n- one\n- two\n\nCo-Authored-By: A <a@example.invalid>', added: 12, deleted: 3, files: [
      { path: 'src/lib/a.ts', status: 'modified' as const, added: 10, deleted: 3 },
      { path: 'src/new.ts', status: 'added' as const, added: 2, deleted: 0 },
      { path: 'old.txt', status: 'deleted' as const, added: 0, deleted: 0 },
      { path: 'logo.png', status: 'added' as const, added: null, deleted: null },
    ] }],
  };
  const live: ResolvedEvidence = { ...base, kind: 'git', facts: { ...facts, tree: { status: { modified: ['a'], staged: [], untracked: ['b', 'c'], deleted: [] }, upstream: { name: 'origin/main', unpushed: 1 } } } };
  const historical: ResolvedEvidence = { ...base, kind: 'git', facts };

  test('closed and open snapshots', async () => {
    await snapshots('git-live', live);
    await snapshots('git-historical', historical);
  });

  test('one fold per commit; header counts and per-file rows match the resolved facts', () => {
    const card = render(live).figure;
    const commit = card.querySelector('.git > details.git-commit')!;
    expect(commit.querySelector('summary.git-commit-h > .git-counts')!.textContent).toBe('4 files +12 −3');
    const rows = [...commit.querySelectorAll('ul.git-files > li')].map((li) => [...li.children].map((child) => child.textContent));
    expect(rows).toEqual([['src/lib/a.ts', '+10', '−3'], ['src/new.ts new', '+2', '−0'], ['old.txt deleted', '+0', '−0'], ['logo.png new', '', '']]);
    expect(commit.querySelector('blockquote')!.textContent).toBe('First line wrapped here.\n\n- one\n- two\n\nCo-Authored-By: A <a@example.invalid>');
    expect(card.querySelector('.git-state')!.textContent).toBe('The working tree has 1 modified, 2 untracked. 1 of the commits is not on origin/main.');
  });

  test('with a historical head the card shows commits only', () => {
    expect(render(historical).figure.querySelector('.git-state')).toBeNull();
  });

  test('messages reflow paragraphs and keep lists and trailers', () => {
    expect(reflowMessage('a\nb\n\n1. x\n2. y\n\nKey: v')).toBe('a b\n\n1. x\n2. y\n\nKey: v');
  });
});

describe('image and capture evidence', () => {
  test('an image folds like a file view, open from the start: kind, summary, the time it was taken; its summary is the alt text', async () => {
    const figure = await snapshots('image', { ...base, kind: 'image', caption: 'The *screen*', selector: '/reviews/r1/shots/page-2026-08-31T08-25-32-092Z.png', dataUri: 'data:image/png;base64,aGVsbG8=', summary: 'The hours page on a *phone*', width: 390, takenAt: '2026-08-31T08:25:32.092Z' });
    const head = figure.querySelector('details.ev-box.fv.shot > summary.fv-head')!;
    expect([head.querySelector('.shot-kind')!.textContent, head.querySelector('.fv-path.shot-sum')!.innerHTML]).toEqual(['Image', 'The hours page on a <em>phone</em>']);
    expect([head.querySelector('time.shot-time')!.getAttribute('datetime'), head.querySelector('time.shot-time')!.textContent]).toEqual(['2026-08-31T08:25:32.092Z', 'Aug 31 08:25']);
    const img = figure.querySelector('.shot-body > img')!;
    expect([img.getAttribute('src'), img.getAttribute('alt'), img.getAttribute('width'), img.getAttribute('tabindex')]).toEqual(['data:image/png;base64,aGVsbG8=', 'The hours page on a *phone*', '390', '-1']);
    expect(figure.querySelector('.ev-img')).toBeNull();
    const plain = render({ ...base, kind: 'image', selector: '/x/plot.png', dataUri: 'data:image/png;base64,aGVsbG8=', summary: 'A plot' }).figure;
    expect([plain.querySelector('time'), plain.querySelector('img')!.hasAttribute('width')]).toEqual([null, false]);
  });

  test('a capture is a Bash call: the same row and body as one in a transcript, the output as Claude Code records it', async () => {
    const captured = captureEntry('capture:log', { command: ['/usr/bin/git', 'log', '--format=%h two'], cwd: '/Repo/Dir', exit: 1, stdout: 'abc\n', stderr: 'warn\n', startedAt: '2026-09-05T10:00:00.000Z' }, 'The *log*');
    const figure = await snapshots('capture', { ...base, kind: 'transcript', clip: true, results: true, entries: [captured] });
    const row = figure.querySelector('.ev-box.tr > details.tr-entry')!;
    expect([row.querySelector('.tr-role')!.textContent, row.querySelector('.tr-sum')!.innerHTML]).toEqual(['BASH', 'The <em>log</em>']);
    const call = toolBody('Bash', { command: "/usr/bin/git log '--format=%h two'" }, 'Exit code 1\nabc\nwarn', true);
    expect(row.querySelector('.tr-body')!.outerHTML).toBe(call.outerHTML);
    const quiet = captureEntry('capture:true', { command: ['true'], cwd: '/', exit: 0, stdout: '', stderr: '', startedAt: '2026-09-05T10:00:00.000Z' }, 'Nothing');
    const body = render({ ...base, kind: 'transcript', clip: true, results: true, entries: [quiet] }).figure.querySelector('.tr-body')!;
    expect(body.outerHTML).toBe(toolBody('Bash', { command: 'true' }, '').outerHTML);
  });
});

describe('inline Markdown image gate', () => {
  const routes: Array<[string, (image: string) => ResolvedEvidence, string]> = [
    ['a caption', (image) => ({ ...base, kind: 'git', caption: `See ${image}`, facts: { commits: [] } }), '.ev-cap'],
    ['a transcript summary', (image) => ({ ...base, kind: 'transcript', clip: true, results: true, entries: [entry(1, { label: 'YOU', role: 'user', summary: `See ${image}`, entry: user('') })] }), '.tr-sum'],
  ];
  test.each(routes)('%s rejects remote and relative images with the item and line, and keeps data images', (_, evidence, selector) => {
    for (const image of ['![x](https://example.invalid/p.png)', '![x](p.png)']) expect(() => render(evidence(image))).toThrow('Item "item", line 7: Markdown images must use data:');
    expect(render(evidence('![x](data:image/png;base64,AA==)')).figure.querySelector(`${selector} img`)!.getAttribute('src')).toBe('data:image/png;base64,AA==');
  });
});
