import { lstat, mkdir, readFile, writeFile } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { z } from 'zod';
import { runCommand } from '../lib/spawn';
import { reviewCaptureMessages as msg } from '../lib/messages';
import { itemIdSchema } from './parse';

const argument = z.string().refine((value) => !value.includes('\0'));
const commandSchema = z.array(argument).min(1).refine((command) => Boolean(command[0]?.length));
export const captureSchema = z.strictObject({
  command: commandSchema,
  cwd: argument.refine(isAbsolute),
  exit: z.number().int(),
  stdout: z.string(),
  stderr: z.string(),
  startedAt: z.iso.datetime(),
});
export type Capture = z.infer<typeof captureSchema>;
export interface CaptureReviewOptions {
  name: string;
  command: Array<string>;
  cwd: string;
  outDir: string;
}

function validateName(name: string): void {
  if (name.length > 128 || !itemIdSchema.safeParse(name).success) {
    throw new Error(msg.invalidName);
  }
}

function directoryPath(path: string): string {
  if (!path || path.includes('\0')) throw new Error(msg.invalidDirectory);
  return resolve(path);
}

function hasCode(error: unknown, code: string): boolean {
  return error instanceof Error && 'code' in error && error.code === code;
}

async function exists(path: string): Promise<boolean> {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if (hasCode(error, 'ENOENT')) return false;
    throw error;
  }
}

function decodeOutput(bytes: Uint8Array, stream: string): string {
  try {
    // Preserve BOMs and NULs as output, unlike the source-file text reader.
    return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    throw new Error(msg.invalidEncoding(stream));
  }
}

/** Execute argv directly and never overwrite an existing capture. */
export async function captureReview(options: CaptureReviewOptions): Promise<{ path: string; capture: Capture }> {
  validateName(options.name);
  const parsed = commandSchema.safeParse(options.command);
  if (!parsed.success) throw new Error(msg.invalidCommand);
  const command = parsed.data;
  const cwd = directoryPath(options.cwd);
  const directory = join(directoryPath(options.outDir), 'captures');
  await mkdir(directory, { recursive: true });
  const path = join(directory, `${options.name}.json`);
  if (await exists(path)) throw new Error(msg.exists(options.name));
  const startedAt = new Date().toISOString();
  const { stdout, stderr, exit } = await runCommand(command, { cwd });
  const capture = captureSchema.parse({
    command, cwd, exit, startedAt,
    stdout: decodeOutput(stdout, 'stdout'), stderr: decodeOutput(stderr, 'stderr'),
  });
  await writeFile(path, `${JSON.stringify(capture)}\n`, { flag: 'wx', mode: 0o600 }).catch((error: unknown) => {
    if (hasCode(error, 'EEXIST')) throw new Error(msg.exists(options.name));
    throw error;
  });
  return { path, capture };
}

/** Read a named capture and validate its recorded schema. */
export async function readCapture(name: string, reviewDir: string): Promise<Capture> {
  validateName(name);
  const path = join(directoryPath(reviewDir), 'captures', `${name}.json`);
  const bytes = await readFile(path);
  try {
    return captureSchema.parse(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)));
  } catch (error) {
    throw new Error(msg.invalidCapture(path), { cause: error });
  }
}
