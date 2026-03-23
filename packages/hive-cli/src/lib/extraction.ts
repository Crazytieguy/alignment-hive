import { createReadStream } from 'node:fs';
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { basename, join } from 'node:path';
import { SessionMetaSchema, parseKnownEntry } from '@alignment-hive/session-data';
import { getOrCreateCheckoutId, loadTranscriptsDirs } from './config';
import { errors } from './messages';
import { getDetectSecretsStats, resetDetectSecretsStats, sanitizeDeep } from './sanitize';
import { isErrorResult } from './auth';
import type { ErrorResult } from './auth';
import type { KnownEntry, SessionMeta } from '@alignment-hive/session-data';

export const HIVE_MIND_VERSION = '0.1' as const;

export function* parseJsonl(content: string) {
  for (const line of content.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      yield JSON.parse(trimmed) as unknown;
    } catch (error) {
      if (process.env.DEBUG) {
        console.warn('Skipping malformed JSONL line:', error);
      }
    }
  }
}

type ExtractedEntry = Exclude<ReturnType<typeof parseKnownEntry>['data'], null>;

export function transformEntry(rawEntry: unknown): { entry: ExtractedEntry | null; error?: string } {
  const result = parseKnownEntry(rawEntry);
  if (result.error) return { entry: null, error: result.error };
  if (!result.data) return { entry: null };

  const type = result.data.type;
  if (type === 'user' || type === 'assistant' || type === 'summary' || type === 'system') {
    return { entry: result.data };
  }
  return { entry: null };
}

interface ExtractSessionOptions {
  rawPath: string;
  outputPath: string;
  checkoutId: string;
  agentId?: string;
}

interface ParseResult {
  hasContent: boolean;
  schemaErrors: Array<string>;
}

/** Parse session without sanitizing - fast check for errors */
export async function parseSessionForErrors(rawPath: string): Promise<ParseResult> {
  const content = await readFile(rawPath, 'utf-8');
  const schemaErrors: Array<string> = [];
  let hasAssistant = false;

  for (const rawEntry of parseJsonl(content)) {
    const { entry, error } = transformEntry(rawEntry);
    if (error) schemaErrors.push(error);
    if (entry?.type === 'assistant') hasAssistant = true;
  }

  return { hasContent: hasAssistant, schemaErrors };
}

export async function extractSession(options: ExtractSessionOptions) {
  const { rawPath, outputPath, checkoutId, agentId } = options;

  const [content, rawStat, existingMetaResult] = await Promise.all([
    readFile(rawPath, 'utf-8'),
    stat(rawPath),
    readExtractedMeta(outputPath),
  ]);

  // Ignore errors when reading existing meta - just treat as not found
  const existingMeta = isMetaError(existingMetaResult) ? null : existingMetaResult;

  const t0Parse = process.env.DEBUG ? performance.now() : 0;
  const entries: Array<ExtractedEntry> = [];
  const schemaErrors: Array<string> = [];

  for (const rawEntry of parseJsonl(content)) {
    const { entry, error } = transformEntry(rawEntry);
    if (error) schemaErrors.push(error);
    if (entry) entries.push(entry);
  }

  // Count non-empty lines to match countRawLines() (which counts all non-empty lines,
  // not just valid JSON). This ensures the line count comparison in checkAllSessions
  // does not cause spurious re-extractions for files with malformed JSON lines.
  const rawLineCount = content.split('\n').filter((l) => l.trim()).length;
  if (process.env.DEBUG) {
    console.log(`[extract] Parsing: ${(performance.now() - t0Parse).toFixed(2)}ms for ${entries.length} entries`);
  }

  if (!entries.some((e) => e.type === 'assistant')) return null;

  const parentSessionId = agentId
    ? entries.find(
        (e): e is ExtractedEntry & { sessionId: string } => 'sessionId' in e && typeof e.sessionId === 'string',
      )?.sessionId
    : undefined;

  const meta: SessionMeta = {
    _type: 'session-meta',
    version: HIVE_MIND_VERSION,
    sessionId: basename(rawPath, '.jsonl'),
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime: rawStat.mtime.toISOString(),
    messageCount: entries.length,
    rawLineCount,
    ...(agentId && { agentId }),
    ...(parentSessionId && { parentSessionId }),
    ...(schemaErrors.length > 0 && { schemaErrors }),
    // Preserve flags from previous extraction
    ...(existingMeta?.excluded && { excluded: true }),
    ...(existingMeta?.uploadedAt && { uploadedAt: existingMeta.uploadedAt }),
  };

  resetDetectSecretsStats();
  const t0 = performance.now();
  const sanitizedEntries = entries.map((e) => sanitizeDeep(e));
  if (process.env.DEBUG) {
    const stats = getDetectSecretsStats();
    console.log(
      `[extract] Sanitization: ${(performance.now() - t0).toFixed(2)}ms | ` +
        `${stats.calls} calls, ${stats.keywordHits} keyword hits, ${stats.regexRuns} regex runs`,
    );
  }

  const lines = [JSON.stringify(meta), ...sanitizedEntries.map((e) => JSON.stringify(e))];
  await writeFile(outputPath, `${lines.join('\n')}\n`);

  return { messageCount: entries.length, schemaErrors };
}

