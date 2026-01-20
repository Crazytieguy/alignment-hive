import { createFileRoute } from '@tanstack/react-router';
import { getAuth } from '@workos/authkit-tanstack-react-start';
import type { User } from '@workos/authkit-tanstack-react-start';
import { Github } from 'lucide-react';
import { Card } from '@/components/ui/card';

export const Route = createFileRoute('/_authenticated/welcome')({
  loader: async () => {
    const { user } = await getAuth();
    return user;
  },
  component: WelcomePage,
});

function WelcomePage() {
  const user = Route.useLoaderData() as User;

  return (
    <div className="min-h-screen flex items-center justify-center p-4">
      <Card className="max-w-2xl w-full p-8">
        <div className="flex items-center justify-between">
          <div>
            <h1 className="text-3xl font-semibold">
              You're connected
            </h1>
            <p className="text-muted-foreground">
              Signed in as {user.email}
            </p>
          </div>
          <a
            href="https://github.com/Crazytieguy/alignment-hive"
            target="_blank"
            rel="noopener noreferrer"
            className="text-muted-foreground hover:text-foreground transition-colors"
            aria-label="GitHub"
          >
            <Github size={30} />
          </a>
        </div>

        <h2 className="text-xl font-medium">Next steps</h2>
        <ol className="list-decimal list-inside space-y-3 text-sm">
          <li>
            Install Claude Code (
            <a
              href="https://code.claude.com/docs/en/overview"
              className="text-blue-600 dark:text-blue-400 underline hover:text-blue-700 dark:hover:text-blue-300"
              target="_blank"
              rel="noopener noreferrer"
            >
              official docs
            </a>
            ) if you haven't already:
            <pre className="p-2 bg-muted rounded text-xs overflow-x-auto">
              curl -fsSL https://claude.ai/install.sh | bash
            </pre>
          </li>
          <li>
            Run Claude Code (the remaining steps happen inside it):
            <pre className="p-2 bg-muted rounded text-xs">claude</pre>
          </li>
          <li>
            Add the alignment-hive marketplace:
            <pre className="p-2 bg-muted rounded text-xs">
              /plugin marketplace add Crazytieguy/alignment-hive
            </pre>
          </li>
          <li>
            Enable auto-update:
            <ol className="list-decimal list-inside ml-4 space-y-1">
              <li>Run <code className="bg-muted px-1 rounded text-xs">/plugin</code></li>
              <li>Go to the <strong>Marketplaces</strong> tab</li>
              <li>Select <strong>alignment-hive</strong></li>
              <li>Select <strong>Enable auto-update</strong></li>
              <li>Press <strong>Esc</strong> twice to exit the menu</li>
            </ol>
          </li>
          <li>
            Install the mats plugin (in user scope):
            <pre className="p-2 bg-muted rounded text-xs">
              /plugin install mats@alignment-hive
            </pre>
          </li>
          <li>
            Exit Claude Code:
            <pre className="p-2 bg-muted rounded text-xs">/exit</pre>
          </li>
          <li>
            Navigate to your project directory and ask Claude to help you set it up:
            <pre className="p-2 bg-muted rounded text-xs">
              cd ~/my-project && claude
            </pre>
            <p className="mt-1 text-muted-foreground">
              Works for both new and existing projects.
            </p>
          </li>
        </ol>

        <div className="text-sm text-muted-foreground">
          <p>
            Questions? Message{' '}
            <strong>Yoav Tzfati</strong> on Slack in{' '}
            <code className="bg-muted px-1 rounded text-xs">#ai-tools</code> or{' '}
            <code className="bg-muted px-1 rounded text-xs">#support-engineering</code>
            .
          </p>
        </div>
      </Card>
    </div>
  );
}
