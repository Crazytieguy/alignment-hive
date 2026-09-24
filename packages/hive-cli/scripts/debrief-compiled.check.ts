import { runCommand } from '../src/lib/spawn';
// Run explicitly: bun test ./scripts/debrief-compiled.check.ts
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { runInNewContext } from 'node:vm';
import { expect, test } from 'bun:test';
import { parseHTML } from 'linkedom';
import { createReviewFixture, reviewDocument, reviewItem } from '../src/debrief/fixtures';

async function run(argv: Array<string>, cwd: string) {
  const { stdout, stderr, exit } = await runCommand(argv, { cwd });
  expect({ exit, stderr: stderr.toString() }).toEqual({ exit: 0, stderr: '' });
  return stdout.toString();
}

test('compiled renderer captures, validates CSS, renders rounds, and emits executable seen state', async () => {
  const fixture = await createReviewFixture();
  try {
    await fixture.stamp();
    const entry = join(fixture.root, 'entry.ts'), binary = join(fixture.root, 'renderer');
    // Import the production modules, not a duplicate parser or a test-only CSS implementation.
    await writeFile(entry, `
import { captureReview } from ${JSON.stringify(join(import.meta.dir, '../src/debrief/capture.ts'))};
import { renderReview } from ${JSON.stringify(join(import.meta.dir, '../src/debrief/render.ts'))};
const [input, cwd, outDir, previousManifestPath, bun] = process.argv.slice(2);
await captureReview({ name: 'compiled', command: [bun, '-e', 'console.log("captured")'], cwd, outDir });
const result = await renderReview(input, { cwd, outDir, previousManifestPath: previousManifestPath || undefined });
console.log(JSON.stringify({ pagePath: result.pagePath, manifest: result.manifest }));
`);
    await run([process.execPath, 'build', '--compile', '--minify', '--no-compile-autoload-dotenv', entry, '--outfile', binary], fixture.root);
    const input = join(fixture.root, 'debrief.md');
    const body = '<style>@media (width <= 600px) { .sample { --fill: url(data:image/png;base64,AA==); color: blue } }</style>\n<div class="sample" style="background:var(--fill)">Local content</div>';
    const store = new Map<string, string>();
    let previous = '';
    for (const round of [1, 2]) {
      await writeFile(input, reviewDocument(reviewItem('choice', body + `\nRound ${round}`), `round: ${round}\n`));
      const outDir = join(fixture.root, `round${round}`);
      const result = JSON.parse(await run([binary, input, fixture.repo, outDir, previous, process.execPath], fixture.root)) as { pagePath: string; manifest: { reviewId: string } };
      expect(JSON.parse(await readFile(join(outDir, 'captures', 'compiled.json'), 'utf8'))).toMatchObject({ stdout: 'captured\n', exit: 0, cwd: fixture.repo });
      const page = await readFile(result.pagePath, 'utf8');
      expect(page).toContain('width <= 600px');
      const { document } = parseHTML(page);
      const inline = [...document.querySelectorAll('script')].filter((script) => !script.src && script.type !== 'application/json');
      const window = { localStorage: { getItem: (key: string) => store.get(key) ?? null, setItem: (key: string, value: string) => { store.set(key, value); } }, addEventListener: () => {} };
      // Execute the actual compiled-generated seen store and mount/overlay scripts; no source toString stand-in.
      const context = { document, window, DiffView: { render: () => {} } };
      runInNewContext(inline.find((script) => script.textContent.startsWith('var debriefSeen='))!.textContent + '\n' + inline.at(-1)!.textContent, context);
      expect(document.querySelectorAll('[data-unseen]').length).toBe(round === 1 ? 0 : 1);
      expect((JSON.parse(store.get(`review-seen:${result.manifest.reviewId}`)!) as { visit: unknown }).visit).toMatchObject({ highestRound: round });
      previous = join(outDir, 'manifest.json');
    }
    // Each invalid input reaches the same compiled renderer CSS resource guard.
    for (const [index, css] of ['@import "https://example.invalid/style.css";', '.x { background: url(https://example.invalid/a.png) }', '.x { --remote: image-set("https://example.invalid/a.png" 1x) }'].entries()) {
      await writeFile(input, reviewDocument(reviewItem('choice', `<style>${css}</style>`)));
      const result = await runCommand([binary, input, fixture.repo, join(fixture.root, `invalid${index}`), '', process.execPath], { cwd: fixture.root });
      const stdout = result.stdout.toString(), stderr = result.stderr.toString(), exit = result.exit;
      expect(exit).not.toBe(0);
      expect(stdout).toBe('');
      expect(stderr).toContain('Item "choice"');
      expect(stderr).toMatch(/Raw HTML CSS (cannot contain @import|resources must use)/);
      expect(stderr).not.toContain('Cannot find module');
    }
  } finally { await fixture.cleanup(); }
}, 60_000);
