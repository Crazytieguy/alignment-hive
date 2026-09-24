/**
 * The viewer's UI state on a rendered review page, in headless Chromium: kept
 * across reloads and same-round re-renders, keyed by what each fold shows,
 * overridden by hash links, and indifferent to corrupt or blocked storage.
 *
 * Run: REVIEW_BROWSER_TESTS=1 bun test src/debrief/browser
 */
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { FIXTURE_SESSION, prompt, reply, reviewDocument, reviewItem, writeTranscript } from '../fixtures';
import { BROWSER_SKIP_REASON, browserTestsEnabled, startPageHarness } from './harness';
import type { BrowserHarness, OpenPage, Page } from './harness';

const suite = browserTestsEnabled ? 'saved UI state in headless Chromium' : `saved UI state in headless Chromium (skipped: ${BROWSER_SKIP_REASON})`;
if (!browserTestsEnabled) console.info(`state.browser.test.ts skipped: ${BROWSER_SKIP_REASON}`);
let served: Awaited<ReturnType<typeof startPageHarness>> | undefined;
let harness: BrowserHarness;
const lines = (n: number, edit: (i: number) => string | undefined = () => undefined) => Array.from({ length: n }, (_, i) => edit(i + 1) ?? `const line${i + 1} = ${i + 1};`).join('\n') + '\n';
const filler = Array.from({ length: 30 }, (_, i) => `Paragraph ${i + 1} of filler, long enough to make the page scroll.`).join('\n\n');

/** One item holding every kind of fold; `order` lists the two cited transcript entries; `swap` puts the plain row first. */
function review(order: ['1', '3'] | ['3', '1'], lede = 'Preserve compatibility', { swap = false, twice = false } = {}): string {
  const summaries = { 1: 'The ask', 3: 'The reply' };
  const body = [
    '```ref\nfile: notes.md\n```',
    '```ref\ntranscript: 11111111:2\nsummary: The long run\n```',
    '```ref\ngit: true\n```',
    '```ref\ndiff: wide.ts\ntitle: wide\n```',
    '```ref\ndiff: focused.ts\nfocus: L5\ntitle: focused\n```',
    `\`\`\`ref\ntranscript: ${order.map((n) => `11111111:${n}`).join(', ')}\nsummary: [${order.map((n) => `"${summaries[n]}"`).join(', ')}]\n\`\`\``,
    ...(twice ? ['```ref\ntranscript: 11111111:3\nsummary: "The reply, cited again"\n```'] : []),
  ].join('\n\n');
  const checked = (id: string, text: string) => reviewItem(id, text).replace('section: main', 'section: checked');
  const evidence = reviewItem('evidence', body).replace('lede: Preserve compatibility', `lede: ${lede}`), choice = reviewItem('choice', 'Why. See [the target](#item-target).');
  return reviewDocument((swap ? choice + evidence : evidence + choice) + checked('filler', filler) + checked('target', 'The target.') + checked('after', filler));
}
const render = (document: string) => served!.render(document);
let visit = 0;
/** A fresh navigation each time (a new query string), so the browser's own scroll restoration never stands in for ours. */
async function go(page: Page, hash = ''): Promise<void> {
  await page.goto(`${harness.origin}/page.html?v=${++visit}${hash}`);
  await page.waitForFunction(() => document.readyState === 'complete' && document.querySelectorAll('.dv-file').length === 2);
}
async function open(init?: string): Promise<OpenPage> {
  const opened = await harness.open({ theme: 'light', viewport: { width: 1280, height: 800 } });
  if (init) await opened.page.addInitScript(init);
  await go(opened.page);
  return opened;
}
const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));

