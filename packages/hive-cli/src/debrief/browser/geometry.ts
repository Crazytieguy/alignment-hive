/**
 * Shared measurement for the invariant gates. A page is measured still (no
 * transitions or animations), with the mouse parked, after the web fonts its
 * measured elements use have loaded: cap heights, baselines and bold label
 * widths are font metrics, so a run on fallback fonts measures something else.
 */
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { Page } from './harness';

export const STILL = '*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}';
/** Two animation frames and a beat, so layout and paint have settled. */
export const frames = (page: Page) => page.evaluate(() => new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(done, 60)))));

/**
 * Each web-font family, weight and style that visible text matching `selector` computes to, whose face (the declared
 * face CSS matching picks: same style, nearest weight) has not loaded. Families with no declared face are the
 * page's own fallbacks and don't count.
 */
function missingFonts(page: Page, selector: string): Promise<Array<string>> {
  return page.evaluate(async (sel) => {
    await document.fonts.ready;
    const unquote = (family: string) => family.trim().replace(/^["']|["']$/g, '');
    const faceList = () => Array.from(document.fonts).map((face) => {
      const [low, high = low] = face.weight.split(' ').map(Number);
      return { family: unquote(face.family), style: face.style, low, high, loaded: face.status === 'loaded' };
    });
    let faces = faceList();
    const needed = new Set<string>();
    for (const el of Array.from(document.querySelectorAll(sel))) {
      const text = Array.from(el.childNodes).some((n) => n.nodeType === 3 && n.textContent!.trim());
      if (!text || !el.getClientRects().length) continue;
      const style = getComputedStyle(el), family = unquote(style.fontFamily.split(',')[0]);
      if (faces.some((face) => face.family === family)) needed.add(`${family}|${style.fontWeight}|${style.fontStyle}`);
    }
    // Ask for each face the text needs, as layout would: one can still be loading when fonts.ready settles.
    await Promise.all([...needed].map((key) => { const [family, weight, style] = key.split('|'); return document.fonts.load(`${style} ${weight} 16px "${family}"`).catch(() => []); }));
    faces = faceList();
    return [...needed].filter((key) => {
      const [family, weight, style] = key.split('|'), w = Number(weight);
      const declared = faces.filter((face) => face.family === family && face.style === style);
      if (!declared.length) return false;
      const distance = (face: { low: number; high: number }) => (w < face.low ? face.low - w : w > face.high ? w - face.high : 0);
      const nearest = Math.min(...declared.map(distance));
      return !declared.some((face) => distance(face) === nearest && face.loaded);
    });
  }, selector);
}

/** A review page loaded, with its `n` diff panels mounted. */
export async function waitForDiffs(page: Page, n: number): Promise<void> {
  await page.waitForFunction((count: number) => document.readyState === 'complete' && document.querySelectorAll('[data-review-diff] > .dv-file').length === count, n);
}

/** Still, mouse parked, fonts loaded (or the run fails: a fallback-font measurement never passes), two frames. */
export async function settle(page: Page, fontSelector: string, fonts: 'local' | 'network' | 'none'): Promise<void> {
  await page.addStyleTag({ content: STILL });
  await page.mouse.move(0, 0);
  if (fonts !== 'none') {
    const missing = await missingFonts(page, fontSelector);
    if (missing.length) throw new Error(`fonts not loaded: ${missing.join(', ')}`);
  }
  await frames(page);
}

/** With REVIEW_MEASURE_OUT set, a gate's full table is kept as JSON for the report. */
export async function keepMeasurements(name: string, table: unknown): Promise<void> {
  const dir = process.env.REVIEW_MEASURE_OUT;
  if (!dir) return;
  await mkdir(dir, { recursive: true });
  await writeFile(join(dir, `${name}.json`), JSON.stringify(table, null, 2) + '\n');
}

/* ---------- G-C: diff tabs keep one geometry folded and open (c003) ---------- */

export interface TabSnapshot { labels: Array<[number, number, number]>; chip: [number, number] | null; title: [number, number] | null; header: number }
export interface TabPanel { id: string; folded: TabSnapshot; open: TabSnapshot; all: TabSnapshot | null; opened: boolean }

/** Runs in the page: every tabbed diff panel measured folded, open, and open on All changes, relative to its header. */
export async function measureTabs(): Promise<Array<TabPanel>> {
  const settled = () => new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => done())));
  const textBox = (el: Element | null) => {
    const text = el && Array.from(el.childNodes).find((n) => n.nodeType === 3 && n.textContent!.trim());
    if (!text) return null;
    const range = document.createRange();
    range.selectNodeContents(text);
    return range.getBoundingClientRect();
  };
  const snap = (panel: Element): TabSnapshot => {
    const head = panel.querySelector('.dv-head')!.getBoundingClientRect();
    const chip = panel.querySelector('.dv-head .dv-chip')?.getBoundingClientRect();
    const title = textBox(panel.querySelector('.dv-head .dv-fname') ?? panel.querySelector('.dv-head .dv-path'));
    return {
      labels: Array.from(panel.querySelectorAll('.dv-tab-name')).map((name) => { const b = textBox(name)!; return [b.left - head.left, b.top - head.top, b.bottom - head.top]; }),
      chip: chip ? [chip.left - head.left, chip.top - head.top] : null,
      title: title ? [title.top - head.top, title.bottom - head.top] : null,
      header: head.height,
    };
  };
  const out: Array<TabPanel> = [];
  for (const panel of Array.from(document.querySelectorAll('.dv-file')).filter((f) => f.querySelector('.dv-tab'))) {
    const head = panel.querySelector<HTMLElement>('.dv-head')!;
    panel.scrollIntoView({ block: 'center' });
    if (!panel.classList.contains('dv-is-collapsed')) head.click();
    await settled();
    const folded = snap(panel);
    head.click();
    await settled();
    const open = snap(panel), opened = !panel.classList.contains('dv-is-collapsed');
    const allTab = panel.querySelector<HTMLElement>('.dv-tab[data-dv-view="all"][aria-selected="false"]');
    let all: TabSnapshot | null = null;
    if (allTab) { allTab.click(); await settled(); all = snap(panel); }
    out.push({ id: panel.id || panel.closest('[id]')?.id || '', folded, open, all, opened });
  }
  return out;
}

