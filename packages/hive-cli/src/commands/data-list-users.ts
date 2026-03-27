import { api } from '../../../web/convex/_generated/api';
import { runDataCommand } from '../lib/data-command';

export async function dataListUsers(argv: Array<string>): Promise<number> {
  let numItems = 25;
  let cursor: string | null = null;

  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--num-items') numItems = parseInt(argv[++i], 10);
    if (argv[i] === '--cursor') cursor = argv[++i];
  }

  return runDataCommand((client) =>
    client.query(api.authorized.listUsers, {
      paginationOpts: { numItems, cursor },
    }),
  );
}
