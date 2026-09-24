import { runInNewContext } from 'node:vm';
import { describe, expect, test } from 'bun:test';
import MarkdownIt from 'markdown-it';
import { parseFragment } from 'parse5';
import {
  SCRIPT_URLS, escapeCss, escapeHtml, escapeJson, escapeScript,
  renderMarkdownHtml, validateMarkdownImages,
} from './html';
import type { DefaultTreeAdapterMap } from 'parse5';

const context = { itemId: 'prototype', line: 12 };
const ascii = (source: string) => expect([...source].every((character) => character.codePointAt(0)! < 128)).toBe(true);
const raw = (source: string) => renderMarkdownHtml(md(), source, context);
const md = () => new MarkdownIt({ html: true, linkify: false });

function text(source: string): string {
  const collect = (node: DefaultTreeAdapterMap['node']): string => {
    if ('value' in node) return node.value;
    return 'childNodes' in node ? node.childNodes.map(collect).join('') : '';
  };
  return parseFragment(source).childNodes.map(collect).join('');
}

describe('context-aware ASCII escaping', () => {
  test('HTML text and quoted attributes use numeric entities', () => {
    const input = 'é עברית 中文 😀 <>&"\'\n';
    const escaped = escapeHtml(input);
    ascii(escaped);
    expect(escaped).toContain('&#233;');
    expect(escaped).toContain('&#128512;');
    expect(escaped).not.toMatch(/&(?:amp|lt|gt|quot);/);
    expect(text(`<span title="${escaped}">${escaped}</span>`)).toBe(input);
  });

  test('JSON escapes script boundaries, non-ASCII, controls, and Unicode separators', () => {
    const value = { text: 'é😀\u2028\u2029</script>\u0000\u007f', quote: '"\\' };
    const escaped = escapeJson(value);
    ascii(escaped);
    expect(escaped).not.toContain('<');
    expect(escaped).toContain('\\u003c/script>');
    expect(JSON.parse(escaped)).toEqual(value);
    expect(() => escapeJson(undefined)).toThrow('not serializable');
  });

  test('ASCII author JavaScript is unchanged, including author-supplied escapes', () => {
    const source = String.raw`const caf\u00e9 = "\u00e9"; result = caf\u00e9;`;
    expect(escapeScript(source)).toBe(source);
    const sandbox = { result: '' };
    runInNewContext(source, sandbox);
    expect(sandbox.result).toBe('é');
  });

  test('forbidden C0/C1 evidence characters use visible escapes instead of browser replacement', () => {
    const result = escapeHtml('x' + String.fromCharCode(0, 27, 128) + '\t\n');
    expect(text(result)).toBe('x\\u0000\\u001b\\u0080\t\n');
  });

  test.each(['const café = "é";', 'String.raw`é`', 'const x = 1;', String.fromCharCode(0)])('rejects non-ASCII or control author scripts without rewriting: %j', (source) => {
    expect(() => escapeScript(source)).toThrow('Inline scripts must be ASCII');
    expect(() => raw(`<script>${source}</script>`)).toThrow('prototype');
  });

  test('JSON script types are data, modules are JS, unsupported inert Unicode is explicit', () => {
    const result = raw('<script type="application/json">{"text":"é😀"}</script><script type="module">export const value = 1;</script>');
    ascii(result.html);
    expect(result.html).toContain('{"text":"\\u00e9\\ud83d\\ude00"}');
    expect(() => raw('<script type="x-template">é</script>')).toThrow('inert script data');
  });

  test.each(['</script>', '</ScRiPt >', '</script\n>', '</script/>'])('guards closing-script sequence %s', (boundary) => {
    expect(() => escapeScript(`const text = '${boundary}';`)).toThrow('closing script');
  });

  test('CSS uses terminated hex escapes, including astral Unicode and style boundaries', () => {
    const escaped = escapeCss('p::after { content: "é😀</style>" }');
    ascii(escaped);
    expect(escaped).toContain('\\e9 ');
    expect(escaped).toContain('\\1f600 ');
    expect(escaped).not.toContain('<');
  });

  test('raw text, attributes, own script/style and SVG all receive their correct context', () => {
    const result = raw('<section title="é😀"><span>é😀</span><script>const value=1;</script><style>.café::before{content:"é😀"}</style><svg><text aria-label="é">😀</text></svg></section>');
    ascii(result.html);
    expect(result.html).toStartWith('<section');
    expect(result.html).toContain('title="&#233;&#128512;"');
    expect(result.html).toContain('const value');
    expect(result.html).toContain('.caf\\e9 ::before');
    expect(result.html).toContain('<text aria-label="&#233;">&#128512;</text>');
  });


});

