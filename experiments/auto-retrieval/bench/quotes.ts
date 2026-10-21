// The quotes an answer cites, as the main agent would read them.
const LOC = /\b([0-9a-f]{8}(?:\/(?:wf_[0-9a-z_-]+\/)?agent-[0-9a-z_-]+)?):(\d+)(?:-\d+)?\b/g;
// A commit cited where a loc would go: (<sha>, <date>) or (<sha>:<path>, <date>); a path never starts with a digit,
// which keeps locs out. A session id cited without an entry number looks the same: callers check it is a commit.
const COMMIT = /\(([0-9a-f]{7,40})(?::([^0-9\s,()][^\s,()]*))?(?:,[^()]*)?\)/g;

/** NONE, possibly fenced or followed by a period: a run that found nothing. */
export const isNone = (text: string): boolean => /^\W*NONE\W*$/.test(text.trim());

/** loc: a transcript loc, or `git:<sha>` / `git:<sha>:<path>` for a commit cite. */
export type Quote = { loc: string; text: string };

/**
 * The quotes an answer cites: each loc with the blockquote lines that end at it (or, cited outside a blockquote,
 * its own line), minus the source label and the trailing (loc, date). A loc cited twice counts once, unless
 * isEveryCitation. With isCommits, commit cites count too.
 */
export function quotesOf(answer: string, isEveryCitation = false, isCommits = false): Array<Quote> {
  const out: Array<Quote> = [];
  let block: Array<string> = [];
  for (const line of answer.split('\n')) {
    const isQuote = /^\s*>/.test(line);
    if (isQuote) block.push(line.replace(/^\s*>\s?/, ''));
    const locs = [...line.matchAll(LOC)].map((m) => `${m[1]}:${m[2]}`);
    if (isCommits) locs.push(...[...line.matchAll(COMMIT)].map((m) => `git:${m[1]}${m[2] ? `:${m[2]}` : ''}`));
    if (locs.length) {
      const body = (isQuote ? block.join(' ') : line).replace(/^\s*\[[a-z]+[^\]]*\]\s*/i, '');
      const text = body.replace(/\s*\(([^()]*\b[0-9a-f]{7,}[^()]*)\)\s*$/, '').trim();
      for (const loc of locs) if (isEveryCitation || !out.some((q) => q.loc === loc)) out.push({ loc, text });
      block = [];
    } else if (!isQuote) block = [];
  }
  return out;
}
