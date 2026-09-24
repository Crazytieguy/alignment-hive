/**
 * Reusable headless-Chromium harness for review renderer browser tests.
 *
 * - Serves an explicit map of URL paths to local files with Bun.serve on
 *   127.0.0.1, port 0 (nothing else on disk is reachable).
 * - Drives the playwright-core from the global bun install (no dependency),
 *   with a Chromium already in the Playwright cache: always headless, never
 *   the Chrome channel, never downloaded.
 * - Writes screenshots and results under a fresh mkdtemp directory, removed
 *   on close unless REVIEW_BROWSER_KEEP=1.
 *
 * Browser tests are opt-in: `REVIEW_BROWSER_TESTS=1 bun test src/debrief/browser`.
 */
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { homedir, platform, tmpdir } from 'node:os';
import { dirname, extname, join } from 'node:path';
import { createReviewFixture } from '../fixtures';
import { SCRIPT_URLS } from '../html';
import { renderReview } from '../render';

export const browserTestsEnabled = process.env.REVIEW_BROWSER_TESTS === '1';
export const BROWSER_SKIP_REASON = 'browser tests are opt-in: set REVIEW_BROWSER_TESTS=1 (headless Chromium from the Playwright cache)';

/* ---------- the slice of the playwright-core API these tests use ---------- */

export interface Locator {
  click: (options?: { position?: { x: number; y: number } }) => Promise<void>;
  count: () => Promise<number>;
  first: () => Locator;
  last: () => Locator;
  locator: (selector: string, options?: { hasText?: string }) => Locator;
  focus: () => Promise<void>;
  getAttribute: (name: string) => Promise<string | null>;
  textContent: () => Promise<string | null>;
  screenshot: (options: { path: string }) => Promise<unknown>;
  evaluate: <TResult>(fn: (element: HTMLElement) => TResult) => Promise<TResult>;
}
export interface Clip { x: number; y: number; width: number; height: number }
export interface Page {
  goto: (url: string) => Promise<unknown>;
  addInitScript: (script: string) => Promise<void>;
  reload: () => Promise<unknown>;
  waitForFunction: ((fn: () => unknown) => Promise<unknown>) & (<TArg>(fn: (arg: TArg) => unknown, arg: TArg) => Promise<unknown>);
  evaluate: (<TResult>(fn: () => TResult | Promise<TResult>) => Promise<TResult>) &
    (<TResult, TArg>(fn: (arg: TArg) => TResult | Promise<TResult>, arg: TArg) => Promise<TResult>);
  locator: (selector: string) => Locator;
  keyboard: { press: (key: string) => Promise<void> };
  mouse: { move: (x: number, y: number, options?: { steps?: number }) => Promise<void>; down: () => Promise<void>; up: () => Promise<void> };
  addStyleTag: (options: { content?: string; url?: string }) => Promise<unknown>;
  waitForTimeout: (ms: number) => Promise<void>;
  screenshot: ((options: { path: string; fullPage?: boolean; clip?: Clip }) => Promise<unknown>) & ((options: { clip: Clip }) => Promise<Uint8Array>);
  setViewportSize: (size: { width: number; height: number }) => Promise<void>;
  on: ((event: 'pageerror', listener: (error: Error) => void) => void) &
    ((event: 'console', listener: (message: { type: () => string; text: () => string }) => void) => void);
}
interface BrowserContext { newPage: () => Promise<Page>; close: () => Promise<void> }
interface Browser {
  newContext: (options: { colorScheme: 'light' | 'dark'; viewport: { width: number; height: number }; timezoneId?: string; deviceScaleFactor?: number }) => Promise<BrowserContext>;
  close: () => Promise<void>;
}
interface Playwright { chromium: { launch: (options: { headless: true; executablePath: string }) => Promise<Browser> } }

/* ---------- locating playwright-core and a cached Chromium ---------- */

const PLAYWRIGHT_CORE = join(process.env.BUN_INSTALL ?? join(homedir(), '.bun'), 'install/global/node_modules/playwright-core');

