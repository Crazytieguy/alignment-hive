import { randomUUID } from 'node:crypto';
import { mkdir, readFile, realpath, rename, unlink, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import diffScript from '../../assets/review-diff.js' with { type: 'text' };
import pageScript from '../../assets/review-page.js' with { type: 'text' };
import diffCss from '../../assets/review-diff.css' with { type: 'text' };
import pageCss from '../../assets/review-page.css' with { type: 'text' };
import codeCss from '../../assets/review-code.css' with { type: 'text' };
import { readStateFile } from '../lib/config';
import { reviewDiffMessages as diffMsg, review as msg, reviewPageMessages as pageMsg, reviewRoundMessages as roundMsg } from '../lib/messages';
import { buildManifest, parseManifest, readManifest } from './manifest';
import { compareRounds, seenOverlayScript } from './rounds';
import { seenStoreScript } from './seen';
import { EvidenceResolver, isInside } from './evidence';
import { FileAnchors, dataKey, evidenceHtml, timeHtml, utcTimes } from './evidence-html';
import { resolveReviewContext } from './git';
import { SCRIPT_URLS, escapeCss, escapeHtml, escapeJson, escapeScript, renderMarkdownHtml, renderMarkdownInline, validateMarkdownImages } from './html';
import { FIXED_SECTIONS, markdown, parseReview, refuseSecret, reviewError } from './parse';
import { auditPage, findSecretsInValue } from './redact';
import { reviewTimes } from './times';
import type { ResolvedItem, ReviewHeader } from './evidence';
import type { MountedDiff, PageState } from './evidence-html';
import type { ChangedFile, ReviewContext } from './git';
import type { ParsedReview } from './parse';
import type { SecretFound, SecretHits } from './redact';
import type { ReviewManifest } from './manifest';

/** `projects`: where transcripts live (default ~/.claude/projects). */
export interface PrepareReviewOptions { cwd?: string; projects?: string }
/** `source` is the debrief file's text; `redactions` counts what redaction took out of the evidence. */
export interface PreparedReview { parsed: ParsedReview; context: ReviewContext; header: ReviewHeader; items: Array<ResolvedItem>; source?: string; redactions?: SecretHits }

export async function prepareReview(inputPath: string, options: PrepareReviewOptions = {}): Promise<PreparedReview> {
  const absolute = resolve(options.cwd ?? process.cwd(), inputPath);
  const source = await readFile(absolute, 'utf8');
  const parsed = parseReview(source);
  let context: ReviewContext;
  try { context = await resolveReviewContext({ ...options, session: parsed.page.session, base: parsed.page.base, head: parsed.page.head }); }
  catch (error) { throw reviewError('page', 1, error instanceof Error ? error.message : String(error)); }
  const resolver = new EvidenceResolver(context, dirname(absolute), options.projects);
  const header = await resolver.header(parsed);
  return { parsed, context, header, items: await resolver.items(parsed.items), source, redactions: resolver.redactions };
}

/** A secret that reached the output is a path the evidence and authored-text checks missed. */
const refuseLeak = (prepared: PreparedReview, found: Array<SecretFound>) => refuseSecret(found.at(0), 'page', prepared.source?.split('\n') ?? []);

export interface AssemblyOptions {
  reviewId?: string;
  manifest?: unknown;
  states?: Record<string, 'new' | 'updated' | 'unchanged'>;
  footerHtml?: string;
  overlayScript?: string;
}
export interface AssembledReview { html: string; warnings: Array<string> }
export const MAX_PAGE_BYTES = 14_000_000;
export const FONTS = 'https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:ital,wght@0,400;0,500;0,600;1,400&family=IBM+Plex+Sans:ital,wght@0,400;0,500;0,600;1,400&family=Newsreader:ital,opsz,wght@0,6..72,400;0,6..72,500;1,6..72,400&display=swap';
/** The diff engine given its copy once: defines `window.DiffView`. */
export const diffEngine = `${diffScript}\nreviewDiff(window, ${escapeJson(diffMsg)});`;
/** What every page ships besides its content: the stylesheet (diff, code, then page rules, in cascade order) and scripts. */
const pageStyle = escapeCss([diffCss, codeCss, pageCss].join('\n'));
const js = escapeScript(diffEngine);
const seenStore = escapeScript(seenStoreScript());
const behaviour = escapeScript(`${pageScript}\nreviewPage(${escapeJson({ viewSource: pageMsg.viewSource, viewRendered: pageMsg.viewRendered, restSeen: pageMsg.restSeen, restNav: pageMsg.restNav, stale: roundMsg.stale, staleTitle: roundMsg.staleTitle })}, ${reviewTimes.toString()}, debriefSeen);`);
const mount = `var reviewDiffs=JSON.parse(document.getElementById('review-diffs').textContent);reviewDiffs.forEach(function(file){DiffView.render(document.getElementById(file.mountId),file);});document.querySelectorAll('.item-body pre code[class], .fv-src pre code[class], .tr-cmd code[class]').forEach(function(code){if(window.hljs)hljs.highlightElement(code);});`;

const fallbackFence = markdown.renderer.rules.fence;
markdown.renderer.rules.fence = (tokens, index, options, environment, self) => {
  const token = tokens[index];
  const { item, page } = environment as { item?: ResolvedItem; page: PageState };
  if (token.info.trim() !== 'ref' || !item) return fallbackFence(tokens, index, options, environment, self);
  const evidence = item.evidence.find((ref) => ref.bodyLine === token.map?.[0]);
  if (!evidence) throw reviewError(item.metadata.id, item.bodyLine + (token.map?.[0] ?? 0), msg.evidenceMissing('ref'));
  return evidenceHtml(evidence, item.metadata.id, page);
};
const inline = (source: string, itemId: string, line: number) => renderMarkdownInline(source, { itemId, line });
function block(source: string, itemId: string, line: number, warnings: Array<string>, env: Record<string, unknown> = {}): string {
  validateMarkdownImages(source, { itemId, line });
  const rendered = renderMarkdownHtml(markdown, source, { itemId, line }, env);
  warnings.push(...rendered.warnings);
  return rendered.html;
}

/**
 * Alternatives go before the first evidence; an item with nothing behind its summary is a plain row. A
 * lower-priority row keeps its heading, meta slot (round chips) and seen box in the summary; its lede opens the body.
 */
function itemHtml(item: ResolvedItem, page: PageState, state: string | undefined, warnings: Array<string>): string {
  const { id, lede, nav, alternatives, 'judgement-call': judgement, 'lower-priority': quiet } = item.metadata;
  const html = block(item.body, id, item.bodyLine, warnings, { item, page });
  const lines = alternatives.map((text) => `<p class="line line-alternative"><b>${pageMsg.alternative}</b>${inline(text, id, item.line)}</p>`).join('');
  const split = html.includes('<figure class="ev ') ? html.indexOf('<figure class="ev ') : html.length;
  const ledeHtml = `<p class="item-lede${quiet ? ' item-lede-in' : ''}">${inline(lede, id, item.line)}</p>`;
  const body = (quiet ? ledeHtml : '') + html.slice(0, split) + lines + html.slice(split);
  const tags = (judgement && alternatives.length ? `<span class="tag tag-attn">${pageMsg.judgementCall}</span>` : '') + (state && state !== 'unchanged' ? `<span class="tag tag-round">${escapeHtml(state)}</span>` : '');
  return `<details class="item${quiet ? ' item-quiet' : ''}${body.trim() ? '' : ' item-empty'}" id="item-${id}" data-item="${id}"${judgement ? ' data-attn' : ''}${quiet ? ' data-quiet' : ''}${state ? ` data-round-state="${state}"` : ''}>
<summary><span class="item-mark" aria-hidden="true"></span><h3 class="item-h" data-nav="${escapeHtml(nav ?? '')}">${inline(item.heading, id, item.line)}</h3><span class="item-meta">${tags}</span>${quiet ? '' : ledeHtml}<label class="seen"><input type="checkbox" id="seen-${id}"> ${pageMsg.seen}</label></summary>
<div class="item-body">${body}</div>
</details>`;
}

/** A section's lower-priority items, folded into one tray after its main items: a count, seen dots, and their names. */
function trayHtml(section: string, items: Array<ResolvedItem>, rendered: Map<ResolvedItem, string>): string {
  const n = items.length;
  const names = items.map((item) => item.metadata.nav ? escapeHtml(item.metadata.nav) : inline(item.heading, item.metadata.id, item.line)).join('<span class="rest-sep" aria-hidden="true"> &#183; </span>');
  const seen = pageMsg.restSeen.replace('{k}', '0').replace('{n}', String(n));
  return `<details class="rest" id="rest-${section}" data-count="${n}"><summary class="rest-sum"><span class="item-mark" aria-hidden="true"></span><span class="rest-label">${escapeHtml(pageMsg.lowerPriority)}<span class="rest-count">${n}</span></span><span class="rest-seen"><span class="rest-dots" aria-hidden="true">${'<i></i>'.repeat(n)}</span><span class="rest-seen-t">${escapeHtml(seen)}</span></span><span class="rest-preview">${names}</span></summary><div class="rest-rows">${items.map((item) => rendered.get(item)).join('\n')}</div></details>`;
}

/** Heading, asks, stats, story and the left-for-you card. Times carry ISO; their text is the UTC fallback. */
function headerHtml(prepared: PreparedReview, warnings: Array<string>): string {
  const { page, lines } = prepared.parsed, { asks, own, span } = prepared.header;
  warnings.push(...prepared.header.warnings);
  // One line each; the page opens a clipped one, and shows the day only where it changes among the rows shown (UTC
  // here, the viewer's zone there). The user's other messages wait behind "All", hidden, each with its day.
  let lastShown = '';
  const askHtml = asks.map((ask) => {
    let time = '';
    if (ask.timestamp) {
      const at = utcTimes.dayTime(ask.timestamp);
      time = `<time datetime="${escapeHtml(new Date(ask.timestamp).toISOString())}"><span class="ask-day"${!ask.rest && at.key === lastShown ? ' hidden' : ''}>${escapeHtml(at.day)} </span>${escapeHtml(at.time)}</time>`;
      if (!ask.rest) lastShown = at.key;
    }
    const quote = ask.elision ? `${escapeHtml(ask.text.slice(0, ask.elision.at))}<span class="ask-elide">[${escapeHtml(ask.elision.note)}]</span>\n\n${escapeHtml(ask.text.slice(ask.elision.at))}` : escapeHtml(ask.text);
    const by = ask.from ? `<div class="ask-by"><span class="ask-from">${escapeHtml(ask.from)}</span>` : '<div>';
    return `<li class="${ask.rest ? 'ask-rest' : 'ask-key'}" data-key="${escapeHtml(dataKey.ask(ask.locator))}"${ask.rest ? ' hidden' : ''}>${time}${by}<q>${quote}</q></div></li>`;
  }).join('');
  const all = asks.some((ask) => ask.rest) ? `<button type="button" class="asks-all" aria-expanded="false" data-more="${escapeHtml(pageMsg.asksAll(own))}" data-less="${escapeHtml(pageMsg.asksKey)}">${escapeHtml(pageMsg.asksAll(own))}</button>\n` : '';
  const stats = page.stats.map((stat) => `<li${stat.warn ? ' class="warn"' : ''}><a href="#item-${stat.item}"><b>${escapeHtml(stat.n)}</b> ${escapeHtml(stat.label)}</a></li>`).join('');
  const title = span ? `${escapeHtml(pageMsg.leftForYouAsOf)} ${timeHtml(span.end, undefined, utcTimes.day)}` : escapeHtml(pageMsg.leftForYou);
  const foryou = page.foryou.length ? `<div class="foryou"><h2>${title}</h2><ol>${page.foryou.map((text, i) => `<li>${inline(text, 'page', lines.foryou[i])}</li>`).join('')}</ol></div>` : '';
  return `<header class="head">
<h1 class="title">${inline(page.heading, 'page', lines.heading)}</h1>
${askHtml ? `<ol class="asks">${askHtml}</ol>\n` : ''}${all}${stats ? `<ul class="stats">${stats}</ul>\n` : ''}<div class="story">${block(page.story, 'page', lines.story + 1, warnings)}</div>
${foryou}
</header>`;
}

export function assembleReview(prepared: PreparedReview, options: AssemblyOptions = {}): AssembledReview {
  const warnings: Array<string> = [];
  const diffs: Array<MountedDiff> = [];
  const reviewId = options.reviewId ?? prepared.parsed.page.session;
  const page: PageState = { diffs, scope: reviewId, files: new FileAnchors(prepared.items) };
  const contributors: Array<{ name: string; bytes: number }> = [];
  const header = headerHtml(prepared, warnings);
  // Items render in file order, so diff mounts are numbered as authored, then join their sections.
  const rendered = new Map(prepared.items.map((item) => {
    const html = itemHtml(item, page, options.states?.[item.metadata.id], warnings);
    contributors.push({ name: item.metadata.id, bytes: Buffer.byteLength(html) + item.evidence.reduce((size, ref) => size + (ref.kind === 'diff' ? Buffer.byteLength(escapeJson(ref.file)) : 0), 0) });
    return [item, html];
  }));
  const headings: Array<{ id: string; title: string; sub?: string }> = [...prepared.parsed.page.sections, ...FIXED_SECTIONS.map((id) => ({ id, ...pageMsg.sections[id] }))];
  const sections = headings.flatMap(({ id, title, sub }) => {
    const members = prepared.items.filter((item) => item.metadata.section === id);
    if (!members.length) return [];
    const lower = members.filter((item) => item.metadata['lower-priority']);
    const main = members.filter((item) => !item.metadata['lower-priority']).map((item) => rendered.get(item)).join('\n');
    return [`<section class="sec" id="sec-${id}" data-sec="${escapeHtml(title)}"><h2 class="sec-title">${escapeHtml(title)}${sub ? `<span class="sec-sub">${escapeHtml(sub)}</span>` : ''}</h2>${main}${lower.length ? trayHtml(id, lower, rendered) : ''}</section>`];
  }).join('\n');
  const themes = (['auto', 'light', 'dark'] as const).map((theme) => `<button type="button" data-theme-pick="${theme}" aria-pressed="${theme === 'auto'}">${pageMsg.themes[theme]}</button>`).join('');
  const rail = `<aside class="rail">
<details class="rail-fold" id="rail-fold" open><summary>${pageMsg.contents}</summary><nav id="nav" aria-label="${pageMsg.contents}"></nav>
<div class="rail-tools">
<div class="tool-row"><span class="tool-label">${pageMsg.theme}</span><span class="seg">${themes}</span></div>
</div>
</details>
</aside>`;
  const scripts = SCRIPT_URLS.map((url, i) => `${i === 2 ? '<script>var module = { exports: {} };</script>\n' : ''}<script src="${url}"></script>`).join('\n');
  contributors.push({ name: 'renderer assets', bytes: Buffer.byteLength(pageStyle) + Buffer.byteLength(js) + Buffer.byteLength(behaviour) });
  const html = `<title>${escapeHtml(prepared.parsed.page.title)}</title>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="${FONTS}">
<style>${pageStyle}</style>
<div class="page">
<main class="content" data-review="${escapeHtml(reviewId)}">
${header}
${sections}
${options.footerHtml ? `<footer class="foot">${options.footerHtml}</footer>` : ''}
</main>
${rail}
</div>

${scripts}
<script type="application/json" id="review-diffs">${escapeJson(diffs)}</script>
${options.manifest ? `<script type="application/json" id="review-manifest">${escapeJson(options.manifest)}</script>\n` : ''}<script>${js}</script>
<script>${seenStore}</script>
<script>${behaviour}</script>
<script>${escapeScript(mount)}${options.overlayScript ? '\n' + escapeScript(options.overlayScript) : ''}</script>
`;
  // Authored wrapper tags were rejected by the HTML tokenizer. Searching the
  // serialized page would also reject inert wrapper-looking JS/CSS strings.
  const bytes = Buffer.byteLength(html);
  if (bytes >= MAX_PAGE_BYTES) throw reviewError('page', 1, msg.oversized(bytes, contributors.sort((a, b) => b.bytes - a.bytes).slice(0, 5).map((entry) => `${entry.name} (${entry.bytes} bytes)`).join(', ')));
  refuseLeak(prepared, auditPage(html));
  return { html, warnings };
}

/** Changed lines and files since base, and how many of them sit in files some item's `diff` ref shows. */
export interface GitCoverage { lines: number; changedLines: number; files: number; changedFiles: number }
const changedLines = (file: ChangedFile) => (file.added ?? 0) + (file.deleted ?? 0);

/**
 * A `diff` ref shows its whole text file, so coverage is per text file; binary files, which no ref can show, and the
 * debrief's own files are left out: `excluded`
 * paths, and anything under one that sits inside the repository rather than at or above its root. Null outside a repository.
 */
export async function gitCoverage(prepared: PreparedReview, excluded: Array<string> = []): Promise<{ coverage: GitCoverage; uncovered: Array<ChangedFile> } | null> {
  const context = prepared.context;
  if (context.git === null) return null;
  const shown = new Set(prepared.items.flatMap((item) => item.evidence.filter((ref) => ref.kind === 'diff').map((ref) => ref.selector)));
  const changed = (await context.git.changedFiles(context.baseCommit, context.historical ? context.headCommit : null))
    .filter((file) => file.added !== null && !excluded.some((path) => { const absolute = join(context.cwd, file.path); return absolute === path || (!isInside(path, context.cwd) && isInside(path, absolute)); }));
  const covered = changed.filter((file) => shown.has(file.path));
  const sum = (files: Array<ChangedFile>) => files.reduce((total, file) => total + changedLines(file), 0);
  return {
    coverage: { lines: sum(covered), changedLines: sum(changed), files: covered.length, changedFiles: changed.length },
    uncovered: changed.filter((file) => !shown.has(file.path)),
  };
}
const UNCOVERED_LISTED = 20;

export interface RenderReviewOptions extends PrepareReviewOptions {
  outDir: string; previousManifestPath?: string;
}
export interface RenderReviewResult {
  pagePath: string; manifestPath: string; manifest: ReviewManifest;
  warnings: Array<string>; coverage: GitCoverage | null;
}

async function canonicalPath(path: string): Promise<string> {
  try { return await realpath(path); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return resolve(path); throw error; }
}

/** Writes a fresh snapshot. A same-session/same-round rerender reuses only its
 * reviewId, never previous evidence. Prior-round inputs cannot be output targets. */
export async function renderReview(inputPath: string, options: RenderReviewOptions): Promise<RenderReviewResult> {
  const cwd = resolve(options.cwd ?? process.cwd());
  const outDir = resolve(cwd, options.outDir), pagePath = join(outDir, 'page.html'), manifestPath = join(outDir, 'manifest.json');
  const previousPath = options.previousManifestPath ? resolve(cwd, options.previousManifestPath) : undefined;
  if (previousPath) {
    const previousCanonical = await canonicalPath(previousPath);
    if (previousCanonical === await canonicalPath(manifestPath) || previousCanonical === await canonicalPath(pagePath)) throw reviewError('page', 1, roundMsg.overwritePrevious);
  }
  const previous = previousPath ? await readManifest(previousPath) : undefined;
  // Read git state before creating transient output staging files.
  const prepared = await prepareReview(inputPath, options);
  const input = resolve(cwd, inputPath);
  const coverage = await gitCoverage(prepared, await Promise.all([input, dirname(input), outDir].map(canonicalPath)));
  await mkdir(outDir, { recursive: true });
  const nonce = randomUUID(), temporaryPage = join(outDir, `.page-${nonce}.tmp`), temporaryManifest = join(outDir, `.manifest-${nonce}.tmp`);
  try {
    const existingText = await readStateFile(manifestPath);
    const existing = existingText === null ? undefined : parseManifest(existingText, manifestPath);
    const reviewId = previous?.reviewId ?? existing?.reviewId;
    if (existing && (existing.session !== prepared.context.session || existing.round !== prepared.parsed.page.round || (reviewId !== undefined && existing.reviewId !== reviewId))) throw reviewError('page', 1, roundMsg.outputConflict);
    const manifest = buildManifest(prepared, { previous, reviewId, coverage: coverage?.coverage });
    refuseLeak(prepared, findSecretsInValue(manifest));
    const comparison = compareRounds(manifest, previous);
    const page = assembleReview(prepared, { reviewId: manifest.reviewId, manifest, states: previous ? comparison.states : undefined, footerHtml: comparison.footerHtml, overlayScript: seenOverlayScript() });
    await writeFile(temporaryPage, page.html, { flag: 'wx', mode: 0o600 });
    await writeFile(temporaryManifest, JSON.stringify(manifest, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
    // Both complete files exist before replacement. Manifest publication is the
    // final commit marker; rename never mutates a linked previous snapshot inode.
    await rename(temporaryPage, pagePath);
    await rename(temporaryManifest, manifestPath);
    const uncovered = coverage?.uncovered ?? [];
    const coverageWarnings = uncovered.length ? [reviewError('page', 1, msg.uncovered(uncovered.slice(0, UNCOVERED_LISTED).map((file) => `${file.path} (${changedLines(file)} line${changedLines(file) === 1 ? '' : 's'})`), uncovered.length)).message] : [];
    return { pagePath, manifestPath, manifest, warnings: [...page.warnings, ...comparison.warnings, ...coverageWarnings], coverage: coverage?.coverage ?? null };
  } finally {
    await Promise.all([unlink(temporaryPage).catch(() => {}), unlink(temporaryManifest).catch(() => {})]);
  }
}
