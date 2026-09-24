import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { runCommand } from '../lib/spawn';
import { getStateDir, readStateFile, statePaths } from '../lib/config';
import { review as msg } from '../lib/messages';
import { FilesystemReader, decodeText, repositoryPath } from './fs';
import { sessionIdSchema } from './parse';

/** Line counts are null for binary files, which numstat reports as `-`. */
export interface GitFile { path: string; status: 'added' | 'modified' | 'deleted'; added: number | null; deleted: number | null }
export interface GitCommit {
  hash: string; subject: string; body: string; files: Array<GitFile>; added: number; deleted: number;
}
/** A file that differs between base and head; counts are null for binary files. */
export type ChangedFile = Omit<GitFile, 'status'>;
export interface GitUpstream { name: string; unpushed: number }
export interface GitStatus {
  modified: Array<string>; staged: Array<string>; untracked: Array<string>; deleted: Array<string>;
}

/** `--numstat -z` records without renames: `added TAB deleted TAB path NUL`, with `-` counts for binary files. */
function parseNumstat(output: string): Array<ChangedFile> {
  return output.split('\0').filter(Boolean).map((record) => {
    const [added, deleted] = record.split('\t', 2);
    const path = record.slice(added.length + deleted.length + 2);
    return { path, added: added === '-' ? null : Number(added), deleted: deleted === '-' ? null : Number(deleted) };
  });
}
/** Added lines of a new file as numstat counts them: null for binary content. */
function newFileLines(bytes: Uint8Array): number | null {
  if (bytes.includes(0)) return null;
  let lines = 0;
  for (const byte of bytes) if (byte === 10) lines++;
  return bytes.length && bytes[bytes.length - 1] !== 10 ? lines + 1 : lines;
}

const gitEnv = () => ({ ...process.env, GIT_OPTIONAL_LOCKS: '0', GIT_NO_REPLACE_OBJECTS: '1' });

/** All arguments are passed directly, never through a shell. Missing blobs alone return null. */
export class GitReader {
  constructor(readonly cwd: string) {}
  async run(args: Array<string>): Promise<Uint8Array> {
    const { stdout, stderr, exit } = await runCommand(['git', '-C', this.cwd, ...args], { env: gitEnv() });
    if (exit !== 0) throw new Error(msg.gitFailed(stderr.toString().trim()));
    return stdout;
  }
  async commit(revision: string): Promise<string> {
    return new TextDecoder().decode(await this.run(['rev-parse', '--verify', '--end-of-options', `${revision}^{commit}`])).trim();
  }
  async blob(commit: string, path: string): Promise<string | null> {
    const clean = repositoryPath(path);
    const oid = await this.commit(commit);
    const entries = new TextDecoder().decode(await this.run(['ls-tree', '-z', oid, '--', clean])).split('\0');
    const record = entries.find((entry) => entry.slice(entry.indexOf('\t') + 1) === clean);
    if (!record) return null;
    const [mode, type, hash] = record.slice(0, record.indexOf('\t')).split(' ');
    if (type !== 'blob' || (mode !== '100644' && mode !== '100755')) throw new Error(msg.nonFile(path));
    return decodeText(await this.run(['cat-file', 'blob', hash]), path);
  }
  async log(base: string, head = 'HEAD'): Promise<Array<GitCommit>> {
    const [baseId, headId] = await Promise.all([this.commit(base), this.commit(head)]);
    // Oldest first, so the card reads in the order the work happened.
    const output = new TextDecoder().decode(await this.run(['log', '--reverse', '--format=%h%x00%s%x00%b%x00', `${baseId}..${headId}`, '--']));
    const parts = output.split('\0'), commits: Array<GitCommit> = [];
    const statuses: Record<string, GitFile['status']> = { A: 'added', M: 'modified', D: 'deleted', T: 'modified' };
    for (let i = 0; i + 2 < parts.length; i += 3) {
      const hash = parts[i].trim();
      const show = async (format: string) => new TextDecoder().decode(await this.run(['show', '--format=', format, '-z', '--first-parent', '--no-renames', hash, '--']));
      const [numstat, names] = await Promise.all([show('--numstat'), show('--name-status').then((text) => text.split('\0'))]);
      const status = new Map<string, GitFile['status']>();
      for (let j = 0; j + 1 < names.length; j += 2) status.set(names[j + 1], statuses[names[j]] ?? 'modified');
      const commit: GitCommit = { hash, subject: parts[i + 1], body: parts[i + 2].trimEnd(), files: [], added: 0, deleted: 0 };
      for (const counts of parseNumstat(numstat)) {
        const file: GitFile = { ...counts, status: status.get(counts.path) ?? 'modified' };
        commit.files.push(file);
        commit.added += file.added ?? 0;
        commit.deleted += file.deleted ?? 0;
      }
      commits.push(commit);
    }
    return commits;
  }
  /** Every file that differs from `base` in `head`, or in the working tree with untracked files when `head` is null. */
  async changedFiles(base: string, head: string | null): Promise<Array<ChangedFile>> {
    const files = parseNumstat(new TextDecoder().decode(await this.run(['diff', '--numstat', '-z', '--no-renames', base, ...(head ? [head] : []), '--'])));
    if (head) return files;
    const untracked = new TextDecoder().decode(await this.run(['ls-files', '-z', '--others', '--exclude-standard'])).split('\0').filter(Boolean);
    for (const path of untracked) {
      const added = await readFile(join(this.cwd, path)).then(newFileLines, () => null);
      files.push({ path, added, deleted: added === null ? null : 0 });
    }
    return files;
  }
  /** The branch's upstream and how many commits since base are not on it; null without an upstream. */
  async upstream(base: string): Promise<GitUpstream | null> {
    let name: string;
    try { name = new TextDecoder().decode(await this.run(['rev-parse', '--abbrev-ref', '--symbolic-full-name', '@{upstream}'])).trim(); }
    catch { return null; }
    const count = new TextDecoder().decode(await this.run(['rev-list', '--count', 'HEAD', `^${await this.commit(base)}`, '^@{upstream}', '--'])).trim();
    return { name, unpushed: Number(count) };
  }
  async status(): Promise<GitStatus> {
    const output = new TextDecoder().decode(await this.run(['status', '--porcelain=v1', '-z', '--untracked-files=all']));
    const records = output.split('\0');
    const status: GitStatus = { modified: [], staged: [], untracked: [], deleted: [] };
    for (let i = 0; i < records.length; i++) {
      const record = records[i];
      if (!record) continue;
      const code = record.slice(0, 2), path = record.slice(3);
      if (code === '??') { status.untracked.push(path); continue; }
      if (code[0] !== ' ' && code[0] !== '?') status.staged.push(path);
      if (code[1] !== ' ' && code[1] !== 'D') status.modified.push(path);
      if (code.includes('D')) status.deleted.push(path);
      if (/[RC]/.test(code)) i++; // Skip the rename/copy source record.
    }
    return status;
  }
}