function playwrightCache(): string {
  if (process.env.PLAYWRIGHT_BROWSERS_PATH) return process.env.PLAYWRIGHT_BROWSERS_PATH;
  return platform() === 'darwin' ? join(homedir(), 'Library/Caches/ms-playwright') : join(homedir(), '.cache/ms-playwright');
}

// Executables inside a Playwright browser directory, newest layouts first.
const EXECUTABLES: Record<'chromium_headless_shell' | 'chromium', Array<string>> = {
  chromium_headless_shell: [
    'chrome-headless-shell-mac-arm64/chrome-headless-shell', 'chrome-headless-shell-mac-x64/chrome-headless-shell',
    'chrome-headless-shell-linux64/chrome-headless-shell', 'chrome-linux/headless_shell', 'chrome-mac/headless_shell',
  ],
  chromium: [
    'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
    'chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
    'chrome-mac/Chromium.app/Contents/MacOS/Chromium', 'chrome-linux64/chrome', 'chrome-linux/chrome',
  ],
};

/** Prefer the build this playwright-core expects; otherwise the newest complete cached build. */
function resolveChromium(): string {
  const manifest = JSON.parse(readFileSync(join(PLAYWRIGHT_CORE, 'browsers.json'), 'utf8')) as { browsers: Array<{ name: string; revision: string }> };
  const expected = manifest.browsers.find((b) => b.name === 'chromium')?.revision ?? '';
  const cache = playwrightCache();
  const builds = (existsSync(cache) ? readdirSync(cache) : []).flatMap((build) => {
    const match = /^(chromium_headless_shell|chromium)-(\d+)$/.exec(build);
    if (!match || !existsSync(join(cache, build, 'INSTALLATION_COMPLETE'))) return [];
    const kind = match[1] as keyof typeof EXECUTABLES;
    const executable = EXECUTABLES[kind].map((relative) => join(cache, build, relative)).find((path) => existsSync(path));
    return executable ? [{ build, kind, revision: match[2], executable }] : [];
  });
  // Matching revision first, then newest; a headless shell before a full Chromium of the same revision.
  builds.sort((a, b) => Number(b.revision === expected) - Number(a.revision === expected) ||
    Number(b.revision) - Number(a.revision) || Number(b.kind === 'chromium_headless_shell') - Number(a.kind === 'chromium_headless_shell'));
  const chosen = builds.at(0);
  if (!chosen) throw new Error(`No complete Chromium build in ${cache}; install one with \`bun x playwright install chromium\`.`);
  return chosen.executable;
}

async function loadPlaywright(): Promise<Playwright> {
  const entry = join(PLAYWRIGHT_CORE, 'index.js');
  if (!existsSync(entry)) throw new Error(`playwright-core is not installed globally at ${PLAYWRIGHT_CORE}; run \`bun add -g playwright-core\`.`);
  return (await import(entry)) as Playwright;
}

/* ---------- files the review page scripts need, served offline ---------- */

/** Walks up from this file to the node_modules directory that holds `name`. */
export function packageDir(name: string): string {
  for (let dir = import.meta.dir; ; dir = dirname(dir)) {
    const candidate = join(dir, 'node_modules', name);
    if (existsSync(join(candidate, 'package.json'))) return candidate;
    if (dirname(dir) === dir) throw new Error(`Cannot find package ${name}; run bun install --frozen-lockfile.`);
  }
}

export const ASSETS_DIR = join(import.meta.dir, '../../../assets');

/**
 * The four pinned libraries the review page loads from its CDN, taken from the
 * repo's node_modules. highlight.js ships no browser bundle on npm, so its
 * common build is bundled once into `outDir`. Load order on a page:
 * diff, hljs, `var module = { exports: {} };`, diff-match-patch, markdown-it.
 */
