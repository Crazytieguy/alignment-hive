import { readFileSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterEach, describe, expect, test } from 'bun:test';
import { parseHTML } from 'linkedom';
import { FIXTURE_SESSION, createReviewFixture, prompt, reply, reviewDocument, reviewItem, writeTranscript } from './fixtures';
import { assembleReview, prepareReview } from './render';
import { reviewTimes } from './times';

const fixtures: Array<Awaited<ReturnType<typeof createReviewFixture>>> = [];
/** A time's text as shown: without its hidden parts. */
const visibleText = (el: Element) => [...el.childNodes].filter((node) => !(node.nodeType === 1 && (node as Element).hasAttribute('hidden'))).map((node) => node.textContent).join('');
afterEach(async () => { await Promise.all(fixtures.splice(0).map((f) => f.cleanup())); });
async function render(document: string, setup?: (f: Awaited<ReturnType<typeof createReviewFixture>>) => Promise<void>) {
  const f = await createReviewFixture(); fixtures.push(f); await f.stamp();
  await f.write('notes.txt', 'evidence\n');
  await setup?.(f);
  const input = join(f.root, 'debrief.md');
  await writeFile(input, document);
  const html = assembleReview(await prepareReview(input, { cwd: f.repo, projects: f.projects }), { reviewId: 'review-id' }).html;
  return { html, document: parseHTML(html).document, f };
}
const texts = (nodes: Iterable<Element>) => [...nodes].map((node) => node.textContent);
const inSection = (section: string, id: string, body = '', extra = '') => reviewItem(id, body, extra).replace('section: main', `section: ${section}`);

/** The structure fixture with its session, endpoints and asks swapped for the fixture's, and every ref pointing at one file. */
function wholePage(): string {
  return readFileSync(join(import.meta.dir, 'fixtures/structure.md'), 'utf8')
    .replace(/^session: .*$/m, `session: ${FIXTURE_SESSION}`).replace(/^(base|head|asks): .*\n/gm, '')
    .replace(/```ref\n[\s\S]*?```/g, '```ref\nfile: notes.txt\n```');
}

