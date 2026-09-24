import { createHash } from 'node:crypto';
import { realpath } from 'node:fs/promises';
import { basename, dirname, extname, isAbsolute, join, relative, resolve } from 'node:path';
import { parseLocator } from '../lib/locators';
import { review as msg } from '../lib/messages';
import { readCapture } from './capture';
import { FilesystemReader, repositoryPath } from './fs';
import { reviewError } from './parse';
import { SecretTally, redactText, redactValue } from './redact';
import { Transcripts, captureEntry } from './transcript';
import type { GitCommit, GitStatus, GitUpstream, ReviewContext } from './git';
import type { EvidenceRef, LineRange, LocatedRef, ParsedReview, ReviewItem } from './parse';
import type { SecretHits } from './redact';
import type { Entry } from '@alignment-hive/session-data';
import type { Ask, SessionSpan, TranscriptEntry } from './transcript';

/** Symlinked directories (macOS /var) resolve alike; the file itself may no longer exist. */
async function samePath(a: string, b: string): Promise<boolean> {
  const real = async (path: string) => join(await realpath(dirname(path)).catch(() => dirname(path)), basename(path));
  return a === b || await real(a) === await real(b);
}
export function isInside(root: string, path: string): boolean {
  const child = relative(root, path);
  return child === '' || (!child.startsWith('..') && !isAbsolute(child));
}
export function hashEvidence(value: string | Uint8Array): string { return createHash('sha256').update(value).digest('hex'); }
export interface EvidenceIdentity { kind: string; selector: string; oldHash: string | null; newHash: string | null }
export interface DiffSpec {
  path: string; status: 'modified' | 'added' | 'deleted'; old: string; new: string;
  focus?: FocusRanges; oldFocus?: FocusRanges; labels: Array<{ line: number; where: string }>; title?: string;
}
/** One range as a pair, several as a list of pairs: the diff engine accepts both. */
export type FocusRanges = [number, number] | Array<[number, number]>;
export type ResolvedEvidence = EvidenceIdentity & { caption?: string; line: number; bodyLine: number } & (
  | { kind: 'diff'; file: DiffSpec }
  | ({ kind: 'file' } & FileView)
  | { kind: 'transcript'; entries: Array<TranscriptEntry>; clip: boolean; results: boolean }
  | { kind: 'image'; dataUri: string; summary: string; width?: number; takenAt?: string }
  | { kind: 'git'; facts: GitFacts }
);
/** `tree` is live state, present only without an authored head. */
export interface GitFacts { commits: Array<GitCommit>; tree?: { status: GitStatus; upstream: GitUpstream | null } }
export interface ResolvedItem extends ReviewItem { evidence: Array<ResolvedEvidence> }
/** `own` counts the user's own messages; `span` is null without the parent transcript; `warnings` are the author's to fix. */
export interface ReviewHeader { asks: Array<Ask>; own: number; span: SessionSpan | null; warnings: Array<string> }

/** `revision` is a commit, `disk`, or the canonical locator of the Write; `created` only when the Write result says. */
export interface FileView {
  path: string; absolutePath: string; source: 'git' | 'disk' | 'written'; revision: string;
  text: string; startLine: number; endLine: number; created?: boolean;
}
/** When a screenshot was taken, from a Playwright-style UTC stamp in its file name; never the file's mtime, which a copy resets. */
export function stampedAt(path: string): string | undefined {
  const stamp = /(\d{4}-\d{2}-\d{2})T(\d{2})-(\d{2})-(\d{2})(?:-(\d{3}))?Z/.exec(basename(path));
  return stamp ? `${stamp[1]}T${stamp[2]}:${stamp[3]}:${stamp[4]}.${(stamp[5] as string | undefined) ?? '000'}Z` : undefined;
}
const lineTotal = (text: string) => text === '' ? 0 : text.split('\n').length - Number(text.endsWith('\n'));
function fileView(origin: Pick<FileView, 'path' | 'absolutePath' | 'source' | 'revision'>, whole: string, range: LineRange | undefined): EvidenceIdentity & { kind: 'file' } & FileView {
  const lines = lineRange(range, whole);
  const text = lines ? whole.split('\n').slice(lines[0] - 1, lines[1]).join('\n') : whole;
  const endLine = lines?.[1] ?? lineTotal(whole);
  return { kind: 'file', selector: `${origin.path}@${origin.revision}${range ? ':' + range.source : ''}`, oldHash: null, newHash: hashEvidence(text), ...origin, text, startLine: lines?.[0] ?? 1, endLine };
}

function lineRange(range: LineRange | undefined, text: string): [number, number] | undefined {
  if (range === undefined) return undefined;
  const { start, end } = range, total = lineTotal(text);
  if (end > total) throw new Error(msg.rangeOutside(range.source, total));
  return [start, end];
}
function focusRanges(ranges: Array<LineRange> | undefined, text: string): FocusRanges | undefined {
  if (ranges === undefined) return undefined;
  const pairs = ranges.map((range) => lineRange(range, text)!);
  return pairs.length === 1 ? pairs[0] : pairs;
}

