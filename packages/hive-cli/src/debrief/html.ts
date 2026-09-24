import { randomUUID } from 'node:crypto';
// Official self-contained ESM bundle: the Node entry dynamically requires JSON
// relative to import.meta.url, which is unavailable inside a Bun executable.
import * as css from 'css-tree/dist/csstree.esm';
import MarkdownIt from 'markdown-it';
import { Parser } from 'parse5';
import { isMap, isScalar, parseDocument } from 'yaml';
import { reviewHtmlMessages as msg } from '../lib/messages';
import type { DefaultTreeAdapterMap } from 'parse5';

type Markdown = ReturnType<typeof MarkdownIt>;
type MarkdownToken = ReturnType<Markdown['parse']>[number];
type MarkdownEnv = Parameters<Markdown['parse']>[1];

export const SCRIPT_URLS = [
  'https://cdnjs.cloudflare.com/ajax/libs/jsdiff/9.0.0/diff.min.js',
  'https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.12.0/highlight.min.js',
  'https://cdnjs.cloudflare.com/ajax/libs/diff-match-patch/1.0.5/index.js',
  'https://cdn.jsdelivr.net/npm/markdown-it@15.0.1/dist/browser/markdown-it.umd.min.js',
] as const;

export interface HtmlContext { itemId: string; line: number }
export interface HtmlResult { html: string; warnings: Array<string> }
type Node = DefaultTreeAdapterMap['node'];
type Element = DefaultTreeAdapterMap['element'];
interface RawRange { start: number; end: number; line: number }
const htmlNamespace = 'http://www.w3.org/1999/xhtml';
const voidTags = new Set('area base br col embed hr img input link meta param source track wbr'.split(' '));
const forbiddenTags = new Set('doctype html head body link iframe object embed base meta frameset frame plaintext xmp noembed noframes noscript'.split(' '));
const urlAttributes = new Set('src href poster background action formaction ping cite longdesc manifest profile codebase data'.split(' '));

/** Text and quoted attribute values, never tag/attribute names or raw-text contexts. */
export function escapeHtml(value: string): string {
  return value.replace(/[&<>"']|[^\x20-\x7e\n\r\t]/gu, (character) => {
    const point = character.codePointAt(0)!;
    // HTML reparses NUL/C1 character references as replacement or Windows-1252
    // characters. Display forbidden controls explicitly instead of losing evidence.
    if (point < 32 || (point >= 127 && point < 160)) return `\\u${point.toString(16).padStart(4, '0')}`;
    return `&#${point};`;
  });
}

export function escapeJson(value: unknown): string {
  const source = JSON.stringify(value) as string | undefined;
  if (source === undefined) throw new Error(msg.invalidJson);
  return source.replace(/[<\x7f-￿]/g,
    (character) => `\\u${character.charCodeAt(0).toString(16).padStart(4, '0')}`);
}

const scriptHazards = /[^\x20-\x7e\n\r\t]/u;
/** Trusted author scripts stay unchanged; authors supply ASCII escapes. */
export function escapeScript(source: string): string {
  if (/<\/script(?=[\s/>])/i.test(source)) throw new Error(msg.closingScript);
  if (scriptHazards.test(source)) throw new Error(msg.invalidScript);
  return source;
}

export function escapeCss(source: string): string {
  return source.replace(/<(?=\/style[\s/>])|[^\x20-\x7e\n\r\t]/giu,
    (character) => `\\${character.codePointAt(0)!.toString(16)} `);
}

function fail(context: HtmlContext, detail: string): never {
  throw new Error(msg.error(context.itemId, context.line, detail));
}

function localUrl(value: string, fragment = false): boolean {
  const normalized = value.trim();
  return /^data:/i.test(normalized) || (fragment && normalized.startsWith('#'));
}

// CSS identifiers retain escapes in css-tree's AST. Decode according to CSS syntax,
// not JS/URL syntax; comments are tokenized by the parser before these checks.
function cssIdentifier(value: string): string {
  return value.replace(/\\([0-9a-f]{1,6})(?:\r\n|[\t\n\r\f ])?|\\([^\n\r\f])/gi,
    (_, hex: string | undefined, literal: string | undefined) => {
      if (!hex) return literal ?? '';
      const point = Number.parseInt(hex, 16);
      return point === 0 || point > 0x10ffff || (point >= 0xd800 && point <= 0xdfff) ? '�' : String.fromCodePoint(point);
    }).toLowerCase();
}