describe('page shell', () => {
  test('a whole page: header, six sections in order, 15 items, marks, alternatives, one empty item, the rail', async () => {
    const { html, document } = await render(wholePage());
    expect(document.querySelector('title')!.textContent).toBe('Rain-aware watering');
    const head = document.querySelector('.page > main.content > header.head')!;
    expect(head.querySelector('h1.title')!.textContent).toBe('Why the garden was watered in the rain, and the fix that landed');
    expect([...head.querySelectorAll('.stats > li')].map((li) => [li.className, li.querySelector('a')!.getAttribute('href'), li.textContent])).toEqual([
      ['warn', '#item-git', '1 commit on main, not pushed'], ['', '#item-git', '4 files, +120 \u221230'], ['', '#item-tests-lint', '42 tests pass, lint clean'],
    ]);
    expect(head.querySelectorAll('.story > p')).toHaveLength(2);
    expect(head.querySelector('.foryou > h2')!.textContent).toBe('Left for you');
    expect(head.querySelectorAll('.foryou > ol > li')).toHaveLength(2);
    expect(head.querySelector('.foryou code')!.textContent).toBe('water status');
    const sections = [...document.querySelectorAll('main > section.sec')];
    expect(sections.map((sec) => [sec.id, sec.getAttribute('data-sec')])).toEqual([
      ['sec-wrong', 'What was wrong'], ['sec-fix', 'The fix'], ['sec-checked', 'How it was checked'],
      ['sec-unverified', 'Not verified'], ['sec-landing', 'Landing'], ['sec-side-effects', 'Side effects'],
    ]);
    expect(sections.map((sec) => sec.querySelector('.sec-sub')?.textContent ?? null)).toEqual([null, null, null,
      "Checks that weren't run but would raise confidence in conclusions above. Ask for any of them to be run.", null,
      "Ask me to undo any that can be undone, or flag any you'd rather I not do again."]);
    const items = [...document.querySelectorAll('section.sec > details.item')];
    expect(items).toHaveLength(15);
    expect(sections.map((sec) => sec.querySelectorAll(':scope > details.item').length)).toEqual([1, 4, 3, 1, 3, 3]);
    expect(items.every((item) => !item.hasAttribute('open'))).toBe(true);
    expect(document.querySelectorAll('details.item[data-attn]')).toHaveLength(8);
    expect(texts(document.querySelectorAll('.item-meta > .tag.tag-attn'))).toEqual(Array(5).fill('judgement call'));
    expect(document.querySelectorAll('[data-attn] .tag-attn')).toHaveLength(5);
    const alternatives = [...document.querySelectorAll('.item-body > p.line.line-alternative')];
    expect(alternatives).toHaveLength(6);
    expect(alternatives.every((line) => line.querySelector('b')!.textContent === 'Alternative')).toBe(true);
    for (const item of items) {
      const children = [...item.querySelector('.item-body')!.children];
      const firstFigure = children.findIndex((child) => child.matches('figure.ev'));
      const lastAlternative = children.findLastIndex((child) => child.matches('.line-alternative'));
      if (lastAlternative >= 0 && firstFigure >= 0) expect(lastAlternative).toBe(firstFigure - 1);
    }
    expect(document.querySelector('#item-fail-open .line-alternative code')).not.toBeNull();
    expect([...document.querySelectorAll('details.item-empty')].map((item) => item.id)).toEqual(['item-temp-files']);
    expect(document.querySelector('#item-temp-files .item-body')!.innerHTML).toBe('');
    expect(document.querySelector('#item-diagnosis .item-h')!.getAttribute('data-nav')).toBe('Why it watered');
    expect(document.querySelector('#item-rain-limit .item-h code')!.textContent).toBe('WATER_RAIN_LIMIT');
    expect(document.querySelector('#item-copy .item-body > ul.copy-pair')).not.toBeNull();
    expect(items.every((item) => item.querySelector('summary > label.seen > input[type="checkbox"]'))).toBe(true);
    const rail = document.querySelector('.page > aside.rail')!;
    expect(rail.querySelector('details#rail-fold > summary')!.textContent).toBe('Contents');
    expect(rail.querySelector('nav#nav')!.getAttribute('aria-label')).toBe('Contents');
    expect([...rail.querySelectorAll('[data-theme-pick]')].map((b) => [b.getAttribute('data-theme-pick'), b.textContent, b.getAttribute('aria-pressed')])).toEqual([['auto', 'Auto', 'true'], ['light', 'Light', 'false'], ['dark', 'Dark', 'false']]);
    expect(document.querySelector('main')!.getAttribute('data-review')).toBe('review-id');
    for (const leftover of ['data-conf', 'data-rank', 'data-kind', 'id="ranked"', 'data-order', 'class="rank"', 'conf-toggle']) expect(html).not.toContain(leftover);
    expect(html).toContain('<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Mono');
    expect([...html].every((character) => character.charCodeAt(0) < 128)).toBe(true);
  }, 30_000);

  test('story sections keep their declared order, fixed sections follow in theirs, and empty ones are left out', async () => {
    const items = [inSection('side-effects', 'late'), inSection('second', 'b', 'Body.'), inSection('checked', 'check'), reviewItem('a', 'Body.')].join('');
    const { document } = await render(reviewDocument(items).replace('sections: [{ id: main, title: Main }]', 'sections: [{ id: main, title: Main }, { id: empty, title: Empty }, { id: second, title: "Second & last" }]'));
    expect([...document.querySelectorAll('section.sec')].map((sec) => [sec.id, sec.getAttribute('data-sec'), texts(sec.querySelectorAll('.item-h'))])).toEqual([
      ['sec-main', 'Main', ['Changed a']], ['sec-second', 'Second & last', ['Changed b']], ['sec-checked', 'How it was checked', ['Changed check']], ['sec-side-effects', 'Side effects', ['Changed late']],
    ]);
  });

  test('the judgement-call tag needs alternatives; the rail mark follows judgement-call alone', async () => {
    const { document } = await render(reviewDocument(inSection('side-effects', 'probe', '', 'judgement-call: true\n') + reviewItem('choice', '', 'judgement-call: true\nalternatives: ["Do *less*"]\n')));
    expect([document.querySelector('#item-probe')!.hasAttribute('data-attn'), document.querySelector('#item-probe .tag')]).toEqual([true, null]);
    expect(document.querySelector('#item-probe')!.classList.contains('item-empty')).toBe(true);
    expect(document.querySelector('#item-choice .tag-attn')!.textContent).toBe('judgement call');
    expect(document.querySelector('#item-choice')!.classList.contains('item-empty')).toBe(false);
    expect(document.querySelector('#item-choice .line-alternative')!.innerHTML).toBe('<b>Alternative</b>Do <em>less</em>');
  });


  const routes: Array<[string, (image: string) => string, string, string]> = [
    ['an item heading', (image) => reviewDocument(reviewItem('bad').replace('## Changed bad', `## See ${image}`)), 'Item "bad", line 8', '#item-bad .item-h'],
    ['a lede', (image) => reviewDocument(reviewItem('bad').replace('lede: Preserve compatibility', `lede: "See ${image}"`)), 'Item "bad", line 8', '#item-bad .item-lede'],
    ['an alternative', (image) => reviewDocument(reviewItem('bad', '', `judgement-call: true\nalternatives: ["See ${image}"]\n`)), 'Item "bad", line 8', '#item-bad .line-alternative'],
    ['the page heading', (image) => reviewDocument(reviewItem('fine')).replace('heading: Review the change', `heading: "See ${image}"`), 'Item "page", line 3', 'h1.title'],
    ['a left-for-you line', (image) => reviewDocument(reviewItem('fine'), `foryou: ["See ${image}"]\n`), 'Item "page", line 7', '.foryou li'],
  ];
  test.each(routes)('%s rejects remote and relative Markdown images with the item and line, and keeps data images', async (_, document, where, selector) => {
    for (const image of ['![x](https://example.invalid/p.png)', '![x](p.png)']) await expect(render(document(image))).rejects.toThrow(`${where}: Markdown images must use data:`);
    const img = (await render(document('![x](data:image/png;base64,AA==)'))).document.querySelector(`${selector} img`)!;
    expect(img.getAttribute('src')).toBe('data:image/png;base64,AA==');
  }, 30_000);

  test('a stat label is plain text, never an image', async () => {
    const { document } = await render(reviewDocument(reviewItem('fine'), 'stats: [{ n: 1, label: "See ![x](https://example.invalid/p.png)", item: fine }]\n'));
    expect([document.querySelector('.stats img'), document.querySelector('.stats a')!.textContent]).toEqual([null, '1 See ![x](https://example.invalid/p.png)']);
  }, 30_000);
});