export class EvidenceResolver {
  readonly transcripts: Transcripts;
  /** Distinct secrets redacted from evidence so far, by kind; counts only. */
  private tally = new SecretTally();
  get redactions(): SecretHits { return this.tally.hits; }
  /** `projects` is where transcripts live (default ~/.claude/projects). */
  constructor(readonly context: ReviewContext, readonly reviewDir: string, projects?: string) {
    this.transcripts = new Transcripts(context.session, projects);
  }
  /** Evidence loses its secrets right after it is read, before anything hashes it. */
  private clean(text: string): string {
    const { text: out, found } = redactText(text);
    this.tally.add(found);
    return out;
  }
  private cleanValue<T>(value: T): T {
    const { value: out, found } = redactValue(value);
    this.tally.add(found);
    return out;
  }
  /** An entry's text, and a tool call's input and result, redacted; input as one object, so a value keeps the property name that marks it secret. */
  private cleanEntry(e: Entry): Entry {
    switch (e.kind) {
      case 'tool': return { ...e, input: this.cleanValue(e.input), ...(e.result !== undefined && { result: this.clean(e.result) }) };
      case 'user': case 'assistant': case 'thinking': case 'system': return { ...e, text: this.clean(e.text) };
      default: return e;
    }
  }
  /** A file as the session's Write call left it, to the ref's path, given as recorded or relative to the reviewed worktree. */
  private async writtenFile(locator: string, file: string, path: string, absolutePath: string, range: LineRange | undefined) {
    const written = await this.transcripts.written(locator);
    if (written.path !== file && !(!isAbsolute(file) && isAbsolute(written.path) && await samePath(written.path, absolutePath))) throw new Error(msg.writtenPath(locator, written.path, file));
    return { ...fileView({ path, absolutePath, source: 'written', revision: written.locator }, this.clean(written.content), range), created: written.created };
  }
  /** The new side of a diff: the authored head commit, or the working tree. */
  private headText(path: string): Promise<string | null> {
    const context = this.context;
    return context.historical ? context.git.blob(context.headCommit, path) : context.fs.text(path);
  }
  /** The context, for evidence only a repository can supply. */
  private repository(what: string) {
    const context = this.context;
    if (context.git === null) throw new Error(msg.needsGit(what));
    return context;
  }
  async resolve(located: LocatedRef, itemId: string): Promise<ResolvedEvidence> {
    try { return await this.resolveRef(located); }
    catch (error) { throw reviewError(itemId, located.line, error instanceof Error ? error.message : String(error)); }
  }
  private async resolveRef(located: LocatedRef): Promise<ResolvedEvidence> {
    const ref: EvidenceRef = located.ref;
    const common = { caption: ref.caption, line: located.line, bodyLine: located.bodyLine, oldHash: null };
    if ('diff' in ref) {
      const path = repositoryPath(ref.diff);
      const { git, baseCommit } = this.repository('diff');
      const [old, current] = (await Promise.all([git.blob(baseCommit, path), this.headText(path)])).map((text) => text === null ? null : this.clean(text));
      if (old === null && current === null) throw new Error(msg.evidenceMissing(path));
      const focus = focusRanges(ref.focus, current ?? '');
      const oldFocus = focusRanges(ref['old-focus'], old ?? '');
      for (const label of ref.labels) lineRange({ source: `L${label.line}`, start: label.line, end: label.line }, current ?? '');
      return { ...common, kind: 'diff', selector: path, oldHash: old === null ? null : hashEvidence(old), newHash: current === null ? null : hashEvidence(current), file: { path, status: old === null ? 'added' : current === null ? 'deleted' : 'modified', old: old ?? '', new: current ?? '', focus, oldFocus, labels: ref.labels, title: ref.title } };
    }
    if ('git' in ref) {
      const { git, baseCommit, headCommit, historical } = this.repository('git');
      const facts: GitFacts = { commits: this.cleanValue(await git.log(baseCommit, headCommit)) };
      if (!historical) facts.tree = this.cleanValue({ status: await git.status(), upstream: await git.upstream(baseCommit) });
      return { ...common, kind: 'git', selector: 'git', newHash: hashEvidence(JSON.stringify(facts)), facts };
    }
    if ('file' in ref) {
      const external = isAbsolute(ref.file);
      const path = external ? ref.file : repositoryPath(ref.file);
      const absolutePath = external ? path : join(this.context.cwd, path);
      if (ref.written) return { ...common, ...await this.writtenFile(ref.written, ref.file, path, absolutePath, ref.range) };
      // An absolute path outside the repository is always read from disk; `at` is for repository files.
      if (external && (ref.at || isInside(this.context.cwd, await realpath(ref.file).catch(() => ref.file)))) throw new Error(msg.invalidPath(ref.file));
      const at = ref.at ?? 'head', context = this.context;
      const revision = external ? 'disk' : at === 'head' ? (context.historical ? context.headCommit : 'disk') : at === 'base' ? this.repository('at: base').baseCommit : await this.repository(`at: ${at}`).git.commit(at);
      const whole = revision === 'disk' ? await context.fs.text(path) : await this.repository(`at: ${at}`).git.blob(revision, path);
      if (whole === null) throw new Error(msg.evidenceMissing(path));
      return { ...common, ...fileView({ path, absolutePath, source: revision === 'disk' ? 'disk' : 'git', revision }, this.clean(whole), ref.range) };
    }
    if ('image' in ref) {
      const path = resolve(this.reviewDir, ref.image);
      const bytes = await new FilesystemReader(this.reviewDir).bytes(path);
      if (bytes === null) throw new Error(msg.evidenceMissing(path));
      const mime: Record<string, string> = { '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.webp': 'image/webp', '.svg': 'image/svg+xml' };
      const type = mime[extname(path).toLowerCase()];
      if (!type) throw new Error(msg.unsupportedImage(path));
      return { ...common, kind: 'image', selector: path, newHash: hashEvidence(bytes), dataUri: `data:${type};base64,${Buffer.from(bytes).toString('base64')}`, summary: ref.summary, width: ref.width, takenAt: stampedAt(path) };
    }
    const entries: Array<TranscriptEntry> = [];
    for (const [i, locator] of ref.transcript.entries()) {
      const summary = typeof ref.summary === 'string' ? ref.summary : ref.summary[i];
      const entry = locator.startsWith('capture:')
        ? captureEntry(locator, await readCapture(locator.slice('capture:'.length), this.reviewDir), summary)
        : await this.transcripts.entry(locator, summary);
      entries.push({ ...entry, entry: this.cleanEntry(entry.entry) });
    }
    // Identity is the record (uuid, block, tool id), not the entry number or branch state, so neither a renumbering
    // nor a later rewind changes the hash of what the page shows.
    const shown = entries.map(({ transcript, label, role, summary, entry: { n: _n, branch: _b, resumes: _r, ...entry } }) => ({ transcript, label, role, summary, entry }));
    const shows = { clip: ref.clip !== false, results: ref.results !== false };
    return { ...common, kind: 'transcript', selector: entries.map((entry) => entry.locator).join(','), newHash: hashEvidence(JSON.stringify(shown)), entries, ...shows };
  }
  /** The key asks in transcript order, each redacted; an elision's anchors match the message as written. */
  async header(parsed: ParsedReview): Promise<ReviewHeader> {
    const asks: Array<Ask> = [], listed = new Set<string>();
    for (const [i, { locator, elide }] of parsed.page.asks.entries()) {
      try {
        const ask = await this.transcripts.ask(locator);
        if (listed.has(ask.locator)) throw new Error(msg.duplicateAsk(locator));
        listed.add(ask.locator);
        if (!elide) { asks.push({ ...ask, text: this.clean(ask.text) }); continue; }
        const from = ask.text.indexOf(elide.from);
        if (from < 0) throw new Error(msg.elideNotFound(locator, 'from'));
        const until = elide.until === undefined ? ask.text.length : ask.text.indexOf(elide.until, from + elide.from.length);
        if (until < 0) throw new Error(msg.elideNotFound(locator, 'until'));
        const before = this.clean(ask.text.slice(0, from));
        asks.push({ ...ask, text: before + this.clean(ask.text.slice(until)), elision: { at: before.length, note: elide.note } });
      } catch (error) { throw reviewError('page', parsed.lines.asks[i], error instanceof Error ? error.message : String(error)); }
    }
    // Beside the key asks, the user's other messages, redacted and whole, behind the "All" toggle.
    const own = asks.length ? await this.transcripts.prompts() : [];
    const rest = own.filter((ask) => !listed.has(ask.locator)).map((ask): Ask => ({ ...ask, text: this.clean(ask.text), rest: true }));
    const unsure = own.filter((ask) => ask.unsure).length;
    const position = (ask: Ask) => parseLocator(ask.locator).range!.from;
    return { asks: [...asks, ...rest].sort((a, b) => position(a) - position(b)), own: own.length, span: await this.transcripts.span(), warnings: unsure ? [msg.unsureBranch(unsure, own.length)] : [] };
  }
  async items(items: Array<ReviewItem>): Promise<Array<ResolvedItem>> {
    // Resolve once per render; previous-round manifests never invoke readers.
    const result: Array<ResolvedItem> = [];
    for (const item of items) result.push({ ...item, evidence: await Promise.all(item.refs.map((ref) => this.resolve(ref, item.metadata.id))) });
    return result;
  }
}