export interface TabDeltas { id: string; opened: boolean; labelMove: number; chipMove: number; headerChange: number; viewMove: number; centreOffset: number | null }
/** The largest movement of any label, the chip and the header height from folded to open, and of a label from Relevant to All. */
export function tabDeltas(panel: TabPanel): TabDeltas {
  const moves = (a: TabSnapshot, b: TabSnapshot) => a.labels.flatMap((l, i) => [0, 1, 2].map((k) => Math.abs(l[k] - b.labels[i][k])));
  const label = Math.max(0, ...moves(panel.folded, panel.open));
  const chip = panel.folded.chip && panel.open.chip ? Math.max(Math.abs(panel.folded.chip[0] - panel.open.chip[0]), Math.abs(panel.folded.chip[1] - panel.open.chip[1])) : 0;
  const view = panel.all ? Math.max(0, ...moves(panel.open, panel.all)) : 0;
  const first = panel.open.labels.at(0), title = panel.open.title;
  return {
    id: panel.id, opened: panel.opened, labelMove: label, chipMove: chip, headerChange: Math.abs(panel.folded.header - panel.open.header), viewMove: view,
    centreOffset: first && title ? (first[1] + first[2]) / 2 - (title[0] + title[1]) / 2 : null,
  };
}

/* ---------- G-A: item chevrons centred on the heading's cap band (c001) ---------- */

export interface ChevronRow { id: string; open: boolean; offset: number | null; inkTop: number | null; inkBottom: number | null; capCentre: number }