describe('lower-priority tray', () => {
  test('a section\'s lower-priority items fold into one tray after its main items, named by nav or heading', async () => {
    const quiet = (id: string, extra = '') => reviewItem(id, 'Details.', `lower-priority: true\n${extra}`);
    const { document } = await render(reviewDocument(reviewItem('main-one', 'Body.') + quiet('quiet-one', 'nav: The first\n') + quiet('quiet-two').replace('## Changed quiet-two', '## The `second` one') + reviewItem('other', 'Body.').replace('section: main', 'section: checked')));
    const section = document.getElementById('sec-main')!;
    expect([...section.children].map((el) => el.tagName === 'DETAILS' ? `${el.className}#${el.id}` : el.tagName)).toEqual(['H2', 'item#item-main-one', 'rest#rest-main']);
    const tray = document.getElementById('rest-main')!;
    expect([tray.getAttribute('data-count'), tray.querySelector('.rest-label')!.textContent, tray.querySelector('.rest-count')!.textContent]).toEqual(['2', 'Lower priority2', '2']);
    expect([tray.querySelectorAll('.rest-dots i').length, tray.querySelector('.rest-seen-t')!.textContent]).toEqual([2, '0 of 2 seen']);
    expect(tray.querySelector('.rest-preview')!.innerHTML).toBe('The first<span class="rest-sep" aria-hidden="true"> · </span>The <code>second</code> one');
    const row = document.getElementById('item-quiet-one')!;
    expect([row.className, row.hasAttribute('data-quiet'), row.parentElement!.className]).toEqual(['item item-quiet', true, 'rest-rows']);
    expect([row.querySelector('summary > .item-lede'), row.querySelector('summary > .item-meta') !== null, row.querySelector('summary .tag-attn')]).toEqual([null, true, null]);
    expect(row.querySelector('.item-body > p.item-lede.item-lede-in:first-child')!.textContent).toBe('Preserve compatibility');
    expect(document.querySelector('#sec-checked .rest')).toBeNull();
  }, 30_000);

  test('a section of lower-priority items only is its title and its tray', async () => {
    const { document } = await render(reviewDocument(reviewItem('main-one', 'Body.') + reviewItem('only', 'Body.', 'lower-priority: true\n').replace('section: main', 'section: checked')));
    expect([...document.getElementById('sec-checked')!.children].map((el) => el.className)).toEqual(['sec-title', 'rest']);
  }, 30_000);
});