/** Everything a viewer can set, as the page shows it now. */
function shown(page: Page) {
  return page.evaluate(() => {
    const q = (selector: string) => document.querySelector(selector)!;
    const panel = (title: string) => [...document.querySelectorAll('.dv-file')].find((p) => p.querySelector('.dv-title')!.textContent === title)!;
    const diff = (title: string) => ({ collapsed: panel(title).classList.contains('dv-is-collapsed'), view: panel(title).querySelector('.dv-tab[aria-selected="true"]')?.getAttribute('data-dv-view') ?? null, rows: panel(title).querySelectorAll('.dv-row').length });
    return {
      items: [...document.querySelectorAll('details.item')].filter((d) => (d as HTMLDetailsElement).open).map((d) => d.id),
      entries: [...document.querySelectorAll('#item-evidence details.tr-entry')].map((d) => [d.querySelector('.tr-sum')!.textContent, (d as HTMLDetailsElement).open]),
      file: [(q('details.fv') as HTMLDetailsElement).open, q('details.fv').classList.contains('fv-show-src'), (q('.fv-src') as HTMLElement).hidden, q('.fv-toggle').textContent],
      commit: (q('details.git-commit') as HTMLDetailsElement).open,
      clipped: q('.tr-out').classList.contains('clip'),
      wide: diff('wide'), focused: diff('focused'),
      seen: [...document.querySelectorAll('.item.is-seen')].map((item) => item.id),
      theme: document.documentElement.getAttribute('data-theme'),
      scrollY: Math.round(scrollY),
    };
  });
}

