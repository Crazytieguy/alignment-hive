/**
 * The diff component in headless Chromium, both colour schemes: fixture
 * disclosure, Relevant | All changes accounting, geometry, page colour bridge,
 * syntax/palette, remembered state, mobile scrolling, 120 randomized
 * accounting trials, 200 random code-diff trials (no replacement ever partly shown),
 * 60 randomized Markdown trials, keyboard focus through fold steps and the
 * remembered horizontal scroll.
 *
 * Run: REVIEW_BROWSER_TESTS=1 bun test src/debrief/browser   (REVIEW_BROWSER_KEEP=1 keeps screenshots)
 */
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { diffEngine } from '../render';
import { checkFoldSteps } from './folds';
import { ASSETS_DIR, BROWSER_SKIP_REASON, browserTestsEnabled, startBrowserHarness, vendorFiles } from './harness';
import type { BrowserHarness, OpenPage } from './harness';

type Range = [number, number];
interface FixtureSpec {
  id: string; path: string; old: string; new: string;
  focus?: Range | Array<Range>; oldFocus?: Range | Array<Range>; labels?: Array<{ line: number; where: string }>;
  scope?: string; stateKey?: string;
}
interface Handle { element: HTMLElement; rerender: () => void; file: FixtureSpec }
interface DiffViewApi {
  render: (host: HTMLElement, spec: object) => Handle;
  countChanges: (spec: object) => { add: number; del: number };
}
interface FixtureWindow { fixtureSpecs: Array<FixtureSpec>; fixtureHandles: Array<Handle>; testReady?: boolean; DiffView: DiffViewApi }
// page.evaluate ships each callback's source to the page, so callbacks reach
// the fixture globals through `window` rather than through Node-side helpers.

const THEMES: Array<'light' | 'dark'> = ['light', 'dark'];
const title = browserTestsEnabled ? 'DiffView in headless Chromium' : `DiffView in headless Chromium (skipped: ${BROWSER_SKIP_REASON})`;
if (!browserTestsEnabled) console.info(`diff.browser.test.ts skipped: ${BROWSER_SKIP_REASON}`);
let harness: BrowserHarness;

async function openFixtures(theme: 'light' | 'dark'): Promise<OpenPage> {
  const opened = await harness.open({ theme });
  await opened.page.goto(`${harness.origin}/diff-fixtures.html`);
  await opened.page.waitForFunction(() => (window as unknown as FixtureWindow).testReady === true);
  return opened;
}

// In-page helpers, passed to page.evaluate as source so each call is self-contained.
/** 200 trials: code files with dense and sparse edits, replacements and long insertions, most focused on one line. */
function codeTrialSpecs(): Array<Record<string, unknown>> {
  let seed = 81473;
  const random = (n: number) => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % n; };
  return Array.from({ length: 200 }, (_, trial) => {
    const old = Array.from({ length: 20 + random(90) }, (__, i) => `const item${i} = ${i};`);
    const next: Array<string> = [];
    old.forEach((line, i) => {
      const action = random(8);
      if (action !== 0) next.push(action === 1 ? `const item${i} = "changed";` : line);
      if (action === 2) next.push(`const added${i} = "new";`);
      if (action === 3 && random(3) === 0) for (let k = 0; k < 12; k++) next.push(`const block${i}_${k} = 0;`);
    });
    const line = 1 + random(next.length);
    const spec: Record<string, unknown> = { path: `trial-${trial}.js`, old: old.join('\n') + '\n', new: next.join('\n') + '\n' };
    if (trial % 4 !== 0) spec.focus = [line, line];
    if (trial % 3 === 0) { const n = 1 + random(old.length); spec.oldFocus = [n, n]; }
    return spec;
  });
}
const FOLD_LINE = /^\d+ unchanged (?:lines?|blocks?)$|^\d+ (?:lines?|blocks?) left out · \+\d+ −\d+$/;