describe('all of the user\'s messages', () => {
  test('the others wait hidden behind "All N of your messages", N counting the user\'s own; none without a key ask', async () => {
    const setup = async (f: Awaited<ReturnType<typeof createReviewFixture>>) => { await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('One'), reply('ok'), prompt('Two'), reply('ok'), prompt('Three')]); };
    const { document } = await render(reviewDocument(reviewItem('one'), 'asks: ["11111111:3"]\n'), setup);
    expect([...document.querySelectorAll('.asks > li')].map((li) => [li.className, li.hasAttribute('hidden'), li.querySelector('q')!.textContent])).toEqual([['ask-rest', true, 'One'], ['ask-key', false, 'Two'], ['ask-rest', true, 'Three']]);
    const all = document.querySelector('.asks + .asks-all')!;
    expect([all.textContent, all.getAttribute('aria-expanded'), all.getAttribute('data-more'), all.getAttribute('data-less')]).toEqual(['All 3 of your messages', 'false', 'All 3 of your messages', 'Only the ones that set direction']);
    expect((await render(reviewDocument(reviewItem('one')), setup)).document.querySelector('.asks, .asks-all')).toBeNull();
  }, 30_000);
});

describe('key asks from another session', () => {
  test('an ask another session sent names its sender before the quote; the user\'s own keep theirs', async () => {
    const { document } = await render(reviewDocument(reviewItem('one'), 'asks: ["11111111:1", "11111111:3"]\n'), async (f) => {
      await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('Mine'), reply('ok'), prompt('<agent-message from="manager">Theirs</agent-message>')]);
    });
    expect([...document.querySelectorAll('.asks > li > div')].map((div) => [div.className, div.querySelector('.ask-from')?.textContent ?? null])).toEqual([['', null], ['ask-by', 'PEER']]);
  }, 30_000);
});

describe('one-line fields', () => {
  test('raw HTML in a heading, lede, alternative, caption or summary is text; an item body keeps it', async () => {
    const body = '<kbd>K</kbd> in the body.\n\n```ref\nfile: notes.txt\ncaption: "a <b>caption</b>"\n```\n\n```ref\ntranscript: 11111111:1\nsummary: "run <session>"\n```';
    const item = reviewItem('one', body, 'judgement-call: true\nalternatives: ["keep <em>it</em>"]\n').replace('## Changed one', '## Changed <session> handling').replace('lede: Preserve compatibility', 'lede: "Pass <session> to hive local"');
    const { document } = await render(reviewDocument(item), async (f) => {
      await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('Run it')]);
    });
    const shown = (selector: string) => document.querySelector(`#item-one ${selector}`)!;
    expect([shown('.item-h').textContent, shown('.item-lede').textContent, shown('.line-alternative').textContent.replace(shown('.line-alternative > b').textContent, ''), shown('.ev-cap').textContent, shown('.tr-sum').textContent])
      .toEqual(['Changed <session> handling', 'Pass <session> to hive local', 'keep <em>it</em>', 'a <b>caption</b>', 'run <session>']);
    expect(document.querySelectorAll('#item-one .item-h session, #item-one .item-lede session, #item-one .tr-sum session')).toHaveLength(0);
    const card = await render(reviewDocument(reviewItem('b'), 'foryou: ["<iframe></iframe>"]\n'));
    expect([card.document.querySelectorAll('iframe').length, card.document.querySelector('.foryou li')!.textContent]).toEqual([0, '<iframe></iframe>']);
    expect(shown('.item-body > p kbd').textContent).toBe('K');
  }, 30_000);
});

describe('saved UI state keys', () => {
  test('each fold and ask carries what it shows as data-key; no positional key list', async () => {
    const body = '```ref\ntranscript: 11111111:3, 11111111:1\nsummary: [Reply, Ask]\n```\n\n```ref\nfile: notes.txt\nrange: L1\n```\n\n```ref\ndiff: source.ts\n```\n\n```ref\ntranscript: 11111111:3\nsummary: Reply again\n```';
    const { document } = await render(reviewDocument(reviewItem('one', body), 'asks: ["11111111:1"]\n'), async (f) => {
      await f.write('source.ts', 'export const value = 2;\n');
      await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('Ask'), reply('ok'), reply('Reply')]);
    });
    expect([...document.querySelectorAll('#item-one [data-key]')].map((el) => [el.className, el.getAttribute('data-key')])).toEqual([
      ['tr-entry', `tr:${FIXTURE_SESSION}:3`], ['tr-entry', `tr:${FIXTURE_SESSION}:1`], ['ev-box fv', 'file:notes.txt@disk:L1'], ['tr-entry', `tr:${FIXTURE_SESSION}:3`],
    ]);
    expect(document.querySelector('.asks li')!.getAttribute('data-key')).toBe(`ask:${FIXTURE_SESSION}:1`);
    expect(document.getElementById('review-ui')).toBeNull();
  }, 30_000);
});

