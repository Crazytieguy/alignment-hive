import { readFile, stat } from 'node:fs/promises';
import { isAbsolute, posix, resolve } from 'node:path';
import { review as msg } from '../lib/messages';

export function repositoryPath(path: string): string {
  if (!path || isAbsolute(path) || path.split('/').some((part) => part === '..') || path.includes('\0')) throw new Error(msg.invalidPath(path));
  const clean = posix.normalize(path);
  if (clean === '.' || clean.endsWith('/')) throw new Error(msg.invalidPath(path));
  return clean;
}
export function decodeText(bytes: Uint8Array, path: string): string {
  try {
    if (bytes.includes(0)) throw new Error();
    return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    throw new Error(msg.invalidText(path));
  }
}

/** Explicit absolute file refs support notes/memory outside the repository. */
export class FilesystemReader {
  constructor(readonly cwd: string) {}
  async bytes(path: string): Promise<Uint8Array | null> {
    const absolute = resolve(this.cwd, path);
    try {
      if (!(await stat(absolute)).isFile()) throw new Error(msg.nonFile(path));
      return await readFile(absolute);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null;
      throw error;
    }
  }
  async text(path: string): Promise<string | null> {
    const bytes = await this.bytes(path);
    return bytes === null ? null : decodeText(bytes, path);
  }
}