/** Runs in the page: the first and last rows of ink in a PNG, within a column and row window given in CSS px. */
async function inkRows(args: { b64: string; scale: number; x0: number; x1: number; y0: number; y1: number }): Promise<{ first: number; last: number } | null> {
  const bitmap = await createImageBitmap(await (await fetch(`data:image/png;base64,${args.b64}`)).blob());
  const canvas = new OffscreenCanvas(bitmap.width, bitmap.height), context = canvas.getContext('2d')!;
  context.drawImage(bitmap, 0, 0);
  const d = context.getImageData(0, 0, bitmap.width, bitmap.height).data;
  // Ink differs from the clip's corner pixel by more than 40 on average over the channels.
  const ink = (i: number) => (Math.abs(d[i] - d[0]) + Math.abs(d[i + 1] - d[1]) + Math.abs(d[i + 2] - d[2])) / 3 > 40;
  let first = -1, last = -1;
  const [xa, xb] = [Math.max(0, Math.floor(args.x0 * args.scale)), Math.min(bitmap.width, Math.ceil(args.x1 * args.scale))];
  for (let y = Math.max(0, Math.floor(args.y0 * args.scale)); y < Math.min(bitmap.height, Math.ceil(args.y1 * args.scale)); y++) {
    for (let x = xa; x < xb; x++) if (ink((y * bitmap.width + x) * 4)) { if (first < 0) first = y; last = y; break; }
  }
  return first < 0 ? null : { first, last };
}

/**
 * Every item (and tray) chevron, closed and open: the ink centre of its mark against the centre of its
 * heading's cap band (baseline from a zero-size probe, cap height from canvas in the heading's font).
 * `scale` is the context's deviceScaleFactor.
 */
export async function measureChevrons(page: Page, scale: number, selector = 'details.item:not(.item-empty), details.rest'): Promise<Array<ChevronRow>> {
  const ids = await page.evaluate((sel) => Array.from(document.querySelectorAll(sel)).map((d) => d.id), selector);
  const rows: Array<ChevronRow> = [];
  for (const id of ids) {
    for (const open of [false, true]) {
      await page.evaluate((args) => {
        const d = document.getElementById(args.id) as HTMLDetailsElement;
        const tray = d.parentElement?.closest<HTMLDetailsElement>('details.rest');
        if (tray) tray.open = true;
        d.open = args.open;
        // The summary, not the item: an open item can be taller than the screen.
        d.querySelector(':scope > summary')!.scrollIntoView({ block: 'center' });
      }, { id, open });
      await frames(page);
      const info = await page.evaluate((target) => {
        const summary = document.getElementById(target)!.querySelector(':scope > summary')!;
        const heading = summary.querySelector('.item-h, .rest-label')!;
        const probe = document.createElement('span');
        probe.style.cssText = 'display:inline-block;width:0;height:0';
        heading.insertBefore(probe, heading.firstChild);
        const base = probe.getBoundingClientRect().bottom;
        probe.remove();
        const style = getComputedStyle(heading), context = document.createElement('canvas').getContext('2d')!;
        context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
        const cap = context.measureText('H').actualBoundingBoxAscent;
        const m = summary.querySelector('.item-mark')!.getBoundingClientRect();
        return { capCentre: base - cap / 2, left: m.left, top: m.top, width: m.width, height: m.height };
      }, id);
      if (info.width <= 0 || info.height <= 0) throw new Error(`G-A: no chevron to measure at ${id} (${open ? 'open' : 'closed'})`);
      const x0 = Math.max(0, Math.floor(info.left) - 4), y0 = Math.max(0, Math.floor(info.top) - 8);
      const png = await page.screenshot({ clip: { x: x0, y: y0, width: 24, height: 32 } });
      // Ink counts only inside the mark's own box, ±1px, so a neighbouring rule or shadow doesn't.
      const ink = await page.evaluate(inkRows, { b64: Buffer.from(png).toString('base64'), scale, x0: info.left - x0 - 1, x1: info.left - x0 + info.width + 1, y0: info.top - y0 - 1, y1: info.top - y0 + info.height + 1 });
      const top = ink ? y0 + ink.first / scale : null, bottom = ink ? y0 + (ink.last + 1) / scale : null;
      rows.push({ id, open, capCentre: info.capCentre, inkTop: top, inkBottom: bottom, offset: top === null || bottom === null ? null : (top + bottom) / 2 - info.capCentre });
    }
  }
  return rows;
}

/* ---------- G-B: opening or closing anything moves nothing already on screen (c001, c003) ---------- */

/**
 * A control kind: its selector, how a viewer toggles it, and whether a second press undoes it. `exempt`: allowed to
 * move what is on screen (by design), but toggling it back must restore everything.
 */
