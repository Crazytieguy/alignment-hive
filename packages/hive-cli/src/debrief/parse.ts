import MarkdownIt from 'markdown-it';
import { LineCounter, isAlias, isNode, isScalar, parseDocument, visit } from 'yaml';
import { z } from 'zod';
import { review as msg } from '../lib/messages';
import { findSecrets } from './redact';
import type { SecretFound } from './redact';

export const markdown = new MarkdownIt({ html: true, linkify: false });
const slug = z.string().regex(/^[a-z0-9]+(?:-[a-z0-9]+)*$/);
export const itemIdSchema = slug;
const oneLine = z.string().trim().min(1).refine((s) => !/[\r\n]/.test(s));
export const sessionIdSchema = z.string().uuid();

/** Sections the renderer supplies after the author's story sections, in this order. */
export const FIXED_SECTIONS = ['checked', 'unverified', 'landing', 'side-effects'] as const;

/** `<session or subagent id prefix>:<entry>`, the numbering `hive local` prints. */
const locatorSchema = z.string().trim().regex(/^[^\s:,]+:[1-9]\d*$/, { message: msg.invalidLocator });
/** A transcript ref's entries, comma-separated in author order: locators, or `capture:<name>` for a command captured while writing the page. */
const entryList = z.string().transform((value) => value.split(',').map((part) => part.trim()))
  .pipe(z.array(z.string().regex(/^(?:capture:\S+|[^\s:,]+:[1-9]\d*)$/, { message: msg.invalidEntry })).min(1));

export const rangeSchema = z.string().trim().regex(/^L[1-9]\d*(?:-L?[1-9]\d*)?$/, { message: msg.invalidRange }).transform((source) => {
  const [start, end = start] = source.replaceAll('L', '').split('-').map(Number);
  return { source, start, end };
}).refine(({ start, end }) => Number.isSafeInteger(start) && Number.isSafeInteger(end) && start <= end, { message: msg.invalidRange });
export type LineRange = z.infer<typeof rangeSchema>;
/** `L10-L20, L88`: one or more ranges. */
export const rangesSchema = z.string().transform((value) => value.split(',')).pipe(z.array(rangeSchema).min(1));

const elideSchema = z.strictObject({ from: oneLine, until: oneLine.optional(), note: oneLine });
interface KeyAsk { locator: string; elide?: z.infer<typeof elideSchema> }

export const pageSchema = z.strictObject({
  title: oneLine,
  heading: oneLine,
  session: sessionIdSchema,
  base: oneLine.optional(),
  head: oneLine.optional(),
  round: z.number().int().positive().default(1),
  /** The key asks: a locator, or `{ ask, elide }` to leave out a long paste (`from` its first text, `until` the first text kept). */
  asks: z.array(z.union([
    locatorSchema.transform((locator): KeyAsk => ({ locator })),
    z.strictObject({ ask: locatorSchema, elide: elideSchema }).transform(({ ask, elide }): KeyAsk => ({ locator: ask, elide })),
  ])).default([]),
  stats: z.array(z.strictObject({ n: z.union([oneLine, z.number().transform(String)]), label: oneLine, item: slug, warn: z.boolean().default(false) })).max(3).default([]),
  story: z.string().trim().min(1),
  foryou: z.array(oneLine).default([]),
  sections: z.array(z.strictObject({ id: slug, title: oneLine })).default([]),
  dispositions: z.record(slug, z.string().regex(/^(resolved|withdrawn|superseded: [a-z0-9]+(?:-[a-z0-9]+)*)$/)).default({}),
});
const itemSchema = z.strictObject({
  id: slug, was: slug.optional(), section: slug, nav: oneLine.optional(), lede: oneLine,
  'judgement-call': z.boolean().default(false),
  /** Folds into its section's tray of lower-priority items; never a judgement call. */
  'lower-priority': z.boolean().default(false),
  alternatives: z.array(z.string().trim().min(1)).default([]),
});

