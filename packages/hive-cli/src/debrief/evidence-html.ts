import { dirname, extname, isAbsolute, resolve } from 'node:path';
import { reviewPageMessages as msg } from '../lib/messages';
import { hashEvidence } from './evidence';
import { escapeHtml, renderMarkdownInline as inline, renderMarkdownFile, renderProse, validateMarkdownImages } from './html';
import { reviewTimes } from './times';
import { isDeniedResult, isUpdatedResult } from './transcript';
import type { DiffSpec, FileView, GitFacts, ResolvedEvidence, ResolvedItem } from './evidence';
import type { FileLink, HtmlContext } from './html';
import type { TranscriptEntry } from './transcript';

/** The renderer's fallback text is UTC; the page reformats it in the viewer's zone. */
export const utcTimes = reviewTimes('UTC');
/** A `<time>` carrying ISO; `text` formats the fallback. */
export function timeHtml(iso: string | undefined, className?: string, text: (iso: string) => string = utcTimes.stamp): string {
  const date = iso === undefined ? undefined : new Date(iso);
  if (!date || Number.isNaN(date.getTime())) return '';
  return `<time${className ? ` class="${className}"` : ''} datetime="${escapeHtml(date.toISOString())}">${escapeHtml(text(date.toISOString()))}</time>`;
}
/** A path with its directory muted. */
function pathHtml(path: string): string {
  const i = path.lastIndexOf('/');
  return i < 0 ? escapeHtml(path) : `<span class="dir">${escapeHtml(path.slice(0, i + 1))}</span>${escapeHtml(path.slice(i + 1))}`;
}
/** Text for a `pre`: the parser drops one leading newline after the tag. */
const preText = (text: string) => (text.startsWith('\n') ? '\n' : '') + escapeHtml(text);
export const lineCount = (text: string) => text.replace(/\n$/, '').split('\n').length;
/** Outputs clip past this many lines; replies and prompts past it or past CLIP_CHARS characters. */
const CLIP_LINES = 30, CLIP_CHARS = 1800;
/** The control that unclips a clipped block. */
function showAllButton(label: string): string {
  const more = escapeHtml(label);
  return `<button type="button" class="ev-more" data-more="${more}" data-less="${escapeHtml(msg.showLess)}">${more}</button>`;
}
/** Output past CLIP_LINES lines is clipped behind a Show all control. */
function clipped(text: string, className: string): string {
  const lines = lineCount(text), clip = lines > CLIP_LINES;
  return `<div class="tr-wrap"><pre class="${className}${clip ? ' clip' : ''}">${preText(text)}</pre>${clip ? showAllButton(msg.showAll(lines)) : ''}</div>`;
}
/** A long reply or prompt, as prose, clipped unless the ref says `clip: false`. */
function clippedProse(text: string, context: HtmlContext, clip: boolean): string {
  const html = renderProse(text, context);
  return clip && (text.length > CLIP_CHARS || lineCount(text) > CLIP_LINES) ? `<div class="tr-wrap"><div class="tr-clipbox clip">${html}</div>${showAllButton(msg.showAllProse)}</div>` : html;
}
const block = (text: string, className: string) => `<div class="tr-wrap"><pre class="${className}">${preText(text)}</pre></div>`;
const chevron = '<span class="git-chev" aria-hidden="true"></span>';

/** The diff engine's LANG_BY_EXT, so a file view and a diff of the same file highlight alike. */
const languageByExtension: Record<string, string> = {
  '.rs': 'rust', '.ts': 'typescript', '.tsx': 'typescript', '.js': 'javascript', '.mjs': 'javascript', '.json': 'json', '.sh': 'bash', '.bash': 'bash', '.zsh': 'bash',
  '.toml': 'ini', '.yml': 'yaml', '.yaml': 'yaml', '.md': 'markdown', '.css': 'css', '.html': 'xml', '.py': 'python', '.sql': 'sql', '.lock': 'ini',
};
function codeHtml(text: string, path: string): string {
  const language = languageByExtension[extname(path).toLowerCase()];
  return `<code${language ? ` class="language-${language}"` : ''}>${escapeHtml(text)}</code>`;
}
const isMarkdown = (path: string) => extname(path).toLowerCase() === '.md';
/** A home directory shown as `~`; the path keeps its case. */
const homePath = (path: string) => path.replace(/^\/(?:Users|home)\/[^/]+(?=\/)/, '~');