export interface ControlKind { name: string; selector: string; press: 'click' | 'enter'; inverse: 'same' | 'other-tab' | null; exempt?: boolean }
/** Items and trays measured on the default page; the rest with every item open, then with every fold and panel open. */
export const CONTROL_PHASES: Array<{ phase: 'closed' | 'items-open' | 'all-open'; kinds: Array<ControlKind> }> = [
  { phase: 'closed', kinds: [
    { name: 'item', selector: 'details.item:not(.item-empty):not(.item-quiet) > summary', press: 'enter', inverse: 'same' },
    { name: 'tray', selector: 'details.rest > summary', press: 'click', inverse: 'same' },
    { name: 'ask', selector: '.asks li.ask-clipped', press: 'enter', inverse: 'same' },
    // "All N" inserts the user's other messages among the key asks: exempt from staying put, not from the round trip.
    { name: 'asks-all', selector: '.asks-all', press: 'click', inverse: 'same', exempt: true },
  ] },
  { phase: 'items-open', kinds: [
    { name: 'quiet-item', selector: '.rest-rows > details.item:not(.item-empty) > summary', press: 'enter', inverse: 'same' },
    { name: 'entry', selector: 'details.tr-entry > summary', press: 'click', inverse: 'same' },
    { name: 'file', selector: 'details.fv > summary', press: 'click', inverse: 'same' },
    { name: 'commit', selector: 'details.git-commit > summary', press: 'click', inverse: 'same' },
    { name: 'diff', selector: '.dv-file .dv-collapse', press: 'click', inverse: 'same' },
  ] },
  { phase: 'all-open', kinds: [
    { name: 'tab', selector: '.dv-tab[aria-selected="false"]', press: 'click', inverse: 'other-tab' },
    { name: 'source', selector: '.dv-src-toggle', press: 'click', inverse: 'same' },
    { name: 'file-source', selector: '.fv-toggle', press: 'click', inverse: 'same' },
    { name: 'clip', selector: '.tr-wrap .ev-more', press: 'click', inverse: 'same' },
    { name: 'zoom', selector: '.shot-body img.zoomable', press: 'click', inverse: 'same' },
    { name: 'step', selector: '.dv-fold-line .dv-gap-btn', press: 'click', inverse: null },
  ] },
];

/** Opens every item and tray (and, for `all-open`, every fold and diff panel), then clears the saved state they wrote. */
export async function preparePhase(page: Page, phase: 'closed' | 'items-open' | 'all-open'): Promise<void> {
  await page.evaluate((which) => {
    // Room below the page, so closing something near its end is measured as the page draws it, not as the
    // browser's scroll clamp at the bottom of the document (which moves everything and belongs to no component).
    if (!document.querySelector('[data-gate-room]')) document.body.insertAdjacentHTML('beforeend', '<div data-gate-room style="height:150vh"></div>');
    const open = (selector: string) => document.querySelectorAll<HTMLDetailsElement>(selector).forEach((d) => { d.open = true; });
    if (which === 'closed') return;
    open('details.item:not(.item-empty)'); open('details.rest');
    if (which === 'all-open') {
      open('.item-body details');
      document.querySelectorAll<HTMLElement>('.dv-file.dv-is-collapsed .dv-collapse').forEach((b) => b.click());
    }
  }, phase);
  await frames(page);
}

type Box = [number, number, number];
interface Recorded { anchors: Record<string, { box: Box; freeHeight: boolean }>; after: Array<Box>; scrollY: number; rowPath?: Array<number> }
/**
 * Runs in the page: position a control, then record its anchors (document space) and what follows it on screen.
 * A fold step rebuilds its panel's body, so after one the row above the fold is found again by its path of child
 * indices from the body (`rowPath`): everything before the fold line renders as it did.
 */
