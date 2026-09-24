import { runInNewContext } from 'node:vm';
import { expect, test } from 'bun:test';
import { parseHTML } from 'linkedom';
import { FIXTURE_SESSION } from './fixtures';
import { compareRounds, computeSeen, seenOverlayScript } from './rounds';
import { seenStoreScript } from './seen';
import type { ReviewManifest } from './manifest';

const ID = '22222222-2222-4222-8222-222222222222';
const TIME = '2026-09-16T00:00:00.000Z';
const A = 'a'.repeat(64), B = 'b'.repeat(64), C = 'c'.repeat(64);
function manifest(round: number, entries: Array<[string, string, string?]>): ReviewManifest {
  return { reviewId: ID, session: FIXTURE_SESSION, round, baseCommit: 'a'.repeat(40), headCommit: 'b'.repeat(40), items: entries.map(([id, hash, was]) => ({ id, hash, ...(was ? { was } : {}), kind: 'code', heading: `Changed ${id}`, evidence: [] })), dispositions: {}, history: Object.fromEntries(Array.from({ length: round - 1 }, (_, i) => [i + 1, {}])), renderedAt: TIME, rendererVersion: '1' };
}

test('round changes honor ids/renames and require valid dispositions', () => {
  const before = manifest(1, [['same', A], ['renamed', A], ['resolved', A], ['superseded', A], ['withdrawn', A], ['constructor', A]]);
  const current = manifest(2, [['same', A], ['new-name', B, 'renamed'], ['new', C]]);
  current.dispositions = { resolved: 'resolved', superseded: 'superseded: new', withdrawn: 'withdrawn' };
  const comparison = compareRounds(current, before);
  expect(comparison.states).toEqual({ same: 'unchanged', 'new-name': 'updated', new: 'new' });
  expect(comparison.footerHtml).toContain('data-disposition="resolved"');
  expect(comparison.footerHtml).toContain('href="#item-new"');
  expect(comparison.footerHtml).toContain('data-disposition="withdrawn"');
  expect(comparison.warnings).toHaveLength(1);
  expect(comparison.warnings[0]).toContain('constructor');
  current.dispositions.same = 'resolved';
  expect(() => compareRounds(current, before)).toThrow('Disposition must name a dropped');
});

test('ambiguous rename claims and nonexistent successors hard-error', () => {
  const before = manifest(1, [['old', A], ['other', B]]);
  expect(() => compareRounds(manifest(2, [['first', A, 'old'], ['second', A, 'old']]), before)).toThrow('was must identify');
  expect(() => compareRounds(manifest(2, [['old', A], ['new', A, 'old']]), before)).toThrow('was must identify');
  expect(() => compareRounds(manifest(2, [['other', A, 'old']]), before)).toThrow('was must identify');
  const after = manifest(2, [['new', A]]); after.dispositions.old = 'superseded: absent';
  expect(() => compareRounds(after, before)).toThrow('Superseded item');
  expect(() => compareRounds(manifest(3, []), before)).toThrow('immediately preceding');
});

test('seen state handles first visit, reload, next and skipped rounds, and older revisits', () => {
  const first = manifest(1, [['same', A], ['changed', A]]);
  const initial = computeSeen(first, null, TIME);
  expect(initial.unseen).toEqual([]);
  expect(initial.next?.highestRound).toBe(1);
  expect(computeSeen(first, initial.next, TIME).unseen).toEqual([]);
  const second = manifest(2, [['same', A], ['changed', B], ['new', C]]);
  second.history[1] = Object.fromEntries(first.items.map((item) => [item.id, item.hash]));
  const next = computeSeen(second, initial.next, TIME);
  expect(next.unseen).toEqual(['changed', 'new']);
  expect(next.added).toEqual(['new']);
  expect(computeSeen(second, next.next, TIME).unseen).toEqual([]);
  const third = manifest(3, [['same', A], ['changed', C]]); third.history[1] = second.history[1];
  expect(computeSeen(third, initial.next, TIME).unseen).toEqual(['changed']);
  expect(computeSeen(first, next.next, TIME)).toEqual({ unseen: [] });
  expect(computeSeen(second, { ...initial.next, reviewId: 'other' }, TIME).unseen).toEqual([]);
});

test('seen hashes rather than re-rendered history are authoritative', () => {
  const first = manifest(1, [['a', A]]);
  const store = computeSeen(first, null, TIME).next;
  const current = manifest(3, [['a', B]]);
  current.history[1] = { a: B }; // Later rerender of round1 was not what the reader saw.
  expect(computeSeen(current, store, TIME).unseen).toEqual(['a']);
  const renamed = manifest(2, [['b', A, 'a']]); renamed.history[1] = { a: A };
  expect(computeSeen(renamed, store, TIME).unseen).toEqual([]);
  expect(computeSeen(current, { ...store, itemHashes: {} }, TIME).unseen).toEqual(['a']);
});

function storage() {
  const values = new Map<string, string>();
  return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}