export interface ReviewContextOptions { session: string; base?: string; head?: string; cwd?: string }
interface ContextBase { cwd: string; session: string; fs: FilesystemReader }
/**
 * Git is optional: outside a repository only diff and git evidence, `at:` revisions and `base`/`head` are unavailable.
 * `historical` means an authored head commit replaces the working tree as the new side.
 */
export type ReviewContext = ContextBase & (
  | { git: GitReader; baseCommit: string; headCommit: string; historical: boolean }
  | { git: null; baseCommit: null; headCommit: null; historical: false }
);

/** The repository holding `cwd`, or null outside one or when git is not installed. */
export async function repositoryRoot(cwd: string): Promise<string | null> {
  let result: Awaited<ReturnType<typeof runCommand>>;
  try { result = await runCommand(['git', '-C', cwd, 'rev-parse', '--show-toplevel'], { env: gitEnv() }); }
  catch { return null; }
  if (result.exit === 0) return new TextDecoder().decode(result.stdout).trim();
  const detail = result.stderr.toString().trim();
  if (/not a git repository/i.test(detail)) return null;
  throw new Error(msg.gitFailed(detail));
}

/** No transcript lookup: preflight can verify a fresh session immediately after its hook. */
export async function resolveReviewContext(options: ReviewContextOptions): Promise<ReviewContext> {
  const session = sessionIdSchema.parse(options.session);
  const cwd = resolve(options.cwd ?? process.cwd());
  const root = await repositoryRoot(cwd);
  if (root === null) {
    if (options.base !== undefined || options.head !== undefined) throw new Error(msg.needsGit(options.base !== undefined ? 'base' : 'head'));
    return { cwd, session, git: null, baseCommit: null, headCommit: null, historical: false, fs: new FilesystemReader(cwd) };
  }
  const git = new GitReader(root);
  const stamp = options.base ?? (await readStateFile(statePaths(getStateDir(root)).commitHash(session)))?.trim();
  if (!stamp) throw new Error(msg.missingStamp(session));
  const [baseCommit, headCommit] = await Promise.all([git.commit(stamp), git.commit(options.head ?? 'HEAD')]);
  return { cwd: root, session, baseCommit, headCommit, historical: options.head !== undefined, git, fs: new FilesystemReader(root) };
}