function recordControl(args: { selector: string; index: number; kind: string; position: boolean; rowPath?: Array<number> }): Recorded {
  /** A fold step's anchors: the row above the fold line and the panel's header. */
  const stepAnchors = (panel: Element, above: Element | null | undefined): Recorded['anchors'] => {
    const anchors: Recorded['anchors'] = {};
    const box = (el: Element) => { const r = el.getBoundingClientRect(); return [r.top + scrollY, r.left + scrollX, r.height] as Box; };
    if (above) anchors['row-above'] = { box: box(above), freeHeight: false };
    panel.querySelectorAll('.dv-head .dv-tab-name, .dv-head .dv-chip, .dv-head .dv-collapse, .dv-head .dv-path').forEach((el, i) => { anchors[`head-${i}`] = { box: box(el), freeHeight: false }; });
    return anchors;
  };
  if (args.rowPath) {
    const panel = document.querySelector('[data-gate="panel"]')!;
    let row = panel.querySelector('.dv-body') as Element | undefined;
    for (const i of args.rowPath) row = row?.children[i];
    return { anchors: stepAnchors(panel, row), after: [], scrollY };
  }
  const control = document.querySelectorAll<HTMLElement>(args.selector)[args.index];
  const visible = (el: Element): boolean => el.getClientRects().length > 0 && el.getBoundingClientRect().height > 0;
  const container: Element = (() => {
    switch (args.kind) {
      case 'item': case 'quiet-item': case 'tray': case 'entry': case 'file': case 'commit': return control.parentElement!;
      case 'ask': case 'asks-all': return control;
      case 'clip': return control.closest('.tr-wrap')!;
      case 'zoom': return control.closest('details.fv')!;
      case 'step': return control.closest('.dv-fold-line')!;
      default: return control.closest('.dv-file') ?? control.closest('details.fv')!;
    }
  })();
  if (args.position) {
    if (args.kind === 'clip') {
      // Show less must not trigger its recovery scroll: the clipped box sits 40px below the top.
      const box = container.getBoundingClientRect();
      scrollTo(scrollX, scrollY + box.top - 40);
    } else if (control.closest('.dv-head')) {
      // A diff header is sticky: centring a stuck one only pins it. Its panel's top goes to the middle, so the header
      // sits in place, as a reader who has not scrolled into the panel sees it. (A tab switch from deep inside a panel
      // brings the panel's top back by design; that recovery scroll is not a shift.)
      const top = control.closest('.dv-file')!.getBoundingClientRect().top;
      scrollTo(scrollX, scrollY + top - innerHeight / 2);
    } else {
      const rect = control.getBoundingClientRect();
      scrollTo(scrollX, scrollY + rect.top + rect.height / 2 - innerHeight / 2);
    }
  }
  const doc = (r: DOMRect): Box => [r.top + scrollY, r.left + scrollX, r.height];
  const firstLine = (el: Element): DOMRect | null => {
    const range = document.createRange();
    range.selectNodeContents(el);
    return Array.from(range.getClientRects()).find((r) => r.width > 0 && r.height > 0) ?? null;
  };
  const anchors: Recorded['anchors'] = {};
  const add = (name: string, el: Element | null, options: { text?: boolean; freeHeight?: boolean } = {}) => {
    if (!el || !visible(el)) return;
    const rect = options.text ? firstLine(el) : el.getBoundingClientRect();
    if (rect) anchors[name] = { box: doc(rect), freeHeight: !!options.freeHeight };
  };
  const within = (root: Element, selector: string) => Array.from(root.querySelectorAll(selector));
  const header = (root: Element) => {
    const head = root.querySelector('.dv-head, .fv-head')!;
    add('title', head.querySelector('.dv-title'), { text: true });
    add('path', head.querySelector('.dv-fname') ?? head.querySelector('.dv-path, .fv-path'), { text: true });
    within(head, '.dv-tab-name').forEach((n, i) => add(`tab-${i}`, n, { text: true }));
    within(head, '.dv-chip, .fv-chip').forEach((n, i) => add(`chip-${i}`, n));
    add('collapse', head.querySelector('.dv-collapse'));
    add('toggle', head.querySelector('.dv-src-toggle, .fv-toggle'));
    add('chevron', head.querySelector('.git-chev'));
  };
  switch (args.kind) {
    case 'item': case 'quiet-item':
      add('heading', control.querySelector('.item-h'), { text: true }); add('mark', control.querySelector('.item-mark'));
      within(control, '.item-meta > *').forEach((n, i) => add(`meta-${i}`, n)); add('seen', control.querySelector('.seen'));
      add('lede', control.querySelector(':scope > .item-lede'), { text: true });
      break;
    case 'tray':
      add('mark', control.querySelector('.item-mark')); add('label', control.querySelector('.rest-label'), { text: true });
      add('count', control.querySelector('.rest-count')); add('seen', control.querySelector('.rest-seen'));
      break;
    case 'entry':
      add('chevron', control.querySelector('.tr-chevron')); add('role', control.querySelector('.tr-role'), { text: true });
      add('summary', control.querySelector('.tr-sum'), { text: true }); add('time', control.querySelector('time'));
      break;
    case 'file':
      add('chevron', control.querySelector('.git-chev')); add('path', control.querySelector('.fv-path'), { text: true });
      add('kind', control.querySelector('.shot-kind'), { text: true }); add('time', control.querySelector('time'));
      within(control, '.fv-chip').forEach((n, i) => add(`chip-${i}`, n));
      break;
    case 'commit':
      add('chevron', control.querySelector('.git-chev')); add('subject', control.querySelector('.git-subject'), { text: true });
      add('hash', control.querySelector('.git-hash')); add('counts', control.querySelector('.git-counts'));
      break;
    case 'asks-all':
      add('toggle', control);
      break;
    case 'ask': {
      // The time is a stretched grid item: its box grows with an opened row, its text stays.
      add('time', control.querySelector('time'), { text: true }); add('quote', control.querySelector('q'), { text: true });
      let above = control.previousElementSibling, i = 0;
      while (above) { add(`above-${i++}`, above); above = above.previousElementSibling; }
      break;
    }
    case 'clip': {
      const box = container.querySelector('.clip, pre, .tr-clipbox') ?? container.firstElementChild;
      add('box', box, { freeHeight: true }); if (box) add('first-line', box, { text: true });
      break;
    }
    case 'zoom':
      add('header', container.querySelector('.fv-head'));
      { const r = control.getBoundingClientRect(); anchors['image-top'] = { box: [r.top + scrollY, r.left + scrollX, 0], freeHeight: true }; }
      break;
    case 'step': {
      const panel = control.closest('.dv-file')!, body = panel.querySelector('.dv-body')!, above = container.previousElementSibling;
      const path: Array<number> = [];
      for (let node: Element | null = above; node && node !== body; node = node.parentElement) path.unshift(Array.prototype.indexOf.call(node.parentElement!.children, node));
      return { anchors: stepAnchors(panel, above), after: [], scrollY, rowPath: above ? path : [] };
    }
    default:
      header(container);
  }
  // Every control also anchors the nearest visible block before it in document order.
  for (let node: Element | null = container; node && node !== document.body; node = node.parentElement) {
    let previous = node.previousElementSibling;
    while (previous && !visible(previous)) previous = previous.previousElementSibling;
    if (previous) { add('before', previous); break; }
  }
  // For the round trip: what follows the control on screen, outside what it opens.
  const after = Array.from(document.querySelectorAll('.page *')).filter((el) => {
    if (container.contains(el) || !(control.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING)) return false;
    const r = el.getBoundingClientRect();
    return r.height > 0 && r.bottom > 0 && r.top < innerHeight;
  }).slice(0, 80).map((el) => doc(el.getBoundingClientRect()));
  return { anchors, after, scrollY };
}

