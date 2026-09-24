import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterEach, expect, test } from 'bun:test';
import { parseHTML } from 'linkedom';
import { FIXTURE_SESSION, createReviewFixture, expectSnapshot, prompt, reviewDocument, reviewItem, snapshotHtml, syntheticSecret, writeTranscript, writes } from './fixtures';
import { MAX_PAGE_BYTES, assembleReview, prepareReview, renderReview } from './render';

const fixtures: Array<Awaited<ReturnType<typeof createReviewFixture>>> = [];
afterEach(async () => { await Promise.all(fixtures.splice(0).map((f) => f.cleanup())); });
async function setup(items: string, front = '') {
  const f = await createReviewFixture(); fixtures.push(f); await f.stamp();
  const input = join(f.root, 'debrief.md'); await writeFile(input, reviewDocument(items, front));
  return { ...f, input };
}

test('assembly emits only ASCII, mounted V4 pairs, disclosure and git facts from a git ref', async () => {
  const f = await setup(reviewItem('unicode', 'Decision café.\n\n```ref\ndiff: source.ts\nfocus: L1\nlabels: [{line: 1, where: Console prompt}]\ncaption: The prompt changed\n```\n\n```ref\ngit: true\n```', 'judgement-call: true\nalternatives: [Consider another option]\n'));
  await f.write('source.ts', 'export const value = "☃ </script>";\n');
  await f.write('untracked.txt', 'new\n');
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  const result = assembleReview(prepared);
  expect([...result.html].every((character) => character.charCodeAt(0) < 128)).toBe(true);
  expect(result.html).not.toContain('\0');
  expect(result.html).not.toContain('<!DOCTYPE');
  expect(result.html).toContain('data-item="unicode"');
  expect(result.html).toContain('data-review-diff');
  expect(result.html).not.toContain(f.base);
  expect(result.html).toContain('1 untracked');
  expect(result.html).toContain('Consider another option');
  expect(result.html).toContain('Console prompt');
  const payload = /id="review-diffs">([^<]*)<\/script>/.exec(result.html)![1];
  expect(JSON.parse(payload)[0]).toMatchObject({ path: 'source.ts', new: 'export const value = "☃ </script>";\n', focus: [1, 1] });
  expect(prepared.context.session).toBe(FIXTURE_SESSION);
});

test('no git card is added without a git ref', async () => {
  const f = await setup(reviewItem('facts', 'The tree was already dirty.'));
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  expect(prepared.items).toHaveLength(1);
  const html = assembleReview(prepared).html;
  expect(html).toContain('The tree was already dirty.');
  expect(parseHTML(html).document.querySelector('.ev-git')).toBeNull();
});

test('git ref shows one folded commit with identity, reflowed message, per-file counts and the tree as git knows it now', async () => {
  const f = await setup(reviewItem('facts', '```ref\ngit: true\n```'));
  await f.write('source.ts', 'new\nsecond\n'); await f.write('image.png', new Uint8Array([0, 1, 2, 0]));
  await f.git('add', '.'); await f.git('commit', '-m', 'Subject', '-m', 'Body <quoted>\nwrapped line\n\n- kept\n- list');
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  const document = parseHTML(assembleReview(prepared).html).document;
  const commit = document.querySelector('.ev-git details.git-commit')!;
  expect(commit.hasAttribute('open')).toBe(false);
  expect(commit.querySelector('.git-hash')!.textContent).toBe(await f.git('rev-parse', '--short', 'HEAD'));
  expect(commit.querySelector('.git-subject')!.textContent).toBe('Subject');
  expect(commit.querySelector('.git-commit-h > .git-counts')!.textContent).toBe('2 files +2 \u22121');
  expect(commit.querySelector('.git-msg-l')!.textContent).toBe('Commit message');
  expect(commit.querySelector('blockquote')!.textContent).toBe('Body <quoted> wrapped line\n\n- kept\n- list');
  const files = [...commit.querySelectorAll('.git-files li')].map((li) => [...li.children].map((child) => child.textContent));
  expect(files).toEqual([['image.png new', '', ''], ['source.ts', '+2', '\u22121']]);
  expect(document.querySelector('.git-state')!.textContent).toBe('The working tree is clean. The branch has no upstream.');
  await f.write('source.ts', 'dirty\n'); await f.write('new.txt', 'new\n');
  const dirty = parseHTML(assembleReview(await prepareReview(f.input, { cwd: f.repo, projects: f.projects })).html).document;
  expect(dirty.querySelector('.git-state')!.textContent).toBe('The working tree has 1 modified, 1 untracked. The branch has no upstream.');
});

