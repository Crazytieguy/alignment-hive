import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterEach, expect, test } from 'bun:test';
import { FIXTURE_SESSION, createReviewFixture, prompt, reply, reviewDocument, reviewItem, syntheticSecret, writeTranscript } from './fixtures';
import { readerView } from './view';

const fixtures: Array<Awaited<ReturnType<typeof createReviewFixture>>> = [];
afterEach(async () => { await Promise.all(fixtures.splice(0).map((f) => f.cleanup())); });

test('the reader view: READMEs as of base, the user\'s own messages, the page top, and items as the page lays them out', async () => {
  const f = await createReviewFixture(); fixtures.push(f);
  const secret = syntheticSecret(28, 11);
  await f.write('README.md', 'Root readme at base.\n');
  await f.write('docs/README.md', 'Docs readme at base.\n');
  await f.write('other/README.md', 'Unrelated readme.\n');
  await f.git('add', '.'); await f.git('commit', '-qm', 'Readmes');
  await f.stamp(); await writeFile(join(f.stateDir, `${FIXTURE_SESSION}-commit.txt`), await f.git('rev-parse', 'HEAD') + '\n');
  await f.write('README.md', 'Root readme edited later.\n');
  await f.write('docs/guide/page.md', 'changed\n');
  await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('Make the change'), reply('Done.'), prompt(`Also this https://app.example.test/invite/${secret}`)]);
  const body = 'First paragraph.\n\n- a list\n- of two\n\n```ref\ndiff: docs/guide/page.md\ntitle: The page\n```\n\nLast paragraph.';
  const nested = '- a point\n\n  ```ref\n  diff: docs/guide/page.md\n  ```\n- another';
  const input = join(f.root, 'debrief.md');
  await writeFile(input, reviewDocument(
    reviewItem('quiet', '', 'lower-priority: true\n') + reviewItem('one', body, 'judgement-call: true\nalternatives: [Do it another way]\n') + reviewItem('nested', nested),
    'foryou: [Land it]\n',
  ));

  const view = await readerView(input, { cwd: f.repo, projects: f.projects });
  expect(view.readmes).toEqual([{ path: 'README.md', text: 'Root readme at base.\n' }, { path: 'docs/README.md', text: 'Docs readme at base.\n' }]);
  expect(view.messages[0]).toBe('Make the change');
  expect(view.messages).toHaveLength(2);
  expect(view.messages[1]).not.toContain(secret);
  expect(view.top).toEqual({ title: 'Fixture review', heading: 'Review the change', story: 'The change, reviewed.', stats: [], foryou: ['Land it'] });
  // Main items first, then the section's lower-priority tray, whose lede opens the body.
  expect(view.items.map((item) => [item.id, item.lede, item.isJudgementCall, item.isLowerPriority])).toEqual([
    ['one', 'Preserve compatibility', true, false],
    ['nested', 'Preserve compatibility', false, false],
    ['quiet', undefined, false, true],
  ]);
  expect(view.items[0].pieces).toEqual(['First paragraph.', '- a list\n- of two', 'Alternative: Do it another way', '[Folded diff: The page]', 'Last paragraph.']);
  expect(view.items[1].pieces).toEqual(['- a point\n\n  [Folded diff: docs/guide/page.md]\n- another']);
  expect(view.items[2].pieces).toEqual(['Preserve compatibility']);
});
