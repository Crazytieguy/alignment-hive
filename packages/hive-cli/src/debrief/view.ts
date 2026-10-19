// What a reader of a debrief meets, as data for the reader check: what they knew going in (the READMEs as of the
// session's start and their own messages), the top of the page, and the items in page order, each as what shows
// collapsed and its body as pieces, folded evidence reduced to its label. The parser has already refused secrets in
// authored text; the READMEs and messages are redacted here as the page's evidence is.
import { posix } from 'node:path';
import { review as msg, reviewPageMessages as pageMsg } from '../lib/messages';
import { markdown, reviewError } from './parse';
import { openReview, pageSections } from './render';
import { redactText } from './redact';
import { Transcripts } from './transcript';
import type { EvidenceRef, ReviewItem } from './parse';
import type { ReviewContext } from './git';

export interface ReaderItem {
  id: string; section: string; heading: string;
  /** What shows beside the heading while collapsed; a lower-priority item's lede opens its body instead. */
  lede?: string;
  isJudgementCall: boolean; isLowerPriority: boolean;
  pieces: Array<string>;
}
export interface ReaderView {
  readmes: Array<{ path: string; text: string }>;
  messages: Array<string>;
  top: { title: string; heading: string; story: string; stats: Array<string>; foryou: Array<string> };
  items: Array<ReaderItem>;
}

function refLabel(ref: EvidenceRef): string {
  const caption = ref.caption ? ` (${ref.caption})` : '';
  if ('image' in ref) return `[Image: ${ref.summary}${caption}]`;
  if ('git' in ref) return `[Folded evidence: commits and tree state${caption}]`;
  if ('transcript' in ref) return `[Folded evidence: ${[ref.summary].flat().join(' / ')}${caption}]`;
  if ('diff' in ref) return `[Folded diff: ${ref.title ?? ref.diff}${caption}]`;
  return `[Folded file: ${ref.file}${caption}]`;
}

/**
 * An item's body as the page lays it out, block by block: each ref fence, at any depth, stands as its label, and
 * the alternatives come before the first evidence, as the renderer places them.
 */
function pieces(item: ReviewItem): Array<string> {
  const lines = item.body.split('\n');
  const fences = markdown.parse(item.body, {}).filter((t) => t.type === 'fence' && t.info.trim() === 'ref' && t.map);
  for (const fence of fences.reverse()) {
    const [start, end] = fence.map!;
    const located = item.refs.find((ref) => ref.bodyLine === start);
    if (!located) throw reviewError(item.metadata.id, item.bodyLine + start, msg.evidenceMissing('ref'));
    lines.splice(start, end - start, `${/^\s*/.exec(lines[start])![0]}${refLabel(located.ref)}`);
  }
  const text = lines.join('\n');
  const blocks = markdown.parse(text, {}).filter((t) => t.level === 0 && t.map).map((t) => lines.slice(t.map![0], t.map![1]).join('\n').trim()).filter(Boolean);
  const firstEvidence = blocks.findIndex((block) => /^\[(?:Folded|Image)/.test(block));
  const split = firstEvidence < 0 ? blocks.length : firstEvidence;
  const alternatives = item.metadata.alternatives.map((alternative) => `${pageMsg.alternative}: ${alternative}`);
  return [...blocks.slice(0, split), ...alternatives, ...blocks.slice(split)];
}

/** The root README and the README nearest above each changed file, as of base; without git, the root README on disk. */
async function readmes(context: ReviewContext): Promise<Array<{ path: string; text: string }>> {
  if (context.git === null) {
    const text = await context.fs.text('README.md');
    return text === null ? [] : [{ path: 'README.md', text }];
  }
  const { git, baseCommit, headCommit, historical } = context;
  const atBase = new Set((await git.files(baseCommit)).filter((path) => posix.basename(path) === 'README.md'));
  const wanted = new Set(['README.md']);
  for (const file of await git.changedFiles(baseCommit, historical ? headCommit : null)) {
    for (let dir = posix.dirname(file.path); dir !== '.'; dir = posix.dirname(dir)) {
      if (atBase.has(`${dir}/README.md`)) { wanted.add(`${dir}/README.md`); break; }
    }
  }
  const found = await Promise.all([...wanted].map(async (path) => ({ path, text: await git.blob(baseCommit, path) })));
  return found.filter((readme): readme is { path: string; text: string } => readme.text !== null);
}

export async function readerView(inputPath: string, options: { cwd?: string; projects?: string } = {}): Promise<ReaderView> {
  const { parsed: { page, items }, context } = await openReview(inputPath, options);
  const clean = (text: string) => redactText(text).text;
  return {
    readmes: (await readmes(context)).map(({ path, text }) => ({ path, text: clean(text) })),
    messages: (await new Transcripts(page.session, options.projects).prompts()).map((m) => clean(m.text)),
    top: { title: page.title, heading: page.heading, story: page.story, stats: page.stats.map((s) => `${s.n} ${s.label}`), foryou: page.foryou },
    items: pageSections(page, items).flatMap(({ title, main, lower }) => [...main, ...lower].map((item): ReaderItem => {
      const m = item.metadata, quiet = m['lower-priority'], body = pieces(item);
      return {
        id: m.id, section: title, heading: item.heading, ...(quiet ? {} : { lede: m.lede }),
        isJudgementCall: m['judgement-call'] && m.alternatives.length > 0, isLowerPriority: quiet,
        pieces: quiet ? [m.lede, ...body] : body,
      };
    })),
  };
}
