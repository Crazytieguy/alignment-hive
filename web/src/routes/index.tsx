import { createFileRoute, useSearch } from '@tanstack/react-router';
import { getAuth, getSignInUrl } from '@workos/authkit-tanstack-react-start';
import { Button } from '@/components/ui/button';

export const Route = createFileRoute('/')({
  component: Home,
  loader: async () => {
    const { user } = await getAuth();
    const signInUrl = await getSignInUrl();

    return { user, signInUrl };
  },
});

function Home() {
  const { user, signInUrl } = Route.useLoaderData();

  return (
    <div className="min-h-screen bg-gradient-to-br from-slate-50 to-slate-100 dark:from-slate-950 dark:to-slate-900 flex flex-col">
      <nav className="px-6 py-4 border-b border-slate-200 dark:border-slate-800">
        <div className="max-w-4xl mx-auto">
          <h2 className="text-lg font-semibold text-slate-900 dark:text-slate-100">
            Alignment Hive
          </h2>
        </div>
      </nav>

      <main className="flex-1 flex flex-col items-center justify-center px-6 py-12">
        <div className="w-full max-w-lg space-y-8">
          <div className="space-y-3">
            <h1 className="text-3xl font-serif font-bold text-slate-900 dark:text-slate-100">
              Shared AI agent infrastructure for alignment researchers.
            </h1>
          </div>

          <div className="flex flex-col gap-4">
            {user ? (
              <Button asChild size="lg" className="w-full">
                <a href="/welcome">Setup instructions</a>
              </Button>
            ) : (
              <>
                <Button asChild size="lg" className="w-full">
                  <a href={signInUrl}>Log in</a>
                </Button>
                <div className="text-sm text-center">
                  <p className="text-slate-600 dark:text-slate-400">
                    To request access,{' '}
                    <a
                      href="mailto:yoav.tzfati@gmail.com"
                      className="text-blue-600 dark:text-blue-400 underline hover:text-blue-700 dark:hover:text-blue-300"
                    >
                      email me
                    </a>
                  </p>
                </div>
              </>
            )}
          </div>
        </div>
      </main>

      <ErrorBanner />
    </div>
  );
}

function ErrorBanner() {
  const search = useSearch({ from: '/' });

  if (!search.error) return null;

  return (
    <div className="fixed bottom-4 right-4 max-w-sm p-4 bg-red-50 dark:bg-red-950/20 border border-red-200 dark:border-red-800 rounded-lg shadow-lg">
      <p className="text-sm text-red-800 dark:text-red-200">
        {search.error === 'auth_failed'
          ? 'Authentication failed. Please try again.'
          : 'Something went wrong.'}
      </p>
    </div>
  );
}
