import { checkAuthStatus, getUserDisplayName } from "../lib/auth";
import { extractAllSessions } from "../lib/extraction";
import {
  extractedMessage,
  loggedInMessage,
  notLoggedInMessage,
} from "../lib/messages";
import { hookOutput } from "../lib/output";

export async function sessionStart(): Promise<number> {
  const messages: Array<string> = [];

  const status = await checkAuthStatus(true);

  if (status.needsLogin) {
    messages.push(notLoggedInMessage());
  } else if (status.user) {
    messages.push(loggedInMessage(getUserDisplayName(status.user)));
  }

  const cwd = process.env.CWD || process.cwd();
  const transcriptPath = process.env.TRANSCRIPT_PATH;

  try {
    const { extracted, schemaErrors } = await extractAllSessions(cwd, transcriptPath);
    if (extracted > 0) {
      messages.push(extractedMessage(extracted));
    }
    if (schemaErrors.length > 0) {
      const errorCount = schemaErrors.reduce((sum, s) => sum + s.errors.length, 0);
      const uniqueErrors = [...new Set(schemaErrors.flatMap((s) => s.errors))];
      messages.push(
        `Schema errors (${errorCount} entries in ${schemaErrors.length} sessions): ${uniqueErrors.join("; ")}`,
      );
    }
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : String(error);
    messages.push(`Extraction failed: ${errorMsg}`);
  }

  if (messages.length > 0) {
    hookOutput(messages.join("\n"));
  }

  return 0;
}