test('remote images in either deferred Markdown diff side are rejected', async () => {
  const f = await setup(reviewItem('images', '```ref\ndiff: readme.md\ncaption: Changed prose\n```'));
  await f.write('readme.md', '![remote](https://example.invalid/image.png)\n');
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  expect(() => assembleReview(prepared)).toThrow(/images.*line/);
});

test.each([
  '![preview](https://example.invalid/preview.png)',
  '![preview](preview.png)',
  '![preview][image]\n\n[image]: https://example.invalid/preview.png',
])('body Markdown images cannot bypass resource validation: %s', async (body) => {
  const f = await setup(reviewItem('body-image', body));
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  expect(() => assembleReview(prepared)).toThrow(/body-image.*line/);
});

test('body data images and inert wrapper-looking script strings remain valid', async () => {
  const f = await setup(reviewItem('inline-image', '![pixel](data:image/png;base64,aGVsbG8=)\n\n<script>const template = "<body>Preview</body>";</script>'));
  const html = assembleReview(await prepareReview(f.input, { cwd: f.repo, projects: f.projects })).html;
  expect(html).toContain('data:image/png;base64,aGVsbG8=');
  expect(html).toContain('Preview');
  await writeFile(f.input, reviewDocument(reviewItem('wrapper', '<body>Preview</body>')));
  const bad = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  expect(() => assembleReview(bad)).toThrow();
});

test('ordinary Markdown anchors and paired raw inline markup survive', async () => {
  const f = await setup(reviewItem('markup', '<em>café</em> [Reference](https://example.invalid/)\n'));
  const rendered = assembleReview(await prepareReview(f.input, { cwd: f.repo, projects: f.projects }));
  expect(rendered.html).toContain('<em>caf');
  expect(rendered.html).toContain('href="https://example.invalid/"');
});

test('14 MB guard identifies the largest item rather than emitting oversized output', async () => {
  const f = await setup(reviewItem('large'));
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  prepared.items[0].body = 'x'.repeat(MAX_PAGE_BYTES);
  expect(() => assembleReview(prepared)).toThrow(/Largest contributors: large/);
});

test('file views fold closed with the path, line range and original line numbers', async () => {
  const ref = (at: string, range: string) => `\n\n\`\`\`ref\nfile: src/source.ts\nat: ${at}\nrange: ${range}\ncaption: Selected source\n\`\`\``;
  const f = await setup(reviewItem('files', ref('base', 'L1-L1') + ref('head', 'L3-L4')));
  await f.write('src/source.ts', 'first\nsecond\nthird\nfourth\n');
  await f.git('add', '.'); await f.git('commit', '-qm', 'Source');
  await writeFile(join(f.stateDir, `${FIXTURE_SESSION}-commit.txt`), await f.git('rev-parse', 'HEAD') + '\n');
  await f.write('src/source.ts', 'first\nsecond\nnew third\nfourth\n');
  const document = parseHTML(assembleReview(await prepareReview(f.input, { cwd: f.repo, projects: f.projects })).html).document;
  const views = [...document.querySelectorAll('figure.ev-file')];
  expect(views.map((view) => view.querySelector('details.fv')!.hasAttribute('open'))).toEqual([false, false]);
  expect(views.map((view) => view.querySelector('.fv-path')!.innerHTML)).toEqual(['<span class="dir">src/</span>source.ts', '<span class="dir">src/</span>source.ts']);
  expect(views.map((view) => view.querySelector('.fv-chips')!.textContent)).toEqual(['L1\u2013L1', 'L3\u2013L4']);
  expect(views.map((view) => view.querySelector('.fv-ln')!.textContent)).toEqual(['1', '3\n4']);
  expect(views.map((view) => view.querySelector('.fv-src code')!.textContent)).toEqual(['first', 'new third\nfourth']);
  expect(views[0].querySelector('.fv-src code')!.getAttribute('class')).toBe('language-typescript');
  expect(views[0].querySelector('.fv-toggle')).toBeNull();
  expect(views[0].querySelector('figcaption.ev-cap')!.textContent).toBe('Selected source');
}, 30_000);

