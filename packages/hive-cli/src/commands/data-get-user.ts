import { api } from '../../../web/convex/_generated/api';
import { runDataCommand } from '../lib/data-command';
import type { Id } from '../../../web/convex/_generated/dataModel';

export async function dataGetUser(userId: string): Promise<number> {
  if (!userId) {
    console.error('Usage: hive data get-user <userId>');
    return 1;
  }

  return runDataCommand((client) =>
    client.query(api.authorized.getUser, { userId: userId as Id<'users'> }),
  );
}
