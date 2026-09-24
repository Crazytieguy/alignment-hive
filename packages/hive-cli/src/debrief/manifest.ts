import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { z } from 'zod';
import { version } from '../../package.json';
import { reviewRoundMessages as msg } from '../lib/messages';
import { hashEvidence } from './evidence';
import { pageSchema, reviewError, sessionIdSchema, itemIdSchema as slug } from './parse';
import type { ResolvedItem } from './evidence';
import type { PreparedReview } from './render';

const hash = z.string().regex(/^[a-f0-9]{64}$/);
const count = z.number().int().nonnegative();
const commit = z.string().regex(/^[a-f0-9]{40}(?:[a-f0-9]{24})?$/);
export const manifestSchema = z.strictObject({
  reviewId: z.string().uuid(), round: z.number().int().positive(), session: sessionIdSchema,
  /** Null when the debrief was rendered outside a git repository. */
  baseCommit: commit.nullable(), headCommit: commit.nullable(),
  items: z.array(z.strictObject({
    id: slug, was: slug.optional(), heading: z.string(), hash,
    evidence: z.array(z.strictObject({ kind: z.string(), selector: z.string(), oldHash: hash.nullable(), newHash: hash.nullable() })),
  })),
  dispositions: pageSchema.shape.dispositions,
  history: z.record(z.string().regex(/^[1-9]\d*$/), z.record(slug, hash)),
  renderedAt: z.iso.datetime(), rendererVersion: z.string().min(1),
  /** Changed lines and files since base that items show; absent outside a git repository. */
  coverage: z.strictObject({ lines: count, changedLines: count, files: count, changedFiles: count }).optional(),
}).superRefine((manifest, context) => {
  if (new Set(manifest.items.map((item) => item.id)).size !== manifest.items.length) context.addIssue({ code: 'custom', message: msg.duplicateIds, path: ['items'] });
  if (Object.keys(manifest.history).length !== manifest.round - 1 || Object.keys(manifest.history).some((round) => Number(round) >= manifest.round)) context.addIssue({ code: 'custom', message: msg.invalidHistory, path: ['history'] });
});
export type ReviewManifest = z.infer<typeof manifestSchema>;
export type ManifestItem = ReviewManifest['items'][number];

/** Stable object-key ordering; array order remains meaningful to the authored review. */
export function canonicalJson(value: unknown): string {
  const order = (entry: unknown): unknown => {
    if (Array.isArray(entry)) return entry.map(order);
    if (entry !== null && typeof entry === 'object') return Object.fromEntries(Object.entries(entry).filter(([, v]) => v !== undefined).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([key, v]) => [key, order(v)]));
    return entry;
  };
  return JSON.stringify(order(value));
}
export function manifestItem(item: ResolvedItem): ManifestItem {
  const evidence: ManifestItem['evidence'] = item.evidence.map(({ kind, selector, oldHash, newHash }) => ({ kind, selector, oldHash, newHash }));
  // Source selectors are already in body. Resolved selectors may have round-local
  // absolute image paths; identity is the content actually disclosed, not its storage location.
  const resolvedHashes = evidence.map(({ kind, oldHash, newHash }) => ({ kind, oldHash, newHash }));
  return { id: item.metadata.id, ...(item.metadata.was ? { was: item.metadata.was } : {}), heading: item.heading, hash: hashEvidence(canonicalJson({ heading: item.heading, metadata: item.metadata, body: item.body, evidence: resolvedHashes })), evidence };
}

export function buildManifest(prepared: PreparedReview, options: { previous?: ReviewManifest; reviewId?: string; coverage?: ReviewManifest['coverage'] } = {}): ReviewManifest {
  const page = prepared.parsed.page, previous = options.previous;
  if (page.round === 1 && (previous || Object.keys(page.dispositions).length || prepared.items.some((item) => item.metadata.was))) throw reviewError('page', 1, msg.unexpectedPrevious);
  if (page.round > 1 && !previous) throw reviewError('page', 1, msg.previousRequired);
  if (previous && (previous.session !== page.session || previous.round !== page.round - 1)) throw reviewError('page', 1, msg.wrongPrevious);
  if (previous && options.reviewId && previous.reviewId !== options.reviewId) throw reviewError('page', 1, msg.outputConflict);
  const history = previous ? { ...previous.history, [previous.round]: Object.fromEntries(previous.items.map((item) => [item.id, item.hash])) } : {};
  return manifestSchema.parse({ reviewId: previous?.reviewId ?? options.reviewId ?? randomUUID(), round: page.round, session: prepared.context.session, baseCommit: prepared.context.baseCommit, headCommit: prepared.context.headCommit, items: prepared.items.map(manifestItem), dispositions: page.dispositions, history, renderedAt: new Date().toISOString(), rendererVersion: version, coverage: options.coverage });
}

export function parseManifest(text: string, path: string): ReviewManifest {
  try { return manifestSchema.parse(JSON.parse(text)); }
  catch (error) { throw reviewError('page', 1, msg.invalidManifest(path, error instanceof Error ? error.message : String(error))); }
}
export async function readManifest(path: string): Promise<ReviewManifest> {
  let text: string;
  try { text = await readFile(path, 'utf8'); }
  catch (error) { throw reviewError('page', 1, msg.invalidManifest(path, error instanceof Error ? error.message : String(error))); }
  return parseManifest(text, path);
}