test('Markdown file evidence renders dotted front matter and inert prose, with the exact source behind a toggle', async () => {
  const f = await setup(reviewItem('markdown', '```ref\nfile: notes.md\n```'));
  const text = '---\ntitle: "A caf\u00e9"\ntags: [one, two]\nmetadata:\n  type: reference\nempty: ""\n---\n\n# Heading\n\n**Bold** and `<tag>`.\n\n<script>window.untrusted = true</script>\n';
  await f.write('notes.md', text);
  const html = assembleReview(await prepareReview(f.input, { cwd: f.repo, projects: f.projects })).html;
  const document = parseHTML(html).document;
  const view = document.querySelector('details.fv')!;
  expect(view.querySelector('.fv-toggle')!.textContent).toBe('View source');
  const rendered = view.querySelector('.fv-md')!;
  expect(rendered.querySelector('h1')!.textContent).toBe('Heading');
  expect(rendered.querySelector('strong')!.textContent).toBe('Bold');
  expect([...rendered.querySelectorAll('.fv-fm dt')].map((node) => node.textContent)).toEqual(['title', 'tags', 'metadata.type']);
  expect([...rendered.querySelectorAll('.fv-fm dd')].map((node) => node.textContent)).toEqual(['A caf\u00e9', '["one","two"]', 'reference']);
  expect(view.querySelector('.fv-src')!.hasAttribute('hidden')).toBe(true);
  expect(view.querySelector('.fv-src code')!.textContent).toBe(text.slice(0, -1));
  expect(view.querySelector('.fv-ln')!.textContent).toBe(Array.from({ length: text.split('\n').length - 1 }, (_, i) => i + 1).join('\n'));
  expect(view.querySelector('script')).toBeNull();
  expect([...html].every((character) => character.charCodeAt(0) < 128)).toBe(true);
});

test('relative Markdown links reach the shown view of their target, per revision and source, else become text', async () => {
  const f = await setup(reviewItem('placeholder'));
  await f.write('docs/a.md', '[b](b.md)\n'); await f.write('docs/b.md', 'old b\n');
  await f.git('add', '.'); await f.git('commit', '-qm', 'Docs');
  const revision = await f.git('rev-parse', 'HEAD');
  await f.write('docs/a.md', '[b](b.md) [c](c.md) [missing](missing.md) [web](https://example.invalid/) [code](../source.ts) [self](./a.md#top)\n');
  await f.write('docs/b.md', 'new b\n'); await f.write('docs/c.md', 'c\n');
  const notes = join(f.root, 'notes'); await mkdir(notes);
  const memory = join(notes, 'MEMORY.md'), written = join(notes, 'written.md');
  await writeFile(memory, '[w](written.md) [gone](gone.md)\n');
  await writes(join(f.root, 'transcripts'), written);
  const refs = [{ file: 'docs/a.md' }, { file: 'docs/a.md', at: revision }, { file: 'docs/b.md' }, { file: 'docs/b.md', at: revision }, { file: 'docs/b.md', range: 'L1' }, { file: memory }, { file: written, written: `${FIXTURE_SESSION}:2` }];
  await writeFile(f.input, reviewDocument(reviewItem('links', refs.map((ref) => '```ref\n' + Object.entries(ref).map(([key, value]) => `${key}: "${value}"`).join('\n') + '\n```').join('\n\n'))));
  const document = parseHTML(assembleReview(await prepareReview(f.input, { cwd: f.repo, projects: f.projects })).html).document;
  const views = [...document.querySelectorAll('.item-body > .ev-file')];
  const [aDisk, aOld, bDisk, bOld, bAgain, memoryId, writtenId] = views.map((view) => view.getAttribute('id'));
  expect(new Set([aDisk, aOld, bDisk, bOld, memoryId, writtenId]).size).toBe(6);
  expect(bAgain).toBeNull();
  const links = (view: Element) => [...view.querySelectorAll('.fv-md a')].map((link) => link.getAttribute('href'));
  expect(links(views[0])).toEqual([`#${bDisk}`, 'https://example.invalid/', `#${aDisk}`]);
  expect(views[0].querySelector('.fv-md')!.textContent.trim()).toBe('b c missing web code self');
  expect(links(views[1])).toEqual([`#${bOld}`]);
  expect(links(views[5])).toEqual([`#${writtenId}`]);
  expect(views[6].querySelector('.fv-chips')!.textContent).toBe('newL1\u2013L2');
  expect(views[5].querySelector('.fv-chips')!.textContent).toBe('L1\u2013L1');
  expect(views[5].querySelector('.fv-path')!.textContent).toBe('notes/MEMORY.md');
  expect(views[5].querySelector('.fv-path')!.getAttribute('title')).toBe(memory);
}, 30_000);

