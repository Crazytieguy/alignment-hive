import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { describe, expect, test } from 'bun:test';
import * as Diff from 'diff';
import DiffMatchPatch from 'diff-match-patch';
import hljs from 'highlight.js';
import { parseHTML } from 'linkedom';
import MarkdownIt from 'markdown-it';
import { VISIONS, lab, paletteReport, simulate } from './browser/palette';
import { diffEngine } from './render';

const css = readFileSync(new URL('../../assets/review-diff.css', import.meta.url), 'utf8');
const pageCss = readFileSync(new URL('../../assets/review-page.css', import.meta.url), 'utf8');
type Range = [number, number];
interface FileSpec {
  path: string;
  old: string;
  new: string;
  status?: string;
  scope?: string;
  focus?: Range | Array<Range>;
  oldFocus?: Range | Array<Range>;
  labels?: Array<{ line: number; where: string }>;
  title?: string;
  stateKey?: string;
  mountId?: string;
  viewed?: boolean;
}
interface Handle { element: HTMLElement; file: FileSpec; rerender: () => void }
interface DiffView {
  render: (container: HTMLElement | null, file: FileSpec) => Handle | null;
  countChanges: (file: FileSpec) => { add: number; del: number };
}
type StorageMode = 'ok' | 'denied' | 'readonly';

// Each test gets a fresh DOM, VM, caches, and inert storage. The
// real jsdiff/dmp/markdown-it engines run locally; no CDN or browser is needed.
// Pass a shared `storage` map to simulate a reload of the same page.
function setup(markdown = true, highlight = false, storage = new Map<string, string>(), mode: StorageMode = 'ok') {
  const { document } = parseHTML('<!doctype html><html><body><main></main></body></html>');
  const scrolls: Array<[number, number]> = [];
  const denied = () => { throw new Error('SecurityError: storage is disabled'); };
  const window = {
    Diff, diff_match_patch: DiffMatchPatch, hljs: highlight ? hljs : undefined,
    markdownit: markdown ? MarkdownIt : undefined,
    localStorage: {
      getItem: (key: string) => (mode === 'denied' ? denied() : storage.get(key) ?? null),
      setItem: (key: string, value: string) => (mode === 'ok' ? storage.set(key, value) : denied()),
      removeItem: (key: string) => (mode === 'ok' ? storage.delete(key) : denied()),
    },
    scrollX: 37, scrollY: 510,
    scrollTo({ left, top }: { left: number; top: number }) {
      this.scrollX = left; this.scrollY = top; scrolls.push([left, top]);
    },
    DiffView: undefined as DiffView | undefined,
  };
  runInNewContext(diffEngine, { window, document, NodeFilter: { SHOW_TEXT: 4 } });
  const api = window.DiffView!;
  const host = document.querySelector('main')!;
  function render(file: Partial<FileSpec> = {}) {
    return api.render(host, { path: 'test.ts', old: '', new: '', ...file })!;
  }
  return { api, host, document, render, window, scrolls, storage };
}
function query(host: ParentNode, selector: string): Array<HTMLElement> {
  return Array.from(host.querySelectorAll<HTMLElement>(selector));
}
function click(host: ParentNode, text: string) {
  const button = query(host, 'button').find((b) => b.textContent === text);
  expect(button).toBeDefined();
  button!.click();
}
function tab(host: ParentNode, view: 'relevant' | 'all'): HTMLElement {
  return host.querySelector<HTMLElement>(`.dv-tab[data-dv-view="${view}"]`)!;
}
function codes(host: ParentNode): Array<string> {
  return query(host, '.dv-code').map((node) => node.textContent || '');
}
function numbers(host: ParentNode, side: 'old' | 'new'): Array<number> {
  return query(host, `.dv-n-${side}`).filter((n) => n.textContent).map((n) => Number(n.textContent));
}
// A fold's label without its step buttons, and the buttons on their own.
function gapLabels(host: ParentNode): Array<string> {
  return query(host, '.dv-gap-label').map((label) =>
    Array.from(label.childNodes).filter((n) => !(n.nodeType === 1 && (n as HTMLElement).classList.contains('dv-gap-btn'))).map((n) => n.textContent).join(''));
}
function gapButtons(host: ParentNode): Array<Array<string>> {
  return query(host, '.dv-gap-label').map((label) => query(label, '.dv-gap-btn').map((b) => b.textContent || ''));
}
function shown(host: ParentNode) {
  return { add: query(host, '.dv-row.dv-add').length, del: query(host, '.dv-row.dv-del').length };
}
function tabCounts(host: ParentNode, view: 'relevant' | 'all') {
  const t = tab(host, view);
  return { add: Number(t.querySelector('.dv-plus')!.textContent.slice(1)), del: Number(t.querySelector('.dv-minus')!.textContent.slice(1)) };
}
function lines(n: number): string { return Array.from({ length: n }, (_, i) => `line ${i + 1}`).join('\n'); }
function edited(n: number, at: Array<number>): string {
  return Array.from({ length: n }, (_, i) => (at.includes(i + 1) ? `changed ${i + 1}` : `line ${i + 1}`)).join('\n');
}
function expandAll(host: ParentNode) {
  for (let i = 0; host.querySelector('.dv-gap-btn'); i++) {
    expect(i).toBeLessThan(500);
    host.querySelector<HTMLElement>('.dv-gap-btn')!.click();
  }
}