/* ---------- transcript entries ---------- */

type Input = Record<string, unknown>;
const str = (value: unknown) => (typeof value === 'string' ? value : undefined);
const num = (value: unknown) => (typeof value === 'number' ? value : undefined);
/** A call's label: `head` in the label's capitals, then `about` as written. */
const labelHtml = (head: string, about?: string) => `<p class="tr-label">${escapeHtml(head)}${about ? `<span class="tr-label-about">${head ? ' &#183; ' : ''}${escapeHtml(about)}</span>` : ''}</p>`;
/** A file path or URL in its own case, a home directory shown as `~`. */
const pathLine = (path: string) => `<p class="tr-path" title="${escapeHtml(path)}">${escapeHtml(homePath(path))}</p>`;
const note = (line: string) => `<p class="tr-meta">${escapeHtml(line)}</p>`;
const codeBlock = (code: string, path: string) => `<div class="tr-wrap"><pre class="tr-cmd${isMarkdown(path) ? ' tr-prose' : ''}">${codeHtml(code, path)}</pre></div>`;
/** A result's first line, at most 200 characters, marked when there is more. */
const firstLine = (result: string) => { const text = result.trim(), line = text.split('\n')[0].slice(0, 200); return line.length < text.length ? `${line}\u2026` : line; };
/** A result's fields when it is a JSON object (TaskStop, SendMessage). */
const jsonFields = (result: string): Input => { try { return (JSON.parse(result) ?? {}) as Input; } catch { return {}; } };
/** A result in one line: its JSON `message`, else its first line. */
const resultNote = (result: string) => firstLine(str(jsonFields(result).message) ?? result);
/** A send to another session reports `“summary” → <its name> (another Claude session …; queued there …)`; older ones named the socket. */
function crossSession(result = ''): { name?: string; queued: boolean } | undefined {
  const { success, message } = jsonFields(result);
  if (success !== true || typeof message !== 'string' || !message.startsWith('\u201c')) return undefined;
  return { name: / \u2192 (.+?) \(another Claude session/.exec(message)?.[1], queued: message.includes('queued there') };
}

/**
 * How one tool's call shows, and its result: an output block unless `result` says prose, or gives the one-line note
 * (undefined: nothing worth a line). A call whose input lacks the view's fields returns undefined and shows as fields;
 * a failed call's result shows whole.
 */
interface ToolView { call: (input: Input, context: HtmlContext, clip: boolean, result?: string) => string | undefined; prose?: boolean; result?: 'prose' | ((result: string) => string | undefined) }
const commandView: ToolView = {
  call: (input) => {
    const command = str(input.command), description = str(input.description);
    return command === undefined ? undefined : `${description ? labelHtml(description) : ''}${block(command, 'tr-cmd')}`;
  },
};
const agent: ToolView = {
  prose: true, result: 'prose',
  // The agent's type as a label; its description as written. Its report, the result, is prose.
  call: (input, context, clip) => {
    const prompt = str(input.prompt), type = str(input.subagent_type), about = str(input.description);
    return prompt === undefined ? undefined : labelHtml(type || (about ? '' : msg.agentPrompt), about) + clippedProse(prompt, context, clip);
  },
};
/** A socket another session listens on reads as that session's name when the result gives it, else by its process; any other recipient as given. */
const recipient = (to: string, result?: string) => { const socket = /^uds:.*?(\d+)\.sock$/.exec(to); return socket ? crossSession(result)?.name ?? msg.anotherSession(socket[1]) : to; };
const VIEWS: Partial<Record<string, ToolView>> = {
  Bash: commandView, Monitor: commandView, Agent: agent, Task: agent,
  Write: {
    call: (input) => {
      const path = str(input.file_path), content = str(input.content);
      return path === undefined || content === undefined ? undefined : pathLine(path) + codeBlock(content, path);
    },
  },
  SendMessage: {
    prose: true,
    call: (input, context, clip, result) => {
      const to = str(input.to) ?? str(input.recipient), message = str(input.message) ?? str(input.content);
      return to === undefined || message === undefined ? undefined : labelHtml(msg.sendTo(recipient(to, result)), str(input.summary)) + clippedProse(message, context, clip);
    },
    // Another session's delivery notice repeats the label; an agent's (queued, resumed) says something.
    result: (result) => { const sent = crossSession(result); return sent ? (sent.queued ? msg.queued : msg.sent) : resultNote(result); },
  },
  Read: {
    call: (input) => {
      const path = str(input.file_path), offset = num(input.offset), limit = num(input.limit), from = offset ?? 1;
      if (path === undefined) return undefined;
      const range = limit !== undefined ? msg.lineRange(from, from + limit - 1) : offset !== undefined ? msg.fromLine(from) : undefined;
      return pathLine(path) + (range ? note(range) : '');
    },
  },
  Edit: {
    result: (result) => (isUpdatedResult(result) ? undefined : resultNote(result)),
    call: (input) => {
      const path = str(input.file_path), before = str(input.old_string), after = str(input.new_string);
      if (path === undefined || before === undefined || after === undefined) return undefined;
      return `${pathLine(path)}${input.replace_all === true ? note(msg.everyOccurrence) : ''}${labelHtml(msg.replaced)}${codeBlock(before, path)}${labelHtml(msg.replacedWith)}${codeBlock(after, path)}`;
    },
  },
  WebFetch: {
    prose: true, result: 'prose',
    call: (input, context, clip) => {
      const url = str(input.url), prompt = str(input.prompt);
      return url === undefined ? undefined : pathLine(url) + (prompt ? labelHtml(msg.asked) + clippedProse(prompt, context, clip) : '');
    },
  },
  TodoWrite: {
    result: () => undefined,
    call: (input) => {
      if (!Array.isArray(input.todos)) return undefined;
      const marks = new Map([['completed', ['&#10003;', msg.todo.done]], ['in_progress', ['&#9656;', msg.todo.doing]]]);
      const items = input.todos.map((todo) => {
        const { content, status } = (todo ?? {}) as Input;
        const [mark, word] = marks.get(String(status)) ?? ['&#9675;', msg.todo.open];
        return `<li><span class="tr-todo" title="${word}">${mark}</span> ${escapeHtml(String(content ?? ''))}</li>`;
      });
      return `<ul class="tr-todos">${items.join('')}</ul>`;
    },
  },
  AskUserQuestion: {
    prose: true, result: 'prose',
    call: (input, context, clip) => {
      if (!Array.isArray(input.questions)) return undefined;
      const asked = input.questions.map((q) => {
        const { question, header, options } = (q ?? {}) as Input;
        const choices = Array.isArray(options) ? options.map((o) => { const { label, description } = (o ?? {}) as Input; return `- **${String(label ?? '')}**${description ? `: ${String(description)}` : ''}`; }) : [];
        return [`${header ? `**${String(header)}**: ` : ''}${String(question ?? '')}`, ...choices].join('\n');
      });
      return clippedProse(asked.join('\n\n'), context, clip);
    },
  },
  ExitPlanMode: { prose: true, result: resultNote, call: (input, context, clip) => { const plan = str(input.plan); return plan === undefined ? undefined : clippedProse(plan, context, clip); } },
  Skill: { result: (result) => (result.startsWith('Launching skill: ') ? undefined : resultNote(result)), call: (input) => { const skill = str(input.skill); return skill === undefined ? undefined : labelHtml(skill, str(input.args)); } },
  TaskStop: { result: resultNote, call: fieldsHtml },
  ToolSearch: {
    call: fieldsHtml,
    // What loaded, by name: the parser writes each tool_reference block as `[tool_reference: Name]`.
    result: (result) => { const names = [...result.matchAll(/\[tool_reference: ([^\]]+)\]/g)].map((m) => m[1]); return names.length ? msg.loaded(names.join(', ')) : firstLine(result); },
  },
};
/** A call as its fields: a value that fits one line of 120 characters inline (a home directory as `~`); a longer one as a block, a string wrapped and anything else as JSON. */
function fieldsHtml(input: Input): string {
  return Object.entries(input).map(([key, value]) => {
    const shown = typeof value === 'string' ? value : JSON.stringify(value, null, 2);
    if (shown.length <= 120 && !shown.includes('\n')) return `<p class="tr-field"><span class="tr-key">${escapeHtml(key)}</span><code>${escapeHtml(homePath(shown))}</code></p>`;
    return labelHtml(key) + clipped(shown, typeof value === 'string' ? 'tr-out tr-wrap-lines' : 'tr-out');
  }).join('');
}
/** A tool call and its result share one block, as the session viewer pairs them; text entries are prose. */
function entryBody({ entry: e }: TranscriptEntry, context: HtmlContext, shows: { clip: boolean; results: boolean }): string {
  switch (e.kind) {
    case 'tool': {
      const view = VIEWS[e.tool], call = view?.call(e.input, context, shows.clip, e.result);
      const shown = call === undefined ? undefined : view, mode = e.error ? undefined : shown?.result;
      let result = '';
      if (shows.results && e.result !== undefined) {
        if (isDeniedResult(e.result)) result = `<p class="tr-label tr-denied">${msg.denied}</p>${clipped(e.result, 'tr-out tr-wrap-lines')}`;
        else if (typeof mode === 'function') { const line = mode(e.result); result = line === undefined ? '' : note(line || msg.noOutput); }
        else {
          // A failed call of a view that shows its result as a note or prose is a message, so it wraps.
          const out = e.result || msg.noOutput;
          result = labelHtml(msg.result) + (mode === 'prose' ? clippedProse(out, context, shows.clip) : clipped(out, e.error && shown?.result ? 'tr-out tr-wrap-lines' : 'tr-out'));
        }
      }
      return `<div class="tr-body${shown?.prose ? ' prose' : ''}">${call ?? fieldsHtml(e.input)}${result}</div>`;
    }
    case 'user': case 'assistant': case 'thinking': case 'system': return `<div class="tr-body prose">${clippedProse(e.text, context, shows.clip)}</div>`;
    default: return '';
  }
}
/** `key` names what the fold shows (see dataKey); the page files its saved state under it. */
function entryRow(key: string, role: string, label: { label: string; title?: string }, summary: string, time: string, body: string): string {
  const title = label.title ? ` title="${escapeHtml(label.title)}"` : '';
  return `<details class="tr-entry" data-key="${escapeHtml(key)}"><summary><span class="tr-n"><span class="tr-chevron" aria-hidden="true">&#9656;</span></span><span class="tr-role" data-role="${role}"${title}>${escapeHtml(label.label)}</span><span class="tr-sum">${summary}</span>${time}</summary>${body}</details>`;
}
function transcriptHtml(evidence: Extract<ResolvedEvidence, { kind: 'transcript' }>, context: HtmlContext): string {
  return `<div class="ev-box tr">${evidence.entries.map((entry) => entryRow(dataKey.entry(entry.locator), entry.role, entry, inline(entry.summary, context), timeHtml(entry.entry.time, 'tr-time'), entryBody(entry, context, evidence))).join('')}</div>`;
}
/* ---------- file views ---------- */

