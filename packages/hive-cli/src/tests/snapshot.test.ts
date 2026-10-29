import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, expect, test } from 'bun:test';
import { sessionFiles } from '../lib/locators';
import { remembered, snapshot } from '../lib/snapshot';

const S1 = 'a4eff20e-647c-4390-8d0a-0560616839cf';
const S2 = 'bbc0a27d-5ee3-464c-8a0c-5a47e34f5499';

let dir: string;
beforeAll(async () => {
  dir = await mkdtemp(join(tmpdir(), 'hive-snapshot-'));
  await writeFile(join(dir, `${S1}.jsonl`), '{}\n');
});
afterAll(() => rm(dir, { recursive: true, force: true }));

const ids = async () => (await sessionFiles([dir])).map((r) => r.session).sort();

test('inside a snapshot a directory is listed once; a new snapshot, or none, lists it again', async () => {
  await snapshot(async () => {
    expect(await ids()).toEqual([S1]);
    await writeFile(join(dir, `${S2}.jsonl`), '{}\n');
    expect(await ids()).toEqual([S1]);
  });
  expect(await ids()).toEqual([S1, S2]);
  expect(await snapshot(ids)).toEqual([S1, S2]);
});

test('remembered reads once per key within a snapshot and every time outside one', async () => {
  let reads = 0;
  const read = () => ++reads;
  expect([remembered('k', read), remembered('k', read)]).toEqual([1, 2]);
  await snapshot(async () => {
    expect([remembered('k', read), remembered('k', read), remembered('other', read)]).toEqual([3, 3, 4]);
  });
});
