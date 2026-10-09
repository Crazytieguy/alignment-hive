import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { expect, test } from 'bun:test';

// A plugin cannot import another plugin's files, so every plugin that draws notices in the band
// above the prompt carries its own hooks/notice-band.tsx. After editing one, copy it to the
// others: bun run --filter '@alignment-hive/plugins' sync-notice-band <plugin you edited>
test('every plugin carries the same notice-band.tsx', async () => {
  const paths = Array.from(new Bun.Glob('*/hooks/notice-band.tsx').scanSync(import.meta.dir)).sort();
  expect(paths.length).toBeGreaterThan(1);
  const copies = await Promise.all(paths.map((path) => readFile(join(import.meta.dir, path), 'utf8')));
  const differing = paths.filter((_, index) => copies[index] !== copies[0]);
  expect({ differing, from: paths[0] }).toEqual({ differing: [], from: paths[0] });
});
