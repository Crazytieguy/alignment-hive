import { execSync } from 'node:child_process';
import { fetchRequestHandler } from '@trpc/server/adapters/fetch';
// Embedded at compile time by Bun. The .bundle extension avoids Bun's HTML bundling feature.
import reviewHtmlPath from '../../../review-app/dist/review.bundle' with { type: 'file' };
import { ensureStateDir, getConfig } from '../lib/config';
import { reviewCmd } from '../lib/messages';
import { printInfo, printSuccess } from '../lib/output';
import { createReviewRouter } from '../lib/review-router';

export async function uploadReview(): Promise<number> {
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);
  await ensureStateDir(stateDir);

  const html = await Bun.file(reviewHtmlPath).text();
  const router = createReviewRouter(stateDir, cwd);

  const server = Bun.serve({
    port: 0,
    hostname: 'localhost',
    idleTimeout: 30,
    async fetch(req) {
      const url = new URL(req.url);

      if (url.pathname.startsWith('/trpc')) {
        return fetchRequestHandler({
          endpoint: '/trpc',
          req,
          router,
          createContext: () => ({}),
        });
      }

      return new Response(html, {
        headers: { 'Content-Type': 'text/html; charset=utf-8' },
      });
    },
  });

  const url = `http://localhost:${server.port}`;
  printSuccess(reviewCmd.running(url));
  printInfo(reviewCmd.stopHint);

  try {
    if (process.platform === 'darwin') {
      execSync(`open "${url}"`, { stdio: 'ignore' });
    } else if (process.platform === 'linux') {
      execSync(`xdg-open "${url}"`, { stdio: 'ignore' });
    }
  } catch {
    // Browser open failed — URL is printed
  }

  // Keep server alive until Ctrl+C; never resolves
  return new Promise<number>(() => {});
}
