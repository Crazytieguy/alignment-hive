import { api } from '../../../web/convex/_generated/api';
import { runDataCommand } from '../lib/data-command';

export async function dataGetSession(sessionId: string): Promise<number> {
  if (!sessionId) {
    console.error('Usage: hive data get-session <sessionId>');
    return 1;
  }

  return runDataCommand((client) =>
    client.query(api.authorized.getSession, { sessionId }),
  );
}
