import { readFile, readdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { getHiveMindSessionsDir, readExtractedMeta } from '../lib/extraction.js';
import { excludeCmd } from '../lib/messages.js';
import { colors, printError, printInfo, printSuccess } from '../lib/output.js';
import { confirm, formatSessionId, lookupSession } from '../lib/utils.js';
import type { HiveMindMeta } from '../lib/schemas.js';

async function excludeSession(sessionPath: string): Promise<boolean> {
  try {
    const content = await readFile(sessionPath, 'utf-8');
    const lines = content.split('\n');
    if (lines.length === 0) return false;

    const meta = JSON.parse(lines[0]) as HiveMindMeta;
    meta.excluded = true;
    lines[0] = JSON.stringify(meta);

    await writeFile(sessionPath, lines.join('\n'));
    return true;
  } catch {
    return false;
  }
}

async function excludeAll(cwd: string): Promise<number> {
  const sessionsDir = getHiveMindSessionsDir(cwd);

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    printError(excludeCmd.noSessionsDir);
    return 1;
  }

  const sessionFiles = files.filter((f) => f.endsWith('.jsonl'));
  if (sessionFiles.length === 0) {
    console.log(excludeCmd.noSessions);
    return 0;
  }

  let nonExcludedCount = 0;
  for (const file of sessionFiles) {
    const meta = await readExtractedMeta(join(sessionsDir, file));
    if (meta && !meta.excluded && !meta.agentId) {
      nonExcludedCount++;
    }
  }

  if (nonExcludedCount === 0) {
    console.log(excludeCmd.allAlreadyExcluded);
    return 0;
  }

  console.log(excludeCmd.foundNonExcluded(nonExcludedCount));
  console.log('');

  if (!(await confirm(excludeCmd.confirmExcludeAll))) {
    console.log(excludeCmd.cancelled);
    return 0;
  }

  let succeeded = 0;
  let failed = 0;

  for (const file of sessionFiles) {
    const sessionPath = join(sessionsDir, file);
    const meta = await readExtractedMeta(sessionPath);
    if (!meta || meta.excluded || meta.agentId) continue;

    const sessionId = file.replace('.jsonl', '');
    if (await excludeSession(sessionPath)) {
      console.log(`  ${colors.yellow('✗')} ${formatSessionId(sessionId)} excluded`);
      succeeded++;
    } else {
      console.log(`  ${colors.red('!')} ${formatSessionId(sessionId)} failed`);
      failed++;
    }
  }

  console.log('');
  if (succeeded > 0) {
    printSuccess(excludeCmd.excludedCount(succeeded));
  }
  if (failed > 0) {
    printError(excludeCmd.failedCount(failed));
  }

  return failed > 0 ? 1 : 0;
}

async function excludeOne(cwd: string, sessionIdPrefix: string): Promise<number> {
  const lookup = await lookupSession(cwd, sessionIdPrefix);

  if (lookup.type === 'not_found') {
    printError(excludeCmd.sessionNotFound(sessionIdPrefix));
    return 1;
  }

  if (lookup.type === 'ambiguous') {
    printError(excludeCmd.ambiguousSession(sessionIdPrefix, lookup.matches.length));
    console.log(excludeCmd.matches);
    for (const m of lookup.matches) {
      console.log(`  ${m}`);
    }
    return 1;
  }

  const { sessionPath, meta } = lookup;

  if (meta.agentId) {
    printError(excludeCmd.cannotExcludeAgent);
    return 1;
  }

  if (meta.excluded) {
    printInfo(excludeCmd.alreadyExcluded(formatSessionId(meta.sessionId)));
    return 0;
  }

  if (await excludeSession(sessionPath)) {
    printSuccess(excludeCmd.excluded(formatSessionId(meta.sessionId)));
    return 0;
  }
  printError(excludeCmd.failedToExclude(formatSessionId(meta.sessionId)));
  return 1;
}

export async function exclude(): Promise<number> {
  const cwd = process.env.CWD || process.cwd();
  const args = process.argv.slice(3);

  if (args.includes('--all')) {
    return await excludeAll(cwd);
  }

  const sessionId = args.find((a) => !a.startsWith('-'));
  if (!sessionId) {
    printError(excludeCmd.usage);
    return 1;
  }

  return await excludeOne(cwd, sessionId);
}