/** File views keyed by absolute path and revision, so one path shown at two revisions or sources never collides. */
export class FileAnchors {
  private anchors: Array<{ absolutePath: string; revision: string; id: string }> = [];
  private placed = new Set<string>();
  constructor(items: Array<ResolvedItem>) {
    for (const evidence of items.flatMap((item) => item.evidence)) {
      if (evidence.kind === 'file' && !this.find(evidence)) this.anchors.push({ absolutePath: evidence.absolutePath, revision: evidence.revision, id: `file-${hashEvidence(`${evidence.absolutePath}\0${evidence.revision}`).slice(0, 16)}` });
    }
  }
  private find(view: FileView) { return this.anchors.find((anchor) => anchor.absolutePath === view.absolutePath && anchor.revision === view.revision); }
  /** The anchor id for the first view of a file at a revision; later views of it carry none. */
  place(view: FileView): string | undefined {
    const id = this.find(view)?.id;
    if (!id || this.placed.has(id)) return undefined;
    this.placed.add(id);
    return id;
  }
  /** Links from `view` prefer the target at the same revision, else its first view on the page. */
  link(view: FileView): FileLink {
    return (relativePath) => {
      const target = resolve(dirname(view.absolutePath), relativePath);
      const shown = this.anchors.filter((anchor) => anchor.absolutePath === target);
      return (shown.find((anchor) => anchor.revision === view.revision) ?? shown.at(0))?.id;
    };
  }
}
/** A path outside the repository shows its last two segments; the title keeps it whole. */
function fileHead(path: string, chips: string, extra = ''): string {
  const shown = isAbsolute(path) ? path.split('/').slice(-2).join('/') : path;
  return `<summary class="fv-head">${chevron}<span class="fv-path" title="${escapeHtml(path)}">${pathHtml(shown)}</span>${chips}${extra}</summary>`;
}
function fileHtml(view: FileView & { selector: string }, context: HtmlContext, link: FileLink): string {
  const markdownFile = isMarkdown(view.path);
  const numbers = Array.from({ length: Math.max(0, view.endLine - view.startLine + 1) }, (_, i) => view.startLine + i).join('\n');
  const rendered = markdownFile ? `<div class="fv-md">${renderMarkdownFile(view.text, context, view.startLine, link)}</div>` : '';
  const source = `<div class="fv-src"${markdownFile ? ' hidden' : ''}><pre class="fv-ln">${numbers}</pre><pre>${codeHtml(view.text.replace(/\n$/, ''), view.path)}</pre></div>`;
  const chips = `<span class="fv-chips">${view.created ? `<span class="fv-chip">${msg.newFile}</span>` : ''}${view.endLine ? `<span class="fv-chip">${escapeHtml(msg.lineRange(view.startLine, view.endLine))}</span>` : ''}</span>`;
  return `<details class="ev-box fv" data-key="${escapeHtml(dataKey.file(view.selector))}">${fileHead(view.path, chips, markdownFile ? `<button type="button" class="fv-toggle" data-alt="${msg.viewRendered}">${msg.viewSource}</button>` : '')}${rendered}${source}</details>`;
}
/** An image fold, open from the start since a picture is read at a glance: its kind, what it shows, and when it was taken; the image zooms when wider than its column. */
function imageHtml(evidence: Extract<ResolvedEvidence, { kind: 'image' }>, context: HtmlContext): string {
  const time = timeHtml(evidence.takenAt, 'shot-time');
  const width = evidence.width ? ` width="${evidence.width}"` : '';
  return `<details class="ev-box fv shot" open data-key="${escapeHtml(dataKey.image(evidence.selector))}"><summary class="fv-head">${chevron}<span class="shot-kind">${msg.imageKind}</span><span class="fv-path shot-sum">${inline(evidence.summary, context)}</span>${time ? `<span class="fv-chips">${time}</span>` : ''}</summary><div class="shot-body"><img src="${escapeHtml(evidence.dataUri)}" alt="${escapeHtml(evidence.summary)}"${width} tabindex="-1"></div></details>`;
}