describe('dates and times', () => {
  test('the as-of date comes from the parent transcript\'s end, as ISO with UTC text', async () => {
    const { document } = await render(reviewDocument(reviewItem('one'), 'foryou: ["Re-login"]\nasks: ["11111111:1", "11111111:3"]\n'), async (f) => {
      await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt('First\nsecond line'), reply('ok'), prompt('Next')]);
    });
    const title = document.querySelector('.foryou h2')!;
    expect([title.textContent, title.querySelector('time')!.getAttribute('datetime')]).toEqual(['Left for you, as of Sep 16', '2026-09-16T03:02:00.000Z']);
    expect([...document.querySelectorAll('.asks > li')].map((li) => [li.querySelector('time')!.getAttribute('datetime'), visibleText(li.querySelector('time')!), li.querySelector('q')!.textContent])).toEqual([
      ['2026-09-16T03:00:00.000Z', 'Sep 16 03:00', 'First\nsecond line'], ['2026-09-16T03:02:00.000Z', '03:02', 'Next'],
    ]);
  }, 30_000);

  test('formats: stamps, days and an ask\'s day and time, in any zone', () => {
    const utc = reviewTimes('UTC'), la = reviewTimes('America/Los_Angeles');
    expect(utc.stamp('2026-09-05T10:16:09.824Z')).toBe('Sep 5 10:16');
    expect(la.stamp('2026-09-05T10:16:09.824Z')).toBe('Sep 5 03:16');
    expect(utc.stamp('2026-09-05T00:05:00+02:00')).toBe('Sep 4 22:05');
    // Across midnight UTC: one day in Los Angeles, two in UTC; the key tells years apart.
    expect(['2026-09-07T23:50:00Z', '2026-09-08T00:26:19Z'].map(la.dayTime)).toEqual([{ day: 'Sep 7', time: '16:50', key: '2026-Sep-7' }, { day: 'Sep 7', time: '17:26', key: '2026-Sep-7' }]);
    expect(['2026-09-07T23:50:00Z', '2026-09-08T00:26:19Z'].map(utc.dayTime)).toEqual([{ day: 'Sep 7', time: '23:50', key: '2026-Sep-7' }, { day: 'Sep 8', time: '00:26', key: '2026-Sep-8' }]);
    expect(utc.dayTime('2025-09-08T00:26:19Z').key).not.toBe(utc.dayTime('2026-09-08T00:26:19Z').key);
    expect(la.day('2026-09-08T00:28:29Z')).toBe('Sep 7');
  });

  test('each ask is one line of its verbatim text; the day shows where it changes; an elision marks the cut', async () => {
    const long = Array.from({ length: 9 }, (_, i) => `line <${i + 1}>`).join('\n');
    const { document } = await render(reviewDocument(reviewItem('one'), 'asks: [{ ask: "11111111:5", elide: { from: "line <3>", until: "line <9>", note: "six lines" } }, "11111111:1", "11111111:3"]\n'), async (f) => {
      await writeTranscript(join(f.root, 'transcripts', `${FIXTURE_SESSION}.jsonl`), FIXTURE_SESSION, [prompt(long), reply('ok'), prompt('Next'), reply('ok'), prompt(long, '2026-09-17T01:00:00.000Z')]);
    });
    const asks = [...document.querySelectorAll('.asks > li')];
    expect(asks.map((li) => [li.className, li.getAttribute('data-key'), visibleText(li.querySelector('time')!), li.querySelector(':scope > div > q')!.innerHTML])).toEqual([
      ['ask-key', `ask:${FIXTURE_SESSION}:1`, 'Sep 16 03:00', long.replaceAll('<', '&lt;').replaceAll('>', '&gt;')],
      ['ask-key', `ask:${FIXTURE_SESSION}:3`, '03:02', 'Next'],
      ['ask-key', `ask:${FIXTURE_SESSION}:5`, 'Sep 17 01:00', 'line &lt;1&gt;\nline &lt;2&gt;\n<span class="ask-elide">[six lines]</span>\n\nline &lt;9&gt;'],
    ]);
    expect(document.querySelectorAll('.asks button, .asks .clip, .asks .tr-wrap')).toHaveLength(0);
  }, 30_000);
});
