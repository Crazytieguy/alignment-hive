import { createReadStream } from "node:fs";
import { mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { homedir } from "node:os";
import { basename, dirname, join } from "node:path";
import { getCheckoutId } from "./config";
import { getDetectSecretsStats, resetDetectSecretsStats, sanitizeDeep } from "./sanitize";
import {
  
  HiveMindMetaSchema,
  
  parseKnownEntry
} from "./schemas";
import type {HiveMindMeta, KnownEntry} from "./schemas";

const HIVE_MIND_VERSION = "0.1" as const;

const INCLUDED_ENTRY_TYPES = ["user", "assistant", "summary", "system"] as const;

export function* parseJsonl(content: string) {
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      yield JSON.parse(trimmed) as unknown;
    } catch (error) {
      if (process.env.DEBUG) {
        console.warn("Skipping malformed JSONL line:", error);
      }
    }
  }
}

function transformEntry(rawEntry: unknown): { entry: ExtractedEntry | null; error?: string } {
  const result = parseKnownEntry(rawEntry);
  if (result.error) {
    return { entry: null, error: result.error };
  }
  if (!result.data) {
    return { entry: null };
  }

  if (INCLUDED_ENTRY_TYPES.includes(result.data.type as (typeof INCLUDED_ENTRY_TYPES)[number])) {
    return { entry: result.data as ExtractedEntry };
  }

  return { entry: null };
}

type ExtractedEntry = Exclude<ReturnType<typeof parseKnownEntry>["data"], null>;

/** Returns the summary where leafUuid exists in the same session. */
function findValidSummary(entries: Array<ExtractedEntry>) {
  const uuids = new Set<string>();
  const summaries: Array<{ summary: string; leafUuid?: string }> = [];

  for (const entry of entries) {
    if ("uuid" in entry && typeof entry.uuid === "string") {
      uuids.add(entry.uuid);
    }
    if (entry.type === "summary") {
      summaries.push({
        summary: entry.summary,
        leafUuid: entry.leafUuid,
      });
    }
  }

  // Find a summary whose leafUuid exists in this file (not cross-contaminated)
  for (const s of summaries) {
    if (s.leafUuid && uuids.has(s.leafUuid)) {
      return s.summary;
    }
  }

  // Fallback: return the last summary if no valid one found
  return summaries.at(-1)?.summary;
}

interface ExtractSessionOptions {
  rawPath: string;
  outputPath: string;
  agentId?: string;
}

export async function extractSession(options: ExtractSessionOptions) {
  const { rawPath, outputPath, agentId } = options;

  // outputPath is <cwd>/.claude/hive-mind/sessions/<id>.jsonl
  // hiveMindDir is <cwd>/.claude/hive-mind
  const hiveMindDir = dirname(dirname(outputPath));

  const [content, rawStat, checkoutId] = await Promise.all([
    readFile(rawPath, "utf-8"),
    stat(rawPath),
    getCheckoutId(hiveMindDir),
  ]);

  const t0Parse = process.env.DEBUG ? performance.now() : 0;
  const entries: Array<ExtractedEntry> = [];
  const schemaErrors: string[] = [];

  for (const rawEntry of parseJsonl(content)) {
    const { entry, error } = transformEntry(rawEntry);
    if (error) {
      schemaErrors.push(error);
    }
    if (entry) {
      entries.push(entry);
    }
  }
  if (process.env.DEBUG) {
    console.log(`[extract] Parsing: ${(performance.now() - t0Parse).toFixed(2)}ms for ${entries.length} entries`);
  }

  // Skip sessions with no assistant messages
  const hasAssistantMessage = entries.some((e) => e.type === "assistant");
  if (!hasAssistantMessage) {
    return null;
  }

  // For agent sessions, extract parentSessionId from first entry with sessionId
  const parentSessionId = agentId
    ? entries.find((e): e is ExtractedEntry & { sessionId: string } => "sessionId" in e && typeof e.sessionId === "string")?.sessionId
    : undefined;

  const summary = findValidSummary(entries);
  const sessionId = basename(rawPath, ".jsonl");

  const meta: HiveMindMeta = {
    _type: "hive-mind-meta",
    version: HIVE_MIND_VERSION,
    sessionId,
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime: rawStat.mtime.toISOString(),
    messageCount: entries.length,
    summary,
    rawPath,
    ...(agentId && { agentId }),
    ...(parentSessionId && { parentSessionId }),
    ...(schemaErrors.length > 0 && { schemaErrors }),
  };

  resetDetectSecretsStats();
  const t0 = performance.now();
  const sanitizedEntries = entries.map((e) => sanitizeDeep(e));
  const sanitizeMs = performance.now() - t0;
  if (process.env.DEBUG) {
    const stats = getDetectSecretsStats();
    console.log(`[extract] Sanitization: ${sanitizeMs.toFixed(2)}ms for ${entries.length} entries | detectSecrets: ${stats.calls} calls, ${stats.keywordHits} keyword hits, ${stats.regexRuns} regex runs, ${stats.totalMs.toFixed(2)}ms`);
  }

  // Write output
  await mkdir(dirname(outputPath), { recursive: true });

  const lines = [
    JSON.stringify(meta),
    ...sanitizedEntries.map((e) => JSON.stringify(e)),
  ];

  await writeFile(outputPath, `${lines.join("\n")}\n`);

  return { messageCount: entries.length, summary, schemaErrors };
}

