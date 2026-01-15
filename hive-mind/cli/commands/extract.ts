import { extractSingleSession } from "../lib/extraction";

export async function extract(): Promise<number> {
  const cwd = process.env.CWD || process.cwd();
  const sessionId = process.argv[3];

  if (!sessionId) {
    return 1;
  }

  try {
    const success = await extractSingleSession(cwd, sessionId);
    return success ? 0 : 1;
  } catch (error) {
    if (process.env.DEBUG) {
      console.error(`[extract] ${error instanceof Error ? error.message : String(error)}`);
    }
    return 1;
  }
}