describe('vendored DiffView review disclosure', () => {
  test('focus reveals three context lines; hidden stretches are hairlines that say what they hold', () => {
    const { render, host } = setup();
    render({ old: lines(30), new: edited(30, [25]), focus: [10, 10] });
    expect(numbers(host, 'new')).toEqual([7, 8, 9, 10, 11, 12, 13]);
    expect(query(host, '.dv-gap.dv-fold-line')).toHaveLength(2);
    expect(query(host, '.dv-fold')).toHaveLength(0);
    expect(gapLabels(host)).toEqual(['6 unchanged lines', '18 lines left out · +1 −1']);
    // A stretch at the start of the file can only grow upward, one at the end only downward.
    expect(gapButtons(host)).toEqual([['↑ 10'], ['↓ 10']]);
    expect(query(host, '.dv-gap-btn').map((b) => b.title)).toEqual(['Show 10 more lines from the bottom', 'Show 10 more lines from the top']);
    expect(host.textContent).not.toContain('hidden');
  });

  test('without a focus every change is shown and only unchanged stretches fold', () => {
    const { render, host } = setup();
    render({ old: lines(40), new: edited(40, [5, 30]) });
    expect(shown(host)).toEqual({ add: 2, del: 2 });
    expect(gapLabels(host)).toEqual(['18 unchanged lines', '7 unchanged lines']);
    expect(gapButtons(host)).toEqual([['↓ 10', '↑ 10'], ['↓ 10']]);
    expect(query(host, '.dv-tabs,.dv-bracket,.dv-core,.dv-other')).toHaveLength(0);
    const added = setup();
    added.render({ new: lines(4) });
    expect(numbers(added.host, 'new')).toEqual([1, 2, 3, 4]);
    expect(query(added.host, '.dv-gap')).toHaveLength(0);
  });

  test('stretches of three or fewer rows are never folded; four are', () => {
    for (const [n, at, folded] of [[15, [4, 12], []], [17, [4, 14], []], [18, [4, 15], ['4 unchanged lines']]] as const) {
      for (const focus of [{}, { focus: [4, 4] as Range, oldFocus: [at[1], at[1]] as Range }]) {
        const { render, host } = setup();
        render({ old: lines(n), new: edited(n, [...at]), ...focus });
        expect(gapLabels(host)).toEqual([...folded]);
        expect(shown(host)).toEqual({ add: 2, del: 2 });
      }
    }
  });

  test('code metadata is absent', () => {
    const { render, host } = setup();
    render({ new: 'const value = 1;' });
    expect(host.textContent).not.toContain('Line diff');
    expect(host.textContent).not.toContain('removals first');
  });

  test('strict inclusive head/base focus keeps original coordinates and whole replacements', () => {
    const { render, host } = setup();
    render({ old: 'before\nold one\nold two\nafter', new: 'before\ninserted\nnew one\nnew two\nafter', focus: [3, 4], oldFocus: [2, 2] });
    expect(numbers(host, 'old')).toEqual([1, 2, 3, 4]);
    expect(numbers(host, 'new')).toEqual([1, 2, 3, 4, 5]);
    expect(codes(host)).toEqual(['before', 'old one', 'old two', 'inserted', 'new one', 'new two', 'after']);
    expect(query(host, '.dv-gap,.dv-tabs')).toHaveLength(0);
  });

  test('head focus does not fold a one-row deletion', () => {
    const { render, host } = setup();
    render({ old: 'old value', new: 'new value', focus: [1, 1] });
    expect(codes(host)).toEqual(['old value', 'new value']);
    expect(numbers(host, 'old')).toEqual([1]);
  });

  test.each(['added', 'deleted', 'modified'])('no focus shows every change of %s files with their labels', (status) => {
    const { render, host } = setup();
    render({ status, old: status === 'added' ? '' : 'old', new: status === 'deleted' ? '' : 'new', labels: [{ line: 1, where: 'Shown label' }] });
    expect(codes(host).length).toBe(status === 'modified' ? 2 : 1);
    expect(query(host, '.dv-review-label').map((n) => n.textContent)).toEqual(status === 'deleted' ? [] : ['→ Shown label']);
    expect(query(host, '.dv-gap,.dv-fold,.dv-tabs')).toHaveLength(0);
    expect(query(host, '.dv-chip').map((n) => n.textContent)).toEqual(status === 'modified' ? [] : [status]);
  });

  test('focus works in a wholly unchanged file without a hunk', () => {
    const { render, host } = setup();
    render({ old: lines(12), new: lines(12), focus: [5, 6] });
    expect(numbers(host, 'new')).toEqual(Array.from({ length: 12 }, (_, i) => i + 1));
    expect(numbers(host, 'old')).toEqual(Array.from({ length: 12 }, (_, i) => i + 1));
    expect(query(host, '.dv-gap')).toHaveLength(0);
  });

  test('old-only focus works for a deletion and equal context', () => {
    const { render, host } = setup();
    render({ old: lines(6), new: '', oldFocus: [3, 4] });
    expect(numbers(host, 'old')).toEqual([1, 2, 3, 4, 5, 6]);
    expect(numbers(host, 'new')).toEqual([]);
  });

  test('step buttons reveal ten rows from either end and keep neighbouring folds', () => {
    const { render, host, api } = setup();
    render({ old: lines(45), new: lines(45), focus: [23, 23] });
    expect(numbers(host, 'new')).toEqual([20, 21, 22, 23, 24, 25, 26]);
    click(host, '↑ 10');
    expect(numbers(host, 'new')).toEqual(Array.from({ length: 17 }, (_, i) => i + 10));
    expect(gapLabels(host)).toEqual(['9 unchanged lines', '19 unchanged lines']);
    click(host, '↓ 10');
    expect(numbers(host, 'new')).toEqual(Array.from({ length: 27 }, (_, i) => i + 10));
    expandAll(host);
    expect(numbers(host, 'new')).toEqual(Array.from({ length: 45 }, (_, i) => i + 1));
    expect(query(host, '.dv-gap')).toHaveLength(0);
  });

  test('a partly revealed stretch of other changes recounts what is still left out', () => {
    const { render, host } = setup();
    render({ old: lines(60), new: edited(60, [30, 45]), focus: [5, 5] });
    // Rows, not head lines: a replacement's removed row counts too.
    expect(gapLabels(host)).toEqual(['54 lines left out · +2 −2']);
    click(host, '↓ 10');
    expect(gapLabels(host)).toEqual(['44 lines left out · +2 −2']);
    click(host, '↓ 10');
    click(host, '↓ 10');
    expect(gapLabels(host)).toEqual(['24 lines left out · +1 −1']);
    // Other changes revealed from Relevant are washed out, with no bracket rows: one arriving with the step would
    // push down everything above the fold that was stepped.
    expect(query(host, '.dv-row.dv-other.dv-add').map((r) => r.querySelector('.dv-n-new')!.textContent)).toEqual(['30']);
    expect(query(host, '.dv-bracket')).toHaveLength(0);
  });

  test('labels are full-width arrow rows under their head row and always disclose it', () => {
    const { render, host } = setup();
    render({ old: lines(40), new: edited(40, [5, 30]), focus: [5, 5],
      labels: [{ line: 5, where: 'First label' }, { line: 5, where: '<img src="https://invalid.test/x">' },
        { line: 30, where: 'outside the focus' }, { line: 20, where: 'unchanged guidance' }] });
    const labels = query(host, '.dv-review-label');
    expect(labels.map((n) => n.textContent)).toEqual(['→ First label', '→ <img src="https://invalid.test/x">', '→ unchanged guidance', '→ outside the focus']);
    expect(labels.every((n) => n.classList.contains('dv-label-row') && n.parentElement!.classList.contains('dv-rows'))).toBe(true);
    expect(labels.map((n) => n.getAttribute('data-dv-label-line'))).toEqual(['5', '5', '20', '30']);
    expect(labels[0].previousElementSibling!.querySelector('.dv-n-new')!.textContent).toBe('5');
    expect(labels[3].previousElementSibling!.querySelector('.dv-n-new')!.textContent).toBe('30');
    expect(query(host, '.dv-code .dv-review-label,.dv-chip.dv-review-label')).toHaveLength(0);
    expect(query(host, 'img')).toHaveLength(0);
    // Labelled lines count as focus, so no tab can claim a change is left out.
    expect(query(host, '.dv-tabs')).toHaveLength(0);
    expect(shown(host)).toEqual({ add: 2, del: 2 });
  });

  test('Relevant | All changes tabs appear only when the focus leaves changes out, counted from the same rows', () => {
    const { api, render, host } = setup();
    const file = { old: lines(110), new: edited(110, [10, 11, 12, 50, 51, 52, 90, 91, 92]), focus: [51, 51] as Range, title: 'the caller side' };
    const handle = render(file);
    expect(query(host, '.dv-tab').map((t) => t.querySelector('.dv-tab-name')!.textContent)).toEqual(['Relevant', 'All changes']);
    expect(host.querySelector('.dv-tabs')!.getAttribute('role')).toBe('tablist');
    expect(host.querySelector('.dv-tabs')!.getAttribute('aria-label')).toBe('Lines shown');
    expect(tab(host, 'relevant').getAttribute('aria-selected')).toBe('true');
    expect(tab(host, 'all').getAttribute('aria-selected')).toBe('false');
    expect(query(host, '.dv-head > .dv-totals')).toHaveLength(0);
    expect(tabCounts(host, 'relevant')).toEqual({ add: 3, del: 3 });
    expect(tabCounts(host, 'all')).toEqual({ add: 9, del: 9 });
    expect(api.countChanges(handle.file)).toEqual({ add: 9, del: 9 });
    expect(shown(host)).toEqual(tabCounts(host, 'relevant'));
    expect(numbers(host, 'new')).toEqual([47, 48, 49, 50, 51, 52, 53, 54, 55]);
    expect(gapLabels(host)).toEqual(['49 lines left out · +3 −3', '58 lines left out · +3 −3']);
    expect(query(host, '.dv-bracket,.dv-other')).toHaveLength(0);
    expect(host.querySelector('.dv-head .dv-title')!.textContent).toBe('the caller side');
    const relevantRows = query(host, '.dv-row').length;

    tab(host, 'all').click();
    expect(tab(host, 'all').getAttribute('aria-selected')).toBe('true');
    expect(shown(host)).toEqual(tabCounts(host, 'all'));
    expect(query(host, '.dv-row.dv-core')).toHaveLength(relevantRows);
    expect(query(host, '.dv-row.dv-other.dv-add')).toHaveLength(6);
    expect(query(host, '.dv-bracket-open')).toHaveLength(1);
    expect(query(host, '.dv-bracket-open span').map((n) => n.textContent)).toEqual(['relevant']);
    expect(query(host, '.dv-bracket-open')[0].getAttribute('aria-label')).toBe('Relevant lines');
    expect(query(host, '.dv-bracket-close')[0].getAttribute('aria-label')).toBe('End of relevant lines');
    // All changes still folds long unchanged stretches, never changes.
    expect(gapLabels(host).every((l) => /^\d+ unchanged lines$/.test(l))).toBe(true);
    expect(gapLabels(host).length).toBeGreaterThan(0);
    expandAll(host);
    expect(query(host, '.dv-row')).toHaveLength(119);

    tab(host, 'relevant').click();
    expect(gapLabels(host)).toEqual(['49 lines left out · +3 −3', '58 lines left out · +3 −3']);
  });

  test('tabs are hidden when the focus covers every change or there is no focus', () => {
    for (const extra of [{ focus: [10, 12] as Range }, {}]) {
      const { render, host } = setup();
      render({ old: lines(30), new: edited(30, [10, 11, 12]), ...extra });
      expect(query(host, '.dv-tabs')).toHaveLength(0);
      expect(query(host, '.dv-head > .dv-totals .dv-plus').map((n) => n.textContent)).toEqual(['+3']);
      expect(query(host, '.dv-expand-file,.dv-reset,.dv-viewed,input')).toHaveLength(0);
      expect(host.textContent).not.toContain('Expand whole file');
    }
  });

  test('arrow keys move between tabs and focus the selected one', () => {
    const { render, host, document } = setup();
    render({ old: lines(60), new: edited(60, [5, 50]), focus: [5, 5] });
    const tabs = host.querySelector('.dv-tabs')!;
    const focused: Array<string | null> = [];
    for (const t of query(host, '.dv-tab')) t.focus = () => { focused.push(t.getAttribute('data-dv-view')); };
    const key = (name: string) => {
      const event = new (document.defaultView!.Event)('keydown', { bubbles: true, cancelable: true });
      Object.defineProperty(event, 'key', { value: name });
      tabs.dispatchEvent(event);
    };
    key('ArrowRight');
    expect(tab(host, 'all').getAttribute('aria-selected')).toBe('true');
    key('ArrowLeft');
    expect(tab(host, 'relevant').getAttribute('aria-selected')).toBe('true');
    key('Enter');
    expect(tab(host, 'relevant').getAttribute('aria-selected')).toBe('true');
    expect(focused).toEqual(['all', 'relevant']);
  });

  test('focus and old-focus accept several ranges on both sides', () => {
    const { render, host } = setup();
    const old = lines(120);
    const next = edited(120, [10, 40, 70, 100]);
    render({ old, new: next, focus: [[10, 10], [100, 100]], oldFocus: [[40, 40]] });
    // An old-side focus reveals its whole replacement, head line included.
    expect(query(host, '.dv-row.dv-add .dv-n-new').map((n) => n.textContent)).toEqual(['10', '40', '100']);
    expect(query(host, '.dv-row.dv-del .dv-n-old').map((n) => n.textContent)).toEqual(['10', '40', '100']);
    expect(tabCounts(host, 'relevant')).toEqual({ add: 3, del: 3 });
    expect(tabCounts(host, 'all')).toEqual({ add: 4, del: 4 });
    expect(gapLabels(host).filter((l) => l.includes('left out'))).toEqual(['54 lines left out · +1 −1']);
    tab(host, 'all').click();
    expect(query(host, '.dv-bracket-open')).toHaveLength(3);
  });

  test('panels start collapsed; the header row and chevron toggle, its controls do not', () => {
    const { render, host } = setup();
    const handle = render({ old: lines(60), new: edited(60, [5, 50]), focus: [5, 5] });
    const panel = handle.element;
    const chevron = panel.querySelector<HTMLElement>('.dv-collapse')!;
    expect(panel.classList.contains('dv-is-collapsed')).toBe(true);
    expect(chevron.getAttribute('aria-expanded')).toBe('false');
    expect(chevron.getAttribute('aria-label')).toBe('Show this diff');
    panel.querySelector<HTMLElement>('.dv-path')!.click();
    expect(panel.classList.contains('dv-is-collapsed')).toBe(false);
    expect(chevron.getAttribute('aria-label')).toBe('Collapse this diff');
    chevron.click();
    expect(panel.classList.contains('dv-is-collapsed')).toBe(true);
    chevron.click();
    expect(panel.classList.contains('dv-is-collapsed')).toBe(false);
    // A tab switches view without folding; from a folded panel it opens the panel.
    tab(host, 'all').click();
    expect(panel.classList.contains('dv-is-collapsed')).toBe(false);
    chevron.click();
    tab(host, 'relevant').click();
    expect(panel.classList.contains('dv-is-collapsed')).toBe(false);
    expect(tab(host, 'relevant').getAttribute('aria-selected')).toBe('true');
  });

  test('prose focus uses full-source coordinates after frontmatter and blank lines', () => {
    const { render, host } = setup();
    const old = '---\ntitle: Old title\n---\n\n# Heading\n\nFirst old paragraph.\n\nMiddle one.\n\nMiddle two.\n\nMiddle three.\n\n' +
      'Selected old paragraph\non two lines.\n\nMiddle four.\n\nMiddle five.\n\nMiddle six.\n\nLast old paragraph.';
    render({ path: 'copy.md', old, new: old.replaceAll('old', 'new'),
      focus: [16, 16], labels: [{ line: 15, where: 'Paragraph label' }, { line: 16, where: 'Second label' }] });
    expect(query(host, '.dv-pblocks')).toHaveLength(1);
    expect(host.textContent).toContain('Selected');
    // Three source lines of context around the whole replacement: one paragraph each side.
    expect(host.textContent).toContain('Middle three.');
    expect(host.textContent).toContain('Middle four.');
    for (const hidden of ['Old title', 'Heading', 'First', 'Middle two.', 'Middle five.', 'Last']) expect(host.textContent).not.toContain(hidden);
    expect(query(host, '.dv-review-label').map((n) => n.textContent)).toEqual(['→ Paragraph label', '→ Second label']);
    expect(query(host, '.dv-review-label').every((n) => n.parentElement!.querySelector('.dv-prose'))).toBe(true);
    expect(gapLabels(host)).toEqual(['5 blocks left out · +1 \u22121', '3 blocks left out · +1 \u22121']);
    expect(gapButtons(host)).toEqual([['↑ 3'], ['↓ 3']]);
    expect(tabCounts(host, 'relevant')).toEqual({ add: 1, del: 1 });
    expect(tabCounts(host, 'all')).toEqual({ add: 3, del: 3 });
  });

  test('old prose focus is independent of shifted head coordinates', () => {
    const { render, host } = setup();
    const paras = Array.from({ length: 12 }, (_, i) => `Paragraph ${i + 1}.`);
    const next = ['New introduction.', ...paras.slice(0, 5), 'Selected new paragraph.', ...paras.slice(6)];
    render({ path: 'copy.md', old: paras.join('\n\n'), new: next.join('\n\n'), oldFocus: [11, 11] });
    expect(host.textContent).toContain('Selected new paragraph.');
    expect(host.textContent).not.toContain('Paragraph 1.');
    expect(host.textContent).not.toContain('Paragraph 12.');
    expect(host.textContent).not.toContain('New introduction');
    tab(host, 'all').click();
    expect(host.textContent).toContain('New introduction');
  });

  test('no-focus prose shows every changed block, including frontmatter and nested code', () => {
    const { render, host } = setup();
    render({ path: 'copy.md', old: '', new: '---\ntitle: Metadata\n---\n\n# Added heading\n\nParagraph.\n\n- Item\n\n  ```js\n  const added = 1;\n  ```' });
    expect(query(host, '.dv-pblocks')).toHaveLength(1);
    expect(host.textContent).toContain('Metadata');
    expect(host.textContent).toContain('const added = 1;');
    expect(query(host, '.dv-meta.dv-p-add')).toHaveLength(1);
    expect(query(host, '.dv-meta .dv-meta-sign,.dv-card,.dv-row')).toHaveLength(0);
    expect(query(host, '.dv-p-add .dv-prose pre')).toHaveLength(1);
    expect(query(host, '.dv-p-add').every((node) => node.querySelector('.dv-label')?.textContent === '+added')).toBe(true);
    expect(query(host, '.dv-fold,.dv-gap,.dv-tabs')).toHaveLength(0);
  });

  test('unchanged prose blocks fold behind a hairline stepped three blocks at a time', () => {
    const { render, host } = setup();
    const paras = Array.from({ length: 10 }, (_, i) => `Paragraph ${i + 1}.`);
    render({ path: 'copy.md', old: paras.join('\n\n'), new: [...paras.slice(0, 9), 'Changed ten.'].join('\n\n') });
    expect(gapLabels(host)).toEqual(['8 unchanged blocks']);
    expect(gapButtons(host)).toEqual([['↑ 3']]);
    expect(query(host, '.dv-gap-btn')[0].title).toBe('Show 3 more blocks from the bottom');
    expect(host.textContent).not.toContain('Paragraph 1.');
    click(host, '↑ 3');
    expect(gapLabels(host)).toEqual(['5 unchanged blocks']);
    expect(host.textContent).toContain('Paragraph 6.');
    // Two blocks of one line each are too few to fold: they show.
    click(host, '↑ 3');
    expect(query(host, '.dv-fold-line')).toHaveLength(0);
    expect(host.textContent).toContain('Paragraph 1.');
    expect(query(host, '.dv-fold, .dv-tools')).toHaveLength(0);
  });

  test.each(['list', 'quote'])('nested %s fence rows honor original coordinates without forcing source', (kind) => {
    const { render, host } = setup();
    const nested = kind === 'list'
      ? '- Item\n\n  ```js\n  const first = 1;\n  const chosen = 2;\n  const last = 3;\n  ```'
      : '> Item\n>\n> ```js\n> const first = 1;\n> const chosen = 2;\n> const last = 3;\n> ```';
    const old = '---\ntitle: Same\n---\n\n' + nested + '\n\nTail paragraph.';
    render({ path: 'nested.md', old, new: old.replace('chosen = 2', 'chosen = 20'), focus: [9, 9], oldFocus: [9, 9], labels: [{ line: 9, where: 'Nested code' }] });
    expect(query(host, '.dv-pblocks')).toHaveLength(1);
    expect(codes(host)).toEqual(['const first = 1;', 'const chosen = 2;', 'const chosen = 20;', 'const last = 3;']);
    expect(numbers(host, 'old')).toEqual([8, 9, 10]);
    expect(numbers(host, 'new')).toEqual([8, 9, 10]);
    expect(query(host, '.dv-review-label')[0].previousElementSibling!.querySelector('.dv-code')!.textContent).toBe('const chosen = 20;');
    expect(query(host, '.dv-tabs')).toHaveLength(0);
  });

  test('flat frontmatter stays paragraph-like when focused', () => {
    const { render, host } = setup();
    const tail = Array.from({ length: 6 }, (_, i) => `Not focused ${i}.`).join('\n\n');
    render({ path: 'meta.md', old: '---\ntitle: Old title\nowner: Existing owner\n---\n\n' + tail,
      new: '---\ntitle: New title\nauthor: Added author\n---\n\n' + tail, focus: [2, 3],
      labels: [{ line: 2, where: 'Page title' }] });
    expect(query(host, '.dv-meta')).toHaveLength(1);
    expect(query(host, '.dv-meta .dv-prose').length).toBeGreaterThan(0);
    expect(host.textContent).not.toContain('Not focused 5.');
    expect(query(host, '.dv-meta .dv-review-label')[0].textContent).toBe('→ Page title');
    expect(query(host, '.dv-meta-entry.dv-p-del dt')[0].textContent).toBe('owner−');
    expect(query(host, '.dv-meta-entry.dv-p-add dt')[0].textContent).toBe('author+');
    expect(query(host, '.dv-meta-entry.dv-pb dd .dv-prose').map((node) => node.textContent)).toEqual(['Existing owner', 'Added author']);
    expect(query(host, '.dv-meta-entry.dv-pb .dv-ins,.dv-meta-entry.dv-pb .dv-del-mark')).toHaveLength(0);
  });

  test('complex frontmatter is a focused source card, not a full-file source fallback', () => {
    const { render, host } = setup();
    const tail = Array.from({ length: 6 }, (_, i) => `A paragraph ${i}.`).join('\n\n');
    const old = '---\ntitle: |\n  old title\n  extra line\n---\n\n' + tail;
    render({ path: 'meta.md', old, new: old.replace('old title', 'new title'), focus: [3, 3], oldFocus: [3, 3] });
    expect(query(host, '.dv-pblocks')).toHaveLength(1);
    expect(codes(host)).toEqual(['---', 'title: |', '  old title', '  new title', '  extra line', '---']);
    expect(numbers(host, 'new')).toEqual([1, 2, 3, 4, 5]);
    expect(host.textContent).toContain('extra line');
    expect(host.textContent).not.toContain('A paragraph 5.');
    const added = setup();
    added.render({ path: 'meta.md', old: '', new: old, focus: [3, 3] });
    expect(query(added.host, '.dv-source-block.dv-p-add')).toHaveLength(1);
    expect(codes(added.host)).toEqual(['---', 'title: |', '  old title', '  extra line', '---']);
    expect(numbers(added.host, 'new')).toEqual([1, 2, 3, 4, 5]);
    expect(query(added.host, '.dv-row.dv-add,.dv-row.dv-del')).toHaveLength(0);
    expect(added.host.textContent).toContain('extra line');
  });

  test('tab choice and folds survive prose/source toggles', () => {
    const { render, host } = setup();
    const paras = Array.from({ length: 14 }, (_, i) => `Paragraph ${i + 1}.`);
    const next = paras.map((p, i) => (i === 1 || i === 12 ? p.replace('.', ' changed.') : p));
    render({ path: 'copy.md', old: paras.join('\n\n'), new: next.join('\n\n'), focus: [3, 3] });
    expect(host.textContent).not.toContain('Paragraph 13 changed.');
    tab(host, 'all').click();
    expect(host.textContent).toContain('Paragraph 13 changed.');
    click(host, 'View source');
    expect(tab(host, 'all').getAttribute('aria-selected')).toBe('true');
    expect(shown(host)).toEqual({ add: 2, del: 2 });
    click(host, 'View rendered');
    expect(query(host, '.dv-pblocks')).toHaveLength(1);
    expect(host.textContent).toContain('Paragraph 13 changed.');
  });

  test('prose oldFocus can disclose a verbatim moved block at its head location', () => {
    const { render, host } = setup();
    const kept = Array.from({ length: 10 }, (_, i) => `Kept ${i}.`);
    const tail = Array.from({ length: 5 }, (_, i) => `Tail ${i}.`);
    render({ path: 'move.md', old: ['Moved paragraph.', ...kept, ...tail].join('\n\n'),
      new: [...kept, 'Moved paragraph.', ...tail, 'Entire added paragraph.'].join('\n\n'), oldFocus: [1, 1],
      labels: [{ line: 21, where: 'New location' }] });
    expect(query(host, '.dv-p-moved .dv-prose')[0].textContent).toContain('Moved paragraph.');
    expect(host.textContent).not.toContain('Kept 5.');
    expect(query(host, '.dv-review-label')[0].textContent).toBe('→ New location');
    expect(host.textContent).not.toContain('Entire added paragraph.');
    tab(host, 'all').click();
    expect(query(host, '.dv-p-add .dv-prose')[0].textContent).toContain('Entire added paragraph.');
    expect(query(host, '.dv-p-add .dv-ins,.dv-p-add .dv-del-mark')).toHaveLength(0);
  });
});