test('Markdown file evidence rejects remote images using the shared validation gate', async () => {
  const f = await setup(reviewItem('markdown', '```ref\nfile: notes.md\n```'));
  await f.write('notes.md', '![image](https://example.invalid/image.png)');
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  expect(() => assembleReview(prepared)).toThrow(/markdown.*line/);
});

test('deterministic golden review page (UPDATE_SNAPSHOTS=1 to replace)', async () => {
  const f = await setup(reviewItem('prompt', 'Keep the command local.\n\n```ref\ndiff: source.ts\nfocus: L1\nlabels: [{line: 1, where: Console prompt}]\ncaption: One changed line\n```', 'judgement-call: true\nalternatives: [Ship it behind a flag]\n'));
  await f.write('source.ts', 'export const value = 2;\n');
  const html = snapshotHtml(assembleReview(await prepareReview(f.input, { cwd: f.repo, projects: f.projects }), { reviewId: 'fixture-review' }).html);
  await expectSnapshot('review-page.html', html);
});

/* ---------- secrets ---------- */

/** Built at run time, so no committed file holds a literal that secret scanning would flag. */
const synthetic = (seed: number) => syntheticSecret(28, seed);
async function expectNothingWritten(outDir: string, secret: string): Promise<void> {
  const files = await readdir(outDir, { recursive: true }).catch(() => []);
  expect(files.filter((name) => /page\.html|manifest\.json|\.tmp$/.test(name))).toEqual([]);
  for (const name of files) expect((await readFile(join(outDir, name), 'utf8').catch(() => '')).includes(secret)).toBe(false);
}

test('evidence secrets are redacted before the page, the diff payload and the manifest see them', async () => {
  const link = synthetic(1), nested = synthetic(2), code = synthetic(3);
  const f = await setup(reviewItem('run', '```ref\ntranscript: 11111111-:2, 11111111-:3\nsummary: [the invite, the tool]\n```\n\n```ref\ndiff: notes.txt\n```'));
  const call = (id: string, name: string, input: unknown) => ({ type: 'assistant' as const, model: 'claude-fable-5-1', content: [{ type: 'tool_use', id, name, input }] });
  const result = (id: string, text: string) => ({ type: 'user' as const, content: [{ type: 'tool_result', tool_use_id: id, content: text }] });
  await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('Invite the facilitators'),
    call('b1', 'Bash', { command: 'make-invite' }), result('b1', `https://app.example.test/invite/${link}\nGET /cb?a=1&code=${code}`),
    call('m1', 'mcp__store__put', { env: { token: nested }, path: 'a' }), result('m1', 'ok'),
  ]);
  await f.write('notes.txt', `callback: https://app.example.test/cb?a=1&code=${code}\n`);
  const out = join(f.root, 'out');
  const rendered = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: out });
  const [page, manifest] = await Promise.all([readFile(rendered.pagePath, 'utf8'), readFile(rendered.manifestPath, 'utf8')]);
  for (const [name, text] of [['page', page], ['manifest', manifest]]) for (const secret of [link, nested, code]) expect([name, secret, text.includes(secret)]).toEqual([name, secret, false]);
  expect(page).toContain('https://app.example.test/invite/[token]');
  expect(page).toContain('code=[token]');
  expect(page).toContain('&#34;token&#34;: &#34;[token]&#34;');
  expect(JSON.parse(/id="review-diffs">([^<]*)<\/script>/.exec(page)![1])[0].new).toBe('callback: https://app.example.test/cb?a=1&code=[token]\n');
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  // Distinct secrets: the same code in a result and in the diff counts once.
  expect(prepared.redactions).toEqual({ link: 1, query: 1, value: 1 });
});

