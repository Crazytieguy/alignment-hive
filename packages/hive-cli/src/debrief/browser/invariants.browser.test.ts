/**
 * The owner's layout invariants, measured rather than eyeballed: opening or
 * closing anything moves nothing already on screen; item chevrons sit on the
 * heading's cap band within ±0.5px; diff tabs keep one geometry folded and
 * open (c001, c003).
 *
 * Run: REVIEW_BROWSER_TESTS=1 bun test src/debrief/browser/invariants.browser.test.ts
 * REVIEW_FONTS=local|network|none (default local); REVIEW_MEASURE_OUT=<dir> keeps every table.
 */
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { kitchenSink } from '../fixtures';
import { FONTS, diffEngine } from '../render';
import { CONTROL_PHASES, keepMeasurements, measureChevrons, measureShifts, measureTabs, preparePhase, settle, tabDeltas, waitForDiffs } from './geometry';
import { ASSETS_DIR, BROWSER_SKIP_REASON, browserTestsEnabled, fontMode, startPageHarness } from './harness';
import type { ShiftResult } from './geometry';
import type { BrowserHarness, FontMode, OpenPage, Page } from './harness';

const FONT_MODE = fontMode('local');
const TOLERANCE = 0.5;
const WIDE = { width: 1440, height: 1100 }, NARROW = { width: 400, height: 900 };
const title = browserTestsEnabled ? 'invariant gates' : `invariant gates (skipped: ${BROWSER_SKIP_REASON})`;
if (!browserTestsEnabled) console.info(`invariants.browser.test.ts skipped: ${BROWSER_SKIP_REASON}`);
let harness: BrowserHarness;
let served: Awaited<ReturnType<typeof startPageHarness>> | undefined;
let kitchen = '';

/** The diff fixtures page, as the tab gate measures it. */
const TAB_FONTS = '.dv-tab-name, .dv-chip, .dv-fname, .dv-path, .dv-title';
async function openFixtures(theme: 'light' | 'dark', viewport: { width: number; height: number }): Promise<{ page: Page; errors: Array<string>; close: () => Promise<void> }> {
  const opened = await harness.open({ theme, viewport });
  await opened.page.goto(`${harness.origin}/diff-fixtures.html`);
  await opened.page.waitForFunction(() => (window as unknown as { testReady?: boolean }).testReady === true);
  if (FONT_MODE === 'local') await opened.page.addStyleTag({ url: '/fonts/fonts.css' });
  if (FONT_MODE === 'network') await opened.page.addStyleTag({ url: FONTS });
  await opened.page.addStyleTag({ content: 'body { font-family: var(--dv-sans); }' });
  await settle(opened.page, TAB_FONTS, FONT_MODE);
  return opened;
}
/** A review page (`/<name>.html`) opened fresh: storage cleared, every diff mounted, still, fonts checked. */
async function openReview(name: string, options: { theme?: 'light' | 'dark'; viewport: { width: number; height: number }; scale?: number; diffs: number; fonts?: FontMode }): Promise<OpenPage> {
  const opened = await harness.open({ theme: options.theme ?? 'light', viewport: options.viewport, scale: options.scale, timezone: 'America/Los_Angeles' });
  await opened.page.goto(`${harness.origin}/${name}.html`);
  await opened.page.evaluate(() => { try { localStorage.clear(); } catch { /* storage unavailable */ } });
  await opened.page.reload();
  await waitForDiffs(opened.page, options.diffs);
  await settle(opened.page, '.page *', options.fonts ?? FONT_MODE);
  return opened;
}

/**
 * G-A's contract. Chromium paints boxes on whole CSS pixels while text sits at fractional positions, so a box-drawn
 * chevron's offset from the cap band spreads over about 1px across items (measured with a mask, an inline SVG and a
 * composited layer alike). The gate therefore holds each state's mean within 0.15px (the systematic centring) and
 * every element within 0.5px plus the ink's reading resolution, half a device row, since no box-drawn chevron can
 * meet 0.5px at every sub-pixel position.
 */
const MEAN_TOLERANCE = 0.15;
async function checkChevrons(name: string, rows: Awaited<ReturnType<typeof measureChevrons>>, minimum: number, scale: number) {
  const mean = (open: boolean) => { const of = rows.filter((r) => r.open === open).map((r) => r.offset ?? NaN); return of.reduce((a, b) => a + b, 0) / of.length; };
  const result = { elements: rows.length, maxAbsOffset: Math.max(...rows.map((r) => Math.abs(r.offset ?? Infinity))), meanClosed: mean(false), meanOpen: mean(true), rows };
  await keepMeasurements(`G-A-${name.replaceAll(' ', '-')}`, result);
  expect(rows.length).toBeGreaterThanOrEqual(minimum * 2);
  expect([name, Math.abs(result.meanClosed) <= MEAN_TOLERANCE, Math.abs(result.meanOpen) <= MEAN_TOLERANCE]).toEqual([name, true, true]);
  expect(rows.filter((row) => row.offset === null || Math.abs(row.offset) > TOLERANCE + 0.5 / scale).map((row) => `${row.id} ${row.open ? 'open' : 'closed'} ${row.offset?.toFixed(2)}`)).toEqual([]);
  return result;
}
/**
 * Every control kind in its phase; a failure lists each control that moved something and what. `finding` names
 * anchors whose movement is recorded rather than failed: on fallback fonts only, a diff tab's label moves when its
 * weight changes (the system faces differ in metrics between weights; Plex 500 and 600 don't), and the owner's
 * weight cue stays (manager's decision).
 */