describe('DiffView disclosure', () => {
  test('a focus inside a pure addition shows the focused lines and their context, not the whole run', () => {
    const { render, host } = setup();
    render({ status: 'added', old: '', new: lines(60), focus: [30, 30] });
    expect(numbers(host, 'new')).toEqual([27, 28, 29, 30, 31, 32, 33]);
    expect(gapLabels(host)).toEqual(['26 lines left out · +26 \u22120', '27 lines left out · +27 \u22120']);
  });

  test('a step never ends inside a replacement: its rows show all or none', () => {
    const { render, host } = setup();
    const next = Array.from({ length: 80 }, (_, i) => (i + 1 >= 30 && i + 1 <= 41 ? `replaced ${i + 1}` : `line ${i + 1}`)).join('\n');
    render({ old: lines(80), new: next, focus: [5, 5] });
    // Relevant folds the replacement away; stepping down from the focus crosses it.
    const seen: Array<number> = [];
    for (let i = 0; i < 12 && host.querySelector('.dv-gap-btn'); i++) {
      const { add, del } = shown(host);
      expect([add === del, add === 0 || add === 12]).toEqual([true, true]);
      seen.push(add);
      click(host, '↓ 10');
    }
    expect(seen).toContain(0);
    expect(seen).toContain(12);
    expect(shown(host)).toEqual({ add: 12, del: 12 });
  });

  test('a stepped prose fold and the horizontal scroll survive a reload', () => {
    const paras = Array.from({ length: 20 }, (_, i) => `Paragraph ${i + 1}.`);
    const file = { path: 'copy.md', old: paras.join('\n\n'), new: [...paras.slice(0, 19), 'Changed.'].join('\n\n'), scope: 's', stateKey: 'k' };
    const first = setup();
    first.render(file).element.classList.remove('dv-is-collapsed');
    click(first.host, '↑ 3');
    expect(gapLabels(first.host)).toEqual(['15 unchanged blocks']);
    expect(JSON.parse(first.storage.get('dv-state:s:k')!).pgaps).toEqual({ p0: { top: 0, bottom: 3 } });
    first.storage.set('dv-state:s:k', JSON.stringify({ ...JSON.parse(first.storage.get('dv-state:s:k')!), scrollX: 120 }));
    const reload = setup(true, false, first.storage);
    reload.render({ ...file });
    expect(gapLabels(reload.host)).toEqual(['15 unchanged blocks']);
    expect(JSON.parse(reload.storage.get('dv-state:s:k') ?? first.storage.get('dv-state:s:k')!).scrollX).toBe(120);
  });
});