export interface ShiftResult { kind: string; index: number; label: string; forward: number; back: number; roundTrip: number; scroll: number; moved: Array<string> }
const worst = (a: Recorded, b: Recorded, moved: Array<string>, direction: string) => {
  let max = 0;
  for (const [name, anchor] of Object.entries(a.anchors)) {
    const other = b.anchors[name] as { box: Box } | undefined;
    if (!other) { moved.push(`${direction} ${name} vanished`); max = Infinity; continue; }
    const d = Math.max(Math.abs(anchor.box[0] - other.box[0]), Math.abs(anchor.box[1] - other.box[1]), anchor.freeHeight ? 0 : Math.abs(anchor.box[2] - other.box[2]));
    if (d > 0.5) moved.push(`${direction} ${name} ${d.toFixed(2)}`);
    max = Math.max(max, d);
  }
  return max;
};

/**
 * Toggles each control of a kind (every one up to `limit`, else a deterministic sample of `limit`), and measures
 * its anchors before and after, and, for toggles, everything recorded once it is toggled back.
 */
export async function measureShifts(page: Page, kind: ControlKind, limit = 40): Promise<Array<ShiftResult>> {
  // Only controls a reader can reach: none inside a closed fold.
  const visibleCount = (sel: string) => page.evaluate((selector) => Array.from(document.querySelectorAll(selector)).filter((el) => el.getClientRects().length).length, sel);
  const count = await visibleCount(kind.selector);
  const step = count > limit ? count / limit : 1;
  const results: Array<ShiftResult> = [];
  for (let k = 0; k < Math.min(count, limit); k++) {
    // A fold step has no inverse and changes how many step buttons there are.
    const now = await visibleCount(kind.selector);
    if (!now) break;
    const index = Math.min(Math.floor(k * step), now - 1);
    try { results.push(await measureShift(page, kind, index)); }
    catch (error) { results.push({ kind: kind.name, index, label: '', forward: Infinity, back: 0, roundTrip: 0, scroll: 0, moved: [`checker error: ${String(error).slice(0, 160)}`] }); }
  }
  return results;
}

