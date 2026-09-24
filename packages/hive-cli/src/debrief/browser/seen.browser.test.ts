/**
 * Seen marks in the artifact database, in headless Chromium, with `window.claude` mocked: an in-memory `db` and a
 * `user` whose id is "viewer". The page without `window.claude` is covered by the other suites.
 *
 * Run: REVIEW_BROWSER_TESTS=1 bun test src/debrief/browser
 */
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { reviewDocument, reviewItem } from '../fixtures';
import { BROWSER_SKIP_REASON, browserTestsEnabled, startPageHarness } from './harness';
import type { BrowserHarness, OpenPage, Page } from './harness';

const title = browserTestsEnabled ? 'seen marks in the artifact database, headless Chromium' : `seen marks in the artifact database (skipped: ${BROWSER_SKIP_REASON})`;
if (!browserTestsEnabled) console.info(`seen.browser.test.ts skipped: ${BROWSER_SKIP_REASON}`);
let served: Awaited<ReturnType<typeof startPageHarness>> | undefined;
let harness: BrowserHarness;

/**
 * A mock of the runtime: `use("db")` and `use("user")` resolve a store kept on `window.__db`, seeded from
 * `window.__seed` when it is first used; `window.__db.fail` queues rejection codes for the next writes;
 * `window.__noDb` makes `use("db")` resolve null.
 */
const MOCK = `(() => {
  const state = { docs: {}, writes: [], attempts: 0, fail: [], seeded: false };
  window.__db = state;
  const seed = () => { if (!state.seeded) { state.seeded = true; if (window.__seed) state.docs['seen/viewer'] = window.__seed; } };
  const snap = (path) => ({ exists: path in state.docs, data: () => state.docs[path], metadata: { hasPendingWrites: false } });
  const db = { doc: (path) => ({
    set: (data) => { state.attempts++; const code = state.fail.shift(); if (code) return Promise.reject({ code, message: code }); state.writes.push(JSON.parse(JSON.stringify(data))); state.docs[path] = data; return Promise.resolve(); },
    onSnapshot: (next) => { seed(); setTimeout(() => next(snap(path)), 30); return () => {}; },
  }) };
  window.claude = { use: (name) => new Promise((done) => setTimeout(() => done(name === 'db' ? (window.__noDb ? null : db) : name === 'user' ? { id: () => Promise.resolve('viewer') } : null), 10)) };
})();`;

let visit = 0;
async function open(inits: Array<string> = [MOCK]): Promise<OpenPage> {
  const opened = await harness.open({ theme: 'light', viewport: { width: 1280, height: 800 } });
  for (const init of inits) await opened.page.addInitScript(init);
  await load(opened.page);
  return opened;
}
async function load(page: Page): Promise<void> {
  await page.goto(`${harness.origin}/page.html?v=${++visit}`);
  await page.waitForFunction(() => document.readyState === 'complete');
  await page.waitForTimeout(150);
}
const db = (page: Page) => page.evaluate(() => (window as unknown as { __db: { docs: Record<string, unknown>; writes: Array<{ items: Record<string, { hash: string | null }>; migrated?: true }>; attempts: number } }).__db);
const seenBoxes = (page: Page) => page.evaluate(() => [...document.querySelectorAll('details.item')].filter((d) => (d.querySelector('.seen input') as HTMLInputElement).checked).map((d) => d.getAttribute('data-item')));
const reviewId = (page: Page) => page.evaluate(() => document.querySelector('main')!.getAttribute('data-review')!);
const seed = (doc: unknown) => `window.__seed = ${JSON.stringify(doc)};`;
const mark = (hash: string | null) => ({ hash, round: 1, heading: 'Changed choice', at: '2026-09-30T00:00:00.000Z' });