describe('DiffView remembered state', () => {
  const spec = () => ({ old: lines(80), new: edited(80, [5, 60]), focus: [5, 5] as Range,
    scope: 'review-1', stateKey: 'item-a/0' });

  test('stateKey keeps tab, folds, source view and collapse across a reload', () => {
    const first = setup();
    first.render(spec());
    expect(first.storage.size).toBe(0);
    tab(first.host, 'all').click();
    click(first.host, '↓ 10');
    const rows = numbers(first.host, 'new');
    const saved = JSON.parse(first.storage.get('dv-state:review-1:item-a/0')!);
    expect(saved).toEqual({ gaps: expect.any(Object), pgaps: {}, view: 'all', source: false, collapsed: false, scrollX: 0 });
    expect([...first.storage.keys()]).toEqual(['dv-state:review-1:item-a/0']);

    const reload = setup(true, false, first.storage);
    reload.render(spec());
    expect(query(reload.host, '.dv-is-collapsed')).toHaveLength(0);
    expect(tab(reload.host, 'all').getAttribute('aria-selected')).toBe('true');
    expect(numbers(reload.host, 'new')).toEqual(rows);
    reload.host.querySelector<HTMLElement>('.dv-collapse')!.click();

    const again = setup(true, false, first.storage);
    again.render(spec());
    expect(query(again.host, '.dv-is-collapsed')).toHaveLength(1);
    // Another evidence key or review scope starts fresh.
    again.render({ ...spec(), stateKey: 'item-b/0' });
    again.render({ ...spec(), scope: 'review-2' });
    expect(query(again.host, '.dv-file').map((f) => f.classList.contains('dv-is-collapsed'))).toEqual([true, true, true]);
    expect(query(again.host, '.dv-tab[data-dv-view="relevant"][aria-selected="true"]')).toHaveLength(2);
  });

  test('markdown source view persists', () => {
    const first = setup();
    const file = { path: 'copy.md', old: 'Old.', new: 'New.', scope: 's', stateKey: 'k' };
    first.render(file);
    click(first.host, 'View source');
    const reload = setup(true, false, first.storage);
    reload.render({ ...file });
    expect(query(reload.host, '.dv-pblocks')).toHaveLength(0);
    expect(query(reload.host, 'button').map((b) => b.textContent)).toContain('View rendered');
  });

  test('without stateKey nothing is written', () => {
    const { render, host, storage } = setup();
    render({ old: lines(80), new: edited(80, [5, 60]), focus: [5, 5], scope: 'review-1' });
    tab(host, 'all').click();
    host.querySelector<HTMLElement>('.dv-collapse')!.click();
    expect(storage.size).toBe(0);
  });

  test.each(['denied', 'readonly'] as const)('%s storage is harmless', (mode) => {
    const { render, host } = setup(true, false, new Map(), mode);
    const handle = render(spec());
    expect(handle.element.classList.contains('dv-is-collapsed')).toBe(true);
    tab(host, 'all').click();
    click(host, '↓ 10');
    handle.element.querySelector<HTMLElement>('.dv-collapse')!.click();
    expect(handle.element.classList.contains('dv-is-collapsed')).toBe(true);
    expect(shown(host)).toEqual({ add: 2, del: 2 });
  });

  test.each([
    '{', '"text"', '42', '[]', 'null',
    '{"gaps":"x","pgaps":[1],"view":"bogus","collapsed":"no","source":"yes"}',
    '{"gaps":{"rgap14":"x","req0":{"top":"a","bottom":-1},"__proto__":{"top":1,"bottom":1}},"pgaps":{"p0":"yes"},"view":"all"}',
    '{"gaps":{"rgap14":{"top":1e999,"bottom":0}},"view":"relevant","collapsed":false}',
    '{"pgaps":{"p0":{"top":1.5,"bottom":0},"p1":{"top":-3,"bottom":1}},"scrollX":"Infinity","view":"relevant"}',
    '{"scrollX":-40,"pgaps":{"p0":[1]}}',
  ])('corrupt saved state %s falls back to defaults', (value) => {
    const storage = new Map([['dv-state:review-1:item-a/0', value]]);
    const { render, host } = setup(true, false, storage);
    const handle = render(spec());
    expect(shown(host).add).toBeGreaterThan(0);
    expect(query(host, '.dv-tab[aria-selected="true"]')).toHaveLength(1);
    expect(host.querySelector('.dv-gap-label')).not.toBeNull();
    handle.element.classList.remove('dv-is-collapsed');
    click(host, gapButtons(host).flat()[0]);
    tab(host, 'all').click();
    expandAll(host);
    expect(shown(host)).toEqual({ add: 2, del: 2 });
    expect(query(host, '.dv-row')).toHaveLength(82);
  });

  test('a viewed option never hides the body and nothing renders a Viewed box', () => {
    const { render, host, storage } = setup();
    const handle = render({ old: 'a', new: 'b', viewed: true, scope: 'review' });
    expect(handle.element.classList.contains('dv-is-viewed')).toBe(false);
    expect(query(host, '.dv-viewed,input,label')).toHaveLength(0);
    expect(host.textContent).not.toContain('Viewed');
    expect(codes(host)).toEqual(['a', 'b']);
    expect([...storage.keys()]).toEqual([]);
  });

  test('the review page mount spec renders as is', () => {
    const { render, host } = setup();
    const handle = render({ path: 'src/lib.ts', status: 'modified', old: lines(40), new: edited(40, [3, 35]),
      focus: [[3, 3]], oldFocus: [[3, 3]], labels: [{ line: 3, where: 'in the prompt' }], title: 'The caller',
      mountId: 'review-diff-0', scope: 'fixture-review', stateKey: 'item-1:diff:src/lib.ts:3' });
    expect(handle.element.classList.contains('dv-is-collapsed')).toBe(true);
    expect(handle.element.id).toBe('');
    expect(host.querySelector('.dv-title')!.textContent).toBe('The caller');
    expect(tabCounts(host, 'relevant')).toEqual({ add: 1, del: 1 });
    expect(tabCounts(host, 'all')).toEqual({ add: 2, del: 2 });
    expect(query(host, '.dv-review-label').map((n) => n.textContent)).toEqual(['→ in the prompt']);
  });
});