test('outside a git repository a file and a transcript ref render; diff, git and at: refs name the missing repository', async () => {
  const f = await createReviewFixture(); fixtures.push(f);
  const plain = join(f.root, 'plain');
  await mkdir(plain);
  await writeFile(join(plain, 'notes.txt'), 'plain notes\n');
  await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('Take notes'), prompt('Thanks')]);
  const input = join(f.root, 'debrief.md');
  await writeFile(input, reviewDocument(reviewItem('notes', '```ref\nfile: notes.txt\n```\n\n```ref\ntranscript: 11111111-:1\nsummary: the ask\n```')));
  const out = join(f.root, 'out');
  const rendered = await renderReview(input, { cwd: plain, projects: f.projects, outDir: out });
  expect(await readFile(rendered.pagePath, 'utf8')).toContain('plain notes');
  expect(JSON.parse(await readFile(rendered.manifestPath, 'utf8'))).toMatchObject({ baseCommit: null, headCommit: null });
  expect(rendered.coverage).toBeNull();
  expect(JSON.parse(await readFile(rendered.manifestPath, 'utf8')).coverage).toBeUndefined();
  for (const [ref, what] of [['diff: notes.txt', 'diff'], ['git: true', 'git'], ['file: notes.txt\nat: base', 'at: base']]) {
    await writeFile(input, reviewDocument(reviewItem('notes', `\`\`\`ref\n${ref}\n\`\`\``)));
    await expect(prepareReview(input, { cwd: plain, projects: f.projects })).rejects.toThrow(`${what} needs a git repository`);
  }
});

test('coverage counts changed lines since base per file a diff ref shows, and warns for the rest', async () => {
  const f = await setup(reviewItem('source', '```ref\ndiff: source.ts\n```'));
  await f.write('source.ts', 'export const value = 2;\nexport const more = 3;\n');
  await f.write('docs/committed.md', 'one\ntwo\n'); await f.git('add', 'docs'); await f.git('commit', '-qm', 'Docs');
  await f.write('notes.txt', 'a\nb\nc');
  await f.write('image.png', new Uint8Array([0, 1, 2, 0]));
  const rendered = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'out') });
  // source.ts: 2 added, 1 deleted; docs/committed.md: 2; notes.txt: 3 (no final newline); image.png is binary, so no ref could show it.
  expect(rendered.coverage).toEqual({ lines: 3, changedLines: 8, files: 1, changedFiles: 3 });
  expect(JSON.parse(await readFile(rendered.manifestPath, 'utf8')).coverage).toEqual(rendered.coverage);
  const warning = rendered.warnings.find((line) => line.includes('No item shows'));
  expect(warning).toBe('debrief: page, line 1: No item shows 2 changed files: docs/committed.md (2 lines), notes.txt (3 lines); give each a diff ref, in a brief lower-priority item when it needs no attention');
});

test('full coverage gives no warning, and a debrief kept inside the repository is not counted', async () => {
  const f = await createReviewFixture(); fixtures.push(f); await f.stamp();
  await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('Change it')]);
  await f.write('source.ts', 'export const value = 2;\n');
  const dir = join(f.repo, 'notes', 'debrief');
  await f.write('notes/debrief/debrief.md', reviewDocument(reviewItem('source', '```ref\ndiff: source.ts\n```')));
  const rendered = await renderReview(join(dir, 'debrief.md'), { cwd: f.repo, projects: f.projects, outDir: dir });
  expect(rendered.coverage).toEqual({ lines: 2, changedLines: 2, files: 1, changedFiles: 1 });
  expect(rendered.warnings.filter((line) => line.includes('No item shows'))).toEqual([]);
});

test('a streamed response and a lower-case assignment in evidence are redacted end to end', async () => {
  const streamed = synthetic(4), assigned = synthetic(5);
  const f = await setup(reviewItem('files', '```ref\nfile: stream.txt\n```\n\n```ref\nfile: settings.py\n```'));
  await f.write('stream.txt', `data: {"access_token":"${streamed}"}\n\n`);
  await f.write('settings.py', `client_secret = "${assigned}"\n`);
  const out = join(f.root, 'out');
  const rendered = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: out });
  const [page, manifest] = await Promise.all([readFile(rendered.pagePath, 'utf8'), readFile(rendered.manifestPath, 'utf8')]);
  for (const [name, text] of [['page', page], ['manifest', manifest]]) for (const secret of [streamed, assigned]) expect([name, secret, text.includes(secret)]).toEqual([name, secret, false]);
  expect((await prepareReview(f.input, { cwd: f.repo, projects: f.projects })).redactions).toEqual({ value: 2 });
});