async function readFirstLine(filePath: string): Promise<string | null> {
  const stream = createReadStream(filePath, { encoding: 'utf-8' });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });

  try {
    for await (const line of rl) {
      stream.destroy();
      return line;
    }
    return null;
  } finally {
    rl.close();
  }
}

export type ReadMetaResult = SessionMeta | ErrorResult | null;

export async function readExtractedMeta(extractedPath: string): Promise<ReadMetaResult> {
  try {
    const firstLine = await readFirstLine(extractedPath);
    if (!firstLine) return null;
    const parsed = SessionMetaSchema.safeParse(JSON.parse(firstLine));
    if (!parsed.success) {
      return { error: errors.schemaError(extractedPath, parsed.error.message) };
    }
    return parsed.data;
  } catch {
    return null;
  }
}

export const isMetaError = isErrorResult<SessionMeta>;

export type ReadSessionResult = { meta: SessionMeta; entries: Array<KnownEntry> } | { error: string } | null;

export async function readExtractedSession(extractedPath: string): Promise<ReadSessionResult> {
  let content: string;
  try {
    content = await readFile(extractedPath, 'utf-8');
  } catch (err: unknown) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') return null;
    return { error: `Failed to read ${extractedPath}: ${err instanceof Error ? err.message : String(err)}` };
  }

  const lines = content.split('\n').filter((l) => l.trim());
  if (lines.length === 0) return null;

  const metaParsed = SessionMetaSchema.safeParse(JSON.parse(lines[0]));
  if (!metaParsed.success) {
    return { error: errors.schemaError(extractedPath, metaParsed.error.message) };
  }

  const entries: Array<KnownEntry> = [];
  for (let i = 1; i < lines.length; i++) {
    const result = parseKnownEntry(JSON.parse(lines[i]));
    if (result.data) entries.push(result.data);
  }

  return { meta: metaParsed.data, entries };
}

export const isSessionError = isErrorResult<{ meta: SessionMeta; entries: Array<KnownEntry> }>;

export type MarkUploadedResult = { success: true } | { success: false; error?: string };

export async function markSessionUploaded(sessionPath: string): Promise<MarkUploadedResult> {
  try {
    const content = await readFile(sessionPath, 'utf-8');
    const newlineIndex = content.indexOf('\n');
    if (newlineIndex === -1) return { success: false };

    const firstLine = content.slice(0, newlineIndex);
    const parsed = SessionMetaSchema.safeParse(JSON.parse(firstLine));
    if (!parsed.success) {
      return { success: false, error: errors.schemaError(sessionPath, parsed.error.message) };
    }

    const meta = { ...parsed.data, uploadedAt: new Date().toISOString() };
    const newContent = JSON.stringify(meta) + content.slice(newlineIndex);

    await writeFile(sessionPath, newContent);
    return { success: true };
  } catch (err) {
    return { success: false, error: errors.markUploadedFailed(err instanceof Error ? err.message : String(err)) };
  }
}

export function getHiveMindSessionsDir(projectCwd: string): string {
  return join(projectCwd, '.claude', 'hive-mind', 'sessions');
}

/** Count non-empty lines in a file by streaming (no parsing) */
export async function countRawLines(filePath: string): Promise<number> {
  const stream = createReadStream(filePath, { encoding: 'utf-8' });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  let count = 0;
  for await (const line of rl) {
    if (line.trim()) count++;
  }
  return count;
}

