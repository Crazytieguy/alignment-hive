import { createReadStream } from "node:fs";
import { mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import { basename, dirname, join } from "node:path";
import { getCheckoutId } from "./config";
import { getDetectSecretsStats, resetDetectSecretsStats, sanitizeDeep } from "./sanitize";
import { HiveMindMetaSchema, parseKnownEntry } from "./schemas";
import type { HiveMindMeta, KnownEntry } from "./schemas";

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

type ExtractedEntry = Exclude<ReturnType<typeof parseKnownEntry>["data"], null>;

function transformEntry(rawEntry: unknown): { entry: ExtractedEntry | null; error?: string } {
  const result = parseKnownEntry(rawEntry);
  if (result.error) return { entry: null, error: result.error };
  if (!result.data) return { entry: null };

  if (INCLUDED_ENTRY_TYPES.includes(result.data.type as (typeof INCLUDED_ENTRY_TYPES)[number])) {
    return { entry: result.data as ExtractedEntry };
  }
  return { entry: null };
}

interface ExtractSessionOptions {
  rawPath: string;
  outputPath: string;
  agentId?: string;
}

export async function extractSession(options: ExtractSessionOptions) {
  const { rawPath, outputPath, agentId } = options;
  const hiveMindDir = dirname(dirname(outputPath));

  const [content, rawStat, checkoutId] = await Promise.all([
    readFile(rawPath, "utf-8"),
    stat(rawPath),
    getCheckoutId(hiveMindDir),
  ]);

  const t0Parse = process.env.DEBUG ? performance.now() : 0;
  const entries: Array<ExtractedEntry> = [];
  const schemaErrors: Array<string> = [];

  for (const rawEntry of parseJsonl(content)) {
    const { entry, error } = transformEntry(rawEntry);
    if (error) schemaErrors.push(error);
    if (entry) entries.push(entry);
  }
  if (process.env.DEBUG) {
    console.log(`[extract] Parsing: ${(performance.now() - t0Parse).toFixed(2)}ms for ${entries.length} entries`);
  }

  if (!entries.some((e) => e.type === "assistant")) return null;

  const parentSessionId = agentId
    ? entries.find((e): e is ExtractedEntry & { sessionId: string } =>
        "sessionId" in e && typeof e.sessionId === "string"
      )?.sessionId
    : undefined;

  const meta: HiveMindMeta = {
    _type: "hive-mind-meta",
    version: HIVE_MIND_VERSION,
    sessionId: basename(rawPath, ".jsonl"),
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime: rawStat.mtime.toISOString(),
    messageCount: entries.length,
    rawPath,
    ...(agentId && { agentId }),
    ...(parentSessionId && { parentSessionId }),
    ...(schemaErrors.length > 0 && { schemaErrors }),
  };

  resetDetectSecretsStats();
  const t0 = performance.now();
  const sanitizedEntries = entries.map((e) => sanitizeDeep(e));
  if (process.env.DEBUG) {
    const stats = getDetectSecretsStats();
    console.log(
      `[extract] Sanitization: ${(performance.now() - t0).toFixed(2)}ms | ` +
      `${stats.calls} calls, ${stats.keywordHits} keyword hits, ${stats.regexRuns} regex runs`
    );
  }

  await mkdir(dirname(outputPath), { recursive: true });
  const lines = [JSON.stringify(meta), ...sanitizedEntries.map((e) => JSON.stringify(e))];
  await writeFile(outputPath, `${lines.join("\n")}\n`);

  return { messageCount: entries.length, schemaErrors };
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

export async function readExtractedMeta(extractedPath: string): Promise<HiveMindMeta | null> {
  try {
    const firstLine = await readFirstLine(extractedPath);
    if (!firstLine) return null;
    const parsed = HiveMindMetaSchema.safeParse(JSON.parse(firstLine));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

export async function readExtractedSession(
  extractedPath: string
): Promise<{ meta: HiveMindMeta; entries: Array<KnownEntry> } | null> {
  try {
    const content = await readFile(extractedPath, "utf-8");
    const lines = content.split("\n").filter((l) => l.trim());
    if (lines.length === 0) return null;

    const metaParsed = HiveMindMetaSchema.safeParse(JSON.parse(lines[0]));
    if (!metaParsed.success) return null;

    const entries: Array<KnownEntry> = [];
    for (let i = 1; i < lines.length; i++) {
      const result = parseKnownEntry(JSON.parse(lines[i]));
      if (result.data) entries.push(result.data);
    }

    return { meta: metaParsed.data, entries };
  } catch {
    return null;
  }
}

export function getHiveMindSessionsDir(projectCwd: string): string {
  return join(projectCwd, ".claude", "hive-mind", "sessions");
}

async function findRawSessions(rawDir: string) {
  const files = await readdir(rawDir);
  const sessions: Array<{ path: string; agentId?: string }> = [];

  for (const f of files) {
    if (f.endsWith(".jsonl")) {
      if (f.startsWith("agent-")) {
        sessions.push({ path: join(rawDir, f), agentId: f.replace("agent-", "").replace(".jsonl", "") });
      } else {
        sessions.push({ path: join(rawDir, f) });
      }
      continue;
    }

    const subagentsDir = join(rawDir, f, "subagents");
    try {
      const subagentFiles = await readdir(subagentsDir);
      for (const sf of subagentFiles) {
        if (sf.endsWith(".jsonl") && sf.startsWith("agent-")) {
          sessions.push({
            path: join(subagentsDir, sf),
            agentId: sf.replace("agent-", "").replace(".jsonl", ""),
          });
        }
      }
    } catch {}
  }
  return sessions;
}

async function needsExtraction(rawPath: string, extractedPath: string): Promise<boolean> {
  try {
    const rawStat = await stat(rawPath);
    const meta = await readExtractedMeta(extractedPath);
    if (!meta) return true;
    return rawStat.mtime.toISOString() !== meta.rawMtime;
  } catch {
    return true;
  }
}

export interface ExtractionResult {
  extracted: number;
  failed: number;
  schemaErrors: Array<{ sessionId: string; errors: Array<string> }>;
}

export async function extractAllSessions(cwd: string, transcriptsDir: string): Promise<ExtractionResult> {
  const extractedDir = getHiveMindSessionsDir(cwd);

  const rawSessions = await findRawSessions(transcriptsDir);
  let extracted = 0;
  let failed = 0;
  const schemaErrors: ExtractionResult["schemaErrors"] = [];

  for (const session of rawSessions) {
    const { path: rawPath, agentId } = session;
    const extractedPath = join(extractedDir, basename(rawPath));

    if (await needsExtraction(rawPath, extractedPath)) {
      try {
        const result = await extractSession({ rawPath, outputPath: extractedPath, agentId });
        if (result) {
          if (!agentId) {
            extracted++;
          }
          if (result.schemaErrors.length > 0) {
            schemaErrors.push({ sessionId: basename(rawPath, ".jsonl"), errors: result.schemaErrors });
          }
        }
      } catch (error) {
        if (!agentId) {
          failed++;
        }
      }
    }
  }

  return { extracted, failed, schemaErrors };
}