const caption = { caption: z.string().trim().min(1).optional() };
const summary = z.union([oneLine, z.array(oneLine).min(1)]);
function summaryCount(value: { summary?: string | Array<string> }, count: number, context: z.RefinementCtx): void {
  if (Array.isArray(value.summary) && value.summary.length !== count) context.addIssue({ code: 'custom', path: ['summary'], message: msg.summaryCount(count) });
  if (typeof value.summary === 'string' && count !== 1) context.addIssue({ code: 'custom', path: ['summary'], message: msg.summaryCount(count) });
}
const refSchemas = {
  diff: z.strictObject({
    diff: oneLine, title: oneLine.optional(), focus: rangesSchema.optional(), 'old-focus': rangesSchema.optional(),
    labels: z.array(z.strictObject({ line: z.number().int().positive(), where: oneLine })).default([]), ...caption,
  }),
  file: z.strictObject({ file: oneLine, at: oneLine.optional(), written: locatorSchema.optional(), range: rangeSchema.optional(), ...caption })
    .refine((ref) => !(ref.at && ref.written), { message: msg.writtenAndAt, path: ['written'] }),
  /** `clip: false` shows long replies and prompts whole; `results: false` hides every entry's tool results. */
  transcript: z.strictObject({ transcript: entryList, summary, clip: z.literal(false).optional(), results: z.literal(false).optional(), ...caption })
    .superRefine((ref, context) => summaryCount(ref, ref.transcript.length, context)),
  git: z.strictObject({ git: z.literal(true), ...caption }),
  /** `summary` heads the fold and is the image's alt text; `width` is a preferred CSS width, never wider than the column. */
  image: z.strictObject({ image: oneLine, summary: oneLine, width: z.number().int().positive().optional(), ...caption }),
};
type RefKind = keyof typeof refSchemas;
const refKinds = Object.keys(refSchemas) as Array<RefKind>;
/** A ref's kind is the one kind key it carries, so key errors name the real schema. */
function selectRefSchema(raw: unknown): z.ZodType<EvidenceRef> {
  const keys = raw && typeof raw === 'object' && !Array.isArray(raw) ? Object.keys(raw) : [];
  const kinds = refKinds.filter((kind) => keys.includes(kind));
  if (kinds.length !== 1) return z.never({ message: msg.refKind });
  return refSchemas[kinds[0]] as z.ZodType<EvidenceRef>;
}
export const refSchema = z.unknown().transform((raw, context) => {
  const result = selectRefSchema(raw).safeParse(raw);
  if (!result.success) { for (const issue of result.error.issues) context.addIssue({ ...issue, code: 'custom' } as never); return z.NEVER; }
  return result.data;
});

export type ReviewPage = z.infer<typeof pageSchema>;
export type ItemMetadata = z.infer<typeof itemSchema>;
export type EvidenceRef = { [K in RefKind]: z.infer<(typeof refSchemas)[K]> }[RefKind];
export interface LocatedRef { ref: EvidenceRef; line: number; bodyLine: number }
export interface ReviewItem { metadata: ItemMetadata; heading: string; body: string; line: number; bodyLine: number; refs: Array<LocatedRef> }
export interface PageLines { asks: Array<number>; story: number; foryou: Array<number>; heading: number }
export interface ParsedReview { page: ReviewPage; items: Array<ReviewItem>; lines: PageLines }

export function reviewError(item: string, line: number, detail: string): Error {
  return new Error(msg.error(item, line, detail));
}

interface ParsedYaml<T> { data: T; lineOf: (path: Array<string | number>) => number }