function runOverlay(page: ReviewManifest, store: Pick<Storage, 'getItem' | 'setItem'>) {
  const { document } = parseHTML(`<script id="review-manifest" type="application/json">${JSON.stringify(page)}</script>${page.items.map((item) => `<details data-item="${item.id}"><summary>${item.heading}<span class="item-meta"></span></summary></details>`).join('')}`);
  let revisit: ((event: { persisted: boolean }) => void) | undefined;
  const window = { localStorage: store, addEventListener: (_name: string, listener: typeof revisit) => { revisit = listener; } };
  runInNewContext(seenStoreScript() + seenOverlayScript(), { document, window });
  return { document, revisit: () => revisit?.({ persisted: true }) };
}
const storedVisit = (store: ReturnType<typeof storage>) => store.values.get(`review-seen:${ID}`) && JSON.stringify((JSON.parse(store.values.get(`review-seen:${ID}`)!) as { visit?: unknown }).visit);

test('actual bundled overlay writes once, marks updates, and older pages never write', () => {
  const store = storage(), first = manifest(1, [['a', A]]), second = manifest(2, [['a', B]]);
  expect(runOverlay(first, store).document.querySelectorAll('[data-unseen]').length).toBe(0);
  const newPage = runOverlay(second, store);
  expect(newPage.document.querySelector('[data-item]')?.getAttribute('data-unseen')).toBe('true');
  const chip = newPage.document.querySelector('.item-meta > .review-unseen');
  expect([chip?.textContent, chip?.getAttribute('title')]).toEqual(['changed', 'Changed since you last opened this page']);
  const before = storedVisit(store);
  expect(runOverlay(first, store).document.querySelectorAll('[data-unseen]').length).toBe(0);
  expect(storedVisit(store)).toBe(before);
  expect(runOverlay(second, store).document.querySelectorAll('[data-unseen]').length).toBe(0);
  const third = manifest(3, [['a', B], ['b', A]]); third.history[1] = { a: A }; third.history[2] = { a: B };
  const added = runOverlay(third, store).document.querySelector('[data-item="b"] .review-unseen');
  expect([added?.textContent, added?.getAttribute('title')]).toEqual(['new', 'Not on the page when you last opened it']);
});

test('a lower-priority row keeps its meta slot, so the unseen chip lands on it', async () => {
  const { assembleReview, prepareReview } = await import('./render');
  const { createReviewFixture, reviewDocument, reviewItem } = await import('./fixtures');
  const { writeFile } = await import('node:fs/promises');
  const { join } = await import('node:path');
  const f = await createReviewFixture();
  try {
    await f.stamp();
    const input = join(f.root, 'debrief.md');
    await writeFile(input, reviewDocument(reviewItem('main-row', 'Body.') + reviewItem('quiet-row', 'Body.', 'lower-priority: true\n'), 'round: 2\n'));
    const html = assembleReview(await prepareReview(input, { cwd: f.repo, projects: f.projects }), { states: { 'main-row': 'unchanged', 'quiet-row': 'updated' } }).html;
    const { document } = parseHTML(html);
    expect(document.querySelector('#item-quiet-row .item-meta')!.textContent).toBe('updated');
    const second = manifest(2, [['main-row', A], ['quiet-row', B]]);
    second.history[1] = { 'main-row': A, 'quiet-row': A };
    const store = storage(); store.values.set(`review:${ID}`, JSON.stringify(computeSeen(manifest(1, [['main-row', A], ['quiet-row', A]]), null, TIME).next));
    const page = parseHTML(`<script id="review-manifest" type="application/json">${JSON.stringify(second)}</script>${document.querySelector('.page')!.outerHTML}`).document;
    runInNewContext(seenStoreScript() + seenOverlayScript(), { document: page, window: { localStorage: store, addEventListener: () => {} } });
    expect(page.querySelector('#item-quiet-row .item-meta > .review-unseen')?.textContent).toBe('changed');
  } finally { await f.cleanup(); }
});

test('storage denial and invalid JSON leave no marks; a stored visit still marks when writes are denied; BFCache clears old marks', () => {
  const first = manifest(1, [['a', A]]), second = manifest(2, [['a', B]]), third = manifest(3, [['a', C]]);
  const denied = () => { throw new Error('Storage denied'); };
  expect(runOverlay(second, { getItem: denied, setItem: denied }).document.querySelectorAll('[data-unseen]').length).toBe(0);
  const prior = JSON.stringify(computeSeen(first, null, TIME).next);
  expect(runOverlay(second, { getItem: (key) => (key === `review:${ID}` ? prior : null), setItem: denied }).document.querySelectorAll('[data-unseen]').length).toBe(1);
  expect(runOverlay(second, { getItem: () => '{bad', setItem: denied }).document.querySelectorAll('[data-unseen]').length).toBe(0);
  const store = storage(); runOverlay(first, store);
  const page = runOverlay(second, store); expect(page.document.querySelectorAll('[data-unseen]').length).toBe(1);
  runOverlay(third, store); const latest = storedVisit(store);
  page.revisit();
  expect(page.document.querySelectorAll('[data-unseen]').length).toBe(0);
  expect(storedVisit(store)).toBe(latest);
});