describe('raw HTML structure and resource guards', () => {
  test.each(['<!doctype html>', '<html><p>ok</p></html>', '<head></head>', '<body>ok</body>', '</body>',
    '<link rel="stylesheet" href="data:text/css,a{}">', '<iframe src="data:text/html,x"></iframe>',
    '<object data="data:text/html,x"></object>', '<embed src="data:text/html,x">', '<base href="data:,x">',
    '<meta http-equiv="refresh" content="0;url=https://example.test">',
  ])('rejects forbidden authored structure %s', (source) => {
    expect(() => raw(source)).toThrow(/Item "prototype", line 12: Raw HTML cannot contain/);
  });

  test('wrapper-looking literals in comments and scripts are not treated as tags', () => {
    expect(() => raw('<!-- <body> --><script>const html = "<body>";</script>')).not.toThrow();
  });

  test.each(['<script>const answer = 1;', '<script src="data:text/javascript,void(0)">', '<svg><script>const x=1;</svg>'])('rejects unclosed scripts %s', (source) => {
    expect(() => raw(source)).toThrow('explicit closing tag');
  });

  test.each(['https://example.test/a', '//example.test/a', '/a', './a', 'javascript:alert(1)', 'file:///tmp/a', 'h&#116;tps://example.test'])('rejects unsupported raw URLs %s', (url) => {
    expect(() => raw(`<a href="${url}">link</a>`)).toThrow('href must use');
    expect(() => raw(`<img src="${url}">`)).toThrow('src must use');
  });

  test('permits data resources and local fragment anchors', () => {
    expect(() => raw('<a href="#target">here</a><img src="data:image/png;base64,AA=="><svg><use href="#target"></use><rect fill="url(#gradient)"></rect></svg>')).not.toThrow();
  });

  test('exports the four exact script pins, with no prefixes or alternate resource exceptions', () => {
    expect(SCRIPT_URLS).toEqual([
      'https://cdnjs.cloudflare.com/ajax/libs/jsdiff/9.0.0/diff.min.js',
      'https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.12.0/highlight.min.js',
      'https://cdnjs.cloudflare.com/ajax/libs/diff-match-patch/1.0.5/index.js',
      'https://cdn.jsdelivr.net/npm/markdown-it@15.0.1/dist/browser/markdown-it.umd.min.js',
    ]);
    for (const url of SCRIPT_URLS) {
      expect(() => raw(`<script src="${url}"></script>`)).not.toThrow();
      expect(() => raw(`<script src="${url}?x=1"></script>`)).toThrow();
      expect(() => raw(`<script src=" ${url}"></script>`)).toThrow();
      expect(() => raw(`<img src="${url}">`)).toThrow();
      expect(() => raw(`<a href="${url}">x</a>`)).toThrow();
    }
  });

  test.each([
    '<svg><g><image xlink:href="https://example.test/img"></image></g></svg>',
    '<svg><g><image href="https://example.test/img"></image></g></svg>',
    '<svg><foreignObject><iframe src="data:,x"></iframe></foreignObject></svg>',
    '<template><img src="https://example.test/img"></template>',
    '<video poster="https://example.test/poster"></video>',
    '<form action="https://example.test/send"></form>',
    '<a href="#a" ping="https://example.test/log">a</a>',
    '<svg><rect fill="url(https://example.test/fill)"></rect></svg>',
  ])('checks nested/alternate resource channels %s', (source) => {
    expect(() => raw(source)).toThrow();
  });

  test.each([
    'https://example.test/img 1x',
    'data:image/png;base64,AA== 1x, https://example.test/img 2x',
    'data:image/png;base64,AA==, https://example.test/img',
    'data:image/png;base64,AA== 1x, /img 2x',
    'data:image/png;base64,AA== 1x, img.png 2x',
  ])('rejects non-data srcset candidates %s', (srcset) => {
    expect(() => raw(`<img srcset="${srcset}">`)).toThrow('cannot contain srcset');
  });

  test('rejects even data srcsets instead of parsing descriptors', () => {
    expect(() => raw('<img srcset="data:image/png;base64,AA== 1x, data:image/png;base64,BB== 2x">')).toThrow('cannot contain srcset');
  });

  test('arbitrary trusted inline prototypes and event handlers remain available', () => {
    expect(() => raw('<button onclick="this.textContent = \'done\'">Run</button><script>document.querySelector("button").dataset.ready = "yes";</script>')).not.toThrow();
  });

  test('diagnostics identify the authored line', () => {
    expect(() => raw('<div>\n<span>ok</span>\n<img src="https://example.test/img">\n</div>')).toThrow('line 14');
  });
});

