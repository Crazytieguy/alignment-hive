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
  } catch {
    return 1;
  }
}
