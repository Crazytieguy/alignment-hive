import { basename, join } from "node:path";
import {
  extractSession,
  findRawSessions,
  getHiveMindSessionsDir,
  getProjectsDir,
  needsExtraction,
} from "../lib/extraction";
import { printError, printInfo, printSuccess } from "../lib/output";

export async function extract(): Promise<void> {
  const cwd = process.cwd();
  const rawDir = getProjectsDir(cwd);
  const extractedDir = getHiveMindSessionsDir(cwd);

  printInfo(`Scanning for sessions in ${rawDir}...`);

  const sessions = await findRawSessions(rawDir);
  if (sessions.length === 0) {
    printInfo("No sessions found to extract.");
    return;
  }

  printInfo(
    `Found ${sessions.length} sessions. Checking which need extraction...`,
  );

  // Find sessions that need extraction
  const toExtract: typeof sessions = [];
  for (const session of sessions) {
    const { path: rawPath } = session;
    // Use same filename as original (agent-<id>.jsonl or <sessionId>.jsonl)
    const extractedPath = join(extractedDir, basename(rawPath));

    if (await needsExtraction(rawPath, extractedPath)) {
      toExtract.push(session);
    }
  }

  if (toExtract.length === 0) {
    printSuccess("All sessions already extracted.");
    return;
  }

  printInfo(`Extracting ${toExtract.length} sessions...`);
  console.log("");

  let extracted = 0;
  let failed = 0;

  for (let i = 0; i < toExtract.length; i++) {
    const session = toExtract[i];
    const { path: rawPath, agentId } = session;
    const id = agentId || basename(rawPath, ".jsonl");

    // Use same filename as original
    const extractedPath = join(extractedDir, basename(rawPath));

    const progress = `[${i + 1}/${toExtract.length}]`;
    process.stdout.write(`\r  ${progress} Extracting ${id.slice(0, 8)}...`);

    try {
      await extractSession({ rawPath, outputPath: extractedPath, agentId });
      extracted++;
    } catch (error) {
      failed++;
      console.log("");
      printError(`Failed to extract ${id}: ${error}`);
    }
  }

  console.log("");
  console.log("");

  if (failed === 0) {
    printSuccess(`Extracted ${extracted} sessions successfully.`);
  } else {
    printSuccess(`Extracted ${extracted} sessions, ${failed} failed.`);
  }

  console.log(`Output: ${extractedDir}`);
}