describe('CSS parser resource validation', () => {
  test.each([
    '@import "https://example.test/a.css";',
    '@IMPORT url(data:text/css,a{});',
    '@\\69mport "https://example.test/a.css";',
    '@im\\70ort/**/"https://example.test/a.css";',
    'a{background:url(https://example.test/img)}',
    'a{background:URL(https://example.test/img)}',
    'a{background:u\\72l(https://example.test/img)}',
    'a{background:\\75rl("https://example.test/img")}',
    'a{background:url(\\68ttps://example.test/img)}',
    'a{background:url(/*comment*/https://example.test/img)}',
    'a{background:image-set("https://example.test/img" 1x)}',
    'a{background:-webkit-image-set("https://example.test/img" 1x)}',
    'a{--remote:url(https://example.test/img);background:var(--remote)}',
    '@font-face{font-family:test;src:url(https://example.test/font)}',
  ])('rejects authored stylesheet resource escape %s', (source) => {
    expect(() => raw(`<style>${source}</style>`)).toThrow();
  });

  test('style attributes and SVG CSS values use the same canonical gate', () => {
    expect(() => raw('<div style="background:u\\72l(https://example.test/img)"></div>')).toThrow();
    expect(() => raw('<svg><rect fill="u\\72l(https://example.test/fill)"></rect></svg>')).toThrow();
  });

  test('preserves CSS media/container range comparison operators', () => {
    const source = '@media (width < 600px){.café{color:blue}}@container (width <= 800px){a{color:red}}';
    const escaped = escapeCss(source);
    expect(escaped).toContain('(width < 600px)');
    expect(escaped).toContain('(width <= 800px)');
    expect(raw(`<style>${source}</style>`).html).toContain('(width < 600px)');
  });

  test('comments and CSS strings are not interpreted as resource syntax', () => {
    expect(() => raw('<style>/* @import "https://example.test/a.css" */a::before{content:"url(https://example.test/img)"}</style>')).not.toThrow();
  });

  test('allows embedded CSS data and local paint-server references', () => {
    expect(() => raw('<style>a{background:url(data:image/png;base64,AA==);filter:url(#blur)}b{background:image-set("data:image/png;base64,AA==" 1x)}</style>')).not.toThrow();
  });
});

