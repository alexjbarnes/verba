import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../src/main.js', import.meta.url), 'utf8');
const start = source.indexOf('async function fetchSharedArticle');
const end = source.indexOf('// ── Sidebar & navigation', start);
assert.ok(start >= 0 && end > start);
const saved = [], opened = [];
let refreshes = 0;
let fetchFails = false;
const context = vm.createContext({
  invoke: async (command, args) => {
    assert.equal(command, 'library_add');
    saved.push(args);
    return { id: 'article-1' };
  },
  fetchArticleHtml: async () => {
    if (fetchFails) throw new Error('offline');
    return '<html></html>';
  },
  extractArticleSmart: async () => ({ title: 'Test article', body: 'Article text' }),
  loadLibrary: async () => { refreshes++; },
  openReading: id => opened.push(id),
  showToast: () => {},
});
vm.runInContext(source.slice(start, end), context);

await context.importSharedText('https://example.com/article');
await context.importSharedText('Shared plain text');
assert.equal(saved.length, 2);
assert.equal(refreshes, 2);
assert.equal(opened.length, 0);
assert.equal(saved[1].body, 'Shared plain text');

await context.importSharedText('https://example.com/manual', ['tag'], true);
assert.deepEqual(opened, ['article-1']);
assert.deepEqual(saved[2].tags, ['tag']);
assert.ok(source.includes('importSharedText(url, tags, true);'));

// A shared link that won't fetch must not swallow the share: the text is
// saved as a plain item instead, with no URL attached.
fetchFails = true;
await context.importSharedText('Some notes with https://example.com/x inside them');
assert.equal(saved[3].body, 'Some notes with https://example.com/x inside them');
assert.equal(saved[3].url, null);
assert.equal(opened.length, 1); // still not opened

// Headings survive extraction as markdown-style `##` prefixes and render as
// demoted heading elements; word counts and search strip the prefixes.
const text = v => ({ nodeType: 3, nodeValue: v });
const el2 = (tag, kids) => ({ nodeType: 1, tagName: tag, childNodes: kids, getAttribute: () => null });
const doc = { body: null, createTextNode: text, querySelectorAll: () => [] };
class DOMParserStub { parseFromString() { return doc; } }
const extractCtx = vm.createContext({ DOMParser: DOMParserStub });
doc.body = el2('#body', [
  el2('P', [text('population is increasing.')]),
  el2('H2', [text('Government reaction')]),
  el2('P', [text('To suggest we stock up supplies.')]),
  el2('H3', [text("The farmer's view")]),
]);
vm.runInContext(source.slice(source.indexOf('const BLOCK_TAGS'), source.indexOf('// Short display form')), extractCtx);
assert.equal(
  extractCtx.bodyWithImages('<h2>x</h2>', 'https://x/'),
  'population is increasing.\n\n## Government reaction\n\nTo suggest we stock up supplies.\n\n### The farmer\'s view');

// Hard-wrapped source text reads as one paragraph, as a browser would show
// it; <pre> keeps its own line breaks and indentation.
doc.body = el2('#body', [
  el2('P', [text('I have a lot of\nmixed feelings,\n  about this.')]),
  el2('PRE', [text('line one\n  indented')]),
]);
assert.equal(
  extractCtx.bodyWithImages('<p>x</p>', 'https://x/'),
  'I have a lot of mixed feelings, about this.\n\nline one\n  indented');

const el = { innerHTML: '', scrollTop: 0 };
const renderCtx = vm.createContext({
  document: { getElementById: () => el },
  escapeHtml: v => v.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;'),
  readingEndActionsHtml: () => '',
  IMG_MARKER: /!\[img\]\((https?:\/\/[^\s)]+)\)/g,
  LINK_MARKER: /\[([^\]\n]*)\]\((https?:\/\/[^\s)]+)\)/g,
});
vm.runInContext(source.slice(source.indexOf('function renderReading(text) {'), source.indexOf('// Actions at the natural end')), renderCtx);
renderCtx.renderReading('Intro\n\n## Government reaction\n\n### The farmer\'s view\n\n![img](https://example.com/a.jpg)\n\nSee: [Stock up](https://bbc.co/1) and [x](https://e.com/a&b)');
const out = el.innerHTML;
assert.match(out, /<h3 class="text-primary\/90 font-bold tracking-tight my-3"[^>]*>Government reaction<\/h3>/, out);
assert.match(out, /<h4[^>]*>The farmer's view<\/h4>/, out);
assert.match(out, /<img src="https:\/\/example\.com\/a\.jpg"/);
assert.match(out, /<a data-link="https:\/\/bbc\.co\/1" class="text-primary underline">Stock up<\/a>/, out);
assert.match(out, /data-link="https:\/\/e\.com\/a&amp;b"/, out);
assert.ok(!out.includes('href='), out);
const plain = out.replace(/<[^>]+>/g, '');
assert.ok(!plain.includes('##'), plain);
assert.ok(!plain.includes(']('), plain);

renderCtx.renderReading('Before.\n\n## Government reaction\n\nGovernment reaction? Body text.\n\nNext paragraph.');
assert.match(el.innerHTML, /<\/h3>Government reaction\? Body text\./);
assert.doesNotMatch(el.innerHTML, /<\/h[1-6]>\s/);
assert.ok(el.innerHTML.includes('Body text.\n\nNext paragraph.'));

// A linked image (<a><img></a>) must unwrap to a bare image marker, not get
// wrapped in link brackets (render would leave stray brackets around it).
const linkCtx = vm.createContext({});
vm.runInContext(source.slice(source.indexOf('const IMG_MARKER'), source.indexOf('// Extract an article')), linkCtx);
assert.equal(linkCtx.stripImageMarkers('[![img](https://x/a.jpg)](https://x/a.jpg)'), '');

const stripCtx = vm.createContext({});
vm.runInContext(source.slice(source.indexOf('const IMG_MARKER'), source.indexOf('// Extract an article')), stripCtx);
assert.equal(
  stripCtx.stripImageMarkers('## Head\n\nBody ![img](https://x/y.jpg)\n\n### Sub [link](https://x/y)'),
  'Head\n\nBody \n\nSub link');

console.log('Share import checks passed');