describe('DiffView compatibility and safety', () => {
  test('reordered edited paragraphs emit an intervening insertion once', () => {
    const { render, host } = setup();
    render({ path: 'reorder.md', old: 'Alpha apples apricots avocados are delicious.\n\nBeta bicycles buses boats are vehicles.',
      new: 'Beta bicycles buses boats are useful vehicles.\n\nINSERTED zebra paragraph.\n\nAlpha apples apricots avocados are very delicious.' });
    expect(host.textContent.match(/INSERTED zebra paragraph\./g)).toHaveLength(1);
    expect(query(host, '.dv-p-add').filter((node) => node.textContent.includes('INSERTED'))).toHaveLength(1);
  });

  test('reference definitions remain source evidence and links use revision environments', () => {
    const { render, host } = setup();
    render({ path: 'references.md', old: 'See [policy][p].\n\n[p]: https://old.example',
      new: 'See [policy][p].\n\n[p]: https://new.example', focus: [3, 3], oldFocus: [3, 3] });
    expect(codes(host)).toEqual(['[p]: https://old.example', '[p]: https://new.example']);
    expect(host.querySelector('a')?.getAttribute('href')).toBe('https://new.example');
    const revised = setup();
    revised.render({ path: 'references.md', old: 'Read [policy][p] before acting.\n\n[p]: https://old.example',
      new: 'Read [policy][p] after acting.\n\n[p]: https://new.example' });
    expect(query(revised.host, '.dv-s-old a')[0].getAttribute('href')).toBe('https://old.example');
    expect(query(revised.host, '.dv-s-new a')[0].getAttribute('href')).toBe('https://new.example');
  });

  test('blank-only focus has an explicit diagnostic and source view', () => {
    const { render, host } = setup();
    render({ path: 'blank.md', old: 'One.\n\nTwo.', new: 'One.\n\nTwo.', focus: [2, 2] });
    expect(host.textContent).toContain('Some focused lines have no rendered Markdown block');
    click(host, 'View source');
    expect(numbers(host, 'new')).toEqual([1, 2, 3]);
  });

  test('a focused change to a fence\'s language stays visible', () => {
    const { render, host } = setup();
    render({ path: 'fence.md', old: 'Paragraph old\n\n```js\nconst value=1;\n```\n', new: 'Paragraph new\n\n```ts\nconst value=1;\n```\n', focus: [3, 3], oldFocus: [3, 3] });
    expect(host.textContent).toContain('```ts');
  });

  test('50,000 paragraphs fall back to source before quadratic alignment', () => {
    const { render, host } = setup();
    const old = Array.from({ length: 50_000 }, (_, i) => `Paragraph ${i}.`).join('\n\n');
    render({ path: 'large.md', old, new: old.replace('Paragraph 0.', 'Changed 0.'), focus: [1, 1] });
    expect(query(host, '.dv-pblocks')).toHaveLength(0);
    expect(query(host, '.dv-add .dv-code')[0].textContent).toBe('Changed 0.');
    expect(numbers(host, 'new')).toEqual([1, 2, 3, 4]);
  }, 10_000);

  test.each(['test.ts', 'test.md'])('terminal newline changes remain visible in %s', (path) => {
    for (const [old, next, side] of [['x\n', 'x', 'add'], ['x', 'x\n', 'del']]) {
      const { api, render, host } = setup();
      const file = { path, old, new: next, focus: [1, 1] as Range, oldFocus: [1, 1] as Range };
      render(file);
      expect(api.countChanges(file)).toEqual({ add: 1, del: 1 });
      expect(query(host, `.dv-${side} .dv-no-newline`)).toHaveLength(1);
      expect(query(host, '.dv-no-newline')[0].textContent).toContain('No newline at end of file');
    }
  });

  test.each(["'", '"'])('real highlighter preserves change offsets after %s entities', (quote) => {
    const { render, host } = setup(true, true);
    const old = `const message = ${quote}Hello old friend${quote};`;
    const next = old.replace('old', 'new');
    render({ old, new: next });
    expect(query(host, '.dv-del-mark').map((node) => node.textContent).join('')).toBe('old');
    expect(query(host, '.dv-ins').map((node) => node.textContent).join('')).toBe('new');
    expect(codes(host)).toEqual([old, next]);
    expect(query(host, '.hljs-string .dv-ins')).toHaveLength(1);
  });

  test('exports render and countChanges, returns a handle and counts whole files', () => {
    const { api, render, host, storage } = setup();
    expect(Object.keys(api).sort()).toEqual(['countChanges', 'render']);
    expect(api.render(null, { path: 'x', old: '', new: '' })).toBeNull();
    const handle = render({ old: 'old\nsame', new: 'new\nsame', scope: 'review' });
    expect(Object.keys(handle).sort()).toEqual(['element', 'file', 'rerender']);
    expect(api.countChanges(handle.file)).toEqual({ add: 1, del: 1 });
    expect(host.querySelector('.dv-head > .dv-totals')!.textContent).toBe('+1−1');
    expect([...storage.keys()]).toEqual([]);
  });

  test('zero totals are marked neutral', () => {
    const { render, host } = setup();
    render({ status: 'added', new: 'x' });
    expect(query(host, '.dv-head .dv-zero').map((n) => n.textContent)).toEqual(['−0']);
  });

  test('no toolbar: rendered Markdown gets one header toggle labelled from copy', () => {
    const code = setup();
    code.render({ old: lines(20), new: lines(20), focus: [10, 10] });
    expect(query(code.host, '.dv-tools, .dv-src-toggle')).toHaveLength(0);
    const prose = setup();
    prose.render({ path: 'copy.md', old: 'Old.', new: 'New.' });
    expect(query(prose.host, '.dv-tools')).toHaveLength(0);
    expect(query(prose.host, '.dv-head .dv-src-toggle').map((b) => [b.textContent, b.getAttribute('data-dv-ctl')])).toEqual([['View source', 'source']]);
    click(prose.host, 'View source');
    expect(query(prose.host, '.dv-pblocks')).toHaveLength(0);
    expect(prose.host.querySelector('.dv-src-toggle')!.textContent).toBe('View rendered');
  });

  test('without optional markdown-it or highlighting, source focus still works', () => {
    const { render, host } = setup(false);
    render({ path: 'copy.md', old: 'Old\nSecond', new: 'New\nSecond', focus: [2, 2] });
    expect(codes(host)).toEqual(['Old', 'New', 'Second']);
  });

  test('ordinary anchors and data images retain prototype rendering', () => {
    const { render, host } = setup();
    render({ path: 'images.md', new: '[anchor](https://example.test/path)\n\n![local](data:image/png;base64,aGVsbG8=)', focus: [1, 3] });
    expect(query(host, 'img').map((n) => n.getAttribute('src'))).toEqual(['data:image/png;base64,aGVsbG8=']);
    expect(host.querySelector('a')!.getAttribute('href')).toBe('https://example.test/path');
  });

  test('image rendering is unchanged (inert DOM)', () => {
    const { render, host } = setup();
    // The page generator validates image destinations before mounting. This
    // component must not implement a second policy or silently rewrite evidence.
    render({ path: 'images.md', new: '![remote](https://invalid.test/a.png)', focus: [1, 1] });
    expect(host.querySelector('img')!.getAttribute('src')).toBe('https://invalid.test/a.png');
  });

  test('titles and paths are text, never markup', () => {
    const { render, host } = setup();
    render({ path: 'dir/<b>x</b>.ts', new: 'x', title: '<img src=x onerror=alert(1)>' });
    expect(query(host, 'img,b')).toHaveLength(0);
    expect(host.querySelector('.dv-title')!.textContent).toBe('<img src=x onerror=alert(1)>');
    expect(host.querySelector('.dv-path')!.textContent).toBe('dir/<b>x</b>.ts');
  });

  test('rerender builds off-DOM, retains panel, and restores scroll if replacement shifts it', () => {
    const { render, host, window, scrolls } = setup();
    const handle = render({ old: lines(20), new: lines(20), focus: [10, 10] });
    const body = host.querySelector<HTMLElement>('.dv-body')!;
    Object.defineProperty(body, 'innerHTML', { set() { throw new Error('Must not empty mounted body'); } });
    const replace = body.replaceWith.bind(body);
    body.replaceWith = (...nodes) => { replace(...nodes); window.scrollY = 0; };
    handle.rerender();
    expect(host.querySelector('.dv-file')).toBe(handle.element);
    expect(numbers(host, 'new')).toEqual([7, 8, 9, 10, 11, 12, 13]);
    expect(scrolls).toEqual([[37, 510]]);
  });

  test('scripts stay ASCII with no closing script tag, and the engine with its copy is NUL-free', () => {
    expect(diffEngine).not.toContain(String.fromCharCode(0));
    expect(/[^\x20-\x7e\n\r\t]/.test(diffEngine)).toBe(false);
    expect(/<\/script/i.test(diffEngine)).toBe(false);
    expect(/[^\x20-\x7e\n\r\t]/.test(css)).toBe(false);
  });

  test('measured red/green tokens remain unchanged in every theme', () => {
    const values: Record<string, Array<string>> = {
      'add-bg': ['#dafbe1', '#142b20', '#142b20'], 'add-hi': ['#aceebb', '#19442a', '#19442a'],
      'add-rule': ['#1a7f37', '#3fb950', '#3fb950'], 'add-ink': ['#116329', '#7ee787', '#7ee787'],
      'del-bg': ['#ffdedb', '#4a2d32', '#4a2d32'], 'del-hi': ['#ffb1af', '#803e47', '#803e47'],
      'del-rule': ['#cf222e', '#f85149', '#f85149'], 'del-ink': ['#82071e', '#ffa198', '#ffa198'],
    };
    for (const [name, expected] of Object.entries(values)) {
      expect(Array.from(css.matchAll(new RegExp(`--dv-${name}:\\s*(#[0-9a-f]+)`, 'g')), (m) => m[1])).toEqual(expected);
    }
  });

  test.each(['light', 'dark'] as const)('%s syntax colours stay readable on changed rows; red/green differ in lightness for protan and deutan', (theme) => {
    const report = paletteReport(css, theme);
    // Every token reads at 4.5:1 on both row tints, for normal vision and under protanopia.
    for (const token of ['comment', 'meta', 'keyword', 'string', 'number', 'title']) {
      const { add, del, protanAdd, protanDel } = report.syntaxContrast[token];
      expect([token, Math.min(add, del, protanAdd, protanDel) >= 4.5]).toEqual([token, true]);
    }
    for (const vision of ['protan', 'deutan']) expect(Math.abs(report.visions[vision].deltaL)).toBeGreaterThanOrEqual(3);
  });

  test.each(['light', 'dark'] as const)('%s: the agent role colour is told from YOU by lightness under protanopia', (theme) => {
    // Light tokens are the first :root block; dark ones the explicit [data-theme="dark"] block.
    const block = theme === 'light' ? pageCss.split(':root {')[1] : pageCss.split(':root[data-theme="dark"] {')[1];
    const token = (name: string) => new RegExp(`--role-${name}:\\s*(#[0-9a-f]{6})`).exec(block.split('}')[0])![1];
    const [you, agent] = [lab(simulate(token('user'), VISIONS.protan)), lab(simulate(token('agent'), VISIONS.protan))];
    // The review's targets: ΔL 12 in light (the old #6b3fa0 gave 8.1) and 9 in dark (the old #b48ee6 gave 5.1).
    expect(Math.abs(you[0] - agent[0])).toBeGreaterThanOrEqual(theme === 'light' ? 12 : 8.9);
  });

  test('neutral diff tokens follow the page bridge in every theme, and the page defines it', () => {
    const blocks = [':root {', ':root:not([data-theme="light"]) {', ':root[data-theme="dark"] {'].map((marker) => css.split(marker)[1].split('}')[0]);
    for (const body of blocks) {
      for (const name of ['ground', 'surface', 'surface-2', 'border', 'text', 'muted', 'accent']) {
        expect(body).toMatch(new RegExp(`--dv-${name}:\\s*var\\(--dv-page-`));
      }
    }
    const bridge = Object.fromEntries(Array.from(pageCss.matchAll(/--dv-page-([\w-]+):\s*([^;]+);/g), (m) => [m[1], m[2]]));
    expect(bridge).toEqual({ bg: 'var(--surface)', 'bg-2': 'var(--surface-2)', rule: 'var(--rule)', ink: 'var(--ink)', muted: 'var(--muted)', accent: 'var(--accent)' });
    expect(pageCss).toContain('.dv-row > .dv-code');
  });
});