/** Extract parentSessionId from a flat agent file by reading its first line's sessionId field. */
async function extractParentSessionId(agentPath: string): Promise<string | undefined> {
  const stream = createReadStream(agentPath, { encoding: 'utf-8' });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  try {
    for await (const line of rl) {
      try {
        const parsed = JSON.parse(line) as Record<string, unknown>;
        if (typeof parsed.sessionId === 'string') return parsed.sessionId;
      } catch { /* skip */ }
      return undefined; // Only check first line
    }
    return undefined;
  } catch {
    return undefined;
  } finally {
    rl.close();
    stream.destroy();
  }
}

export async function findRawSessions(rawDir: string) {
  const files = await readdir(rawDir);
  const sessions: Array<{ path: string; agentId?: string; parentSessionId?: string }> = [];

  for (const f of files) {
    if (f.endsWith('.jsonl')) {
      if (f.startsWith('agent-')) {
        const agentPath = join(rawDir, f);
        const parentSessionId = await extractParentSessionId(agentPath);
        sessions.push({ path: agentPath, agentId: f.replace('agent-', '').replace('.jsonl', ''), parentSessionId });
      } else {
        sessions.push({ path: join(rawDir, f) });
      }
      continue;
    }

    const subagentsDir = join(rawDir, f, 'subagents');
    try {
      const subagentFiles = await readdir(subagentsDir);
      for (const sf of subagentFiles) {
        if (sf.endsWith('.jsonl') && sf.startsWith('agent-')) {
          sessions.push({
            path: join(subagentsDir, sf),
            agentId: sf.replace('agent-', '').replace('.jsonl', ''),
            parentSessionId: f,
          });
        }
      }
    } catch {
      // Subagents directory doesn't exist - continue
    }
  }
  return sessions;
}

export interface SessionToExtract {
  sessionId: string;
  rawPath: string;
  agentId?: string;
}

export interface SessionCheckResult {
  /** Sessions that need extraction (new or updated) */
  sessionsToExtract: Array<SessionToExtract>;
  /** Schema errors found during parsing */
  schemaErrors: Array<{ sessionId: string; errors: Array<string> }>;
  /** All extracted sessions with their metadata (for eligibility checks) */
  extractedSessions: Array<{ sessionId: string; meta: SessionMeta }>;
  /** All errors encountered during session checking */
  errors: Array<string>;
}

const verbose = () => process.env.HIVE_MIND_VERBOSE === '1';

/** Check all sessions: which need extraction and provide metadata for eligibility */
export async function checkAllSessions(cwd: string, transcriptsDirs: Array<string>): Promise<SessionCheckResult> {
  const extractedDir = getHiveMindSessionsDir(cwd);
  const collectedErrors: Array<string> = [];

  const t0 = performance.now();

  // Load raw sessions from all directories and extracted metadata in parallel
  const [rawSessionArrays, extractedResult] = await Promise.all([
    Promise.all(
      transcriptsDirs.map(async (dir) => {
        try {
          return await findRawSessions(dir);
        } catch (err) {
          collectedErrors.push(errors.readTranscriptsDirFailed(dir, err instanceof Error ? err.message : String(err)));
          return [];
        }
      }),
    ),
    loadExtractedMetadata(extractedDir),
  ]);

  const tAfterLoad = performance.now();

  const { metaMap: extractedMetaMap, errors: metadataErrors } = extractedResult;
  collectedErrors.push(...metadataErrors);

  // Flatten and deduplicate by session ID (later directories override earlier)
  const rawSessionMap = new Map<string, { path: string; agentId?: string }>();
  for (const sessions of rawSessionArrays) {
    for (const session of sessions) {
      const sessionId = basename(session.path, '.jsonl');
      rawSessionMap.set(sessionId, session);
    }
  }
  const rawSessions = [...rawSessionMap.values()];

  const sessionsToExtract: Array<SessionToExtract> = [];
  const schemaErrors: Array<{ sessionId: string; errors: Array<string> }> = [];

  // Check each raw session against extracted metadata
  let parseCount = 0;
  await Promise.all(
    rawSessions.map(async (session) => {
      const { path: rawPath, agentId } = session;
      const sessionId = basename(rawPath, '.jsonl');
      const existingMeta = extractedMetaMap.get(sessionId);

      // Check if extraction is needed by comparing mtime with stored rawMtime
      let needsExtract = false;
      if (!existingMeta) {
        needsExtract = true;
      } else {
        try {
          const rawStat = await stat(rawPath);
          const storedMtime = new Date(existingMeta.rawMtime).getTime();
          if (rawStat.mtime.getTime() > storedMtime) {
            // Mtime changed - check if content actually changed via line count
            if (existingMeta.rawLineCount != null) {
              const currentLineCount = await countRawLines(rawPath);
              needsExtract = currentLineCount !== existingMeta.rawLineCount;
            } else {
              // No stored line count (old extraction) - always re-extract
              needsExtract = true;
            }
          }
        } catch (err) {
          collectedErrors.push(errors.statFailed(rawPath, err instanceof Error ? err.message : String(err)));
          needsExtract = true;
        }
      }

      if (needsExtract) {
        parseCount++;
        try {
          const parseResult = await parseSessionForErrors(rawPath);
          if (parseResult.schemaErrors.length > 0) {
            schemaErrors.push({ sessionId, errors: parseResult.schemaErrors });
          }
          if (parseResult.hasContent) {
            sessionsToExtract.push({ sessionId, rawPath, agentId });
          }
        } catch (err) {
          collectedErrors.push(errors.parseSessionFailed(sessionId, err instanceof Error ? err.message : String(err)));
          sessionsToExtract.push({ sessionId, rawPath, agentId });
        }
      }
    }),
  );

  const tEnd = performance.now();

  if (verbose()) {
    console.error(
      `[session-start] checkAllSessions: ${(tEnd - t0).toFixed(0)}ms total | ` +
        `findRaw+loadMeta: ${(tAfterLoad - t0).toFixed(0)}ms | ` +
        `statCheck+parse: ${(tEnd - tAfterLoad).toFixed(0)}ms | ` +
        `raw=${rawSessions.length} extracted=${extractedMetaMap.size} needsParse=${parseCount}`,
    );
  }

  const extractedSessions = [...extractedMetaMap.entries()].map(([sessionId, meta]) => ({
    sessionId,
    meta,
  }));

  return { sessionsToExtract, schemaErrors, extractedSessions, errors: collectedErrors };
}