describe.skipIf(!browserTestsEnabled)(suite, () => {
  beforeAll(async () => {
    served = await startPageHarness();
    const { fixture } = served;
    harness = served.harness;
    await fixture.write('wide.ts', lines(60));
    await fixture.write('focused.ts', lines(40));
    await fixture.git('add', '.'); await fixture.git('commit', '-qm', 'Base');
    await fixture.stamp();
    await writeFile(join(fixture.stateDir, `${FIXTURE_SESSION}-commit.txt`), await fixture.git('rev-parse', 'HEAD') + '\n');
    await fixture.write('wide.ts', lines(60, (i) => i === 1 || i === 60 ? `const changed${i} = 0;` : undefined));
    await fixture.write('focused.ts', lines(40, (i) => i === 5 || i === 35 ? `const changed${i} = 0;` : undefined));
    await fixture.git('add', '.'); await fixture.git('commit', '-qm', 'Change');
    await writeTranscript(join(fixture.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [
      prompt('Run it'),
      { type: 'assistant', model: 'claude-fable-5-1', content: [{ type: 'tool_use', id: 'b1', name: 'Bash', input: { command: 'seq 40' } }] },
      { type: 'user', content: [{ type: 'tool_result', tool_use_id: 'b1', content: Array.from({ length: 40 }, (_, i) => String(i + 1)).join('\n') }] },
      reply('Done.', 'claude-fable-5-1'),
    ]);
    await render(review(['1', '3']));
  }, 60_000);
  afterAll(async () => { await served?.close(); });

  test('state survives a reload and a same-round re-render with a copy edit; reordered evidence keeps its own state', async () => {
    const { page, errors, close } = await open();
    const initial = await shown(page);
    expect(initial).toMatchObject({ items: [], entries: [['The long run', false], ['The ask', false], ['The reply', false]], file: [false, false, true, 'View source'], commit: false, clipped: true, seen: [], theme: null, wide: { collapsed: true }, focused: { collapsed: true, view: 'relevant' } });
    await page.locator('#item-evidence > summary').click();
    await page.locator('.fv-head').click();
    await page.locator('.fv-toggle').click();
    await page.locator('.git-commit-h').click();
    await page.locator('#item-evidence details.tr-entry:has(.tr-out) > summary').click();
    await page.locator('.ev-more').click();
    await page.locator('#item-evidence details.tr-entry:has(.tr-sum:text-is("The reply")) > summary').click();
    const panel = (title: string) => `.dv-file:has(.dv-title:text-is("${title}"))`;
    await page.locator(`${panel('wide')} .dv-collapse`).click();
    const wideRows = await page.locator(panel('wide')).evaluate((p) => p.querySelectorAll('.dv-row').length);
    await page.locator(`${panel('wide')} .dv-gap-btn`).first().click();
    await page.locator(`${panel('focused')} .dv-collapse`).click();
    await page.locator(`${panel('focused')} .dv-tab[data-dv-view="all"]`).click();
    await page.locator('#seen-choice').click();
    await page.locator('[data-theme-pick="dark"]').click();
    await page.locator('#item-filler > summary').click();
    await page.evaluate(() => { scrollTo(0, 700); });
    await wait(600);
    const set = await shown(page);
    expect(set).toMatchObject({
      items: ['item-evidence', 'item-filler'], entries: [['The long run', true], ['The ask', false], ['The reply', true]], file: [true, true, false, 'View rendered'], commit: true, clipped: false,
      focused: { collapsed: false, view: 'all' }, wide: { collapsed: false }, seen: ['item-choice'], theme: 'dark', scrollY: 700,
    });
    expect(set.wide.rows).toBeGreaterThan(wideRows);

    await page.reload();
    await page.waitForFunction(() => document.readyState === 'complete');
    expect(await shown(page)).toEqual(set);

    await render(review(['1', '3'], 'An edited lede'));
    await go(page);
    expect(await page.locator('#item-evidence .item-lede').textContent()).toBe('An edited lede');
    expect(await shown(page)).toEqual(set);

    await render(review(['3', '1']));
    await go(page);
    expect((await shown(page)).entries).toEqual([['The long run', true], ['The reply', true], ['The ask', false]]);
    // Items reordered: every fold keeps its state, since none is keyed by position.
    await render(review(['1', '3'], 'Preserve compatibility', { swap: true }));
    await go(page);
    expect(await page.evaluate(() => [...document.querySelectorAll('details.item')].map((d) => d.id).slice(0, 2))).toEqual(['item-choice', 'item-evidence']);
    expect(await shown(page)).toEqual(set);
    await render(review(['1', '3']));
    expect(errors).toEqual([]);
    await close();
  }, 60_000);

  test('the same entry cited twice in one item keeps two independent states', async () => {
    await render(review(['1', '3'], 'Preserve compatibility', { twice: true }));
    const { page, errors, close } = await open();
    await page.locator('#item-evidence > summary').click();
    await page.locator('#item-evidence details.tr-entry:has(.tr-sum:text-is("The reply, cited again")) > summary').click();
    const entries = () => page.evaluate(() => [...document.querySelectorAll('#item-evidence details.tr-entry')].map((d) => [d.getAttribute('data-key')!.replace(/:.*:/, ':…:'), (d as HTMLDetailsElement).open]));
    const before = await entries();
    expect(before).toEqual([['tr:…:2', false], ['tr:…:1', false], ['tr:…:3', false], ['tr:…:3', true]]);
    await wait(100);
    await page.reload();
    await page.waitForFunction(() => document.readyState === 'complete');
    expect(await entries()).toEqual(before);
    await render(review(['1', '3']));
    expect(errors).toEqual([]);
    await close();
  }, 60_000);

  test('a reload of the hash just followed keeps what the viewer closed; a fresh visit or a click on its link opens it', async () => {
    const { page, errors, close } = await open();
    const target = () => page.evaluate(() => (document.getElementById('item-target') as HTMLDetailsElement).open);
    await go(page, '#item-target');
    expect(await target()).toBe(true);
    await page.locator('#item-target > summary').click();
    expect(await target()).toBe(false);
    await wait(100); // the toggle event that saves it fires a task later
    await page.reload();
    await page.waitForFunction(() => document.readyState === 'complete');
    expect(await target()).toBe(false);
    // A link to the hash already in the address bar fires no hashchange, and still opens its target.
    await page.locator('#item-choice > summary').click();
    await page.locator('#item-choice a[href="#item-target"]').click();
    expect(await target()).toBe(true);
    await page.locator('#item-target > summary').click();
    await go(page, '#item-target');
    expect(await target()).toBe(true);
    expect(errors).toEqual([]);
    await close();
  }, 60_000);

  test('on a phone the Contents fold keeps the viewer\'s choice across a reload; on a desktop the rail keeps its scroll', async () => {
    const phone = await harness.open({ theme: 'light', viewport: { width: 400, height: 800 } });
    await go(phone.page);
    const fold = () => phone.page.evaluate(() => (document.getElementById('rail-fold') as HTMLDetailsElement).open);
    expect(await fold()).toBe(false);
    await phone.page.locator('#rail-fold > summary').click();
    await wait(100);
    await phone.page.reload();
    await phone.page.waitForFunction(() => document.readyState === 'complete');
    expect(await fold()).toBe(true);
    expect(phone.errors).toEqual([]);
    await phone.close();
    // A rail long enough to scroll.
    const many = Array.from({ length: 60 }, (_, i) => reviewItem(`row-${i}`, `Row ${i}.`)).join('');
    await render(reviewDocument(many));
    const desk = await harness.open({ theme: 'light', viewport: { width: 1280, height: 600 } });
    await desk.page.goto(`${harness.origin}/page.html?v=${++visit}`);
    await desk.page.waitForFunction(() => document.readyState === 'complete');
    await desk.page.locator('.rail').evaluate((rail) => { rail.scrollTop = 300; });
    await wait(600);
    await desk.page.reload();
    await desk.page.waitForFunction(() => document.readyState === 'complete');
    await wait(100);
    expect(await desk.page.locator('.rail').evaluate((rail) => [rail.scrollHeight > rail.clientWidth, Math.round(rail.scrollTop)])).toEqual([true, 300]);
    expect(desk.errors).toEqual([]);
    await desk.close();
    await render(review(['1', '3']));
  }, 60_000);

  test('a hash link opens its target and wins over the saved scroll position', async () => {
    const { page, errors, close } = await open();
    await page.locator('#item-filler > summary').click();
    await page.locator('#item-after > summary').click();
    await page.evaluate(() => { scrollTo(0, document.body.scrollHeight); });
    await wait(600);
    const saved = (await shown(page)).scrollY;
    expect(saved).toBeGreaterThan(1000);
    await go(page, '#item-target');
    const placed = await page.evaluate(() => ({ open: (document.getElementById('item-target') as HTMLDetailsElement).open, top: Math.round(document.getElementById('item-target')!.getBoundingClientRect().top), scrollY: Math.round(scrollY) }));
    expect(placed.open).toBe(true);
    expect(Math.abs(placed.top)).toBeLessThan(40);
    expect(placed.scrollY).not.toBe(saved);
    // Where the link left the viewer is now the place to come back to.
    await wait(600);
    await go(page);
    expect((await shown(page)).scrollY).toBe(placed.scrollY);
    expect(errors).toEqual([]);
    await close();
  }, 60_000);

  test('corrupt stored state is ignored and replaced by the next change', async () => {
    const { page, errors, close } = await open();
    const key = await page.evaluate(() => `review-ui:${document.querySelector('main')!.getAttribute('data-review')}`);
    const corrupt = ['null', '[]', '"text"', '42', '{not json', JSON.stringify({ theme: 5, open: 'x', seen: [1], expanded: { a: 'yes' }, source: null, scrollY: 'far' }), JSON.stringify({ theme: 'sepia', open: { 'item|evidence': 1 }, scrollY: -5 }),
      '{"railOpen":"Infinity","railScroll":1e400,"asksAll":[1],"askOpen":5,"lastHash":5}', JSON.stringify({ railOpen: 1, railScroll: 'Infinity', asksAll: 'yes', askOpen: [1], lastHash: '#' + 'x'.repeat(10_240) }), JSON.stringify({ lastHash: 'item-target', railScroll: -3 })];
    for (const value of corrupt) {
      await page.evaluate(([k, v]) => { localStorage.setItem(k, v); }, [key, value] as const);
      await go(page);
      expect(await shown(page)).toMatchObject({ items: [], seen: [], theme: null, clipped: true, scrollY: 0 });
    }
    await page.locator('[data-theme-pick="light"]').click();
    expect(JSON.parse(await page.evaluate((k) => localStorage.getItem(k)!, key))).toEqual({ theme: 'light', open: {}, expanded: {}, source: {}, askOpen: {} });
    expect(errors).toEqual([]);
    await close();
  }, 60_000);

  test('blocked storage leaves a working page that remembers nothing', async () => {
    const { page, errors, close } = await open('Object.defineProperty(window, "localStorage", { get() { throw new DOMException("denied", "SecurityError"); } });');
    await page.locator('#item-evidence > summary').click();
    await page.locator('.fv-head').click();
    await page.locator('.fv-toggle').click();
    await page.locator('[data-theme-pick="dark"]').click();
    expect(await shown(page)).toMatchObject({ items: ['item-evidence'], file: [true, true, false, 'View rendered'], theme: 'dark' });
    await go(page);
    expect(await shown(page)).toMatchObject({ items: [], theme: null });
    expect(errors).toEqual([]);
    await close();
  }, 60_000);
});
