import { fetchRequestHandler } from '@trpc/server/adapters/fetch';
// Embedded at compile time by Bun. The .bundle extension avoids Bun's HTML bundling feature.
import reviewHtmlPath from '../../../review-app/dist/review.bundle' with { type: 'file' };
import { openBrowser } from '../lib/browser';
import { ensureStateDir, getStateDir } from '../lib/config';
import { reviewCmd } from '../lib/messages';
import { printInfo, printSuccess } from '../lib/output';
import { createReviewRouter } from '../lib/review-router';

export async function uploadReview(): Promise<number> {
  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
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
        return fetchRequestHandler({ endpoint: '/trpc', req, router });
      }

      return new Response(html, {
        headers: { 'Content-Type': 'text/html; charset=utf-8' },
      });
    },
  });

  const url = `http://localhost:${server.port}`;
  printSuccess(reviewCmd.running(url));
  printInfo(reviewCmd.stopHint);

  await openBrowser(url);

  // Keep server alive until Ctrl+C; never resolves
  return new Promise<number>(() => {});
}
