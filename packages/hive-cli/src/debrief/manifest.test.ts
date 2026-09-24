import { mkdir, readFile, readdir, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterEach, expect, test } from 'bun:test';
import { parseHTML } from 'linkedom';
import { FIXTURE_SESSION, createReviewFixture, expectSnapshot, reviewDocument, reviewItem, snapshotHtml } from './fixtures';
import { canonicalJson, manifestSchema, parseManifest, readManifest } from './manifest';
import { renderReview } from './render';

const REVIEW_ID = '22222222-2222-4222-8222-222222222222';
const RENDERED_AT = '2026-09-16T00:00:00.000Z';
const fixtures: Array<Awaited<ReturnType<typeof createReviewFixture>>> = [];
afterEach(async () => { await Promise.all(fixtures.splice(0).map((f) => f.cleanup())); });
async function setup(body = reviewItem('choice', '```ref\ndiff: source.ts\ncaption: Exact source\n```')) {
  const f = await createReviewFixture(); fixtures.push(f); await f.stamp();
  const input = join(f.root, 'debrief.md');
  await writeFile(input, reviewDocument(body));
  return { ...f, input, body };
}

test('items always start closed and round chips require a previous manifest', async () => {
  const f = await setup(reviewItem('choice', 'Original.'));
  const first = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round1') });
  const firstDom = parseHTML(await readFile(first.pagePath, 'utf8')).document;
  expect(firstDom.querySelectorAll('.item[open]')).toHaveLength(0);
  expect(firstDom.querySelectorAll('.item .tag-round')).toHaveLength(0);
  await writeFile(f.input, reviewDocument(reviewItem('choice', 'Updated.') + reviewItem('added', ''), 'round: 2\n'));
  const second = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round2'), previousManifestPath: first.manifestPath });
  const secondDom = parseHTML(await readFile(second.pagePath, 'utf8')).document;
  expect(secondDom.querySelectorAll('.item[open]')).toHaveLength(0);
  expect(secondDom.querySelector('#item-choice .tag-round')!.textContent).toBe('updated');
  expect(secondDom.querySelector('#item-added .tag-round')!.textContent).toBe('new');
});

test('manifests snapshot resolved evidence and hashes, not previous mutable sources', async () => {
  const f = await setup();
  const first = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round1') });
  const original = await readFile(first.manifestPath, 'utf8');
  const oldHash = first.manifest.items[0].hash;
  expect(first.manifest).toMatchObject({ session: FIXTURE_SESSION, round: 1, baseCommit: f.base, headCommit: f.base, history: {} });
  expect(first.manifest.items[0].evidence[0]).toMatchObject({ kind: 'diff', selector: 'source.ts', oldHash: expect.any(String), newHash: expect.any(String) });
  await f.write('source.ts', 'export const value = 3;\n');
  await writeFile(f.input, reviewDocument(f.body, 'round: 2\n'));
  const second = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round2'), previousManifestPath: first.manifestPath });
  expect(second.manifest.items[0].hash).not.toBe(oldHash);
  expect(second.manifest.reviewId).toBe(first.manifest.reviewId);
  expect(second.manifest.history[1].choice).toBe(oldHash);
  expect(await readFile(first.manifestPath, 'utf8')).toBe(original);
  expect(await readManifest(first.manifestPath)).toEqual(first.manifest);
  expect(await readFile(second.pagePath, 'utf8')).toContain('data-round-state="updated"');
  await writeFile(f.input, reviewDocument(f.body, 'round: 3\n'));
  const third = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round3'), previousManifestPath: second.manifestPath });
  expect(Object.keys(third.manifest.history)).toEqual(['1', '2']);
  expect(third.manifest.history[1].choice).toBe(oldHash);
  expect(third.manifest.items[0].hash).toBe(second.manifest.items[0].hash);
  expect(await readFile(third.pagePath, 'utf8')).toContain('data-round-state="unchanged"');
});

