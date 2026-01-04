import { checkAuthStatus, getUserDisplayName } from "../lib/auth";
import { extractAllSessions } from "../lib/extraction";
import {
  extractedMessage,
  loggedInMessage,
  notLoggedInMessage,
} from "../lib/messages";
import { hookOutput } from "../lib/output";

export async function sessionStart(): Promise<void> {
  const messages: string[] = [];

  // 1. Check auth status
  const status = await checkAuthStatus(true);

  if (status.needsLogin) {
    messages.push(notLoggedInMessage());
  } else if (status.user) {
    messages.push(loggedInMessage(getUserDisplayName(status.user)));
  }

  // 2. Extract all sessions that need extraction
  const cwd = process.env.CWD || process.cwd();
  const transcriptPath = process.env.TRANSCRIPT_PATH;

  try {
    const extracted = await extractAllSessions(cwd, transcriptPath);
    if (extracted > 0) {
      messages.push(extractedMessage(extracted));
    }
  } catch {
    // Silent failure for extraction - don't block the session
  }

  // Output all messages together
  if (messages.length > 0) {
    hookOutput(messages.join("\n"));
  }
}
