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

// Longer text that cites links (a newsletter ending in a sources list) is the
// share itself, so it is saved as text even though its first link fetches.
fetchFails = false;
const roundup = 'Tech roundup\n\n' + 'Meta wrote up its agent orchestrator. '.repeat(10) +
  '\n\n## Sources\n\n- [Orchestrator](https://example.com/a)\n- [Retries](https://example.com/b)';
await context.importSharedText(roundup);
assert.equal(saved[4].body, roundup);
assert.equal(saved[4].url, null);

// A headline beside the link is still a link share.
await context.importSharedText('Some headline | Example News\nhttps://example.com/news');
assert.equal(saved[5].body, 'Article text');
assert.equal(saved[5].url, 'https://example.com/news');

// Headings survive extraction as markdown-style `##` prefixes and render as
// demoted heading elements; word counts and search strip the prefixes.
const text = v => ({ nodeType: 3, nodeValue: v });
const el2 = (tag, kids) => ({ nodeType: 1, tagName: tag, childNodes: kids, getAttribute: () => null });
const doc = { body: null, createTextNode: text, querySelectorAll: () => [] };
class DOMParserStub { parseFromString() { return doc; } }
const extractCtx = vm.createContext({ DOMParser: DOMParserStub, URL, normArticleUrl: u => u });
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

// A quote keeps a `> ` on every line, with a bare `>` between its paragraphs
// so it stays one quote. A space just inside a link still separates it from
// the next word.
const link = { getAttribute: n => (n === 'href' ? 'https://x/p' : null), textContent: 'Parkinson’s ' };
const para = el2('P', [text('story of '), link, text('is one.')]);
link.replaceWith = node => { para.childNodes[1] = node; };
doc.querySelectorAll = sel => (sel === 'a' ? [link] : []);
doc.body = el2('#body', [para, el2('BLOCKQUOTE', [
  text('\n  '), el2('P', [text('First.')]), text('\n  '), el2('P', [text('Second.')]), text('\n'),
])]);
assert.equal(
  extractCtx.bodyWithImages('<p>x</p>', 'https://x/'),
  'story of [Parkinson’s](https://x/p) is one.\n\n> First.\n>\n> Second.');
doc.querySelectorAll = () => [];

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

// Quote lines render as one blockquote without their prefixes; a line of
// symbols spaced out with &nbsp; runs is a centred, tightened section break.
const dinkus = ' ❄' + ' '.repeat(16) + '❄' + ' '.repeat(16) + '❄';
renderCtx.renderReading(`Intro\n\n> First.\n>\n> Second [x](https://e.com/q)\n\n${dinkus}\n\nAfter.`);
assert.match(el.innerHTML,
  /^Intro\n\n<blockquote [^>]*>First\.\n\nSecond <a data-link="https:\/\/e\.com\/q"[^>]*>x<\/a><\/blockquote>\n\n/, el.innerHTML);
assert.match(el.innerHTML, /\n\n<span class="inline-block w-full text-center"[^>]*>❄ ❄ ❄<\/span>\n\nAfter\./, el.innerHTML);
assert.ok(!el.innerHTML.includes('&gt;'), el.innerHTML);

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
assert.equal(
  stripCtx.stripImageMarkers('> Quote [a](https://x/a)\n>\n> > Nested\n\nBody'),
  'Quote a\n\nNested\n\nBody');

// Tapping a reader link opens the link-action sheet (open in browser vs add
// to Verba) instead of invoking the opener straight away; "Add" reuses the
// shared import path.
assert.ok(source.includes('openLinkActionSheet(link.dataset.link);'));
assert.ok(source.includes('importSharedText(url, [], false);'));
assert.ok(!source.includes("{ url: link.dataset.link }"));

// The choice sheet: opening it holds the tapped URL and renders both actions;
// picking one routes to the opener or to the shared import.
const linkEls = {};
const linkHandlers = {};
const invoked = [];
const imported = [];
const linkActionCtx = vm.createContext({
  document: { getElementById: id => linkEls[id] },
  invoke: async (cmd, args) => { invoked.push([cmd, args]); return null; },
  showToast: () => {},
  importSharedText: async (text, tags, open) => { imported.push([text, tags, open]); },
});
for (const id of ['link-action-sheet', 'link-action-overlay', 'link-action-list', 'link-action-url', 'link-action-title']) {
  linkEls[id] = { classList: { remove() {}, add() {} }, innerHTML: '', textContent: '', addEventListener: (t, h) => { linkHandlers[`${id}:${t}`] = h; } };
}
const linkSheetStart = source.indexOf("const linkActionSheet = document.getElementById('link-action-sheet');");
const linkSheetEnd = source.indexOf('// Selected Library tab', linkSheetStart);
vm.runInContext(source.slice(linkSheetStart, linkSheetEnd), linkActionCtx);
assert.equal(typeof linkActionCtx.openLinkActionSheet, 'function');
linkActionCtx.openLinkActionSheet('https://example.com/article');
assert.ok(linkEls['link-action-list'].innerHTML.includes('Add to Verba'));
assert.ok(linkEls['link-action-list'].innerHTML.includes('Open in browser'));
assert.equal(linkEls['link-action-url'].textContent, 'https://example.com/article');

const tap = action => linkHandlers['link-action-list:click']({
  target: { closest: sel => sel === '.more-item' ? { dataset: { action } } : null },
});
await tap('add');
assert.equal(imported.length, 1);
assert.equal(imported[0][0], 'https://example.com/article');
assert.equal(imported[0][1].length, 0);
assert.equal(imported[0][2], false);
await tap('open');
assert.equal(invoked.length, 1);
assert.equal(invoked[0][0], 'plugin:opener|open_url');
assert.equal(invoked[0][1].url, 'https://example.com/article');

console.log('Share import checks passed');