async function readFirstLine(filePath: string): Promise<string | null> {
  const stream = createReadStream(filePath, { encoding: "utf-8" });
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

export async function readExtractedMeta(
  extractedPath: string,
): Promise<HiveMindMeta | null> {
  try {
    const firstLine = await readFirstLine(extractedPath);
    if (!firstLine) return null;

    const parsed = HiveMindMetaSchema.safeParse(JSON.parse(firstLine));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

function getProjectsDir(cwd: string): string {
  // Encode cwd: replace / with -
  const encoded = cwd.replace(/\//g, "-");
  return join(homedir(), ".claude", "projects", encoded);
}

export function getHiveMindSessionsDir(projectCwd: string): string {
  return join(projectCwd, ".claude", "hive-mind", "sessions");
}

/** Returns both regular sessions and agent sessions. */
async function findRawSessions(rawDir: string) {
  try {
    const files = await readdir(rawDir);
    const sessions: Array<{ path: string; agentId?: string }> = [];

    for (const f of files) {
      if (!f.endsWith(".jsonl")) continue;

      if (f.startsWith("agent-")) {
        // Agent session: agent-<agentId>.jsonl
        const agentId = f.replace("agent-", "").replace(".jsonl", "");
        sessions.push({ path: join(rawDir, f), agentId });
      } else {
        // Regular session
        sessions.push({ path: join(rawDir, f) });
      }
    }

    return sessions;
  } catch {
    return [];
  }
}

async function needsExtraction(
  rawPath: string,
  extractedPath: string,
): Promise<boolean> {
  try {
    const rawStat = await stat(rawPath);
    const meta = await readExtractedMeta(extractedPath);

    if (!meta) return true;

    // Compare mtimes
    const rawMtime = rawStat.mtime.toISOString();
    return rawMtime !== meta.rawMtime;
  } catch {
    return true;
  }
}

export interface ExtractionResult {
  extracted: number;
  schemaErrors: Array<{ sessionId: string; errors: string[] }>;
}

export async function extractAllSessions(cwd: string, transcriptPath?: string): Promise<ExtractionResult> {
  const rawDir = transcriptPath ? dirname(transcriptPath) : getProjectsDir(cwd);
  const extractedDir = getHiveMindSessionsDir(cwd);

  const rawSessions = await findRawSessions(rawDir);
  let extracted = 0;
  const schemaErrors: ExtractionResult["schemaErrors"] = [];

  for (const session of rawSessions) {
    const { path: rawPath, agentId } = session;
    const extractedPath = join(extractedDir, basename(rawPath));

    if (await needsExtraction(rawPath, extractedPath)) {
      try {
        const result = await extractSession({ rawPath, outputPath: extractedPath, agentId });
        if (result) {
          extracted++;
          if (result.schemaErrors.length > 0) {
            schemaErrors.push({
              sessionId: basename(rawPath, ".jsonl"),
              errors: result.schemaErrors,
            });
          }
        }
      } catch (error) {
        const id = basename(rawPath, ".jsonl");
        console.error(`Failed to extract ${id}:`, error);
      }
    }
  }

  return { extracted, schemaErrors };
}