test('cookie headers dumped as JSON, a structured tool input and a response header are redacted end to end', async () => {
  const [sid, auth, input, header] = [synthetic(6), synthetic(7), synthetic(8), synthetic(10)];
  const f = await setup(reviewItem('headers', '```ref\nfile: request.json\n```\n\n```ref\ntranscript: 11111111-:2\nsummary: the call and its response\n```'));
  await f.write('request.json', `{"headers": {"Cookie": "locale=en; sid=${sid}; auth=${auth}"}}\n`);
  const call = (id: string, name: string, value: unknown) => ({ type: 'assistant' as const, model: 'claude-fable-5-1', content: [{ type: 'tool_use', id, name, input: value }] });
  const result = (id: string, text: string) => ({ type: 'user' as const, content: [{ type: 'tool_result', tool_use_id: id, content: text }] });
  await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
    prompt('Replay the request'),
    call('h1', 'mcp__http__fetch', { url: 'https://api.example.test/me', headers: { Cookie: `theme=dark; session=${input}` } }),
    result('h1', `HTTP/1.1 200 OK\nSet-Cookie: sid=${header}; Path=/; HttpOnly`),
  ]);
  const rendered = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'out') });
  const [page, manifest] = await Promise.all([readFile(rendered.pagePath, 'utf8'), readFile(rendered.manifestPath, 'utf8')]);
  for (const [name, text] of [['page', page], ['manifest', manifest]]) for (const secret of [sid, auth, input, header]) expect([name, secret, text.includes(secret)]).toEqual([name, secret, false]);
  expect((await prepareReview(f.input, { cwd: f.repo, projects: f.projects })).redactions).toEqual({ cookie: 3 });
});

test.each([
  ['a JWT in an item body', (s: string) => reviewItem('body', `Line one.\n\nThe token was ${['eyJ' + s, 'eyJ' + s, s].join('.')} in the log.`), 'body', 17, 'a JWT'],
  ['a secret in an authored JSON block', (s: string) => reviewItem('json', `Config:\n\n\`\`\`json\n{\n  "client_secret": "${s}"\n}\n\`\`\``), 'json', 19, 'a secret value'],
  ['a token in an authored link', (s: string) => reviewItem('link', `See [the callback](https://x.test/cb?a=1&token=${s}).`), 'link', 15, 'a secret URL parameter'],
])('%s fails the render at its line, naming only the kind', async (_name, item, id, line, kind) => {
  const secret = synthetic(9);
  const f = await setup(item(secret));
  const out = join(f.root, 'out');
  const error = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: out }).then(() => new Error('rendered'), (caught: unknown) => caught as Error);
  expect(error.message).toBe(`debrief: ${id}, line ${line}: The page would show ${kind}; remove it from the debrief file`);
  await expectNothingWritten(out, secret);
});

test('authored page and section titles are checked too', async () => {
  const secret = synthetic(10);
  const titled = await setup(reviewItem('a'));
  await writeFile(titled.input, reviewDocument(reviewItem('a')).replace('sections: [{ id: main, title: Main }]', `sections: [{ id: main, title: "Main https://x.test/invite/${secret}" }]`));
  await expect(prepareReview(titled.input, { cwd: titled.repo, projects: titled.projects })).rejects.toThrow('debrief: page, line 6: The page would show a link token');
  await writeFile(titled.input, reviewDocument(reviewItem('a')).replace('title: Fixture review', `title: "Fixture ${['eyJ' + secret, 'eyJ' + secret, secret].join('.')}"`));
  await expect(prepareReview(titled.input, { cwd: titled.repo, projects: titled.projects })).rejects.toThrow('debrief: page, line 2: The page would show a JWT');
});

test('the last guard reads the whole page: a secret in the footer fails assembly', async () => {
  const secret = synthetic(11);
  const f = await setup(reviewItem('a', 'Body.'));
  const prepared = await prepareReview(f.input, { cwd: f.repo, projects: f.projects });
  expect(() => assembleReview(prepared, { footerHtml: `<p>see https://x.test/cb?a=1&amp;token=${secret}</p>` })).toThrow('debrief: page, line 1: The page would show a secret URL parameter');
  expect(() => assembleReview(prepared, { footerHtml: '<p>fine</p>' })).not.toThrow();
});