/** YAML is parsed once as a tree, both to validate authoring rules and retain source locations. */
function parseYaml<T>(source: string, schema: z.ZodType<T> | ((raw: unknown) => z.ZodType<T>), item: string, line: number): ParsedYaml<T> {
  const counter = new LineCounter();
  const doc = parseDocument(source, { lineCounter: counter, uniqueKeys: true });
  const at = (offset: number) => line + counter.linePos(offset).line - 1;
  if (doc.errors.length) throw reviewError(item, at(doc.errors[0].pos[0]), doc.errors[0].message);
  // An id can still contextualize schema/quoting errors in otherwise invalid metadata.
  const id = doc.get('id');
  const context = typeof id === 'string' ? id : item;
  visit(doc, (key, node) => {
    if (isAlias(node)) throw reviewError(context, at(node.range?.[0] ?? 0), msg.aliases);
    // Anchors and tags silently change a plain value's meaning.
    if (isNode(node) && (node.anchor || node.tag)) throw reviewError(context, at(node.range?.[0] ?? 0), msg.aliases);
    if (!isScalar(node) || typeof node.value !== 'string' || key === 'key') return;
    const multiline = /[\r\n]/.test(node.value);
    // YAML removes hash-bearing suffixes before exposing scalar.value.
    const trailingComment = node.type === 'PLAIN' && node.comment !== undefined;
    if ((multiline && node.type !== 'BLOCK_LITERAL') || trailingComment) throw reviewError(context, at(node.range?.[0] ?? 0), msg.quoting);
  });
  const lineOf = (path: Array<string | number>): number => {
    for (let depth = path.length; depth >= 0; depth--) {
      const node = doc.getIn(path.slice(0, depth), true);
      const range = node && typeof node === 'object' && 'range' in node ? (node.range as Array<number> | null) : null;
      if (range) return at(range[0]);
    }
    return line;
  };
  const raw = doc.toJS({ maxAliasCount: 0 }) as unknown;
  const result = (typeof schema === 'function' ? schema(raw) : schema).safeParse(raw);
  if (!result.success) {
    const issue = result.error.issues[0];
    const path = issue.path as Array<string | number>;
    // An unrecognized key is reported on its own line, not its object's.
    const where = issue.code === 'unrecognized_keys' ? [...path, issue.keys[0]] : path;
    throw reviewError(context, lineOf(where), `${path.join('.') || 'metadata'}: ${issue.message}`);
  }
  return { data: result.data, lineOf };
}

/**
 * A secret the page would show fails the render at the debrief file's line that holds it, looking from `from` on,
 * naming its kind, never its text. Authored text is never rewritten; evidence is, before this. Without `from` (the
 * finished page's audit), a secret the file does not hold came from evidence that redaction missed.
 */
export function refuseSecret(hit: SecretFound | undefined, item: string, lines: ReadonlyArray<string>, from?: number): void {
  if (!hit) return;
  const first = hit.match.split('\n')[0], kind = msg.secretKinds[hit.kind];
  const found = lines.findIndex((row, i) => i >= (from ?? 1) - 1 && row.includes(first));
  if (found >= 0) throw reviewError(item, found + 1, msg.secretInPage(kind));
  throw from === undefined ? reviewError(item, 1, msg.secretInEvidence(kind)) : reviewError(item, from, msg.secretInPage(kind));
}