/* ---------- git card ---------- */

/** Commit messages are hard-wrapped; each paragraph reflows, while lists and `Key: value` trailers keep their lines. */
export function reflowMessage(body: string): string {
  return body.split(/\n\n+/).map((paragraph) => {
    const lines = paragraph.split('\n');
    return lines.every((line) => /^(\s*[-*]\s|\s*\d+\.\s|[A-Za-z-]+:\s)/.test(line)) ? paragraph : lines.join(' ').replace(/\s+/g, ' ');
  }).join('\n\n');
}
const plus = (n: number) => `<span class="p">+${n}</span>`;
const minus = (n: number) => `<span class="${n ? 'm' : 'zero'}">&#8722;${n}</span>`;
function gitHtml(facts: GitFacts): string {
  const commits = facts.commits.map((commit) => {
    const files = commit.files.map((file) => {
      const chip = file.status === 'added' ? msg.newFile : file.status === 'deleted' ? msg.deletedFile : '';
      const binary = file.added === null || file.deleted === null;
      return `<li><span>${pathHtml(file.path)}${chip ? ` <span class="fv-chip">${chip}</span>` : ''}</span><span class="git-counts">${binary ? '' : plus(file.added!)}</span><span class="git-counts">${binary ? '' : minus(file.deleted!)}</span></li>`;
    }).join('');
    const message = commit.body ? `<div class="git-msg"><p class="git-msg-l">${msg.commitMessage}</p><blockquote>${escapeHtml(reflowMessage(commit.body))}</blockquote></div>` : '';
    return `<details class="git-commit" data-key="${escapeHtml(dataKey.commit(commit.hash))}"><summary class="git-commit-h">${chevron}<span class="git-hash">${escapeHtml(commit.hash)}</span><span class="git-subject">${escapeHtml(commit.subject)}</span><span class="git-counts">${escapeHtml(msg.files(commit.files.length))} ${plus(commit.added)} ${minus(commit.deleted)}</span></summary>${message}<ul class="git-files">${files}</ul></details>`;
  }).join('');
  let state = '';
  if (facts.tree) {
    const { status, upstream } = facts.tree;
    const counts = (['modified', 'staged', 'untracked', 'deleted'] as const).filter((kind) => status[kind].length).map((kind) => `${status[kind].length} ${msg.treeCounts[kind]}`);
    state = `<div class="git-state"><p>${escapeHtml(`${msg.treeState(counts)} ${msg.upstream(upstream)}`)}</p></div>`;
  }
  return `<div class="git">${commits}${state}</div>`;
}

