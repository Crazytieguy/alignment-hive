import { createHash } from 'node:crypto';
import { parse } from 'parse5';
import { SECRET_NAME, SECRET_SHAPES, namedScan, secretSpans } from '../lib/sanitize';
import { SECRET_RULES } from '../lib/secret-rules';
import type { ShapeId } from '../lib/sanitize';
import type { SecretRule } from '../lib/secret-rules';
import type { DefaultTreeAdapterMap } from 'parse5';

/**
 * What the review page treats as a secret: the repository's gitleaks rules (the upload
 * sanitizer's list), minus its fuzzy ones, plus the shapes gitleaks has no rule for.
 * Evidence is redacted with these before it is hashed; authored text and the finished page
 * are checked with the same rules and refused.
 */
export type SecretKind = ShapeId | 'jwt' | 'key' | 'privateKey';
export type SecretHits = Partial<Record<SecretKind, number>>;
export interface SecretFound { kind: SecretKind; start: number; end: number; match: string }

/** The upload sanitizer's entropy net and `generic-api-key` fire on ordinary code and ids (a WorkOS client id, env names, a fonts URL). */
const FUZZY = new Set(['generic-api-key']);
const RULES: Array<SecretRule> = [...SECRET_SHAPES, ...SECRET_RULES.filter((rule) => !FUZZY.has(rule.id))];
function kindOf(ruleId: string): SecretKind {
  if (SECRET_SHAPES.some((shape) => shape.id === ruleId)) return ruleId as SecretKind;
  return ruleId === 'jwt' ? 'jwt' : ruleId === 'private-key' ? 'privateKey' : 'key';
}
const marker = (kind: SecretKind) => (kind === 'key' || kind === 'privateKey' ? '[key]' : '[token]');
const MARKER = /^\[(?:token|key)\]$/;
export { SECRET_NAME };

/** A base64 data URI (an image's `src`, a CSS `url(…)`): its payload is not text, and random base64 can look like a key. */
const DATA_URI = /data:[\w.+/-]+;base64,[A-Za-z0-9+/=]+/g;
/** One scan for both modes, so what is refused and what is replaced never drift. A replacement keeps the span's newlines. */
function scanText(text: string, replace: boolean): { text: string; found: Array<SecretFound> } {
  // The text between data URIs is scanned piece by piece, each span shifted back to its place in the whole.
  const found: Array<SecretFound> = [];
  let from = 0;
  for (const uri of [...text.matchAll(DATA_URI), null]) {
    const to = uri ? uri.index : text.length;
    for (const { ruleId, valueStart, valueEnd } of secretSpans(text.slice(from, to), RULES)) {
      const hit = { kind: kindOf(ruleId), start: from + valueStart, end: from + valueEnd, match: text.slice(from + valueStart, from + valueEnd) };
      if (!MARKER.test(hit.match)) found.push(hit);
    }
    if (uri) from = uri.index + uri[0].length;
  }
  if (!replace) return { text, found };
  let out = text;
  for (const hit of [...found].reverse()) out = out.slice(0, hit.start) + marker(hit.kind) + '\n'.repeat(hit.match.split('\n').length - 1) + out.slice(hit.end);
  return { text: out, found };
}
function scanValue(value: unknown, replace: boolean, found: Array<SecretFound>, name?: string): unknown {
  if (typeof value === 'string') {
    const scan = namedScan(name, value);
    if (scan.kind === 'line') {
      const scanned = scanText(scan.prefix + value, replace);
      found.push(...scanned.found.map((hit) => ({ ...hit, start: hit.start - scan.prefix.length, end: hit.end - scan.prefix.length })));
      return scanned.text.slice(scan.prefix.length);
    }
    if (scan.kind === 'whole') {
      found.push({ kind: 'value', start: 0, end: value.length, match: value });
      return replace ? '[token]' : value;
    }
    const scanned = scanText(value, replace);
    found.push(...scanned.found);
    return scanned.text;
  }
  if (Array.isArray(value)) return value.map((item) => scanValue(item, replace, found));
  if (value !== null && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, scanValue(item, replace, found, key)]));
  return value;
}
/** `found` holds what was replaced, for counting; it never leaves the process. */
export function redactText(text: string): { text: string; found: Array<SecretFound> } {
  return scanText(text, true);
}
/** Walks objects and arrays: a string under a secret-holding name is replaced whole; every other string is redacted as text. */
export function redactValue<T>(value: T): { value: T; found: Array<SecretFound> } {
  const found: Array<SecretFound> = [];
  return { value: scanValue(value, true, found) as T, found };
}
export function findSecrets(text: string): Array<SecretFound> { return scanText(text, false).found; }
export function findSecretsInValue(value: unknown): Array<SecretFound> {
  const found: Array<SecretFound> = [];
  scanValue(value, false, found);
  return found;
}
/** Counts distinct secrets by kind: the same token seen in a command, its output and a diff counts once. */
export class SecretTally {
  readonly hits: SecretHits = {};
  private seen = new Set<string>();
  add(found: Array<SecretFound>): void {
    for (const { kind, match } of found) {
      const key = `${kind}\0${createHash('sha256').update(match).digest('hex')}`;
      if (this.seen.has(key)) continue;
      this.seen.add(key);
      this.hits[kind] = (this.hits[kind] ?? 0) + 1;
    }
  }
}

type Node = DefaultTreeAdapterMap['node'];
/**
 * The last guard: the finished page as a browser reads it. Decoded text, every attribute and each
 * JSON payload (parsed, so names still mark values). Executable scripts and styles are the
 * renderer's own assets, checked by a unit test instead.
 */
export function auditPage(html: string): Array<SecretFound> {
  const found: Array<SecretFound> = [];
  const walk = (node: Node): void => {
    if ('attrs' in node) for (const attr of node.attrs) found.push(...findSecrets(attr.value));
    if (node.nodeName === '#text' && 'value' in node) found.push(...findSecrets(node.value));
    if (node.nodeName === 'script' || node.nodeName === 'style') {
      const json = node.nodeName === 'script' && 'attrs' in node && node.attrs.some((attr) => attr.name === 'type' && attr.value === 'application/json');
      if (json && 'childNodes' in node) found.push(...findSecretsInValue(JSON.parse(node.childNodes.map((child) => ('value' in child ? child.value : '')).join('')) as unknown));
      return;
    }
    if ('childNodes' in node) for (const child of node.childNodes) walk(child);
    if ('content' in node) walk(node.content);
  };
  walk(parse(html));
  return found;
}