export function parseReview(source: string): ParsedReview {
  const lines = source.replace(/\r\n/g, '\n').split('\n');
  if (lines[0] !== '---') throw reviewError('page', 1, msg.frontmatter);
  const end = lines.indexOf('---', 1);
  if (end < 0) throw reviewError('page', 1, msg.frontmatter);
  const front = parseYaml(lines.slice(1, end).join('\n'), pageSchema, 'page', 2);
  const page = front.data;
  const authored: Array<[string, Array<string | number>]> = [
    [page.title, ['title']], [page.heading, ['heading']], [page.story, ['story']],
    ...page.foryou.map((text, i): [string, Array<string | number>] => [text, ['foryou', i]]),
    ...page.stats.flatMap((stat, i): Array<[string, Array<string | number>]> => [[stat.n, ['stats', i, 'n']], [stat.label, ['stats', i, 'label']]]),
    ...page.sections.map((section, i): [string, Array<string | number>] => [section.title, ['sections', i, 'title']]),
    // Of an elision only the note shows; its anchors are the message's own text.
    ...page.asks.flatMap((ask, i): Array<[string, Array<string | number>]> => (ask.elide ? [[ask.elide.note, ['asks', i, 'elide', 'note']]] : [])),
  ];
  for (const [text, path] of authored) refuseSecret(findSecrets(text).at(0), 'page', lines, front.lineOf(path));
  const sectionIds = new Set<string>(FIXED_SECTIONS);
  page.sections.forEach((section, i) => {
    if (sectionIds.has(section.id)) throw reviewError('page', front.lineOf(['sections', i, 'id']), msg.duplicateSection(section.id));
    sectionIds.add(section.id);
  });
  const offset = end + 1;
  const bodyLines = lines.slice(offset);
  const tokens = markdown.parse(bodyLines.join('\n'), {});
  const headings = tokens.flatMap((t, i) => t.type === 'heading_open' && t.tag === 'h2' && t.level === 0 && t.map ? [{ token: t, heading: tokens[i + 1].content }] : []);
  if (!headings.length) throw reviewError('page', offset + 1, msg.noItems);
  if (bodyLines.slice(0, headings[0].token.map![0]).join('\n').trim()) throw reviewError('page', offset + 1, msg.preamble);
  const ids = new Set<string>();
  const items = headings.map(({ token, heading }, i): ReviewItem => {
    const start = token.map![1];
    const stop = headings[i + 1]?.token.map?.[0] ?? bodyLines.length;
    const line = offset + token.map![0] + 1;
    const metadataToken = tokens.find((t) => t.type === 'fence' && t.level === 0 && t.info.trim() === 'yaml' && t.map && t.map[0] >= start && t.map[0] < stop);
    if (!metadataToken?.map) throw reviewError(heading, line, msg.metadata);
    const parsed = parseYaml(metadataToken.content, itemSchema, heading, offset + metadataToken.map[0] + 2);
    const metadata = parsed.data;
    if (ids.has(metadata.id)) throw reviewError(metadata.id, line, msg.duplicate(metadata.id));
    ids.add(metadata.id);
    if (!sectionIds.has(metadata.section)) throw reviewError(metadata.id, parsed.lineOf(['section']), msg.unknownSection(metadata.section, [...sectionIds]));
    const judgement = metadata['judgement-call'];
    if (metadata['lower-priority'] && (judgement || metadata.alternatives.length)) throw reviewError(metadata.id, parsed.lineOf(['lower-priority']), msg.lowerPriorityJudgement);
    if (metadata.alternatives.length && !judgement) throw reviewError(metadata.id, parsed.lineOf(['alternatives']), msg.alternativesNeedJudgement);
    if (metadata.alternatives.length && metadata.section === 'side-effects') throw reviewError(metadata.id, parsed.lineOf(['alternatives']), msg.sideEffectAlternatives);
    if (judgement && !metadata.alternatives.length && metadata.section !== 'side-effects' && metadata.section !== 'unverified') throw reviewError(metadata.id, parsed.lineOf(['judgement-call']), msg.judgementNeedsAlternatives);
    // Replace metadata with blank lines, preserving all evidence source coordinates.
    const content = bodyLines.slice(start, stop).map((s, j) => start + j >= metadataToken.map![0] && start + j < metadataToken.map![1] ? '' : s).join('\n');
    const bodyLine = offset + start + 1;
    refuseSecret(findSecrets(heading).at(0), metadata.id, lines, line);
    refuseSecret(findSecrets(metadata.lede).at(0), metadata.id, lines, parsed.lineOf(['lede']));
    if (metadata.nav) refuseSecret(findSecrets(metadata.nav).at(0), metadata.id, lines, parsed.lineOf(['nav']));
    metadata.alternatives.forEach((text, j) => refuseSecret(findSecrets(text).at(0), metadata.id, lines, parsed.lineOf(['alternatives', j])));
    refuseSecret(findSecrets(content).at(0), metadata.id, lines, bodyLine);
    const refs = markdown.parse(content, {}).flatMap((t): Array<LocatedRef> => {
      if (t.type !== 'fence' || t.info.trim() !== 'ref' || !t.map) return [];
      const refLine = bodyLine + t.map[0];
      const ref = parseYaml(t.content, selectRefSchema, metadata.id, refLine + 1).data;
      return [{ ref, line: refLine, bodyLine: t.map[0] }];
    });
    return { metadata, heading, body: content, line, bodyLine, refs };
  });
  page.stats.forEach((stat, i) => {
    if (!ids.has(stat.item)) throw reviewError('page', front.lineOf(['stats', i, 'item']), msg.unknownStatItem(stat.item));
  });
  return { page, items, lines: { asks: page.asks.map((_, i) => front.lineOf(['asks', i])), story: front.lineOf(['story']), foryou: page.foryou.map((_, i) => front.lineOf(['foryou', i])), heading: front.lineOf(['heading']) } };
}
