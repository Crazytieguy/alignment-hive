/**
 * The lower-priority tray in headless Chromium: opening it, seen dots and counts,
 * the rail's one line, links and hashes into it, and its state across reloads.
 *
 * Run: REVIEW_BROWSER_TESTS=1 bun test src/debrief/browser
 */
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { reviewDocument, reviewItem } from '../fixtures';
import { BROWSER_SKIP_REASON, browserTestsEnabled, startPageHarness } from './harness';
import type { BrowserHarness, OpenPage, Page } from './harness';

const title = browserTestsEnabled ? 'lower-priority tray in headless Chromium' : `lower-priority tray in headless Chromium (skipped: ${BROWSER_SKIP_REASON})`;
if (!browserTestsEnabled) console.info(`tray.browser.test.ts skipped: ${BROWSER_SKIP_REASON}`);
let served: Awaited<ReturnType<typeof startPageHarness>> | undefined;
let harness: BrowserHarness;
const filler = Array.from({ length: 20 }, (_, i) => `Paragraph ${i + 1}, long enough to push the tray below the fold.`).join('\n\n');

let visit = 0;
async function open(hash = '', width = 1280): Promise<OpenPage> {
  const opened = await harness.open({ theme: 'light', viewport: { width, height: 800 } });
  await opened.page.goto(`${harness.origin}/page.html?v=${++visit}${hash}`);
  await opened.page.waitForFunction(() => document.readyState === 'complete');
  return opened;
}
const tray = (page: Page) => page.evaluate(() => {
  const rest = document.getElementById('rest-main') as HTMLDetailsElement;
  return { open: rest.open, done: rest.classList.contains('rest-done'), dots: [...rest.querySelectorAll('.rest-dots i')].map((i) => i.classList.contains('on')), text: rest.querySelector('.rest-seen-t')!.textContent };
});
const isOpen = (page: Page, id: string) => page.evaluate((target) => (document.getElementById(target) as HTMLDetailsElement).open, id);

describe.skipIf(!browserTestsEnabled)(title, () => {
  beforeAll(async () => {
    served = await startPageHarness();
    harness = served.harness;
    await served.fixture.stamp();
    const quiet = (id: string, nav?: string) => reviewItem(id, 'Details.', `lower-priority: true\n${nav ? `nav: ${nav}\n` : ''}`);
    const items = reviewItem('lead', filler) + quiet('quiet-one', 'The first') + quiet('quiet-two') + reviewItem('after', filler).replace('section: main', 'section: checked');
    await served.render(reviewDocument(items, 'stats: [{ n: 1, label: quiet, item: quiet-two }]\n').replace('story: The change, reviewed.', 'story: "The change; see [the first](#item-quiet-one)."'));
  }, 60_000);
  afterAll(async () => { await served?.close(); });

  test('the tray opens and closes by mouse and keyboard; ticking its rows updates dots and count, with no mark-all', async () => {
    const { page, errors, close } = await open();
    expect(await tray(page)).toEqual({ open: false, done: false, dots: [false, false], text: '0 of 2 seen' });
    await page.locator('#rest-main > summary').click();
    expect((await tray(page)).open).toBe(true);
    await page.locator('#rest-main > summary').evaluate((s) => s.focus());
    await page.keyboard.press('Enter');
    expect((await tray(page)).open).toBe(false);
    await page.keyboard.press('Enter');
    await page.locator('#seen-quiet-two').click();
    expect(await tray(page)).toEqual({ open: true, done: false, dots: [false, true], text: '1 of 2 seen' });
    await page.locator('#seen-quiet-one').click();
    expect(await tray(page)).toEqual({ open: true, done: true, dots: [true, true], text: '2 of 2 seen' });
    expect(await page.evaluate(() => document.querySelectorAll('.rest button, .rest [data-mark-all]').length)).toBe(0);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('the rail lists main items and one line for the tray, which opens it; an active quiet row lights that line', async () => {
    const { page, errors, close } = await open();
    const rail = await page.evaluate(() => [...document.querySelectorAll('#nav .nav-items a')].map((a) => [a.textContent, a.getAttribute('href'), a.classList.contains('nav-rest')]));
    expect(rail).toEqual([['Changed lead', '#item-lead', false], ['+ 2 lower priority', '#rest-main', true], ['Changed after', '#item-after', false]]);
    await page.locator('#nav a.nav-rest').click();
    expect((await tray(page)).open).toBe(true);
    // Room below, so the page end (where the last item is always active) is far away.
    await page.evaluate(() => { (document.getElementById('item-after') as HTMLDetailsElement).open = true; const row = document.getElementById('item-quiet-two')!; row.scrollIntoView({ block: 'start' }); scrollBy(0, -innerHeight * 0.15); });
    await page.waitForFunction(() => document.querySelector('#nav a.active')?.getAttribute('href') === '#rest-main');
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('a hash, a stat and a story link to a quiet row each open its tray and the row', async () => {
    const hashed = await open('#item-quiet-two');
    expect(['hash', await isOpen(hashed.page, 'rest-main'), await isOpen(hashed.page, 'item-quiet-two')]).toEqual(['hash', true, true]);
    expect(hashed.errors).toEqual([]);
    await hashed.close();
    for (const link of ['.stats a[href="#item-quiet-two"]', '.story a[href="#item-quiet-one"]']) {
      const { page, errors, close } = await open();
      await page.locator(link).click();
      const target = link.includes('quiet-two') ? 'item-quiet-two' : 'item-quiet-one';
      // hashchange, which opens the row, fires a task after the click.
      await page.waitForFunction((id: string) => (document.getElementById(id) as HTMLDetailsElement).open, target);
      expect([link, await isOpen(page, 'rest-main'), await isOpen(page, target)]).toEqual([link, true, true]);
      expect(errors).toEqual([]);
      await close();
    }
  }, 30_000);

  test('the tray stays open across a reload', async () => {
    const { page, errors, close } = await open();
    await page.locator('#rest-main > summary').click();
    await page.waitForTimeout(100);
    await page.reload();
    await page.waitForFunction(() => document.readyState === 'complete');
    expect((await tray(page)).open).toBe(true);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test.each([1280, 400])('at %ipx a quiet row\'s empty meta slot takes no room; a round chip lands in it', async (width) => {
    const { page, errors, close } = await open('', width);
    await page.locator('#rest-main > summary').click();
    const layout = () => page.evaluate(() => {
      const summary = document.querySelector('#item-quiet-one > summary')!, box = (s: string) => summary.querySelector(s)!.getBoundingClientRect();
      return { meta: getComputedStyle(summary.querySelector('.item-meta')!).display, seen: [Math.round(box('.seen').left), Math.round(box('.seen').right)], head: Math.round(box('.item-h').right), metaLeft: Math.round(box('.item-meta').left), metaTop: Math.round(box('.item-meta').top), headBottom: Math.round(box('.item-h').bottom) };
    });
    const empty = await layout();
    expect(empty.meta).toBe('none');
    await page.evaluate(() => { document.querySelector('#item-quiet-one .item-meta')!.innerHTML = '<span class="tag review-unseen">unseen</span>'; });
    const chip = await layout();
    expect(chip.meta).toBe('flex');
    // On a desktop the row is one line and the chip sits between heading and seen box, which stays put;
    // on a phone the row stacks like a main item's, the chip under the heading.
    if (width === 1280) expect([chip.seen[1], chip.metaLeft >= chip.head]).toEqual([empty.seen[1], true]);
    else expect([chip.seen[0], chip.metaTop >= chip.headBottom]).toEqual([empty.seen[0], true]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);
});