async function measureShift(page: Page, kind: ControlKind, index: number): Promise<ShiftResult> {
  const label = await page.evaluate((args) => {
    const el = Array.from(document.querySelectorAll(args.sel)).filter((e) => e.getClientRects().length)[args.index];
    return `${el.closest('[id]')?.id ?? ''} ${el.textContent.trim().slice(0, 40)}`;
  }, { sel: kind.selector, index });
  const press = async (target: { selector: string; index: number }) => {
    if (kind.press === 'enter') {
      const focused = await page.evaluate((t) => { const el = document.querySelectorAll<HTMLElement>(t.selector)[t.index]; el.focus({ preventScroll: true }); return document.activeElement === el; }, target);
      if (!focused) throw new Error('the control does not take focus');
      await page.keyboard.press('Enter');
    } else await page.evaluate((t) => { document.querySelectorAll<HTMLElement>(t.selector)[t.index].click(); }, target);
    await frames(page);
  };
  // A tab stops matching its selector once chosen, so it is pressed through a stable marker.
  await page.evaluate((args) => {
    document.querySelectorAll('[data-gate]').forEach((el) => el.removeAttribute('data-gate'));
    const el = Array.from(document.querySelectorAll(args.sel)).filter((e) => e.getClientRects().length)[args.index];
    el.setAttribute('data-gate', 'control');
    el.closest('.dv-file')?.setAttribute('data-gate', 'panel');
  }, { sel: kind.selector, index });
  const target = { selector: '[data-gate="control"]', index: 0 };
  await page.evaluate(recordControl, { ...target, kind: kind.name, position: true });
  await frames(page);
  const first = await page.evaluate(recordControl, { ...target, kind: kind.name, position: false });
  await press(target);
  const toggled = await page.evaluate(recordControl, { ...target, kind: kind.name, position: false, rowPath: first.rowPath });
  const moved: Array<string> = [];
  const exempt: Array<string> = [];
  const forward = worst(first, toggled, kind.exempt ? exempt : moved, 'forward');
  let back = 0, roundTrip = 0, scroll = Math.abs(toggled.scrollY - first.scrollY);
  if (kind.inverse) {
    if (kind.inverse === 'other-tab') {
      await page.evaluate(() => {
        const tab = document.querySelector('[data-gate="control"]')!.closest('.dv-tabs')!.querySelector<HTMLElement>('.dv-tab[aria-selected="false"]')!;
        tab.setAttribute('data-gate', 'inverse');
      });
      await press({ selector: '[data-gate="inverse"]', index: 0 });
    } else await press(target);
    const again = await page.evaluate(recordControl, { ...target, kind: kind.name, position: false });
    back = worst(toggled, again, kind.exempt ? exempt : moved, 'back');
    scroll = Math.max(scroll, Math.abs(again.scrollY - first.scrollY));
    first.after.forEach((box, i) => {
      const now = again.after.at(i);
      const d = now ? Math.max(...box.map((v, j) => Math.abs(v - now[j]))) : Infinity;
      if (d > 0.5) moved.push(`round trip: box ${i} after the control ${d.toFixed(2)}`);
      roundTrip = Math.max(roundTrip, d);
    });
  }
  if (scroll > 0.5) moved.push(`scrollY ${scroll.toFixed(1)}`);
  return { kind: kind.name, index, label, forward, back, roundTrip, scroll, moved };
}