describe('complete Markdown fragment integration', () => {
  test('preserves normal inline open/close structure across emphasis and links', () => {
    const result = renderMarkdownHtml(md(), 'A <span title="é">**bold** and [docs](https://example.test)</span> end.', context);
    expect(result.html).toBe('<p>A <span title="&#233;"><strong>bold</strong> and <a href="https://example.test">docs</a></span> end.</p>\n');
  });

  test('inline script/style bodies are raw source, not Markdown or entity-decoded text', () => {
    const result = renderMarkdownHtml(md(), 'Before <script>const value = "*value* &amp;";</script> after <style>.café::after{content:"**é**"}</style> end.', context);
    expect(result.html).toContain('const value');
    expect(result.html).not.toContain('<em>');
    expect(result.html).toContain('content:"**\\e9 **"');
    expect(() => renderMarkdownHtml(md(), '`<script>` then <script>const s = "value";</script>', context)).not.toThrow();
    expect(() => renderMarkdownHtml(md(), 'Before <script>unclosed', context)).toThrow('explicit closing tag');
  });

  test('preserves nesting across raw blocks with intervening Markdown', () => {
    const result = renderMarkdownHtml(md(), '<section>\n\n**inside** [docs](https://example.test)\n\n</section>', context);
    expect(result.html).toContain('<section>\n<p><strong>inside</strong> <a href="https://example.test">docs</a></p>\n</section>');
  });

  test('author external anchors are rejected while ordinary Markdown links are accepted', () => {
    expect(() => renderMarkdownHtml(md(), '[docs](https://example.test)', context)).not.toThrow();
    expect(() => renderMarkdownHtml(md(), '[docs](https://example.test) <a href="https://example.test">raw</a>', context)).toThrow('href must use');
  });

  test('honors caller fence rules and leaves the shared renderer untouched', () => {
    const markdown = md();
    const original = markdown.renderer.rules.html_inline;
    markdown.renderer.rules.fence = () => '<pre data-evidence="yes">é</pre>';
    const result = renderMarkdownHtml(markdown, '```ref\nfile: x\n```\n\n<span>inside</span>', context);
    expect(result.html).toContain('<pre data-evidence="yes">&#233;</pre>');
    expect(markdown.renderer.rules.html_inline).toBe(original);
  });

  test('code fences and inline code do not become authored raw HTML', () => {
    expect(() => renderMarkdownHtml(md(), '`<iframe src="https://example.test">`\n\n```html\n<img src="https://example.test">\n```', context)).not.toThrow();
  });

  test('checks raw HTML in Markdown headings and nested list content', () => {
    expect(() => renderMarkdownHtml(md(), '### title <img src="https://example.test/img">', context)).toThrow();
    expect(() => renderMarkdownHtml(md(), '- item <a href="https://example.test">raw</a>', context)).toThrow();
  });
});

describe('diff Markdown image resource gate', () => {
  test.each(['![image](https://example.test/a.png)', '![image](//example.test/a.png)', '![image](/local.png)',
    '![image](local.png)', '![image][ref]\n\n[ref]: https://example.test/a.png',
    '[![image](https://example.test/a.png)](https://example.test)',
  ])('rejects image token %s', (source) => {
    expect(() => validateMarkdownImages(source, context)).toThrow('Markdown images must use data:');
  });

  test('permits data image tokens, ordinary links, code, and inert HTML', () => {
    expect(() => validateMarkdownImages('![image](data:image/png;base64,AA==)', context)).not.toThrow();
    expect(() => validateMarkdownImages('[link](https://example.test)\n\n`![image](https://example.test/a)`\n\n```md\n![image](https://example.test/a)\n```', context)).not.toThrow();
    expect(() => validateMarkdownImages('<img src="https://example.test/a">', context)).not.toThrow();
  });

  test('inline mode parses one line: images anywhere in it, and no block syntax', () => {
    expect(() => validateMarkdownImages('# See *[![x](local.png)](#a)*', context, true)).toThrow('Item "prototype", line 12: Markdown images must use data:');
    expect(() => validateMarkdownImages('See ![x](data:image/png;base64,AA==) and `![x](https://example.test/a)`', context, true)).not.toThrow();
  });
});

describe('red/green literal heuristic, not a rejection or accessibility audit', () => {
  test.each([
    'color:red;background:green', 'color:#f00;background:#0f08', 'color:#b22222;background:#228b22',
    'color:salmon;background:forestgreen',
  ])('warns for uncued red/green literals %s', (style) => {
    const result = raw(`<div style="${style}"></div>`);
    expect(result.warnings).toHaveLength(1);
    expect(result.warnings[0]).toContain('add a non-color cue');
  });

  test('allows owner red/green palette with labels or pattern cues', () => {
    expect(raw('<span style="color:red">Removed</span><span style="color:green">Added</span>').warnings).toEqual([]);
    expect(raw('<div style="color:red;background:green;border:1px dashed"></div>').warnings).toEqual([]);
    expect(raw('<svg><path stroke="red" stroke-dasharray="2 2"></path><path stroke="green"></path></svg>').warnings).toEqual([]);
  });

  test('does not scan prose, JS strings, unrelated generated markup, or one color alone', () => {
    expect(raw('<p>red green</p><script>const words = "red green";</script>').warnings).toEqual([]);
    expect(raw('<div style="color:red"></div>').warnings).toEqual([]);
    expect(raw('<style>.red .green { color: blue }</style>').warnings).toEqual([]);
  });
});
