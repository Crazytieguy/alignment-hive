// A Bun macro: session-format.ts imports it with `{ type: 'macro' }`, so it runs when the CLI is bundled and the
// compiled binary carries the stamp as a string literal (the binary can't read its own source files).
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** The files whose text decides what an upload's sanitizer redacts: the rules, which fields they're run over, and which known values they match. */
const RULE_SOURCES = ['sanitize.ts', 'secret-rules.ts', 'upload-session.ts', 'known-values.ts'];

/**
 * `gitleaks@<commit>+<hash of the rule source>`. Any edit to these files changes the hash, even a comment: a
 * needless re-sanitize is harmless, a missed one is not. It hashes code only, never the user's env values.
 */
export function sanitizerStamp(): string {
  const sources = RULE_SOURCES.map((name) =>
    readFileSync(fileURLToPath(new URL(name, import.meta.url)), 'utf8').replaceAll('\r\n', '\n'),
  );
  const ref = /gitleaks\/blob\/([0-9a-f]{40})\//.exec(sources[1])?.[1];
  if (!ref) throw new Error('secret-rules.ts names no gitleaks commit in its header');
  const hash = createHash('sha256');
  for (const source of sources) hash.update(source).update('\0');
  return `gitleaks@${ref.slice(0, 12)}+${hash.digest('hex').slice(0, 12)}`;
}