/* ---------- one figure per ref ---------- */

/**
 * What each fold shows, as a `data-key` the page files saved UI state under (with its item, and an occurrence
 * number when one item shows the same source twice), so state follows the evidence when edits reorder it.
 */
export const dataKey = {
  entry: (locator: string) => `tr:${locator}`,
  file: (selector: string) => `file:${selector}`,
  image: (path: string) => `img:${path}`,
  commit: (hash: string) => `commit:${hash}`,
  ask: (locator: string) => `ask:${locator}`,
};

/** `stateKey` is the item plus the evidence it shows, never its position, so saved diff state survives reordering. */
export interface MountedDiff extends DiffSpec { mountId: string; scope: string; stateKey: string }
export interface PageState { diffs: Array<MountedDiff>; scope: string; files: FileAnchors }
export function evidenceHtml(evidence: ResolvedEvidence, itemId: string, { diffs, scope, files }: PageState): string {
  const context = { itemId, line: evidence.line };
  const caption = evidence.caption ? `<figcaption class="ev-cap">${inline(evidence.caption, context)}</figcaption>` : '';
  const figure = (kind: string, body: string, id?: string) => `<figure class="ev ev-${kind}"${id ? ` id="${id}"` : ''}>${caption}${body}</figure>`;
  switch (evidence.kind) {
    case 'diff': {
      if (isMarkdown(evidence.file.path)) {
        validateMarkdownImages(evidence.file.old, context);
        validateMarkdownImages(evidence.file.new, context);
      }
      const id = `diff-${diffs.length}`;
      diffs.push({ ...evidence.file, mountId: id, scope, stateKey: [itemId, 'diff', evidence.file.path, JSON.stringify(evidence.file.focus ?? null), JSON.stringify(evidence.file.oldFocus ?? null)].join('|') });
      return figure('diff', `<div id="${id}" data-review-diff></div>`);
    }
    case 'file': return figure('file', fileHtml(evidence, context, files.link(evidence)), files.place(evidence));
    case 'git': return figure('git', gitHtml(evidence.facts));
    case 'image': return figure('image', imageHtml(evidence, context));
    default: return figure('transcript', transcriptHtml(evidence, context));
  }
}