export async function vendorFiles(outDir: string): Promise<Record<string, string>> {
  const entry = join(outDir, 'hljs-entry.js');
  await writeFile(entry, `import hljs from ${JSON.stringify(join(packageDir('highlight.js'), 'lib/common.js'))};\nwindow.hljs = hljs;\n`);
  const built = await Bun.build({ entrypoints: [entry], target: 'browser', format: 'iife', minify: true });
  if (!built.success) throw new Error(`highlight.js bundle failed: ${built.logs.join('\n')}`);
  const hljsPath = join(outDir, 'hljs.js');
  await writeFile(hljsPath, await built.outputs[0].text());
  return {
    ...await fontFiles(outDir),
    '/vendor/diff.min.js': join(packageDir('diff'), 'dist/diff.min.js'),
    '/vendor/highlight.min.js': hljsPath,
    '/vendor/diff-match-patch.js': join(packageDir('diff-match-patch'), 'index.js'),
    '/vendor/markdown-it.min.js': join(packageDir('markdown-it'), 'dist/browser/markdown-it.umd.min.js'),
  };
}

/**
 * The page's web fonts, pinned: the Fontsource packages repackage Google Fonts' files. Each
 * stylesheet is the package's own, with its files served under /fonts; Newsreader's variable
 * package names its family "Newsreader Variable", renamed to the page's "Newsreader".
 */
const FONT_SHEETS: Array<{ pkg: string; sheets: Array<string>; rename?: [string, string] }> = [
  { pkg: '@fontsource/ibm-plex-sans', sheets: ['400.css', '500.css', '600.css', '400-italic.css'] },
  { pkg: '@fontsource/ibm-plex-mono', sheets: ['400.css', '500.css', '600.css', '400-italic.css'] },
  { pkg: '@fontsource-variable/newsreader', sheets: ['opsz.css', 'opsz-italic.css'], rename: ["'Newsreader Variable'", "'Newsreader'"] },
];
async function fontFiles(outDir: string): Promise<Record<string, string>> {
  const files: Record<string, string> = {};
  let css = '';
  for (const { pkg, sheets, rename } of FONT_SHEETS) {
    const dir = packageDir(pkg), name = pkg.split('/')[1];
    for (const sheet of sheets) {
      let text = await readFile(join(dir, sheet), 'utf8');
      if (rename) text = text.replaceAll(rename[0], rename[1]);
      css += text.replace(/url\(\.\/files\/([^)]+)\)/g, (_, file: string) => { files[`/fonts/${name}/${file}`] = join(dir, 'files', file); return `url(/fonts/${name}/${file})`; }) + '\n';
    }
  }
  const sheet = join(outDir, 'fonts.css');
  await writeFile(sheet, css);
  return { ...files, '/fonts/fonts.css': sheet };
}

/** `local` serves the pinned fonts, `network` keeps the Google link, `none` leaves the page on its fallbacks. */
export type FontMode = 'local' | 'network' | 'none';
/** REVIEW_FONTS overrides a suite's choice. */
export function fontMode(fallback: FontMode): FontMode {
  const value = process.env.REVIEW_FONTS;
  return value === 'local' || value === 'network' || value === 'none' ? value : fallback;
}

/** A rendered review page as served by the harness: its pinned CDN scripts load from `/vendor`; its fonts as `fonts` says. */
export function localPage(html: string, { fonts = 'none' }: { fonts?: FontMode } = {}): string {
  const vendor = ['/vendor/diff.min.js', '/vendor/highlight.min.js', '/vendor/diff-match-patch.js', '/vendor/markdown-it.min.js'];
  let local = fonts === 'network' ? html : html.replace(/<link [^>]*href="https:\/\/fonts\.[^>]*>\n?/g, '');
  if (fonts === 'local') local = local.replace('<style>', '<link rel="stylesheet" href="/fonts/fonts.css">\n<style>');
  SCRIPT_URLS.forEach((url, i) => { local = local.replace(`<script src="${url}">`, `<script src="${vendor[i]}">`); });
  return local;
}

/* ---------- the harness ---------- */

const TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.json': 'application/json', '.png': 'image/png', '.svg': 'image/svg+xml', '.woff2': 'font/woff2', '.woff': 'font/woff',
};

export interface Theme { theme: 'light' | 'dark'; viewport?: { width: number; height: number }; timezone?: string; scale?: number }
export interface OpenPage { page: Page; errors: Array<string>; close: () => Promise<void> }