interface ColorCues { red: boolean; green: boolean; labels: Set<string>; pattern: boolean }
function colorLiteral(value: string, cues: ColorCues): void {
  // Warning-only literal heuristic, not a CSS color-space parser.
  for (const literal of value.toLowerCase().match(/#[\da-f]{3,8}\b|\b[a-z]+\b/g) ?? []) {
    if (/^(?:red|darkred|firebrick|crimson|salmon|coral|tomato|maroon|brown|pink)$/.test(literal)) cues.red = true;
    if (/^(?:green|darkgreen|lime|limegreen|forestgreen|seagreen|springgreen|lightgreen|palegreen|chartreuse|lawngreen|yellowgreen)$/.test(literal)) cues.green = true;
    if (!/^#(?:[\da-f]{3,4}|[\da-f]{6}|[\da-f]{8})$/.test(literal)) continue;
    const hex = literal.length < 6 ? [...literal.slice(1)].map((part) => part + part).join('') : literal.slice(1);
    const [red, green, blue] = [0, 2, 4].map((offset) => Number.parseInt(hex.slice(offset, offset + 2), 16));
    if (red > green * 1.3 && red > blue * 1.3) cues.red = true;
    if (green > red * 1.3 && green > blue * 1.3) cues.green = true;
  }
}

function validateCss(source: string, context: HtmlContext, cues: ColorCues, mode: 'stylesheet' | 'declarationList' | 'value'): void {
  let tree: css.CssNode;
  try {
    tree = css.parse(source, { context: mode, parseCustomProperty: true, onParseError: () => fail(context, msg.invalidCss) });
  } catch {
    fail(context, msg.invalidCss);
  }
  css.walk(tree, (node) => {
    if (node.type === 'Raw') fail(context, msg.invalidCss);
    if (node.type === 'Atrule' && cssIdentifier(node.name) === 'import') fail(context, msg.cssImport);
    if (node.type === 'Url' && !localUrl(cssIdentifier(node.value), true)) fail(context, msg.cssResource);
    if (node.type === 'Function') {
      const name = cssIdentifier(node.name);
      // CSS permits quoted URLs in image-set() and src(), without a url() node.
      if (['url', 'src', 'image-set', '-webkit-image-set', 'image'].includes(name)) {
        css.walk(node, (child) => {
          if (child.type === 'String' && !localUrl(cssIdentifier(child.value), true)) fail(context, msg.cssResource);
        });
        if (name === 'url' || name === 'src') {
          const value = node.children.toArray().map((child) => css.generate(child)).join('').replace(/^['"]|['"]$/g, '');
          if (!localUrl(cssIdentifier(value), true)) fail(context, msg.cssResource);
        }
      }
    }
    if (node.type === 'Hash') colorLiteral('#' + node.value, cues);
    if (node.type === 'Identifier') {
      const value = cssIdentifier(node.name);
      colorLiteral(value, cues);
      if (['dashed', 'dotted', 'double', 'wavy', 'underline', 'line-through'].includes(value)) cues.pattern = true;
    }
  });
}

function validateAttributes(tag: string, attrs: Element['attrs'], context: HtmlContext, cues: ColorCues): void {
  for (const attr of attrs) {
    const name = attr.name.toLowerCase().split(':').at(-1)!;
    if (/[^\x20-\x7e]/.test(attr.name)) fail(context, msg.invalidName);
    if (urlAttributes.has(name)) {
      const pinnedScript = tag === 'script' && name === 'src' && SCRIPT_URLS.some((url) => url === attr.value);
      if (!pinnedScript && !localUrl(attr.value, name === 'href')) fail(context, msg.forbiddenUrl(name));
    }
    if (name === 'srcset' || name === 'imagesrcset') {
      fail(context, msg.forbiddenSrcset);
    }
    if (name === 'style') validateCss(attr.value, context, cues, 'declarationList');
    if (['fill', 'stroke', 'filter', 'clip-path', 'mask', 'cursor', 'marker', 'marker-start', 'marker-mid', 'marker-end'].includes(name)) {
      validateCss(attr.value, context, cues, 'value');
    }
    if (['color', 'bgcolor'].includes(name)) colorLiteral(attr.value, cues);
    if (['aria-label', 'alt', 'title'].includes(name) && attr.value.trim()) cues.labels.add(attr.value.trim());
    if (name === 'stroke-dasharray' && !/^(?:none|0(?:[ ,]+0)*)$/.test(attr.value)) cues.pattern = true;
    if (name === 'srcdoc') fail(context, msg.forbiddenUrl(name));
  }
}

function processHtml(source: string, context: HtmlContext, rawRanges: Array<RawRange>): HtmlResult {
  const cues: ColorCues = { red: false, green: false, labels: new Set(), pattern: false };
  const rawContext = (offset: number): HtmlContext | undefined => {
    const range = rawRanges.find((entry) => offset >= entry.start && offset < entry.end);
    return range ? { itemId: context.itemId, line: range.line + source.slice(range.start, offset).split('\n').length - 1 } : undefined;
  };
  const parser = Parser.getFragmentParser<DefaultTreeAdapterMap>(null, { sourceCodeLocationInfo: true });
  // A fragment parser silently drops document wrappers. Inspect the same parser's
  // tokens as well as its resulting tree, so dropped/foreign-content tags cannot evade guards.
  const startTag = parser.onStartTag.bind(parser);
  parser.onStartTag = (token) => {
    const at = rawContext(token.location?.startOffset ?? -1);
    if (at) {
      if (forbiddenTags.has(token.tagName)) fail(at, msg.forbiddenTag(token.tagName));
      if (/[^\x20-\x7e]/.test(token.tagName)) fail(at, msg.invalidName);
      validateAttributes(token.tagName, token.attrs, at, cues);
    }
    startTag(token);
  };
  const endTag = parser.onEndTag.bind(parser);
  parser.onEndTag = (token) => {
    const at = rawContext(token.location?.startOffset ?? -1);
    if (at && forbiddenTags.has(token.tagName)) fail(at, msg.forbiddenTag(token.tagName));
    endTag(token);
  };
  const doctype = parser.onDoctype.bind(parser);
  parser.onDoctype = (token) => {
    const at = rawContext(token.location?.startOffset ?? -1);
    if (at) fail(at, msg.forbiddenTag('doctype'));
    doctype(token);
  };
  parser.tokenizer.write(source, true);

  const serialize = (node: Node, authored = false): string => {
    if (node.nodeName === '#text') {
      const text = (node as DefaultTreeAdapterMap['textNode']).value;
      if (authored && text.trim()) cues.labels.add(text.trim());
      return escapeHtml(text);
    }
    if (node.nodeName === '#comment') return ''; // Comments are inert, not authored display content.
    if (!('tagName' in node)) return 'childNodes' in node ? node.childNodes.map((child) => serialize(child)).join('') : '';
    const location = node.sourceCodeLocation;
    const at = rawContext(location?.startOffset ?? -1);
    const isAuthored = Boolean(at) || authored;
    const tag = node.tagName;
    const attrs = node.attrs.map((attr) => ` ${attr.prefix ? attr.prefix + ':' : ''}${attr.name}="${escapeHtml(attr.name === 'style' ? escapeCss(attr.value) : attr.value)}"`).join('');
    let content: string;
    if (tag === 'script' || tag === 'style') {
      if (at && tag === 'script' && !location?.endTag) fail(at, msg.unclosedScript);
      // Read raw HTML text from the source: parse5 replaces actual NUL with U+FFFD.
      // SVG uses CDATA/entity parsing instead, so use its decoded text nodes.
      const text = node.namespaceURI === htmlNamespace && location?.startTag
        ? source.slice(location.startTag.endOffset, location.endTag?.startOffset ?? location.endOffset)
        : node.childNodes.map((child) => child.nodeName === '#text' ? (child as DefaultTreeAdapterMap['textNode']).value : '').join('');
      if (tag === 'style') {
        if (at) validateCss(text, at, cues, 'stylesheet');
        content = escapeCss(text);
      } else {
        const type = node.attrs.find((attr) => attr.name === 'type')?.value.trim().toLowerCase() ?? '';
        try {
          if (type === 'importmap' || type === 'speculationrules' || /(?:^|\/)json$|\+json$/.test(type)) content = escapeJson(JSON.parse(text));
          else if (!type || type === 'module' || /^(?:text|application)\/(?:java|ecma)script$/.test(type)) content = escapeScript(text);
          else if (scriptHazards.test(text)) throw new Error(msg.unsupportedScript);
          else content = text;
        } catch (error) {
          if (at) fail(at, (error as Error).message);
          throw error;
        }
      }
    } else {
      const children = tag === 'template' && 'content' in node ? node.content.childNodes : node.childNodes;
      content = children.map((child) => serialize(child, isAuthored)).join('');
    }
    if (node.namespaceURI === htmlNamespace && voidTags.has(tag)) return `<${tag}${attrs}>`;
    // Preserve leading newlines when the serialized HTML is parsed again.
    if (node.namespaceURI === htmlNamespace && ['pre', 'textarea', 'listing'].includes(tag) && content.startsWith('\n')) content = '\n' + content;
    return `<${tag}${attrs}>${content}</${tag}>`;
  };
  const html = serialize(parser.getFragment());
  const warnings = cues.red && cues.green && !cues.pattern && cues.labels.size < 2 ? [msg.error(context.itemId, context.line, msg.redGreen)] : [];
  return { html, warnings };
}

function visitTokens(tokens: Array<MarkdownToken>, visit: (token: MarkdownToken, line: number) => void, line = 0): void {
  for (const token of tokens) {
    const current = token.map?.[0] ?? line;
    visit(token, current);
    if (token.children) visitTokens(token.children, visit, current);
  }
}

const diffMarkdown = new MarkdownIt({ html: false, linkify: false });
/** Maps a relative `.md` link target to the id of a file view on the page. */
export type FileLink = (relativePath: string) => string | undefined;
// Relative links inside a file view point at files, not at page URLs: they become
// in-page anchors when the target is shown, and plain text otherwise.
diffMarkdown.core.ruler.after('inline', 'file-links', (state) => {
  const link = (state.env as { fileLink?: FileLink } | undefined)?.fileLink;
  if (!link) return;
  const plain = (token: MarkdownToken) => { token.type = 'text'; token.tag = ''; token.nesting = 0; token.content = ''; token.attrs = null; };
  for (const block of state.tokens) {
    const tokens = block.children ?? [];
    tokens.forEach((token, i) => {
      if (token.type !== 'link_open') return;
      const href = String(token.attrGet('href') ?? '');
      if (/^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(href)) return;
      let target: string | undefined;
      try {
        const path = decodeURIComponent(href.split(/[?#]/)[0]);
        if (path.endsWith('.md') && !path.startsWith('/')) target = link(path);
      } catch { /* A malformed URL is plain text too. */ }
      if (target) { token.attrSet('href', `#${target}`); return; }
      plain(token);
      // Links do not nest, so the next close is this link's.
      const close = tokens.slice(i + 1).find((next) => next.type === 'link_close');
      if (close) plain(close);
    });
  }
});

/** Front matter as `key: value` rows, nested keys dotted; empty values are left out. */
function frontMatterRows(value: unknown, prefix = ''): Array<[string, string]> {
  if (value && typeof value === 'object' && !Array.isArray(value)) return Object.entries(value).flatMap(([key, child]) => frontMatterRows(child, prefix ? `${prefix}.${key}` : key));
  if (value == null || value === '' || (Array.isArray(value) && !value.length)) return [];
  return [[prefix, typeof value === 'object' ? JSON.stringify(value) : String(value)]];
}

/** File evidence is inert Markdown, never authored review HTML or ref fences. */
export function renderMarkdownFile(source: string, context: HtmlContext, startLine: number, fileLink: FileLink = () => undefined): string {
  validateMarkdownImages(source, context);
  const front = startLine === 1 ? /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(source) : null;
  let metadata = '';
  if (front) {
    const doc = parseDocument(front[1]);
    if (!doc.errors.length && isMap(doc.contents)) {
      const rows = frontMatterRows(doc.toJS());
      if (rows.length) metadata = `<dl class="fv-fm">${rows.map(([key, value]) => `<dt>${escapeHtml(key)}</dt><dd>${escapeHtml(value)}</dd>`).join('')}</dl>`;
    } else {
      metadata = `<pre><code class="language-yaml">${escapeHtml(front[0])}</code></pre>`;
    }
    source = source.slice(front[0].length);
  }
  return metadata + renderMarkdownHtml(diffMarkdown, source, context, { fileLink }).html;
}

// Transcript text is evidence: no raw HTML, and no images to fetch.
const proseMarkdown = new MarkdownIt({ html: false, linkify: false }).disable('image');
/** A reply or prompt from the transcript, rendered as inert Markdown. */
export function renderProse(source: string, context: HtmlContext): string {
  return renderMarkdownHtml(proseMarkdown, source, context).html;
}

/** Browser diff Markdown uses html:false; this gate prevents its image tokens fetching remotely. `inline` parses one-line fields. */
export function validateMarkdownImages(source: string, context: HtmlContext, inline = false): void {
  visitTokens(inline ? diffMarkdown.parseInline(source, {}) : diffMarkdown.parse(source, {}), (token, line) => {
    if (token.type === 'image' && !localUrl(String(token.attrGet('src') ?? ''))) fail({ itemId: context.itemId, line: context.line + line }, msg.markdownImage);
  });
}

/**
 * Render using the caller's fence rules, then validate authored raw nodes by provenance.
 * Opaque token placeholders avoid parsing open/close html_inline tokens separately and
 * keep generated Markdown links/evidence out of author-only resource restrictions.
 * No renderer rules are mutated, so re-entrant/shared MarkdownIt instances are safe.
 */
export function renderMarkdownHtml(markdown: Markdown, source: string, context: HtmlContext, env: MarkdownEnv = {}, inline = false): HtmlResult {
  const tokens = inline ? markdown.parseInline(source, env) : markdown.parse(source, env);
  const prefix = `reviewraw${randomUUID().replaceAll('-', '')}x`;
  const raw: Array<{ marker: string; source: string; line: number }> = [];
  const remember = (value: string, line: number): string => {
    const marker = `${prefix}${raw.length}x`;
    raw.push({ marker, source: value, line });
    return marker;
  };
  // markdown-it treats inline script/style contents as Markdown (including quote
  // entities and emphasis). Probe its own inline tokenizer for real HTML starts,
  // then shield each complete raw-text element before re-parsing that inline body.
  // This also distinguishes actual HTML from identical text inside code spans.
  for (const token of tokens) {
    if (token.type !== 'inline' || !token.children?.some((child) => child.type === 'html_inline' && /^<(?:script|style)(?=[\s>])/i.test(child.content))) continue;
    const starts: Array<{ token: MarkdownToken; offset: number }> = [];
    const state = new markdown.inline.State(token.content, markdown, env, []);
    const push = state.push.bind(state);
    state.push = (type, tag, nesting) => {
      const child = push(type, tag, nesting);
      if (type === 'html_inline') starts.push({ token: child, offset: state.pos });
      return child;
    };
    markdown.inline.tokenize(state);
    const replacements: Array<{ start: number; end: number; marker: string }> = [];
    let end = 0;
    for (const entry of starts) {
      if (entry.offset < end || !/^<(?:script|style)(?=[\s>])/i.test(entry.token.content)) continue;
      const parser = Parser.getFragmentParser<DefaultTreeAdapterMap>(null, { sourceCodeLocationInfo: true });
      parser.tokenizer.write(token.content.slice(entry.offset), true);
      const element = parser.getFragment().childNodes.find((node) => 'tagName' in node && ['script', 'style'].includes(node.tagName));
      const close = element?.sourceCodeLocation && 'endTag' in element.sourceCodeLocation ? element.sourceCodeLocation.endTag : undefined;
      const line = context.line + (token.map?.[0] ?? 0) + token.content.slice(0, entry.offset).split('\n').length - 1;
      if (!close) fail({ itemId: context.itemId, line }, /^<script/i.test(entry.token.content) ? msg.unclosedScript : msg.unclosedStyle);
      end = entry.offset + close.endOffset;
      replacements.push({ start: entry.offset, end, marker: remember(token.content.slice(entry.offset, end), line) });
    }
    let content = token.content;
    for (const replacement of replacements.toReversed()) content = content.slice(0, replacement.start) + replacement.marker + content.slice(replacement.end);
    token.children = [];
    markdown.inline.parse(content, markdown, env, token.children);
  }
  visitTokens(tokens, (token, line) => {
    if (token.type !== 'html_block' && token.type !== 'html_inline') return;
    const marker = remember(token.content, context.line + line);
    token.type = 'html_inline';
    token.content = marker;
  });
  const rendered = markdown.renderer.render(tokens, markdown.options, env);
  const ranges: Array<RawRange> = [];
  let delta = 0;
  const html = rendered.replace(new RegExp(`${prefix}(\\d+)x`, 'g'), (_marker, index: string, offset: number) => {
    const token = raw[Number(index)];
    const start = offset + delta;
    ranges.push({ start, end: start + token.source.length, line: token.line });
    delta += token.source.length - token.marker.length;
    return token.source;
  });
  return processHtml(html, context, ranges);
}

/** One-line fields take no raw HTML: `<session>` in a lede is text. Only item bodies allow HTML. */
const inlineMarkdown = new MarkdownIt({ html: false, linkify: false });
/** Authored one-line Markdown (headings, ledes, alternatives, captions, summaries), validated like bodies. */
export function renderMarkdownInline(source: string, context: HtmlContext): string {
  validateMarkdownImages(source, context, true);
  return renderMarkdownHtml(inlineMarkdown, source, context, {}, true).html;
}
