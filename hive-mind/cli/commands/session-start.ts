import { checkAuthStatus, getUserDisplayName } from "../lib/auth";
import { extractAllSessions } from "../lib/extraction";
import { hook } from "../lib/messages";
import { hookOutput } from "../lib/output";

export async function sessionStart(): Promise<number> {
  const messages: Array<string> = [];

  const status = await checkAuthStatus(true);

  if (status.needsLogin) {
    messages.push(hook.notLoggedIn());
  } else if (status.user) {
    messages.push(hook.loggedIn(getUserDisplayName(status.user)));
  }

  const cwd = process.env.CWD || process.cwd();
  const transcriptPath = process.env.TRANSCRIPT_PATH;

  try {
    const { extracted, schemaErrors } = await extractAllSessions(cwd, transcriptPath);
    if (extracted > 0) {
      messages.push(hook.extracted(extracted));
    }
    if (schemaErrors.length > 0) {
      const errorCount = schemaErrors.reduce((sum, s) => sum + s.errors.length, 0);
      const allErrors = schemaErrors.flatMap((s) => s.errors);
      messages.push(hook.schemaErrors(errorCount, schemaErrors.length, allErrors));
    }
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : String(error);
    messages.push(hook.extractionFailed(errorMsg));
  }

  if (messages.length > 0) {
    hookOutput(messages.join("\n"));
  }

  return 0;
}