export interface BrowserHarness {
  /** e.g. http://127.0.0.1:53124 (no trailing slash). */
  origin: string;
  /** Temporary output directory for screenshots and results. */
  outDir: string;
  /** Serve a local file at a URL path such as `/fixture.html`. */
  serve: (urlPath: string, file: string) => void;
  /** A fresh context in the given colour scheme; page errors and console errors are collected. */
  open: (options: Theme) => Promise<OpenPage>;
  /** Absolute path for an output file inside `outDir`. */
  output: (name: string) => string;
  close: () => Promise<void>;
}

/**
 * One Chromium per test process, launched on first use and left to Playwright's exit cleanup. Under Bun, a
 * closed browser's pipe objects close their old descriptor numbers again when garbage-collected, and the next
 * browser's pipe has reused them: its connection dies mid-suite (reproduced with two launches and Bun.gc).
 */
let shared: Promise<Browser> | undefined;
function sharedBrowser(): Promise<Browser> {
  shared ??= loadPlaywright().then((playwright) => playwright.chromium.launch({ headless: true, executablePath: resolveChromium() }));
  return shared;
}

export async function startBrowserHarness(options: { keep?: boolean } = {}): Promise<BrowserHarness> {
  const keep = options.keep ?? process.env.REVIEW_BROWSER_KEEP === '1';
  const outDir = await mkdtemp(join(tmpdir(), 'hive-review-browser-'));
  const files = new Map<string, string>();
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port: 0,
    fetch(request) {
      const file = files.get(new URL(request.url).pathname);
      if (!file) return new Response('not found', { status: 404 });
      return new Response(Bun.file(file), { headers: { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store' } });
    },
  });
  const contexts = new Set<BrowserContext>();
  try {
    const launched = await sharedBrowser();
    return {
      origin: `http://127.0.0.1:${server.port}`,
      outDir,
      serve: (urlPath, file) => { files.set(urlPath, file); },
      async open({ theme, viewport = { width: 1280, height: 900 }, timezone, scale }) {
        const context = await launched.newContext({ colorScheme: theme, viewport, ...(timezone && { timezoneId: timezone }), ...(scale && { deviceScaleFactor: scale }) });
        contexts.add(context);
        const page = await context.newPage();
        const errors: Array<string> = [];
        page.on('pageerror', (error) => errors.push(error.message));
        page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
        return { page, errors, close: async () => { contexts.delete(context); await context.close(); } };
      },
      output: (name) => join(outDir, name),
      async close() {
        await Promise.all([...contexts].map((context) => context.close()));
        await server.stop(true);
        if (keep) console.log(`browser test outputs kept in ${outDir}`);
        else await rm(outDir, { recursive: true, force: true });
      },
    };
  } catch (error) {
    await server.stop(true);
    await rm(outDir, { recursive: true, force: true });
    throw error;
  }
}

/** A harness serving one rendered review page at `/page.html`, over a review fixture that holds `notes.md`. */
export async function startPageHarness() {
  const harness = await startBrowserHarness();
  const fixture = await createReviewFixture();
  const close = async () => { await harness.close(); await fixture.cleanup(); };
  try {
    for (const [url, file] of Object.entries(await vendorFiles(harness.outDir))) harness.serve(url, file);
    harness.serve('/page.html', join(harness.outDir, 'page.html'));
    await fixture.write('notes.md', '---\nname: notes\n---\n\n# Notes\n\nBody.\n');
  } catch (error) { await close(); throw error; }
  /** Writes `document` as the fixture's review, renders it, and serves the page. */
  async function render(document: string, { fonts = 'none' }: { fonts?: FontMode } = {}): Promise<void> {
    const input = join(fixture.root, 'debrief.md'), out = join(fixture.root, 'out');
    await writeFile(input, document);
    await renderReview(input, { cwd: fixture.repo, outDir: out, projects: fixture.projects });
    await writeFile(join(harness.outDir, 'page.html'), localPage(await readFile(join(out, 'page.html'), 'utf8'), { fonts }));
  }
  return { harness, fixture, render, close };
}
