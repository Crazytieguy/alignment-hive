import { api } from '../../../web/convex/_generated/api';
import { runDataCommand } from '../lib/data-command';
import type { Id } from '../../../web/convex/_generated/dataModel';

export async function dataListProjects(argv: Array<string>): Promise<number> {
  let userId: string | undefined;

  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--user-id') userId = argv[++i];
  }

  if (!userId) {
    console.error('Usage: hive data list-projects --user-id <userId>');
    return 1;
  }

  return runDataCommand((client) =>
    client.query(api.authorized.listProjects, { userId: userId as Id<'users'> }),
  );
}
