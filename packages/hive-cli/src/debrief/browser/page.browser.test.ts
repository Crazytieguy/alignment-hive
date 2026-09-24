/**
 * A rendered review page in headless Chromium: folds and the controls inside
 * their headers, plain rows, the contents rail, theme and client-side times.
 *
 * Run: REVIEW_BROWSER_TESTS=1 bun test src/debrief/browser
 */
import { copyFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { FIXTURE_SESSION, prompt, reply, reviewDocument, reviewItem, writeTranscript } from '../fixtures';
import { reviewTimes } from '../times';
import { frames } from './geometry';
import { BROWSER_SKIP_REASON, browserTestsEnabled, startPageHarness } from './harness';
import type { BrowserHarness, OpenPage, Page } from './harness';

const title = browserTestsEnabled ? 'review page in headless Chromium' : `review page in headless Chromium (skipped: ${BROWSER_SKIP_REASON})`;
if (!browserTestsEnabled) console.info(`page.browser.test.ts skipped: ${BROWSER_SKIP_REASON}`);
let served: Awaited<ReturnType<typeof startPageHarness>> | undefined;
let harness: BrowserHarness;
const output = Array.from({ length: 40 }, (_, i) => `line ${i + 1}`).join('\n');
const IMAGES = join(import.meta.dir, '../fixtures/images');
const WIDE = 'page-2026-09-16T03-04-05-123Z.png', THUMB = 'shot-2026-09-16T03-10-00-000Z-thumb.png';
const LONG_ASK = `Tidy the guide and ${'keep every heading a reader links to, '.repeat(8)}then run the tests.\nA second line.`;
const longReply = Array.from({ length: 60 }, (_, i) => `Paragraph ${i + 1} of a long reply.`).join('\n\n');

const body = [
  '```ref\nfile: notes.md\n```',
  '```ref\ntranscript: 11111111:2\nsummary: The long run\n```',
  '```ref\ngit: true\n```',
].join('\n\n');

async function open(options: { width?: number; timezone?: string } = {}): Promise<OpenPage> {
  const opened = await harness.open({ theme: 'light', viewport: { width: options.width ?? 1280, height: 900 }, timezone: options.timezone });
  await opened.page.goto(`${harness.origin}/page.html`);
  await opened.page.waitForFunction(() => document.readyState === 'complete');
  return opened;
}
const isOpen = (element: HTMLElement) => (element.closest('details') as HTMLDetailsElement).open;

describe.skipIf(!browserTestsEnabled)(title, () => {
  beforeAll(async () => {
    served = await startPageHarness();
    const { fixture } = served;
    harness = served.harness;
    await fixture.stamp();
    await fixture.git('add', '.'); await fixture.git('commit', '-qm', 'Notes');
    await writeTranscript(join(fixture.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
      prompt('Run it'),
      { type: 'assistant', model: 'claude-fable-5-1', content: [{ type: 'tool_use', id: 'b1', name: 'Bash', input: { command: 'seq 40', description: 'Print forty lines' } }] },
      { type: 'user', content: [{ type: 'tool_result', tool_use_id: 'b1', content: output }] },
      reply(longReply, 'claude-fable-5'),
      prompt(LONG_ASK),
      prompt('Later', '2026-09-16T08:00:00.000Z'),
      prompt('A side note, the next day', '2026-09-17T09:00:00.000Z'),
    ]);
    const plain = reviewItem('plain').replace('section: main', 'section: side-effects').replace('lede: Preserve compatibility', 'lede: "See [the choice](#item-choice)"') + reviewItem('choice', 'Why.', 'judgement-call: true\nalternatives: [Other]\nnav: The choice\n');
    const replyItem = reviewItem('reply', '```ref\ntranscript: 11111111:3\nsummary: The long reply\n```');
    for (const name of [WIDE, THUMB]) await copyFile(join(IMAGES, name), join(fixture.root, name));
    const images = reviewItem('images', [`image: ${WIDE}\nsummary: The page, wide`, `image: ${THUMB}\nsummary: A thumbnail`, `image: ${WIDE}\nsummary: The page at a phone's width\nwidth: 390`].map((ref) => '```ref\n' + ref + '\n```').join('\n\n'));
    await served.render(reviewDocument(reviewItem('evidence', body) + plain + replyItem + images, 'asks: ["11111111:1", "11111111:4", "11111111:5"]\nforyou: [Re-login]\n'));
  }, 60_000);
  afterAll(async () => { await served?.close(); });

  test('every evidence fold starts closed and its header toggles it', async () => {
    const { page, errors, close } = await open();
    await page.locator('#item-evidence > summary').click();
    const folds = await page.evaluate(() => [...document.querySelectorAll('#item-evidence .item-body details')].map((d) => [d.className, (d as HTMLDetailsElement).open]));
    expect(folds).toEqual([['ev-box fv', false], ['tr-entry', false], ['git-commit', false]]);
    for (const header of ['#item-evidence .fv-head', '#item-evidence .tr-entry > summary', '#item-evidence .git-commit-h']) {
      await page.locator(header).click();
      expect(await page.locator(header).evaluate(isOpen)).toBe(true);
      await page.locator(header).click();
      expect(await page.locator(header).evaluate(isOpen)).toBe(false);
    }
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('the source toggle switches the view without folding, by mouse and keyboard', async () => {
    const { page, errors, close } = await open();
    await page.locator('#item-evidence > summary').click();
    // Folded, the toggle is invisible but keeps its box, so opening the view moves nothing in its header.
    expect(await page.locator('#item-evidence .fv-toggle').evaluate((b) => getComputedStyle(b).visibility)).toBe('hidden');
    await page.locator('#item-evidence .fv-head').click();
    const view = () => page.evaluate(() => {
      const fv = document.querySelector('#item-evidence details.fv') as HTMLDetailsElement;
      return [fv.open, (fv.querySelector('.fv-md') as HTMLElement).hidden, (fv.querySelector('.fv-src') as HTMLElement).hidden, fv.querySelector('.fv-toggle')!.textContent];
    });
    expect(await view()).toEqual([true, false, true, 'View source']);
    await page.locator('#item-evidence .fv-toggle').click();
    expect(await view()).toEqual([true, true, false, 'View rendered']);
    await page.locator('#item-evidence .fv-toggle').evaluate((b) => b.focus());
    await page.keyboard.press('Enter');
    expect(await view()).toEqual([true, false, true, 'View source']);
    await page.keyboard.press('Space');
    expect(await view()).toEqual([true, true, false, 'View rendered']);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('a long reply clips behind Show all; Show less from far below brings its button back into view', async () => {
    const { page, errors, close } = await open();
    await page.locator('#item-reply > summary').click();
    await page.locator('#item-reply .tr-entry > summary').click();
    const box = '#item-reply .tr-clipbox';
    const state = () => page.locator(box).evaluate((b) => [b.classList.contains('clip'), b.clientHeight < b.scrollHeight, b.parentElement!.querySelector('.ev-more')!.textContent]);
    expect(await state()).toEqual([true, true, 'Show all']);
    await page.locator('#item-reply .ev-more').click();
    expect(await state()).toEqual([false, false, 'Show less']);
    // Read to the end, then collapse from there: the button would end far above the screen.
    await page.locator('#item-reply .ev-more').evaluate((b) => { scrollTo(0, scrollY + b.getBoundingClientRect().top - 100); });
    await page.locator('#item-reply .ev-more').click();
    expect(await state()).toEqual([true, true, 'Show all']);
    const top = await page.locator('#item-reply .ev-more').evaluate((b) => b.getBoundingClientRect().top);
    expect(top).toBeGreaterThanOrEqual(0);
    expect(top).toBeLessThan(900);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('an image in a closed fold does not zoom until every fold around it is open', async () => {
    const { page, errors, close } = await open();
    const wide = () => page.evaluate(() => { const img = document.querySelector<HTMLImageElement>('#item-images img')!; return [img.classList.contains('zoomable'), img.tabIndex]; });
    // Its own fold open (as saved state restores it) inside a closed item: hidden, so not a control yet.
    await page.evaluate(() => { document.querySelectorAll<HTMLDetailsElement>('#item-images details.shot').forEach((d) => { d.open = true; }); });
    await frames(page);
    expect(await wide()).toEqual([false, -1]);
    await page.locator('#item-images > summary').click();
    await frames(page);
    expect(await wide()).toEqual([true, 0]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test.each([1280, 400])('at %ipx an image wider than its column zooms by mouse and keyboard; a small one doesn\'t; a preferred width is kept', async (width) => {
    const { page, errors, close } = await open({ width });
    await page.locator('#item-images > summary').click();
    await page.evaluate(() => document.querySelectorAll<HTMLDetailsElement>('#item-images details.shot').forEach((d) => { d.open = true; }));
    await page.waitForFunction(() => [...document.querySelectorAll<HTMLImageElement>('#item-images img')].every((img) => img.complete));
    // A fold's toggle event, which re-checks its images, fires a task after it opens.
    await frames(page);
    const shots = () => page.evaluate(() => [...document.querySelectorAll<HTMLImageElement>('#item-images img')].map((img) => ({
      zoomable: img.classList.contains('zoomable'), zoomed: img.classList.contains('zoomed'), tab: img.tabIndex, pressed: img.getAttribute('aria-pressed'),
      width: Math.round(img.getBoundingClientRect().width), inner: img.clientWidth, column: Math.round(img.closest('.shot-body')!.getBoundingClientRect().width),
    })));
    const [wide, thumb, narrow] = await shots();
    expect([wide.zoomable, wide.tab, wide.pressed, wide.width <= wide.column]).toEqual([true, 0, 'false', true]);
    expect([thumb.zoomable, thumb.tab, thumb.width]).toEqual([false, -1, 202]); // 200px and its 1px border
    // The preferred width is the image's own; its 1px border sits outside it.
    expect([narrow.inner <= 390, narrow.width <= narrow.column]).toEqual([true, true]);
    await page.locator('#item-images img >> nth=0').click();
    expect((await shots())[0]).toMatchObject({ zoomed: true, pressed: 'true', width: 1202 });
    await page.locator('#item-images img >> nth=0').evaluate((img) => img.focus());
    await page.keyboard.press('Enter');
    expect((await shots())[0]).toMatchObject({ zoomed: false, pressed: 'false' });
    await page.keyboard.press(' ');
    expect((await shots())[0]).toMatchObject({ zoomed: true });
    await page.locator('#item-images img >> nth=1').click();
    expect((await shots())[1].zoomed).toBe(false);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('Show all expands clipped output and Show less clips it again', async () => {
    const { page, errors, close } = await open();
    await page.locator('#item-evidence > summary').click();
    await page.locator('#item-evidence .tr-entry > summary').click();
    const state = () => page.locator('#item-evidence .tr-out').evaluate((out) => [out.classList.contains('clip'), out.clientHeight < out.scrollHeight, out.parentElement!.querySelector('.ev-more')!.textContent]);
    expect(await state()).toEqual([true, true, 'Show all 40 lines']);
    await page.locator('#item-evidence .ev-more').click();
    expect(await state()).toEqual([false, false, 'Show less']);
    await page.locator('#item-evidence .ev-more').click();
    expect(await state()).toEqual([true, true, 'Show all 40 lines']);
    expect(await page.locator('#item-evidence .tr-out').textContent()).toBe(output);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('a plain row does not open by mouse or keyboard, while its seen box still works', async () => {
    const { page, errors, close } = await open();
    const plain = page.locator('#item-plain');
    await page.locator('#item-plain > summary .item-h').click();
    expect(await plain.evaluate((d) => (d as HTMLDetailsElement).open)).toBe(false);
    await page.locator('#item-plain > summary').evaluate((s) => s.focus());
    await page.keyboard.press('Enter');
    await page.keyboard.press('Space');
    expect(await plain.evaluate((d) => (d as HTMLDetailsElement).open)).toBe(false);
    expect(await page.locator('#item-plain .item-mark').evaluate((m) => getComputedStyle(m).visibility)).toBe('hidden');
    await page.locator('#seen-plain').click();
    expect(await plain.evaluate((d) => [(d as HTMLDetailsElement).open, d.classList.contains('is-seen'), (d.querySelector('.seen input') as HTMLInputElement).checked])).toEqual([false, true, true]);
    expect(await page.locator('#nav a[href="#item-plain"]').evaluate((a) => a.classList.contains('is-seen'))).toBe(true);
    await page.locator('#seen-evidence').click();
    expect(await page.locator('#item-evidence').evaluate((d) => [(d as HTMLDetailsElement).open, d.classList.contains('is-seen')])).toEqual([false, true]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('a link in a plain row navigates to and opens its target, by mouse and keyboard; the rest of the row stays inert', async () => {
    const { page, errors, close } = await open();
    const state = () => page.evaluate(() => [location.hash, (document.getElementById('item-plain') as HTMLDetailsElement).open, (document.getElementById('item-choice') as HTMLDetailsElement).open]);
    await page.locator('#item-plain .item-lede').click({ position: { x: 2, y: 2 } });
    expect(await state()).toEqual(['', false, false]);
    await page.locator('#item-plain .item-lede a').click();
    await page.waitForFunction(() => (document.getElementById('item-choice') as HTMLDetailsElement).open);
    expect(await state()).toEqual(['#item-choice', false, true]);
    await page.evaluate(() => { (document.getElementById('item-choice') as HTMLDetailsElement).open = false; history.replaceState(null, '', location.pathname); });
    await page.locator('#item-plain .item-lede a').evaluate((a) => a.focus());
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => (document.getElementById('item-choice') as HTMLDetailsElement).open);
    expect(await state()).toEqual(['#item-choice', false, true]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('the rail lists sections and items with attention marks; a link opens its item', async () => {
    const { page, errors, close } = await open();
    const nav = await page.evaluate(() => [...document.querySelectorAll('#nav .nav-sec')].map((sec) => [sec.querySelector('.nav-sec-t')!.textContent, [...sec.querySelectorAll('.nav-items a')].map((a) => [a.textContent, a.hasAttribute('data-attn')])]));
    expect(nav).toEqual([['Main', [['Changed evidence', false], ['The choice', true], ['Changed reply', false], ['Changed images', false]]], ['Side effects', [['Changed plain', false]]]]);
    expect(await page.evaluate(() => [getComputedStyle(document.querySelector('.rail')!).position, (document.getElementById('rail-fold') as HTMLDetailsElement).open])).toEqual(['sticky', true]);
    await page.locator('#nav a[href="#item-choice"]').click();
    expect(await page.locator('#item-choice').evaluate((d) => (d as HTMLDetailsElement).open)).toBe(true);
    expect(await page.locator('#item-choice .line-alternative').textContent()).toBe('AlternativeOther');
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('below 960px the rail is a closed Contents disclosure between the header and the sections', async () => {
    const { page, errors, close } = await open({ width: 400 });
    const layout = await page.evaluate(() => {
      const fold = document.getElementById('rail-fold') as HTMLDetailsElement, top = (s: string) => document.querySelector(s)!.getBoundingClientRect().top;
      return [fold.open, getComputedStyle(fold.querySelector('summary')!).display, top('.head') < top('.rail'), top('.rail') < top('.sec'), document.documentElement.scrollWidth <= innerWidth];
    });
    expect(layout).toEqual([false, 'block', true, true, true]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test.each([1280, 400])('the theme picker sits inside the Contents fold and sets and clears the theme at %ipx', async (width) => {
    const { page, errors, close } = await open({ width });
    const theme = () => page.evaluate(() => [document.documentElement.getAttribute('data-theme'), [...document.querySelectorAll('[data-theme-pick]')].map((b) => b.getAttribute('aria-pressed'))]);
    expect(await page.evaluate(() => !!document.querySelector('#rail-fold > .rail-tools [data-theme-pick]'))).toBe(true);
    // On a phone the fold starts closed: the picker is reached by opening Contents.
    if (width === 400) await page.locator('#rail-fold > summary').click();
    await page.locator('[data-theme-pick="dark"]').click();
    expect(await theme()).toEqual(['dark', ['false', 'false', 'true']]);
    await page.locator('[data-theme-pick="auto"]').click();
    expect(await theme()).toEqual([null, ['true', 'false', 'false']]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  // The rows shown: the key asks, and the rest once "All" is on.
  const asks = (page: Page) => page.evaluate(() => [...document.querySelectorAll('.asks li:not([hidden])')].map((li) => ({
    clipped: li.classList.contains('ask-clipped'), open: li.classList.contains('ask-open'), expanded: li.getAttribute('aria-expanded'),
    height: Math.round(li.querySelector('q')!.getBoundingClientRect().height), time: (li.querySelector('time') as HTMLElement).innerText,
  })));
  test.each([1440, 400])('at %ipx each ask is one line, only a long one is clipped, and nothing overflows the page', async (width) => {
    const { page, errors, close } = await open({ width });
    const rows = await asks(page);
    expect(rows.map((row) => [row.clipped, row.expanded, row.height])).toEqual([[false, null, rows[0].height], [true, 'false', rows[0].height], [false, null, rows[0].height]]);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('a clipped ask opens and closes by click, Enter and Space, and stays open after a reload', async () => {
    const { page, errors, close } = await open();
    const long = page.locator('.asks li.ask-clipped'), state = async () => (await asks(page))[1];
    const closed = (await state()).height;
    await long.click();
    expect(await state()).toMatchObject({ open: true, expanded: 'true' });
    expect((await state()).height).toBeGreaterThan(closed * 2);
    await long.click();
    expect(await state()).toMatchObject({ open: false, expanded: 'false', height: closed });
    await long.focus();
    for (const [key, opened] of [['Enter', true], [' ', false], [' ', true]] as const) {
      await page.keyboard.press(key);
      expect([key, (await state()).open]).toEqual([key, opened]);
    }
    await page.reload();
    await page.waitForFunction(() => document.readyState === 'complete');
    expect(await state()).toMatchObject({ open: true, expanded: 'true', clipped: true });
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('selecting text in a clipped ask does not open it', async () => {
    const { page, errors, close } = await open();
    const box = await page.locator('.asks li.ask-clipped q').evaluate((q) => { const r = q.getBoundingClientRect(); return { x: r.left, y: r.top + r.height / 2 }; });
    await page.mouse.move(box.x + 4, box.y); await page.mouse.down();
    await page.mouse.move(box.x + 160, box.y, { steps: 5 }); await page.mouse.up();
    expect(await page.evaluate(() => String(getSelection()).length > 5)).toBe(true);
    expect((await asks(page))[1].open).toBe(false);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test.each([['UTC', ['Sep 16 03:00', '03:04', '08:00']], ['America/Los_Angeles', ['Sep 15 20:00', '20:04', 'Sep 16 01:00']]] as const)('in %s an ask shows its day only where the day changes', async (timezone, times) => {
    const { page, errors, close } = await open({ timezone });
    expect((await asks(page)).map((row) => row.time)).toEqual([...times]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('"All" shows the user\'s other messages, muted, recomputes the day marks, and stays on after a reload', async () => {
    const { page, errors, close } = await open({ timezone: 'UTC' });
    const button = () => page.evaluate(() => { const b = document.querySelector('.asks-all')!; return [b.textContent, b.getAttribute('aria-expanded')]; });
    expect([await button(), (await asks(page)).map((row) => row.time)]).toEqual([['All 4 of your messages', 'false'], ['Sep 16 03:00', '03:04', '08:00']]);
    await page.locator('.asks-all').click();
    expect([await button(), (await asks(page)).map((row) => row.time)]).toEqual([['Only the ones that set direction', 'true'], ['Sep 16 03:00', '03:04', '08:00', 'Sep 17 09:00']]);
    const rest = await page.evaluate(() => { const q = document.querySelector('.asks li.ask-rest q')!; return [getComputedStyle(q).color === getComputedStyle(document.querySelector('.asks li.ask-key q')!).color, q.textContent]; });
    expect(rest).toEqual([false, 'A side note, the next day']);
    await page.reload();
    await page.waitForFunction(() => document.readyState === 'complete');
    expect([await button(), (await asks(page)).length]).toEqual([['Only the ones that set direction', 'true'], 4]);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);

  test('times are reformatted in the viewer\'s zone with the renderer\'s own formats', async () => {
    const zone = 'America/Los_Angeles', local = reviewTimes(zone);
    const { page, errors, close } = await open({ timezone: zone });
    const shown = await page.evaluate(() => ({
      entry: [document.querySelector('.tr-time')!.getAttribute('datetime'), document.querySelector('.tr-time')!.textContent],
      ask: [document.querySelector('.asks time')!.getAttribute('datetime'), document.querySelector('.asks time')!.textContent],
      asOf: [document.querySelector('.foryou time')!.getAttribute('datetime'), document.querySelector('.foryou h2')!.textContent],
    }));
    expect(shown.entry[1]).toBe(local.stamp(shown.entry[0]!));
    expect(shown.ask[1]).toBe(local.stamp(shown.ask[0]!));
    expect(shown.entry[1]).toBe('Sep 15 20:01');
    expect(shown.asOf[1]).toBe(`Left for you, as of ${local.day(shown.asOf[0]!)}`);
    expect(errors).toEqual([]);
    await close();
  }, 30_000);
});
