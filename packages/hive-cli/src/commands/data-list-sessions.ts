import { api } from '../../../web/convex/_generated/api';
import { runDataCommand } from '../lib/data-command';
import type { Id } from '../../../web/convex/_generated/dataModel';

export async function dataListSessions(argv: Array<string>): Promise<number> {
  const args = parseArgs(argv);

  return runDataCommand((client) =>
    client.query(api.authorized.listSessions, {
      paginationOpts: {
        numItems: args.numItems,
        cursor: args.cursor,
      },
      filter: args.filter,
    }),
  );
}

function parseArgs(argv: Array<string>) {
  let numItems = 25;
  let cursor: string | null = null;
  let userId: string | undefined;
  let projectDirectory: string | undefined;
  let projectGitRemote: string | undefined;
  let excludeUserIds: Array<string> | undefined;
  let excludeDirectories: Array<string> | undefined;
  let excludeGitRemotes: Array<string> | undefined;
  let hasUpload: boolean | undefined;

  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case '--num-items':
        numItems = parseInt(argv[++i], 10);
        break;
      case '--cursor':
        cursor = argv[++i];
        break;
      case '--user-id':
        userId = argv[++i];
        break;
      case '--project-directory':
        projectDirectory = argv[++i];
        break;
      case '--project-git-remote':
        projectGitRemote = argv[++i];
        break;
      case '--exclude-user-ids':
        excludeUserIds = argv[++i].split(',');
        break;
      case '--exclude-directories':
        excludeDirectories = argv[++i].split(',');
        break;
      case '--exclude-git-remotes':
        excludeGitRemotes = argv[++i].split(',');
        break;
      case '--has-upload':
        hasUpload = argv[++i] === 'true';
        break;
    }
  }

  if (userId) {
    if (excludeUserIds || excludeDirectories || excludeGitRemotes) {
      console.error('Cannot mix --user-id with --exclude-* flags');
      process.exit(1);
    }
    const project = projectDirectory
      ? ({ directory: projectDirectory } as const)
      : projectGitRemote
        ? ({ gitRemote: projectGitRemote } as const)
        : undefined;
    return {
      numItems,
      cursor,
      filter: {
        type: 'include' as const,
        userId: userId as Id<'users'>,
        project,
        hasUpload,
      },
    };
  }

  if (excludeUserIds || excludeDirectories || excludeGitRemotes || hasUpload !== undefined) {
    return {
      numItems,
      cursor,
      filter: {
        type: 'exclude' as const,
        excludeUserIds: excludeUserIds as Array<Id<'users'>> | undefined,
        excludeDirectories,
        excludeGitRemotes,
        hasUpload,
      },
    };
  }

  return { numItems, cursor, filter: undefined };
}