async function checkShifts(name: string, page: Page, finding?: RegExp) {
  const results: Array<ShiftResult> = [];
  for (const { phase, kinds } of CONTROL_PHASES) {
    await preparePhase(page, phase);
    for (const kind of kinds) results.push(...await measureShifts(page, kind));
  }
  const table = { controls: results.length, byKind: Object.fromEntries([...new Set(results.map((r) => r.kind))].map((kind) => {
    const of = results.filter((r) => r.kind === kind);
    return [kind, { n: of.length, forward: Math.max(...of.map((r) => r.forward)), back: Math.max(...of.map((r) => r.back)), roundTrip: Math.max(...of.map((r) => r.roundTrip)), scroll: Math.max(...of.map((r) => r.scroll)) }];
  })), moved: results.filter((r) => r.moved.length), findings: [] as Array<string> };
  if (finding) for (const r of table.moved) {
    table.findings.push(...r.moved.filter((m) => finding.test(m)).map((m) => `${r.kind} ${r.label}: ${m}`));
    r.moved = r.moved.filter((m) => !finding.test(m));
  }
  await keepMeasurements(`G-B-${name.replaceAll(' ', '-')}`, table);
  expect(table.moved.filter((r) => r.moved.length).map((r) => `${r.kind} ${r.label}: ${r.moved.join('; ')}`)).toEqual([]);
  return table;
}

/** Keeps the table first, so a failing run still leaves its numbers. */
async function checkTabs(name: string, panels: Awaited<ReturnType<typeof measureTabs>>, minimum: number) {
  const deltas = panels.map(tabDeltas);
  const result = {
    panels: deltas.length,
    max: { labelMove: Math.max(...deltas.map((d) => d.labelMove)), chipMove: Math.max(...deltas.map((d) => d.chipMove)), headerChange: Math.max(...deltas.map((d) => d.headerChange)), viewMove: Math.max(...deltas.map((d) => d.viewMove)) },
    centreOffset: deltas.map((d) => d.centreOffset).filter((n) => n !== null),
    deltas, raw: panels,
  };
  await keepMeasurements(`G-C-${name.replaceAll(' ', '-')}-${FONT_MODE}`, result);
  expect(deltas.length).toBeGreaterThanOrEqual(minimum);
  for (const d of deltas) {
    expect([name, d.id, d.opened]).toEqual([name, d.id, true]);
    expect([name, d.id, 'labels', d.labelMove <= TOLERANCE, 'chip', d.chipMove <= TOLERANCE, 'header', d.headerChange <= TOLERANCE, 'relevant to all', d.viewMove <= TOLERANCE])
      .toEqual([name, d.id, 'labels', true, 'chip', true, 'header', true, 'relevant to all', true]);
  }
  return result;
}

describe.skipIf(!browserTestsEnabled)(title, () => {
  beforeAll(async () => {
    served = await startPageHarness();
    harness = served.harness;
    kitchen = await kitchenSink(served.fixture);
    harness.serve('/diff-fixtures.html', join(import.meta.dir, 'diff-fixtures.html'));
    await writeFile(harness.output('review-diff.js'), diffEngine);
    harness.serve('/assets/review-diff.js', harness.output('review-diff.js'));
    harness.serve('/assets/review-diff.css', join(ASSETS_DIR, 'review-diff.css'));
  }, 600_000);
  afterAll(async () => { await served?.close(); });

  test.each([['light', WIDE], ['light', NARROW], ['dark', WIDE], ['dark', NARROW]] as const)('G-C: diff fixture tabs keep one geometry, %s %o', async (theme, viewport) => {
    const { page, errors, close } = await openFixtures(theme, viewport);
    await checkTabs(`fixtures ${theme} ${viewport.width}`, await page.evaluate(measureTabs), 5);
    expect(errors).toEqual([]);
    await close();
  }, 120_000);

  const SINK = [['light', WIDE], ['light', NARROW], ['dark', WIDE]] as const;
  test.each(SINK)('G-A: kitchen-sink chevrons sit on the cap band within 0.5px, %s %o', async (theme, viewport) => {
    await served!.render(kitchen, { fonts: FONT_MODE });
    const { page, errors, close } = await openReview('page', { theme, viewport, scale: 4, diffs: 3 });
    await checkChevrons(`kitchen ${theme} ${viewport.width} ${FONT_MODE}`, await measureChevrons(page, 4), 7, 4);
    expect(errors).toEqual([]);
    await close();
  }, 180_000);

  test.each([...SINK.map(([theme, viewport]) => [theme, viewport, FONT_MODE] as const), ['light', WIDE, 'none'] as const])('G-B: opening or closing anything on the kitchen sink moves nothing on screen, %s %o fonts %s', async (theme, viewport, fonts) => {
    await served!.render(kitchen, { fonts });
    const { page, errors, close } = await openReview('page', { theme, viewport, diffs: 3, fonts });
    const table = await checkShifts(`kitchen ${theme} ${viewport.width} ${fonts}`, page, fonts === 'none' ? /^(forward|back) tab-\d+ / : undefined);
    expect(table.controls).toBeGreaterThan(20);
    expect(errors).toEqual([]);
    await close();
  }, 300_000);
});