describe.skipIf(!browserTestsEnabled)(title, () => {
  beforeAll(async () => {
    harness = await startBrowserHarness();
    const files = await vendorFiles(harness.outDir);
    for (const [url, file] of Object.entries(files)) harness.serve(url, file);
    harness.serve('/diff-fixtures.html', join(import.meta.dir, 'diff-fixtures.html'));
    await writeFile(harness.output('review-diff.js'), diffEngine);
    harness.serve('/assets/review-diff.js', harness.output('review-diff.js'));
    harness.serve('/assets/review-diff.css', join(ASSETS_DIR, 'review-diff.css'));
  });

  afterAll(async () => { await (harness as BrowserHarness | undefined)?.close(); });

  test.each(THEMES)('%s: fixtures, tabs, folds, bridge, geometry and full accounting', async (theme) => {
    const { page, errors, close } = await openFixtures(theme);
    const initial = await page.evaluate(() => {
      const q = (id: string, selector: string) => Array.from(document.querySelectorAll<HTMLElement>(`#${id} ${selector}`));
      const labelText = (label: Element) => Array.from(label.childNodes).filter((n) => !(n.nodeType === 1 && (n as Element).classList.contains('dv-gap-btn'))).map((n) => n.textContent).join('');
      const foldLines = (root: ParentNode) => Array.from(root.querySelectorAll('.dv-gap.dv-fold-line .dv-gap-label')).map((l) => ({
        text: labelText(l), buttons: Array.from(l.querySelectorAll('.dv-gap-btn')).map((b) => b.textContent),
      }));
      const panels = Array.from(document.querySelectorAll<HTMLElement>('.dv-file'));
      return {
        fixtures: (window as unknown as FixtureWindow).fixtureSpecs.length,
        folds: foldLines(document),
        proseBars: Array.from(document.querySelectorAll('.dv-fold')).map((b) => ({ text: b.querySelector('.dv-fold-label')!.textContent, buttons: Array.from(b.querySelectorAll('button')).map((n) => n.textContent) })),
        blockGaps: Array.from(document.querySelectorAll('.dv-gap:not(.dv-fold-line)')).map((g) => ({ text: g.textContent, buttons: g.querySelectorAll('button').length })),
        tabs: panels.filter((f) => f.querySelector('.dv-tabs')).map((f) => f.id),
        scopeButtons: document.querySelectorAll('.dv-expand-file, .dv-reset, .dv-viewed, .dv-file input').length,
        focus: { add: q('focus-both-sides', '.dv-row.dv-add').length, del: q('focus-both-sides', '.dv-row.dv-del').length, folds: foldLines(document.getElementById('focus-both-sides')!), numbers: q('focus-both-sides', '.dv-n-new').map((n) => n.textContent).filter(Boolean).map(Number) },
        labels: q('label-outside', '.dv-label-row').map((n) => ({ line: n.dataset.dvLabelLine, text: n.textContent, previous: n.previousElementSibling!.querySelector('.dv-n-new')!.textContent })),
        twoLabels: q('two-added-labels', '.dv-row.dv-add').length,
        noFocus: { add: q('no-focus', '.dv-row.dv-add').length, folds: foldLines(document.getElementById('no-focus')!).map((f) => f.text) },
        gaps: Object.fromEntries(['gap-1', 'gap-3', 'gap-4', 'unfocused-gap-1', 'unfocused-gap-3', 'unfocused-gap-4'].map((id) => [id, foldLines(document.getElementById(id)!).map((f) => f.text)])),
        added: q('added-file', '.dv-row.dv-add').length,
        deleted: q('deleted-file', '.dv-row.dv-del').length,
        oldFocus: q('old-focus', '.dv-row.dv-del').length,
        multi: {
          add: q('multi-focus', '.dv-row.dv-add .dv-n-new').map((n) => Number(n.textContent)),
          del: q('multi-focus', '.dv-row.dv-del .dv-n-old').map((n) => Number(n.textContent)),
          relevant: q('multi-focus', '.dv-tab[data-dv-view="relevant"]')[0]?.textContent,
          all: q('multi-focus', '.dv-tab[data-dv-view="all"]')[0]?.textContent,
        },
        invalid: { rows: q('invalid-focus', '.dv-row').length, note: q('invalid-focus', '.dv-note').map((n) => n.textContent) },
        title: document.querySelector('#focus-both-sides .dv-head .dv-title')!.textContent,
        codeTools: q('no-focus', '.dv-tools').length,
        headParts: q('no-focus', '.dv-head > *').map((n) => n.className),
        zeroMuted: getComputedStyle(document.querySelector('#syntax-add .dv-minus')!).color === getComputedStyle(document.querySelector('#syntax-add .dv-path .dv-dir')!).color,
        toolCopy: Array.from(document.querySelectorAll('.dv-tools')).map((n) => n.textContent),
        proseLabel: !!document.querySelector('#prose-focus [data-dv-label-line="43"]'),
        frontmatter: !!document.querySelector('#frontmatter .dv-meta-list'),
        wholeFileProse: ['frontmatter-added', 'frontmatter-removed', 'md-added-newline', 'md-deleted-newline'].filter((id) => !document.querySelector(`#${id} .dv-pblocks`)),
        syntax: !!document.querySelector('.dv-code .hljs-keyword'),
        stateKeyCollapsed: document.getElementById('state-key')!.classList.contains('dv-is-collapsed'),
      };
    });
    expect(initial.fixtures).toBe(28);
    // An added or deleted Markdown file renders as prose whether or not it ends in a newline.
    expect(initial.wholeFileProse).toEqual([]);
    // Every fold, code or rendered Markdown, is a hairline that says what it holds, with step buttons only.
    // A code fold never hides three rows or fewer; a block fold steps three blocks at a time.
    for (const fold of initial.folds) {
      expect(fold.text).toMatch(FOLD_LINE);
      expect(fold.buttons.length).toBeGreaterThan(0);
      const blocks = /blocks?/.test(fold.text);
      if (!blocks) expect(Number(/\d+/.exec(fold.text)![0])).toBeGreaterThan(3);
      for (const b of fold.buttons) expect(blocks ? ['↓ 3', '↑ 3'] : ['↓ 10', '↑ 10']).toContain(b);
    }
    expect(initial.folds.some((fold) => /blocks?/.test(fold.text))).toBe(true);
    // No grey Show bar and no buttonless block marker remain.
    expect([initial.proseBars, initial.blockGaps]).toEqual([[], []]);
    for (const id of ['focus-both-sides', 'old-focus', 'prose-focus', 'multi-focus', 'state-key']) expect(initial.tabs).toContain(id);
    for (const id of ['no-focus', 'label-outside', 'two-added-labels', 'invalid-focus', 'gap-1', 'gap-3', 'gap-4']) expect(initial.tabs).not.toContain(id);
    expect(initial.scopeButtons).toBe(0);
    expect([initial.focus.add, initial.focus.del]).toEqual([3, 3]);
    expect(initial.focus.folds).toEqual([{ text: '49 lines left out · +3 −3', buttons: ['↑ 10'] }, { text: '58 lines left out · +3 −3', buttons: ['↓ 10'] }]);
    expect(initial.focus.numbers).toEqual([47, 48, 49, 50, 51, 52, 53, 54, 55]);
    expect(initial.labels).toHaveLength(3);
    expect(initial.labels.every((l) => l.line === l.previous)).toBe(true);
    expect(initial.labels.map((l) => l.text)).toEqual(['→ in the SessionStart hook output', '→ unchanged guidance', '→ CLI error after waiting 30s']);
    expect(initial.twoLabels).toBe(2);
    // No focus: every change shown, only unchanged stretches fold.
    expect(initial.noFocus.add).toBe(3);
    expect(initial.noFocus.folds).toEqual(['6 unchanged lines', '28 unchanged lines', '28 unchanged lines', '7 unchanged lines']);
    for (const prefix of ['', 'unfocused-']) {
      expect(initial.gaps[prefix + 'gap-1']).toEqual([]);
      expect(initial.gaps[prefix + 'gap-3']).toEqual([]);
      expect(initial.gaps[prefix + 'gap-4']).toEqual(['4 unchanged lines']);
    }
    // A focus inside a pure deletion shows the focused line and three lines each side, not the whole run.
    expect([initial.added, initial.deleted, initial.oldFocus]).toEqual([24, 7, 3]);
    expect(initial.multi.add).toEqual([10, 40, 100]);
    expect(initial.multi.del).toEqual([10, 40, 100]);
    expect(initial.multi.relevant).toBe('Relevant+3−3');
    expect(initial.multi.all).toBe('All changes+5−5');
    expect(initial.invalid.rows).toBeGreaterThan(0);
    expect(initial.invalid.note).toEqual([]);
    expect(initial.title).toBe('the caller side');
    expect(initial.codeTools).toBe(0);
    expect(initial.headParts).toEqual(['dv-collapse', 'dv-path', 'dv-totals']);
    expect(initial.zeroMuted).toBe(true);
    expect(initial.toolCopy).toEqual([]);
    expect([initial.proseLabel, initial.frontmatter, initial.syntax, initial.stateKeyCollapsed]).toEqual([true, true, true, true]);

    await page.evaluate((scheme) => {
      const check = (ok: unknown, message: string) => { if (!ok) throw Error(message); };
      const rgb = (hex: string) => `rgb(${[1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)).join(', ')})`;
      const root = document.documentElement;
      for (const id of ['syntax-add', 'syntax-del']) {
        for (const token of ['comment', 'meta', 'keyword', 'string']) {
          const node = document.querySelector(`#${id} .hljs-${token}`);
          check(node, `${id}: missing token ${token}`);
          const actual = getComputedStyle(node!).color;
          check(actual === rgb(getComputedStyle(root).getPropertyValue(`--dv-hl-${token}`).trim()), `${id}: token colour overridden for ${token}`);
        }
      }
      const bridge: Record<string, string> = scheme === 'light'
        ? { bg: '#f3f4f1', 'bg-2': '#e9eeea', rule: '#d9ded9', ink: '#141a19', muted: '#5c6b69', accent: '#0a6c64' }
        : { bg: '#161d1c', 'bg-2': '#1e2825', rule: '#26302e', ink: '#e4e9e6', muted: '#93a19d', accent: '#3cc9b8' };
      const protectedTokens = ['add-bg', 'add-hi', 'add-rule', 'add-ink', 'del-bg', 'del-hi', 'del-rule', 'del-ink'];
      const protectedBefore = protectedTokens.map((key) => getComputedStyle(root).getPropertyValue(`--dv-${key}`));
      for (const [key, value] of Object.entries(bridge)) root.style.setProperty(`--dv-page-${key}`, value);
      const probes: Array<[string, 'backgroundColor' | 'borderTopColor' | 'color', string]> = [
        ['#focus-both-sides .dv-head', 'backgroundColor', 'bg-2'],
        ['#focus-both-sides .dv-tab[aria-selected="true"]', 'backgroundColor', 'bg'],
        ['#focus-both-sides .dv-gap-label', 'color', 'muted'],
        ['#label-outside .dv-label-row', 'backgroundColor', 'bg-2'],
        ['#focus-both-sides', 'borderTopColor', 'rule'],
        ['#focus-both-sides', 'color', 'ink'],
        ['#no-focus .dv-gap-btn', 'backgroundColor', 'bg'],
        ['#prose-focus .dv-src-toggle', 'color', 'muted'],
      ];
      for (const explicitTheme of [null, 'light', 'dark']) {
        if (explicitTheme) root.setAttribute('data-theme', explicitTheme);
        else root.removeAttribute('data-theme');
        for (const [selector, property, key] of probes) {
          check(getComputedStyle(document.querySelector(selector)!)[property] === rgb(bridge[key]), `page bridge failed: ${selector} ${property} (${explicitTheme ?? 'auto'})`);
        }
      }
      root.removeAttribute('data-theme');
      check(protectedTokens.every((key, i) => getComputedStyle(root).getPropertyValue(`--dv-${key}`) === protectedBefore[i]), 'page bridge must not change red/green scales');
      for (const key of Object.keys(bridge)) root.style.removeProperty(`--dv-page-${key}`);
    }, theme);

    // Prose: Relevant leaves the remote change out; All changes shows it with context, unchanged blocks still folded.
    async function checkProse(revealed: boolean) {
      const state = await page.locator('#prose-focus .dv-pblocks').evaluate((blocks) => ({
        paragraphs: [...new Set(Array.from(blocks.querySelectorAll('.dv-prose p'))
          .map((p) => /^Paragraph (\d+) /.exec(p.textContent || '')).filter((m) => m !== null).map((m) => Number(m[1])))].sort((a, b) => a - b),
        unchangedFolds: Array.from(blocks.querySelectorAll(':scope > .dv-fold-line')).filter((f) => /unchanged/.test(f.textContent || '')).length,
        core: blocks.querySelectorAll(':scope > .dv-core').length,
      }));
      expect(state.paragraphs.includes(2)).toBe(revealed);
      expect(state.paragraphs).not.toContain(6);
      expect(state.paragraphs).not.toContain(8);
      if (revealed) {
        expect(state.paragraphs).toContain(1);
        expect(state.paragraphs).toContain(3);
        expect(state.unchangedFolds).toBeGreaterThan(0);
        expect(state.core).toBeGreaterThan(0);
      }
    }
    await checkProse(false);
    await page.locator('#prose-focus .dv-tab[data-dv-view="all"]').click();
    await checkProse(true);
    await page.evaluate(() => (window as unknown as FixtureWindow).fixtureHandles.find((h) => h.file.id === 'prose-focus')!.rerender());
    await checkProse(true);
    await page.evaluate(() => window.scrollTo(0, 0));
    await page.screenshot({ path: harness.output(`${theme}-desktop.png`) });

    // All changes: every hunk, relevant rows bracketed, other changes washed out, unchanged stretches still folded.
    const relevantRows = await page.locator('#focus-both-sides .dv-row').count();
    await page.locator('#focus-both-sides .dv-tab[data-dv-view="all"]').click();
    const all = await page.locator('#focus-both-sides').evaluate((panel) => {
      const coreAdd = panel.querySelector('.dv-row.dv-core.dv-add'), otherAdd = panel.querySelector('.dv-row.dv-other.dv-add');
      return {
        add: panel.querySelectorAll('.dv-row.dv-add').length,
        core: panel.querySelectorAll('.dv-row.dv-core').length,
        leftOut: Array.from(panel.querySelectorAll('.dv-gap-label')).filter((l) => /left out/.test(l.textContent || '')).length,
        unchangedFolds: Array.from(panel.querySelectorAll('.dv-gap-label')).filter((l) => /unchanged/.test(l.textContent || '')).length,
        opens: Array.from(panel.querySelectorAll('.dv-bracket-open')).map((b) => b.textContent),
        closes: panel.querySelectorAll('.dv-bracket-close').length,
        washed: !!coreAdd && !!otherAdd && getComputedStyle(coreAdd).backgroundColor !== getComputedStyle(otherAdd).backgroundColor &&
          getComputedStyle(coreAdd.querySelector('.dv-code')!).color === getComputedStyle(otherAdd.querySelector('.dv-code')!).color,
        selected: panel.querySelector('.dv-tab[aria-selected="true"]')!.getAttribute('data-dv-view'),
      };
    });
    expect(all).toEqual({ add: 9, core: relevantRows, leftOut: 0, unchangedFolds: 4, opens: ['relevant'], closes: 1, washed: true, selected: 'all' });
    // Both directional step buttons, clicked for real.
    for (const glyph of ['↑ 10', '↓ 10']) {
      const before = await page.locator('#focus-both-sides .dv-row').count();
      await page.locator(`#focus-both-sides .dv-gap-btn:text-is("${glyph}")`).first().click();
      const grown = await page.locator('#focus-both-sides .dv-row').count() - before;
      expect(grown).toBeGreaterThan(0);
      expect(grown).toBeLessThanOrEqual(10);
    }
    // Arrow keys move between the tabs and keep focus on the selected one.
    await page.locator('#old-focus .dv-tab[data-dv-view="relevant"]').click();
    await page.keyboard.press('ArrowRight');
    expect(await page.evaluate(() => document.activeElement?.closest('.dv-file')?.id + ':' + document.activeElement?.getAttribute('data-dv-view'))).toBe('old-focus:all');
    expect(await page.locator('#old-focus .dv-tab[aria-selected="true"]').textContent()).toBe('All changes+5−5');

    // Every panel to All changes (or its only view), then every fold opened, checking each intermediate fold.
    const expansion = await page.evaluate((pattern) => {
      const foldLine = new RegExp(pattern);
      document.querySelectorAll<HTMLElement>('.dv-tab[data-dv-view="all"][aria-selected="false"]').forEach((t) => t.click());
      let clicks = 0;
      const open = () => {
        for (let button = document.querySelector<HTMLElement>('.dv-gap-btn'); button; button = document.querySelector<HTMLElement>('.dv-gap-btn')) {
          for (const label of Array.from(document.querySelectorAll('.dv-gap-label'))) {
            const text = Array.from(label.childNodes).filter((n) => !(n.nodeType === 1 && (n as Element).classList.contains('dv-gap-btn'))).map((n) => n.textContent).join('');
            if (!foldLine.test(text) || /left out/.test(text) || (/lines?$/.test(text) && Number(/\d+/.exec(text)![0]) <= 3)) throw Error(`bad fold in All changes: ${text}`);
          }
          button.click();
          if (++clicks > 3000) throw Error('expansion must terminate');
        }
      };
      open();
      // Markdown panels once more as line diffs of their source.
      Array.from(document.querySelectorAll<HTMLElement>('.dv-head .dv-src-toggle')).filter((b) => b.textContent === 'View source').forEach((b) => b.click());
      open();
      return { clicks, gapsLeft: document.querySelectorAll('.dv-gap:not(.dv-bracket)').length };
    }, FOLD_LINE.source);
    expect(expansion.gapsLeft).toBe(0);

    const geometry = await page.evaluate(() => {
      function topText(cell: Element) {
        const walk = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
        while (walk.nextNode()) {
          const node = walk.currentNode as Text;
          if (!node.data || node.parentElement!.closest('.dv-no-newline')) continue;
          const range = document.createRange(); range.setStart(node, 0); range.setEnd(node, 1);
          return range.getBoundingClientRect().top;
        }
        return null;
      }
      const count = (el: Element | null) => Number((el?.textContent || '+0').slice(1));
      const totals = Array.from(document.querySelectorAll('.dv-file')).map((file) => {
        const source = file.querySelector('.dv-tab[data-dv-view="all"]') ?? file.querySelector('.dv-head > .dv-totals');
        return { id: file.id, headerAdd: count(source!.querySelector('.dv-plus')), headerDel: count(source!.querySelector('.dv-minus')),
          rowsAdd: file.querySelectorAll('.dv-row.dv-add').length, rowsDel: file.querySelectorAll('.dv-row.dv-del').length };
      });
      const visibleRows = Array.from(document.querySelectorAll('.dv-row')).filter((row) => row.getBoundingClientRect().height > 0);
      const alignments = visibleRows.slice(0, 20).map((row) => {
        const values = Array.from(row.children).map(topText).filter((n) => n != null);
        return { spread: Math.max(...values) - Math.min(...values), sizes: Array.from(row.children).map((c) => getComputedStyle(c).fontSize), heights: Array.from(row.children).map((c) => getComputedStyle(c).lineHeight) };
      });
      const badRules = Array.from(document.querySelectorAll('.dv-row.dv-add,.dv-row.dv-del')).filter((row) =>
        getComputedStyle(row).borderLeftWidth !== '3px' || Array.from(row.children).some((c) => getComputedStyle(c).borderLeftWidth !== '0px' || getComputedStyle(c).boxShadow !== 'none'),
      ).map((n) => n.textContent);
      const labelGeometry = Array.from(document.querySelectorAll('.dv-rows > .dv-label-row')).filter((l) => l.getBoundingClientRect().height > 0).map((label) => ({
        left: label.getBoundingClientRect().left, codeLeft: label.previousElementSibling!.querySelector('.dv-code')!.getBoundingClientRect().left,
        radius: getComputedStyle(label).borderRadius, display: getComputedStyle(label).display,
      }));
      return { totals, alignments, badRules, labelGeometry, overflow: document.documentElement.scrollWidth > innerWidth };
    });
    for (const t of geometry.totals) expect([t.id, t.rowsAdd, t.rowsDel]).toEqual([t.id, t.headerAdd, t.headerDel]);
    expect(geometry.alignments).toHaveLength(20);
    for (const a of geometry.alignments) { expect(a.spread).toBeLessThanOrEqual(1); expect(new Set(a.sizes).size).toBe(1); expect(new Set(a.heights).size).toBe(1); }
    expect(geometry.badRules).toEqual([]);
    expect(geometry.labelGeometry.length).toBeGreaterThan(0);
    for (const g of geometry.labelGeometry) { expect(Math.abs(g.left - g.codeLeft)).toBeLessThanOrEqual(1); expect([g.radius, g.display]).toEqual(['0px', 'block']); }
    expect(geometry.overflow).toBe(false);

    expect(errors).toEqual([]);
    await close();
  }, 180_000);

  test.each(THEMES)('%s: randomized accounting, collapse, remembered state and mobile scrolling', async (theme) => {
    const { page, errors, close } = await openFixtures(theme);
    await page.evaluate(() => {
      let seed = 81473;
      const random = (n: number) => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % n; };
      const check = (ok: unknown, message: string) => { if (!ok) throw Error(message); };
      const api = (window as unknown as FixtureWindow).DiffView;
      for (let trial = 0; trial < 120; trial++) {
        const old = Array.from({ length: 20 + random(130) }, (_, i) => `const item${i} = ${i};`);
        const next: Array<string> = [];
        // Change density varies per trial, from dense edits to a few changes far apart.
        const sparsity = 8 + random(60);
        old.forEach((line, i) => {
          const action = random(sparsity);
          if (action !== 0) next.push(action === 1 ? `const item${i} = "changed";` : line);
          if (action === 2) next.push(`const added${i} = "new";`);
        });
        const at = () => 1 + random(next.length);
        const spec: Record<string, unknown> = { path: `random-${trial}.js`, old: old.join('\n') + '\n', new: next.join('\n') + '\n' };
        if (trial % 4 === 1) spec.focus = [at(), at()].sort((a, b) => a - b);
        if (trial % 4 === 2 || trial % 4 === 3) spec.focus = [[at(), at()].sort((a, b) => a - b), [at(), at()].sort((a, b) => a - b)];
        if (trial % 3 === 0) { const n = 1 + random(old.length); spec.oldFocus = trial % 2 ? [n, n] : [[n, n], [1, 1]]; }
        const labels = trial % 2 === 0 ? [{ line: at(), where: 'in the test' }] : undefined;
        if (labels) spec.labels = labels;
        const host = document.createElement('div'); document.body.append(host);
        const panel = api.render(host, spec).element, totals = api.countChanges(spec);
        if (labels) check(panel.querySelector(`[data-dv-label-line="${labels[0].line}"]`), `label omitted in trial ${trial}`);
        const shown = () => ({ add: panel.querySelectorAll('.dv-row.dv-add').length, del: panel.querySelectorAll('.dv-row.dv-del').length });
        const leftOut = () => Array.from(panel.querySelectorAll('.dv-gap-label')).filter((l) => /left out/.test(l.textContent || '')).length;
        const relevantTab = panel.querySelector('.dv-tab[data-dv-view="relevant"]');
        if (relevantTab) {
          // Relevant shows exactly the changes its tab counts, and marks what it leaves out.
          const s = shown(), n = (sel: string) => Number(relevantTab.querySelector(sel)!.textContent.slice(1));
          check(s.add === n('.dv-plus') && s.del === n('.dv-minus'), `Relevant tab count mismatch in trial ${trial}`);
          check(s.add + s.del < totals.add + totals.del, `tabs without left-out changes in trial ${trial}`);
          check(leftOut() > 0, `Relevant leaves changes out without a marker in trial ${trial}`);
          panel.querySelector<HTMLElement>('.dv-tab[data-dv-view="all"]')!.click();
        }
        check(!leftOut(), `All changes left a change out in trial ${trial}`);
        check(!panel.querySelector('.dv-expand-file, .dv-reset, .dv-viewed'), `panel grew a scope button in trial ${trial}`);
        for (let iterations = 0; ; iterations++) {
          const s = shown();
          check(s.add === totals.add && s.del === totals.del, `accounting mismatch in trial ${trial}`);
          panel.querySelectorAll('.dv-gap-label').forEach((l) => check(Number(/\d+/.exec(l.textContent || '')![0]) > 3, `short fold in trial ${trial}`));
          const buttons = panel.querySelectorAll<HTMLElement>('.dv-gap-btn');
          if (!buttons.length) break;
          buttons[random(buttons.length)].click();
          check(iterations < 250, `nonterminating fold in trial ${trial}`);
        }
        check(!panel.querySelector('.dv-gap'), `fold left after every button in trial ${trial}`);
        host.remove();
      }
    });

    // A focused panel reaches the whole file through All changes and its hairlines alone.
    await page.locator('#focus-both-sides .dv-tab[data-dv-view="all"]').click();
    await page.evaluate(() => {
      const panel = document.getElementById('focus-both-sides')!;
      for (let b = panel.querySelector<HTMLElement>('.dv-gap-btn'); b; b = panel.querySelector<HTMLElement>('.dv-gap-btn')) b.click();
    });
    expect(await page.locator('#focus-both-sides .dv-gap').count()).toBe(0);
    expect(await page.locator('#focus-both-sides .dv-row').count()).toBe(119);
    await page.locator('#focus-both-sides .dv-tab[data-dv-view="relevant"]').click();
    expect(await page.locator('#focus-both-sides .dv-gap-label:text-matches("left out")').count()).toBe(2);

    // The header row folds the panel; its tabs open it without folding.
    const collapsed = () => page.locator('#focus-both-sides').evaluate((p) => p.classList.contains('dv-is-collapsed'));
    await page.locator('#focus-both-sides .dv-path').click();
    expect(await collapsed()).toBe(true);
    expect(await page.locator('#focus-both-sides .dv-body').evaluate((b) => b.getBoundingClientRect().height)).toBe(0);
    await page.locator('#focus-both-sides .dv-tab[data-dv-view="all"]').click();
    expect(await collapsed()).toBe(false);
    await page.locator('#focus-both-sides .dv-collapse').click();
    expect(await collapsed()).toBe(true);
    await page.locator('#focus-both-sides .dv-collapse').click();
    expect(await collapsed()).toBe(false);

    // stateKey: tab, folds and collapse survive a reload; fixtures without one start fresh.
    const stateRows = () => page.locator('#state-key').evaluate((p) => Array.from(p.querySelectorAll('.dv-n-new')).map((n) => n.textContent).filter(Boolean).join(','));
    expect(await page.locator('#state-key').evaluate((p) => p.classList.contains('dv-is-collapsed'))).toBe(true);
    await page.locator('#state-key .dv-path').click();
    await page.locator('#state-key .dv-tab[data-dv-view="all"]').click();
    await page.locator('#state-key .dv-gap-btn').first().click();
    const rows = await stateRows();
    await page.reload();
    await page.waitForFunction(() => (window as unknown as FixtureWindow).testReady === true);
    expect(await page.locator('#state-key').evaluate((p) => [p.classList.contains('dv-is-collapsed'), p.querySelector('.dv-tab[aria-selected="true"]')!.getAttribute('data-dv-view')])).toEqual([false, 'all']);
    expect(await stateRows()).toBe(rows);
    expect(await page.locator('#focus-both-sides .dv-tab[aria-selected="true"]').evaluate((t) => t.getAttribute('data-dv-view'))).toBe('relevant');
    expect(await page.evaluate(() => Object.keys(localStorage))).toEqual(['dv-state:browser-suite:fixture/state']);
    await page.locator('#state-key .dv-collapse').click();
    await page.reload();
    await page.waitForFunction(() => (window as unknown as FixtureWindow).testReady === true);
    expect(await page.locator('#state-key').evaluate((p) => p.classList.contains('dv-is-collapsed'))).toBe(true);

    // Narrow screens: source scrolls sideways inside the panel, never the page.
    await page.setViewportSize({ width: 400, height: 844 });
    const mobileSource = await page.locator('#long-identifiers').evaluate((file) => {
      const body = file.querySelector('.dv-body')!, code = file.querySelector('.dv-code')!;
      const before = body.scrollLeft; body.scrollLeft = 100;
      const after = body.scrollLeft; body.scrollLeft = before;
      return { scrollWidth: body.scrollWidth, clientWidth: body.clientWidth, scrolled: after,
        codeHeight: code.getBoundingClientRect().height, lineHeight: parseFloat(getComputedStyle(code).lineHeight),
        whiteSpace: getComputedStyle(code).whiteSpace, wordBreak: getComputedStyle(code).wordBreak,
        overflowX: getComputedStyle(body).overflowX, pageScrollX: scrollX };
    });
    expect(mobileSource.scrollWidth).toBeGreaterThan(mobileSource.clientWidth);
    expect(mobileSource.scrolled).toBeGreaterThan(0);
    expect(mobileSource.codeHeight).toBeLessThanOrEqual(mobileSource.lineHeight + 1);
    expect([mobileSource.whiteSpace, mobileSource.wordBreak, mobileSource.overflowX, mobileSource.pageScrollX]).toEqual(['pre', 'normal', 'auto', 0]);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.locator('#long-identifiers').screenshot({ path: harness.output(`${theme}-long.png`) });
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: harness.output(`${theme}-mobile.png`) });

    expect(errors).toEqual([]);
    await close();
  }, 180_000);

  test.each(THEMES)('%s: 200 code-diff trials in both tabs, accounting through the folds, no replacement ever partly shown', async (theme) => {
    const { page, errors, close } = await openFixtures(theme);
    const result = await page.evaluate(checkFoldSteps, { specs: codeTrialSpecs(), seed: 81473 });
    expect(result.specs).toBe(200);
    expect(result.clicks).toBeGreaterThan(1000);
    expect(errors).toEqual([]);
    await close();
  }, 300_000);

  test.each(THEMES)('%s: 60 randomized Markdown diffs: block folds say what they hold and step to the end, with no Show bar', async (theme) => {
    const { page, errors, close } = await openFixtures(theme);
    const result = await page.evaluate(() => {
      let seed = 20260924, clicks = 0;
      const random = (n: number) => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % n; };
      const check = (ok: unknown, message: string) => { if (!ok) throw Error(message); };
      const api = (window as unknown as FixtureWindow).DiffView;
      const kinds = [
        (k: number) => [`Paragraph ${k} says one thing.`],
        (k: number) => [`Paragraph ${k} starts here`, `and continues on a second line.`],
        (k: number) => [`- item ${k} a`, `- item ${k} b`, `- item ${k} c`],
        (k: number) => ['```js', `const x${k} = 1;`, `const y${k} = 2;`, '```'],
        (k: number) => [`## Heading ${k}`],
      ];
      const blockFold = /^\d+ unchanged blocks?$|^\d+ blocks? left out · \+\d+ −\d+$/;
      const labelText = (label: Element) => Array.from(label.childNodes).filter((n) => !(n.nodeType === 1 && (n as Element).classList.contains('dv-gap-btn'))).map((n) => n.textContent).join('');
      for (let trial = 0; trial < 60; trial++) {
        const blocks = Array.from({ length: 8 + random(22) }, (_, k) => kinds[random(kinds.length)](k));
        const nextBlocks: Array<Array<string>> = [];
        blocks.forEach((b, k) => {
          const action = random(7);
          if (action === 0) return;
          if (action === 1) { const i = random(b.length); nextBlocks.push(b.map((l, j) => (j === i && !l.startsWith('```') ? l.replace(/\d+/, (d) => `${d} changed`) : l))); }
          else nextBlocks.push(b);
          if (action === 2) nextBlocks.push([`Inserted paragraph ${k}.`]);
        });
        const front = trial % 3 === 0 ? ['---', 'title: Trial', `key: v${trial}`, '---', ''] : [];
        const frontNext = trial % 6 === 0 ? ['---', 'title: Trial', `key: changed${trial}`, '---', ''] : front;
        const doc = (list: Array<Array<string>>, fm: Array<string>) => [...fm, list.map((b) => b.join('\n')).join('\n\n')].join('\n');
        const spec: Record<string, unknown> = { path: `trial-${trial}.md`, old: doc(blocks, front), new: doc(nextBlocks, frontNext) };
        const lineCount = (spec.new as string).split('\n').length;
        const at = () => 1 + random(lineCount);
        if (trial % 3 === 1) spec.focus = [at(), at()].sort((a, b) => a - b);
        if (trial % 3 === 2) spec.focus = [[at(), at()].sort((a, b) => a - b), [at(), at()].sort((a, b) => a - b)];
        const host = document.createElement('div'); document.body.append(host);
        const panel = api.render(host, spec).element, totals = api.countChanges(spec);
        if (!panel.querySelector('.dv-pblocks')) { host.remove(); continue; }
        const folds = () => Array.from(panel.querySelectorAll('.dv-pblocks > .dv-fold-line .dv-gap-label')).map((l) => {
          const text = labelText(l);
          return { text, add: Number(l.querySelector('.dv-plus')?.textContent.slice(1) ?? 0), del: Number(l.querySelector('.dv-minus')?.textContent.slice(1) ?? 0) };
        });
        const tabCount = (view: string, sel: string) => Number(panel.querySelector(`.dv-tab[data-dv-view="${view}"] ${sel}`)!.textContent.slice(1));
        const relevantTab = panel.querySelector('.dv-tab[data-dv-view="relevant"]');
        for (const view of relevantTab ? ['relevant', 'all'] : ['only']) {
          if (view === 'all') panel.querySelector<HTMLElement>('.dv-tab[data-dv-view="all"]')!.click();
          let before = Infinity;
          for (let iterations = 0; ; iterations++) {
            const list = folds();
            check(!panel.querySelector('.dv-fold, .dv-tools, .dv-expand-file, .dv-reset'), `Show bar or toolbar in Markdown trial ${trial}`);
            for (const f of list) {
              check(blockFold.test(f.text), `bad block fold "${f.text}" in Markdown trial ${trial}`);
              check(Number(/\d+/.exec(f.text)![0]) > 0, `empty fold in Markdown trial ${trial}`);
              if (view !== 'relevant') check(!/left out/.test(f.text), `${view} left a change out in Markdown trial ${trial}`);
            }
            const hiddenAdd = list.reduce((n, f) => n + f.add, 0), hiddenDel = list.reduce((n, f) => n + f.del, 0);
            // Relevant plus what its folds hold never exceeds All changes; a shown block may carry more than its focus.
            if (view === 'relevant') check(tabCount('relevant', '.dv-plus') + hiddenAdd <= totals.add && tabCount('relevant', '.dv-minus') + hiddenDel <= totals.del, `Relevant over-counts in Markdown trial ${trial}`);
            const hidden = list.reduce((n, f) => n + Number(/\d+/.exec(f.text)![0]), 0);
            check(hidden <= before, `a step hid more blocks in Markdown trial ${trial}`);
            before = hidden;
            const buttons = panel.querySelectorAll<HTMLElement>('.dv-gap-btn');
            if (!buttons.length) break;
            buttons[random(buttons.length)].click(); clicks++;
            check(iterations < 200, `nonterminating fold in Markdown trial ${trial}`);
          }
          check(!panel.querySelector('.dv-fold-line'), `fold left after every step in Markdown trial ${trial}`);
        }
        host.remove();
      }
      return { clicks };
    });
    expect(result.clicks).toBeGreaterThan(100);
    expect(errors).toEqual([]);
    await close();
  }, 300_000);

  test('keyboard focus stays on a fold step until its fold is gone, then moves to a neighbour; a mouse click shows no ring', async () => {
    const { page, errors, close } = await openFixtures('light');
    await page.locator('#focus-both-sides .dv-path').click();
    await page.locator('#focus-both-sides .dv-tab[data-dv-view="all"]').click();
    const step = page.locator('#focus-both-sides .dv-fold-line').last().locator('.dv-gap-btn', { hasText: '↓' });
    const ctl = await step.getAttribute('data-dv-ctl');
    await step.focus();
    const active = () => page.evaluate((name) => {
      const a = document.activeElement!;
      return { ctl: a.getAttribute('data-dv-ctl'), tag: a.tagName, inPanel: !!a.closest('#focus-both-sides'), visible: a.matches(':focus-visible'), stillThere: !!document.querySelector(`[data-dv-ctl="${name}"]`) };
    }, ctl);
    let presses = 0;
    for (let state = await active(); ; state = await active()) {
      if (!state.stillThere) { expect([state.tag, state.inPanel]).toEqual(['BUTTON', true]); break; }
      expect(state.ctl).toBe(ctl);
      await page.keyboard.press('Enter');
      expect(++presses).toBeLessThan(40);
    }
    expect(presses).toBeGreaterThan(1);
    await page.locator('#focus-both-sides .dv-gap-btn').first().click();
    expect((await active()).visible).toBe(false);
    expect(errors).toEqual([]);
    await close();
  });

  test('horizontal scroll is kept per panel across a reload and a fold step at 400px', async () => {
    const { page, errors, close } = await openFixtures('light');
    await page.setViewportSize({ width: 400, height: 900 });
    const mount = () => page.evaluate(() => {
      const long = (i: number) => `const value${i} = publishReadableResultWithoutBreakingIdentifiers(access_token_with_a_long_unbroken_identifier_${i});`;
      const old = Array.from({ length: 60 }, (_, i) => long(i));
      const spec = { path: 'scroll.js', old: old.join('\n'), new: old.map((l, i) => (i === 30 ? l + ' // changed' : l)).join('\n'), focus: [31, 31], scope: 'scroll-suite', stateKey: 'scroll' };
      const host = document.createElement('div'); host.id = 'scroll-host'; document.body.prepend(host);
      const panel = (window as unknown as FixtureWindow).DiffView.render(host, spec).element;
      panel.classList.remove('dv-is-collapsed');
      return true;
    });
    await mount();
    const body = page.locator('#scroll-host .dv-body');
    await body.evaluate((b) => { b.scrollLeft = 150; });
    await page.waitForTimeout(400);
    await page.locator('#scroll-host .dv-collapse').click();
    await page.locator('#scroll-host .dv-collapse').click();
    await page.reload();
    await page.waitForFunction(() => (window as unknown as FixtureWindow).testReady === true);
    await mount();
    expect(await body.evaluate((b) => b.scrollLeft)).toBe(150);
    await page.locator('#scroll-host .dv-gap-btn').first().click();
    expect(await body.evaluate((b) => b.scrollLeft)).toBe(150);
    expect(errors).toEqual([]);
    await close();
  });
});