test('same-round rerender preserves reviewId but rejects identity and predecessor overwrites', async () => {
  const f = await setup(), outDir = join(f.root, 'round1');
  const first = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir });
  await f.write('source.ts', 'export const value = 2;\n');
  const rerender = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir });
  expect(rerender.manifest.reviewId).toBe(first.manifest.reviewId);
  expect(rerender.manifest.items[0].hash).not.toBe(first.manifest.items[0].hash);
  const before = await readFile(rerender.manifestPath, 'utf8');
  await writeFile(f.input, reviewDocument(f.body, 'round: 2\n'));
  await expect(renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir, previousManifestPath: rerender.manifestPath })).rejects.toThrow('overwrite');
  const alias = join(f.root, 'previous.json'); await symlink(rerender.manifestPath, alias);
  await expect(renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir, previousManifestPath: alias })).rejects.toThrow('overwrite');
  expect(await readFile(rerender.manifestPath, 'utf8')).toBe(before);
});

test('round validation fails closed and no partial snapshots are published on validation errors', async () => {
  const f = await setup(), outDir = join(f.root, 'round');
  await writeFile(f.input, reviewDocument(f.body, 'round: 2\n'));
  await expect(renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir })).rejects.toThrow('requires the manifest');
  expect(await readdir(outDir)).toEqual([]);
  await writeFile(f.input, reviewDocument(f.body));
  const first = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir });
  await expect(renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'new'), previousManifestPath: first.manifestPath })).rejects.toThrow('Round 1 must not');
  await writeFile(f.input, reviewDocument(f.body, 'round: 3\n'));
  await expect(renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'third'), previousManifestPath: first.manifestPath })).rejects.toThrow('immediately preceding');
  const other = { ...first.manifest, session: '33333333-3333-4333-8333-333333333333' };
  const wrong = join(f.root, 'wrong.json'); await writeFile(wrong, JSON.stringify(other));
  await writeFile(f.input, reviewDocument(f.body, 'round: 2\n'));
  await expect(renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'second'), previousManifestPath: wrong })).rejects.toThrow('same session');
});

test('manifest schemas reject corrupted history/duplicate ids and hash order is deterministic', async () => {
  const f = await setup();
  const first = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round1') });
  expect(manifestSchema.safeParse({ ...first.manifest, items: [...first.manifest.items, first.manifest.items[0]] }).success).toBe(false);
  expect(manifestSchema.safeParse({ ...first.manifest, round: 3, history: { 1: {} } }).success).toBe(false);
  expect(() => parseManifest('{broken', 'broken.json')).toThrow('Invalid debrief manifest');
  expect(canonicalJson({ z: 1, a: { y: 2, b: 3 } })).toBe(canonicalJson({ a: { b: 3, y: 2 }, z: 1 }));
});

test('rename/disposition errors are contextual and a dropped item yields warning plus footer', async () => {
  const f = await setup(reviewItem('old') + reviewItem('gone'));
  const first = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round1') });
  await writeFile(f.input, reviewDocument(reviewItem('new', '', 'was: old\n'), 'round: 2\n'));
  const second = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round2'), previousManifestPath: first.manifestPath });
  expect(second.warnings).toHaveLength(1);
  expect(second.warnings[0]).toContain('gone');
  expect(await readFile(second.pagePath, 'utf8')).toContain('data-disposition="missing"');
  await writeFile(f.input, reviewDocument(reviewItem('new', '', 'was: absent\n'), 'round: 2\n'));
  await expect(renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'invalid'), previousManifestPath: first.manifestPath })).rejects.toThrow('new, line 1');
});

test('full manifested render golden is deterministic (UPDATE_SNAPSHOTS=1)', async () => {
  const f = await setup(reviewItem('choice', '```ref\ndiff: source.ts\nfocus: L1\nold-focus: L1\ncaption: Exact change\n```', 'judgement-call: true\nalternatives: [Keep the old behavior]\n'));
  await f.write('source.ts', 'export const value = 2;\n');
  const result = await renderReview(f.input, { cwd: f.repo, projects: f.projects, outDir: join(f.root, 'round1') });
  for (const [name, source] of [['manifest.json', result.manifestPath], ['manifested-page.html', result.pagePath]]) {
    await expectSnapshot(name, snapshotHtml((await readFile(source, 'utf8')).replaceAll(result.manifest.reviewId, REVIEW_ID).replaceAll(result.manifest.renderedAt, RENDERED_AT)));
  }
});
