import { checkAuthStatus, getUserDisplayName } from "../lib/auth";
import { loggedInMessage, notLoggedInMessage } from "../lib/messages";
import { hookOutput } from "../lib/output";

export async function sessionStart(): Promise<void> {
  const status = await checkAuthStatus(true);

  if (status.needsLogin) {
    hookOutput(notLoggedInMessage());
  } else if (status.user) {
    hookOutput(loggedInMessage(getUserDisplayName(status.user)));
  }
}
