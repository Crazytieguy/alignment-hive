import { readFile } from 'node:fs/promises';
import { generateUploadUrl, saveUpload } from './convex';
import { parseJsonl, transformEntry } from './extraction';
import { sanitizeDeep } from './sanitize';
import { extractSessionSummary } from './summary';
import type { KnownEntry } from '@alignment-hive/session-data';

export async function uploadSingleSession(
  sessionPath: string,
  sessionId: string,
  checkoutId: string,
  rawMtime: string,
  identifiers: { directory: string; gitRemote?: string },
): Promise<{ success: boolean; error?: string }> {
  let rawContent: string;
  try {
    rawContent = await readFile(sessionPath, 'utf-8');
  } catch {
    return { success: false, error: 'Session file not found' };
  }

  const entries: Array<KnownEntry> = [];
  for (const rawEntry of parseJsonl(rawContent)) {
    const { entry } = transformEntry(rawEntry);
    if (entry) entries.push(entry as KnownEntry);
  }

  // Discovery filters via fast string search; verify after full parse
  if (!entries.some((e) => e.type === 'assistant')) {
    return { success: false, error: 'No assistant messages' };
  }

  const sanitizedEntries = entries.map((e) => sanitizeDeep(e));

  const lastModified = new Date(rawMtime).getTime();
  const uploadUrl = await generateUploadUrl(sessionId, {
    checkoutId,
    directory: identifiers.directory,
    gitRemote: identifiers.gitRemote,
    lineCount: entries.length,
    lastModified: isFinite(lastModified) ? lastModified : undefined,
  });
  if (!uploadUrl) {
    return { success: false, error: 'Failed to get upload URL' };
  }

  const meta = {
    _type: 'session-meta',
    version: '0.1',
    sessionId,
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime,
    messageCount: entries.length,
  };

  const lines = [JSON.stringify(meta), ...sanitizedEntries.map((e) => JSON.stringify(e))];
  const content = `${lines.join('\n')}\n`;

  try {
    const response = await fetch(uploadUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-ndjson' },
      body: content,
    });

    if (!response.ok) {
      return { success: false, error: `Upload failed: ${response.status}` };
    }

    const result = (await response.json()) as { storageId?: string };
    if (!result.storageId) {
      return { success: false, error: 'No storage ID returned' };
    }

    const summary = extractSessionSummary(entries);
    const saved = await saveUpload(sessionId, result.storageId, summary);
    if (!saved) {
      return { success: false, error: 'Failed to save upload metadata' };
    }

    return { success: true };
  } catch (error) {
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Unknown upload error',
    };
  }
}