interface LoadMetadataResult {
  metaMap: Map<string, SessionMeta>;
  errors: Array<string>;
}

/** Load all extracted session metadata into a map */
async function loadExtractedMetadata(extractedDir: string): Promise<LoadMetadataResult> {
  const t0 = performance.now();
  const metaMap = new Map<string, SessionMeta>();
  const collectedErrors: Array<string> = [];

  let files: Array<string>;
  try {
    files = await readdir(extractedDir);
  } catch {
    return { metaMap, errors: collectedErrors };
  }

  const jsonlFiles = files.filter((f) => f.endsWith('.jsonl'));
  const tAfterReaddir = performance.now();

  await Promise.all(
    jsonlFiles.map(async (file) => {
      const result = await readExtractedMeta(join(extractedDir, file));
      if (isMetaError(result)) {
        collectedErrors.push(result.error);
      } else if (result) {
        metaMap.set(result.sessionId, result);
      }
    }),
  );

  if (verbose()) {
    console.error(
      `[session-start]   loadExtractedMetadata: ${(performance.now() - t0).toFixed(0)}ms | ` +
        `readdir: ${(tAfterReaddir - t0).toFixed(0)}ms | ` +
        `readMeta: ${(performance.now() - tAfterReaddir).toFixed(0)}ms | ` +
        `files=${jsonlFiles.length}`,
    );
  }

  return { metaMap, errors: collectedErrors };
}

/** Full extraction for a single session (used by background process) */
export async function extractSingleSession(cwd: string, sessionId: string): Promise<boolean> {
  const hiveMindDir = join(cwd, '.claude', 'hive-mind');
  const extractedDir = getHiveMindSessionsDir(cwd);
  const [transcriptsDirs, checkoutId] = await Promise.all([
    loadTranscriptsDirs(hiveMindDir),
    getOrCreateCheckoutId(hiveMindDir),
  ]);
  if (transcriptsDirs.length === 0) return false;

  // Ensure output directory exists
  await mkdir(extractedDir, { recursive: true });

  // Search all transcript directories for the session
  for (const transcriptsDir of transcriptsDirs) {
    try {
      const rawSessions = await findRawSessions(transcriptsDir);
      const session = rawSessions.find((s) => basename(s.path, '.jsonl') === sessionId);
      if (session) {
        const extractedPath = join(extractedDir, basename(session.path));
        const result = await extractSession({
          rawPath: session.path,
          outputPath: extractedPath,
          checkoutId,
          agentId: session.agentId,
        });
        return result !== null;
      }
    } catch {
      // Skip inaccessible directories
      continue;
    }
  }

  return false;
}
