import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const read = (path: string) => readFileSync(fileURLToPath(new URL(path, import.meta.url)), 'utf8');

// The release is cut from package.json's version, and a pin on a version that is never released
// would hold stable back for good (.github/workflows/promote-stable.yml).
test('cli-version pins the hive-cli version in package.json', () => {
  const { version } = JSON.parse(read('../../../packages/hive-cli/package.json')) as { version: string };
  expect(read('../cli-version').trim()).toBe(version);
});