describe.skipIf(!browserTestsEnabled)(title, () => {
  beforeAll(async () => {
    served = await startPageHarness();
    harness = served.harness;
    await served.fixture.stamp();
    await served.render(reviewDocument(reviewItem('choice', 'Why.') + reviewItem('other', 'Why not.')));
  }, 60_000);
  afterAll(async () => { await served?.close(); });

  test('an empty database takes the browser\'s earlier marks in one migration write', async () => {
    const { page, errors, close } = await open();
    expect((await db(page)).writes).toEqual([]);
    const id = await reviewId(page);
    // A browser that last ran the earlier page holds only its keys.
    await page.evaluate((reviewKey) => { localStorage.clear(); localStorage.setItem(`review-ui:${reviewKey}`, JSON.stringify({ seen: { choice: true } })); }, id);
    await load(page);
    await page.waitForTimeout(600);
    const { writes } = await db(page);
    expect(writes).toHaveLength(1);
    expect([writes[0].migrated, Object.keys(writes[0].items)]).toEqual([true, ['choice']]);
    expect(await seenBoxes(page)).toEqual(['choice']);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('the database document sets the boxes and is mirrored locally; a mark on an older version shows updated and unseen', async () => {
    const first = await open();
    const id = await reviewId(first.page);
    await first.close();
    const { page, errors, close } = await open([MOCK, seed({ review: id, items: { choice: mark(null), other: mark('an older hash') } })]);
    expect(await seenBoxes(page)).toEqual(['choice']);
    expect(await page.evaluate(() => [document.querySelector('#item-other .review-stale')?.textContent, document.querySelector('#item-other .review-stale')?.getAttribute('title'), document.querySelector('#item-choice .review-stale')])).toEqual(['updated', 'Changed since you marked it seen', null]);
    const mirrored = await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!) as { items: object }, `review-seen:${id}`);
    expect(Object.keys(mirrored.items).sort()).toEqual(['choice', 'other']);
    await page.locator('#seen-other').click();
    expect(await page.evaluate(() => document.querySelector('#item-other .review-stale'))).toBeNull();
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('rapid toggles make one write, holding the final state and the item hash', async () => {
    const { page, errors, close } = await open();
    await page.locator('#seen-choice').click();
    await page.locator('#seen-other').click();
    await page.locator('#seen-choice').click();
    await page.locator('#seen-choice').click();
    await page.waitForTimeout(700);
    const { writes } = await db(page);
    const hash = await page.evaluate(() => (JSON.parse(document.getElementById('review-manifest')!.textContent) as { items: Array<{ id: string; hash: string }> }).items.find((i) => i.id === 'choice')!.hash);
    expect(writes.filter((w) => Object.keys(w.items).length).length).toBe(1);
    expect(writes.at(-1)!.items.choice.hash).toBe(hash);
    expect(Object.keys(writes.at(-1)!.items).sort()).toEqual(['choice', 'other']);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('a refused write keeps the marks local and is not retried', async () => {
    const { page, errors, close } = await open();
    await page.evaluate(() => { (window as unknown as { __db: { fail: Array<string> } }).__db.fail.push('invalid_argument'); });
    await page.locator('#seen-choice').click();
    await page.waitForTimeout(700);
    await page.locator('#seen-other').click();
    await page.waitForTimeout(700);
    const state = await db(page);
    expect([state.writes.filter((w) => Object.keys(w.items).length).length, state.attempts > 0]).toEqual([0, true]);
    const attempts = state.attempts;
    await page.locator('#seen-other').click();
    await page.waitForTimeout(700);
    expect((await db(page)).attempts).toBe(attempts);
    expect(await seenBoxes(page)).toEqual(['choice']);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('use() resolving null leaves the page as it is without the runtime', async () => {
    const { page, errors, close } = await open(['window.__noDb = true;', MOCK]);
    await page.locator('#seen-choice').click();
    await load(page);
    expect(await seenBoxes(page)).toEqual(['choice']);
    expect((await db(page)).writes).toEqual([]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('one change chip per item: the since-last-visit chip hides the round tag and the stale-seen chip', async () => {
    const { page, errors, close } = await open();
    const shown = await page.evaluate(() => {
      const item = document.getElementById('item-choice')!, meta = item.querySelector('.item-meta')!;
      meta.insertAdjacentHTML('beforeend', '<span class="tag tag-round">updated</span><span class="tag tag-round review-stale">updated</span><span class="tag review-unseen">changed</span>');
      const visible = () => [...meta.children].filter((c) => getComputedStyle(c).display !== 'none').map((c) => c.className);
      const before = visible();
      item.setAttribute('data-unseen', 'true');
      return [before.length, visible()];
    });
    expect(shown).toEqual([3, ['tag review-unseen']]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('a second device with a fresh browser shows the marks the first one wrote', async () => {
    const first = await open();
    await first.page.locator('#seen-other').click();
    await first.page.waitForTimeout(700);
    const written = (await db(first.page)).docs['seen/viewer'];
    await first.close();
    const second = await open([MOCK, seed(written)]);
    expect(await seenBoxes(second.page)).toEqual(['other']);
    expect(second.errors).toEqual([]);
    await second.close();
  }, 30_000);
});
