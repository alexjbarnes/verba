import { markdownToText, countWords, parsePdf } from './import.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;


// ── Confirm dialog (window.confirm doesn't work in WKWebView) ──

// Confirm dialog. Defaults to a destructive "Delete" action (its original
// use); pass { okLabel, danger:false } for a neutral action like Download.
function showConfirm(message, { okLabel = 'Delete', danger = true } = {}) {
  return new Promise((resolve) => {
    const dialog = document.getElementById('confirm-dialog');
    document.getElementById('confirm-msg').textContent = message;
    const ok = document.getElementById('confirm-ok');
    ok.textContent = okLabel;
    ok.classList.toggle('bg-error', danger);
    ok.classList.toggle('bg-primary', !danger);
    dialog.classList.remove('hidden');
    dialog.classList.add('flex');

    const cleanup = (result) => {
      dialog.classList.add('hidden');
      dialog.classList.remove('flex');
      resolve(result);
    };

    document.getElementById('confirm-ok').onclick = () => cleanup(true);
    document.getElementById('confirm-cancel').onclick = () => cleanup(false);
  });
}

// Text-input dialog (window.prompt is unavailable in the webview). Resolves to
// the trimmed string on confirm (may be empty), or null if cancelled.
// `suggestions` turns the field into a typeahead: matches filter as you type,
// the whole list shows while it's empty, and arrows + Enter pick one. Enter
// with nothing highlighted still submits what was typed.
function showPrompt(message, { value = '', okLabel = 'Save', placeholder = '', suggestions = [] } = {}) {
  return new Promise((resolve) => {
    const dialog = document.getElementById('prompt-dialog');
    document.getElementById('prompt-msg').textContent = message;
    const input = document.getElementById('prompt-input');
    const list = document.getElementById('prompt-suggestions');
    input.value = value;
    input.placeholder = placeholder;
    document.getElementById('prompt-ok').textContent = okLabel;
    dialog.classList.remove('hidden');
    dialog.classList.add('flex');
    input.focus();
    input.select();

    let matches = [];
    let active = -1; // index into matches; -1 = typed text wins

    const renderSuggestions = () => {
      const q = input.value.trim().toLowerCase();
      matches = suggestions
        .filter(s => !q || (s.toLowerCase().includes(q) && s.toLowerCase() !== q))
        .slice(0, 6);
      if (active >= matches.length) active = -1;
      list.hidden = !matches.length;
      list.innerHTML = matches.map((s, i) => `
        <button class="prompt-suggestion text-left text-sm px-3 py-2 rounded-lg transition-colors cursor-pointer ${
          i === active ? 'bg-primary/15 text-primary' : 'text-on-surface hover:bg-surface-container-highest'}" data-i="${i}">
          ${escapeHtml(s)}
        </button>`).join('');
    };

    const cleanup = (result) => {
      dialog.classList.add('hidden');
      dialog.classList.remove('flex');
      document.getElementById('prompt-ok').onclick = null;
      document.getElementById('prompt-cancel').onclick = null;
      input.onkeydown = null;
      input.oninput = null;
      list.onclick = null;
      list.hidden = true;
      list.innerHTML = '';
      resolve(result);
    };
    document.getElementById('prompt-ok').onclick = () => cleanup(input.value.trim());
    document.getElementById('prompt-cancel').onclick = () => cleanup(null);
    input.oninput = () => { active = -1; renderSuggestions(); };
    list.onclick = (e) => {
      const btn = e.target.closest('.prompt-suggestion');
      if (!btn) return;
      input.value = matches[Number(btn.dataset.i)];
      active = -1;
      renderSuggestions();
      input.focus();
    };
    input.onkeydown = (e) => {
      if (e.key === 'ArrowDown' && matches.length) {
        e.preventDefault();
        active = (active + 1) % matches.length;
        renderSuggestions();
      } else if (e.key === 'ArrowUp' && matches.length) {
        e.preventDefault();
        active = active <= 0 ? matches.length - 1 : active - 1;
        renderSuggestions();
      } else if (e.key === 'Enter') {
        e.preventDefault();
        // A highlighted suggestion fills the field; a second Enter saves it.
        if (active >= 0) {
          input.value = matches[active];
          active = -1;
          renderSuggestions();
          return;
        }
        cleanup(input.value.trim());
      } else if (e.key === 'Escape') {
        e.preventDefault();
        cleanup(null);
      }
    };
    renderSuggestions();
  });
}

function escapeHtml(str) {
  // Manual map rather than the textContent/innerHTML trick: that trick does
  // NOT escape quotes, which matters now that escaped values land inside
  // attribute positions (src="${...}") as well as text nodes.
  return String(str).replace(/[&<>"']/g, c => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  })[c]);
}

function showToast(msg) {
  const el = document.createElement('div');
  el.textContent = msg;
  el.className = 'fixed bottom-6 left-1/2 -translate-x-1/2 z-50 bg-surface-container-highest text-on-surface text-sm px-5 py-3 rounded-xl shadow-lg border border-outline-variant/20 transition-opacity duration-300';
  document.body.appendChild(el);
  setTimeout(() => { el.style.opacity = '0'; }, 2500);
  setTimeout(() => { el.remove(); }, 2800);
}

// "23 Aug, 14:05" style stamp for a feed's last-checked time.
function formatTimestamp(iso) {
  try {
    const d = new Date(iso);
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) +
      ' ' + d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  } catch (_) {
    return iso;
  }
}

// ── Article extraction (shared by Add-from-URL, feeds and share-import) ──

function extractArticle(html, baseUrl) {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  // Give Readability a base URL for its link/heuristics.
  const base = doc.createElement('base');
  base.href = baseUrl;
  doc.head && doc.head.prepend(base);
  // Lead image, before Readability mutates the doc: og:image is near
  // universal on blogs; twitter:image and link[rel=image_src] mop up the
  // rest. Relative URLs resolve against the page. Stored on the item and
  // rendered as its cover; the generated gradient stays the fallback.
  let imageUrl = '';
  // Checked one selector at a time: a grouped querySelector matches in
  // DOCUMENT order, which would let a twitter:image listed earlier in <head>
  // beat the og:image this list deliberately prefers.
  let imgLink = '';
  for (const sel of ['meta[property="og:image"]', 'meta[name="og:image"]', 'meta[name="twitter:image"]', 'meta[property="twitter:image"]']) {
    const el = doc.querySelector(sel);
    if (el && el.getAttribute('content')) { imgLink = el.getAttribute('content'); break; }
  }
  if (!imgLink) imgLink = (doc.querySelector('link[rel="image_src"]') || {}).href || '';
  if (imgLink) {
    // NOTE: this URL() round-trip is a security boundary, not just
    // normalization — it percent-encodes quotes so image_url can never
    // break out of the src="..." attributes it gets interpolated into.
    try { imageUrl = new URL(imgLink, baseUrl).href; } catch (_) { /* malformed */ }
  }
  // Drop semantic chrome up front. Some sites confuse Readability into
  // climbing past the <article> and keeping the site header/nav; removing
  // these landmarks first keeps it on the actual content. Other sites
  // (Webflow templates) wrap the whole article in a <header>, so a landmark
  // holding most of the page's paragraph text is the content and stays.
  const proseLen = (el) => [...el.querySelectorAll('p')].reduce((n, p) => n + p.textContent.trim().length, 0);
  const pageProse = proseLen(doc.body);
  doc.querySelectorAll('header, footer, nav, aside').forEach(n => {
    if (proseLen(n) <= pageProse / 2) n.remove();
  });
  // Strip footnote/reference markers and the endnotes section itself.
  doc.querySelectorAll('sup[id^="fnref"], sup.reference, a.footnote-ref, [role="doc-noteref"], .footnote-ref').forEach(n => n.remove());
  doc.querySelectorAll('sup > a[href^="#"]').forEach(a => a.parentElement && a.parentElement.remove());
  doc.querySelectorAll('.footnotes, section.footnotes, [role="doc-endnotes"], ol.references').forEach(n => n.remove());
  const article = (typeof Readability !== 'undefined')
    ? new Readability(doc).parse()
    : null;
  if (article && article.textContent && article.textContent.trim().length > 50) {
    return {
      title: (article.title || '').trim(),
      body: stripTrailingTagLine(bodyWithImages(article.content, baseUrl)),
      published: (article.publishedTime || '').trim(),
      imageUrl,
    };
  }
  return null;
}

// Inline images survive extraction as `![img](url)` markers on their own
// lines, so a plain-text body still carries them. The reader renders the
// markers back into <img> elements; search and word counts strip them via
// stripImageMarkers. Headings do the same with markdown-style `##` prefixes,
// links with `[text](url)`.
const IMG_MARKER = /!\[img\]\((https?:\/\/[^\s)]+)\)/g;
const LINK_MARKER = /\[([^\]\n]*)\]\((https?:\/\/[^\s)]+)\)/g;

function stripImageMarkers(text) {
  return text.replace(IMG_MARKER, '').replace(LINK_MARKER, '$1')
    .replace(/^#{1,6} /gm, '');
}

function countImageMarkers(text) {
  return (text.match(IMG_MARKER) || []).length;
}

// Extract an article, recovering client-rendered images when the plain HTTP
// fetch returned a JS shell. Sites like Next.js blogs serve almost no markup:
// real images (and sometimes lazy-loaded ones generally) only exist after the
// page's scripts run. The hidden-WebView fetch executes them, so when the
// first pass found no images AND the HTML looks script-rendered, retry there
// and keep whichever result saw more images. When the page's figures are
// runtime-drawn animations rather than files, nothing can capture them — say
// so in the body instead of silently losing them.
async function extractArticleSmart(html, url) {
  let art = extractArticle(html, url);
  let best = countImageMarkers(art ? art.body : '');
  const richShell = /__next_f|__NUXT__|<canvas|<video|id="__next"|data-reactroot/i.test(html);
  if (!art || (best === 0 && richShell)) {
    try {
      const hydrated = await invoke('webview_fetch', { url });
      const alt = extractArticle(hydrated, url);
      if (alt) {
        const n = countImageMarkers(alt.body);
        if (n > best || !art) {
          // Carry provenance fields the first pass may have had.
          alt.published = alt.published || (art && art.published) || '';
          art = alt;
          best = n;
        }
      }
    } catch (_) { /* no hidden webview on this platform, or fetch failed */ }
  }
  if (art && best === 0 && richShell &&
      /<canvas|<video|framer-motion|animation/i.test(html)) {
    art.body += '\n\nNote: this article contains animated or interactive ' +
      'figures that cannot be captured as still text/images. View them at ' +
      'the original page.';
  }
  return art;
}

// Walk Readability's content HTML, swapping every real image for its marker,
// then return the plain text of what's left. Block-level elements get a
// double line break so paragraphs/lists/headings stay separate (the article
// you submitted was coming through as a single wall of text because we
// relied on textContent, which collapses everything). Relative srcs resolve
// against the page URL; lazy-load variants (data-src) are checked too.
// data:/blob: URIs are dropped — only http(s) images can render later.
const BLOCK_TAGS = new Set(['P', 'BR', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
  'LI', 'UL', 'OL', 'BLOCKQUOTE', 'PRE', 'FIGURE', 'HR', 'TABLE']);
// Whitespace text directly inside these is markup indentation, not content.
const TABLE_PARTS = new Set(['TABLE', 'THEAD', 'TBODY', 'TFOOT', 'TR']);

// Markers end at the first ")", so a URL's own parentheses (common on
// Wikipedia) are percent-encoded. Same address, and the marker stays whole.
const markerUrl = (u) => u.replace(/\(/g, '%28').replace(/\)/g, '%29');

function bodyWithImages(contentHtml, baseUrl) {
  if (!contentHtml) return '';
  const holder = new DOMParser().parseFromString(contentHtml, 'text/html');
  // Image markers carry their own line breaks, so the walk below passes them
  // through as-is instead of collapsing them like source whitespace.
  const markers = new Set();
  holder.querySelectorAll('img').forEach(img => {
    let src = img.getAttribute('src') || img.getAttribute('data-src') || '';
    try { src = new URL(src, baseUrl).href; } catch (_) { src = ''; }
    if (!/^https?:\/\//.test(src)) { img.remove(); return; }
    const marker = holder.createTextNode(`\n\n![img](${markerUrl(src)})\n\n`);
    markers.add(marker);
    img.replaceWith(marker);
  });
  // Keep links: each <a> becomes `[text](url)` so the reader can render it
  // clickable. Bare anchors (icon links, "▶") keep the URL as the text.
  holder.querySelectorAll('a').forEach(a => {
    let href = a.getAttribute('href') || '';
    try { href = new URL(href, baseUrl).href; } catch (_) { href = ''; }
    if (!/^https?:\/\//.test(href)) return; // JS/mailto links: leave as text
    // Links back into this same page (heading permalinks, table-of-contents
    // jumps) go nowhere in the reader: keep any text, drop the link. An
    // icon-only permalink would otherwise print its URL beside the heading.
    if (normArticleUrl(href) === normArticleUrl(baseUrl)) {
      if (a.textContent.trim()) a.replaceWith(...a.childNodes); else a.remove();
      return;
    }
    // A linked image (lightbox-style <a><img></a>) already carries its own
    // ![img] marker at this point; wrapping it in a link marker too would
    // leave orphan brackets once the reader pulls the image out. Unwrap.
    if (/!\[img\]\(/.test(a.textContent)) { a.replaceWith(...a.childNodes); return; }
    const label = (a.textContent || '').replace(/\s+/g, ' ').trim();
    a.replaceWith(holder.createTextNode(`[${label || href}](${markerUrl(href)})`));
  });
  holder.querySelectorAll('picture:empty, source').forEach(n => n.remove());

  // Walk the tree, pushing newlines around block boundaries. <br> is a soft
  // break (single newline), the rest get a blank line between them. Source
  // whitespace collapses to single spaces as a browser would (many sites
  // hard-wrap paragraphs in their HTML), except inside <pre> or an inline
  // white-space: pre style. A table row is one line with its cells split by
  // " | ", so data tables stay legible; layout tables (one cell per row) just
  // read as their contents.
  const out = [];
  const endsInSpace = () => {
    for (let i = out.length - 1; i >= 0; i--) if (out[i]) return /[ \n]$/.test(out[i]);
    return true;
  };
  const walk = (node, pre = false) => {
    for (const child of Array.from(node.childNodes)) {
      if (child.nodeType === 3) {
        let text = child.nodeValue;
        if (!pre && !markers.has(child)) {
          if (TABLE_PARTS.has(node.tagName) && !text.trim()) continue;
          text = text.replace(/[ \t\n\r\f]+/g, ' ');
          if (endsInSpace()) text = text.replace(/^ /, '');
        }
        if (text) out.push(text);
        continue;
      }
      if (child.nodeType !== 1) continue;
      const tag = child.tagName;
      const inPre = pre || tag === 'PRE' || /white-space\s*:\s*pre/i.test(child.getAttribute('style') || '');
      if (tag === 'BR') { out.push('\n'); continue; }
      if (tag === 'TR') {
        let first = true;
        for (const cell of child.children) {
          if (cell.tagName !== 'TD' && cell.tagName !== 'TH') continue;
          if (!first) out.push(' | ');
          first = false;
          // Trimmed, so indentation or a <p> inside a cell can't split the row.
          const start = out.length;
          walk(cell, inPre);
          out.push(out.splice(start).join('').trim());
        }
        out.push('\n');
        continue;
      }
      if (/^H[1-6]$/.test(tag)) {
        out.push(`\n\n${'#'.repeat(+tag[1])} `);
        walk(child, inPre);
        out.push('\n\n');
      } else if (BLOCK_TAGS.has(tag)) {
        out.push('\n\n');
        walk(child, inPre);
        out.push('\n\n');
      } else {
        walk(child, inPre);
      }
    }
  };
  walk(holder.body);
  return out.join('')
    // Lines holding only zero-width characters (Webflow's empty paragraphs)
    // would otherwise survive as blank space.
    .replace(/^[ \t\u200B-\u200D\uFEFF]+$/gm, '')
    .replace(/[ \t]+\n/g, '\n')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}

// Drop a trailing "Tags: a, b, c" footer from an article body. Blogs print
// their tag list at the foot of the page and Readability keeps it. The tags
// themselves are captured from the feed's <category> elements, so nothing is
// lost. Deliberately narrow: last line only, a known label, and a short comma
// list — never touches prose that happens to mention tags.
const TAG_FOOTER = /^(tags|tagged|topics|categories|filed under)\s*[:\-—]\s*(.+)$/i;
function stripTrailingTagLine(body) {
  const lines = body.split('\n');
  let last = lines.length - 1;
  while (last >= 0 && !lines[last].trim()) last--;
  if (last < 1) return body; // a one-line body is the article, not a footer
  const m = lines[last].trim().match(TAG_FOOTER);
  if (!m) return body;
  const list = m[2].trim();
  // A real tag list is short and comma-separated; a sentence isn't.
  if (list.length > 200 || /[.!?]$/.test(list)) return body;
  return lines.slice(0, last).join('\n').trimEnd();
}

// Short display form of an article's publication date: "12 Jun", with the
// year added once it isn't the current one. '' for unknown/unparseable.
function fmtPubDate(s) {
  if (!s) return '';
  const d = new Date(s);
  if (isNaN(d.getTime())) return '';
  const opts = { day: 'numeric', month: 'short' };
  if (d.getFullYear() !== new Date().getFullYear()) opts.year = 'numeric';
  return d.toLocaleDateString(undefined, opts);
}

// Handle text shared to the app from elsewhere. If it contains a URL, fetch the
// page (in Rust, to dodge CORS) and run Readability to pull the article; plain
// text is saved as-is. Either way it lands in the library without opening the
// reader, unless requested by the Add-from-URL modal.

// Fetch and extract a shared link, or null when it can't be read (offline,
// blocked, or no article in the page). Callers fall back to saving the raw
// text rather than dropping the share.
async function fetchSharedArticle(url) {
  try {
    const html = await fetchArticleHtml(url);
    const article = await extractArticleSmart(html, url);
    return article && article.body ? article : null;
  } catch (_) {
    return null;
  }
}

let importingShared = false;
// `tags` come from the Add-from-URL modal; the Android share path passes none.
async function importSharedText(text, tags = [], openAfterImport = false) {
  if (importingShared) return; // ignore a duplicate trigger mid-import
  const raw = (text || '').trim();
  if (!raw) return;
  let url = (raw.match(/https?:\/\/[^\s]+/) || [])[0];
  importingShared = true;
  try {
    let title = null;
    let body = raw;
    let published = null;
    let imageUrl = null;
    if (url) {
      showToast('Fetching article…');
      const article = await fetchSharedArticle(url);
      if (article) {
        title = article.title || url;
        body = article.body;
        published = article.published || null;
        imageUrl = article.imageUrl || null;
      } else {
        // A link that won't fetch (offline, or a URL sitting inside a
        // paragraph) must not swallow the share: the text is still worth
        // keeping, so save it as-is and drop the failed link.
        url = '';
        showToast('Could not read the link, saved as text');
      }
    }
    let item;
    try {
      item = await invoke('library_add', { title, body, url: url || null, published, imageUrl, tags });
    } catch (err) {
      showToast('Save failed: ' + err);
      return;
    }
    await loadLibrary();
    if (openAfterImport && item && item.id) openReading(item.id);
    showToast(url ? 'Article added' : 'Text added');
  } finally {
    importingShared = false;
  }
}

// ── Sidebar & navigation ──

let activeTab = null;

// Detail views hide the bottom nav.
const DETAIL_PANELS = new Set(['reading', 'feed-entries']);

// Bottom-nav layout. `center` is the prominent circular action button (Add).
const BOTTOM_NAV = [
  { tab: 'library', label: 'Library', icon: 'library_books' },
  { action: 'add', label: 'Add', icon: 'add', center: true },
  { tab: 'feeds', label: 'Feeds', icon: 'rss_feed' },
];
// Pages reachable only through the More sheet.
const MORE_ITEMS = [
  { tab: 'general', label: 'Settings', icon: 'settings' },
  { tab: 'debug', label: 'Debug', icon: 'bug_report' },
];

const bottomNav = document.getElementById('bottom-nav');
const moreSheet = document.getElementById('nav-more-sheet');

// Per-tab data refreshers, run when a tab is shown.
const TAB_LOADERS = {
  general: () => applySettingsDefaultExpand(),
  library: () => loadLibrary(),
  feeds: () => loadFeeds(),
};

// Show a top-level tab: swap the panel, refresh its data, and light up the
// matching bottom-nav slot.
function navigateTo(tab) {
  activeTab = tab;
  showPanel(tab);
  setBottomNavVisible(!DETAIL_PANELS.has(tab));
  renderBottomNav();
  const load = TAB_LOADERS[tab];
  if (load) load();
}

function renderBottomNav() {
  // The center button is a raised circle that overhangs the bar and floats
  // over the panel scrolled beneath it; flag the body so those panels pad
  // their last row clear of it (see .has-nav-fab in styles.css).
  document.body.classList.toggle('has-nav-fab', BOTTOM_NAV.some(it => it.center));
  bottomNav.innerHTML = BOTTOM_NAV.map(it => {
    if (it.center) {
      return `<button class="nav-slot flex-1 flex items-center justify-center" data-action="${it.action}">
        <span class="w-14 h-14 -mt-5 flex items-center justify-center rounded-full bg-primary text-on-primary shadow-lg active:scale-95 transition-transform">
          <span class="material-symbols-outlined text-3xl">${it.icon}</span>
        </span>
      </button>`;
    }
    const active = it.tab && it.tab === activeTab;
    const attr = it.overflow ? 'data-overflow="1"' : `data-tab="${it.tab}"`;
    const color = active ? 'text-primary' : 'text-on-surface-variant';
    // The active icon sits in a tonal pill (M3 style) so the current tab
    // reads by shape as well as color.
    const pill = active ? 'bg-primary/15 rounded-full' : '';
    return `<button class="nav-slot flex-1 flex flex-col items-center justify-center gap-0.5 py-1.5 ${color} active:opacity-70 transition-colors" ${attr}>
      <span class="${pill} px-4 py-0.5 transition-colors"><span class="material-symbols-outlined text-[22px]" style="font-variation-settings:'FILL' ${active ? 1 : 0}">${it.icon}</span></span>
      <span class="text-[10px] font-medium leading-none">${it.label}</span>
    </button>`;
  }).join('');
}

function setBottomNavVisible(show) {
  bottomNav.classList.toggle('hidden', !show);
}

bottomNav.addEventListener('click', (e) => {
  const slot = e.target.closest('.nav-slot');
  if (!slot) return;
  if (slot.dataset.action === 'add') { openAddModal(); return; }
  if (slot.dataset.overflow) { openMoreSheet(); return; }
  if (slot.dataset.tab) navigateTo(slot.dataset.tab);
});

// Empty-library call to action mirrors the nav's Add button.
document.getElementById('lib-empty-add').addEventListener('click', () => openAddModal());

// Settings/Debug entry, moved from the bottom bar to the library header.
document.getElementById('lib-more-btn').addEventListener('click', () => openMoreSheet());

// ── More sheet ──

function openMoreSheet() {
  document.getElementById('nav-more-list').innerHTML = MORE_ITEMS.map(it =>
    `<button class="more-item w-full flex items-center gap-3 px-4 py-3 rounded-xl text-left cursor-pointer hover:bg-surface-container-highest text-on-surface" data-tab="${it.tab}">
      <span class="material-symbols-outlined text-lg text-on-surface-variant">${it.icon}</span>
      <span class="text-sm">${it.label}</span>
    </button>`).join('');
  moreSheet.classList.remove('hidden');
}
function closeMoreSheet() { moreSheet.classList.add('hidden'); }
document.getElementById('nav-more-overlay').addEventListener('click', closeMoreSheet);
document.getElementById('nav-more-list').addEventListener('click', (e) => {
  const btn = e.target.closest('.more-item');
  if (!btn) return;
  closeMoreSheet();
  navigateTo(btn.dataset.tab);
});

// ── Settings collapsible groups (findability) ──
//
// Each group folds behind its header, so the headers act as a scannable index
// instead of one long scroll. Clicking a header toggles that group.
function applySettingsDefaultExpand() {
  document.querySelectorAll('#general .settings-group').forEach(g => g.classList.remove('collapsed'));
}

document.getElementById('general').addEventListener('click', (e) => {
  const toggle = e.target.closest('.settings-group-toggle');
  if (toggle) toggle.closest('.settings-group')?.classList.toggle('collapsed');
});

// ── Theme ──

function loadThemePref() {
  try {
    return localStorage.getItem('verba-theme') || 'dark';
  } catch (_) {
    return 'dark';
  }
}

function saveThemePref(pref) {
  try { localStorage.setItem('verba-theme', pref); } catch (_) { /* storage unavailable */ }
}

// Resolves 'system' against the OS setting. Mirrors the inline head script
// that applies the theme before first paint, so this must stay idempotent.
function applyThemePref(pref) {
  const resolved = pref === 'system'
    ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark')
    : pref;
  if (resolved === 'light') document.documentElement.dataset.theme = 'light';
  else delete document.documentElement.dataset.theme;
}

const themeSelect = document.getElementById('theme-select');
themeSelect.addEventListener('change', () => {
  saveThemePref(themeSelect.value);
  applyThemePref(themeSelect.value);
});
// Only follow OS changes live when the user picked "System"; an explicit
// dark/light choice should not be overridden by an OS-level flip.
matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => {
  if (loadThemePref() === 'system') applyThemePref('system');
});
(function initTheme() {
  const pref = loadThemePref();
  themeSelect.value = pref;
  applyThemePref(pref);
})();

// ── Reading text size ──

function loadFontScale() {
  try {
    const n = parseInt(localStorage.getItem('verba-font-scale'), 10);
    return Number.isFinite(n) ? n : 100;
  } catch (_) {
    return 100;
  }
}

function saveFontScale(scale) {
  try { localStorage.setItem('verba-font-scale', String(scale)); } catch (_) { /* storage unavailable */ }
}

// Applied directly to the reading view's font-size (rem, so it still scales
// with the device's base font). Called unconditionally at boot below — must
// not depend on the Settings panel ever having been opened.
function applyFontScale(scale) {
  const el = document.getElementById('reading-text');
  // 100% = 1.0625rem (17px at the default root size), matching the
  // text-[17px] class on #reading-text. This inline style always beats the
  // class, so its baseline must BE the design size or the class is dead.
  if (el) el.style.fontSize = ((scale / 100) * 1.0625).toFixed(4) + 'rem';
}

const fontScaleRange = document.getElementById('font-scale-range');
const fontScaleLabel = document.getElementById('font-scale-label');
fontScaleRange.addEventListener('input', () => {
  const scale = parseInt(fontScaleRange.value, 10);
  fontScaleLabel.textContent = scale + '%';
  saveFontScale(scale);
  applyFontScale(scale);
});
(function initFontScale() {
  const scale = loadFontScale();
  fontScaleRange.value = String(scale);
  fontScaleLabel.textContent = scale + '%';
  applyFontScale(scale);
})();

// ── Reader display settings (one global setting, Kindle-style) ──
//
// Font family and paper colour live in their own localStorage keys; text size
// reuses verba-font-scale so Settings' slider and this sheet stay one setting.
// Applied to #reading whenever it opens or a pref changes.

const READER_FONTS = {
  sans: "'Inter', system-ui, sans-serif",
  serif: "Georgia, 'Iowan Old Style', 'Times New Roman', serif",
  book: "Charter, 'Bitstream Charter', 'Sitka Text', Cambria, serif",
};

function loadReaderPref(key, fallback) {
  try { return localStorage.getItem(key) || fallback; } catch (_) { return fallback; }
}
function saveReaderPref(key, value) {
  try { localStorage.setItem(key, value); } catch (_) {}
}

function applyReaderStyle() {
  const panel = document.getElementById('reading');
  const el = document.getElementById('reading-text');
  if (!panel || !el) return;
  const font = loadReaderPref('verba-reader-font', 'sans');
  const bg = loadReaderPref('verba-reader-bg', '');
  el.style.fontFamily = READER_FONTS[font] || READER_FONTS.sans;
  if (bg) panel.dataset.bg = bg; else delete panel.dataset.bg;
}

function syncReaderSheetChips() {
  const font = loadReaderPref('verba-reader-font', 'sans');
  const bg = loadReaderPref('verba-reader-bg', '');
  document.querySelectorAll('#reader-fonts .reader-chip').forEach(chip =>
    chip.dataset.on = chip.dataset.font === font ? '1' : '0');
  document.querySelectorAll('#reader-backgrounds .reader-bgchip').forEach(chip =>
    chip.dataset.on = (chip.dataset.bg || '') === bg ? '1' : '0');
}

const readerSheet = document.getElementById('reader-sheet');
function openReaderSheet() { syncReaderSheetChips(); readerSheet.classList.remove('hidden'); }
function closeReaderSheet() { readerSheet.classList.add('hidden'); }
document.getElementById('reading-format').addEventListener('click', openReaderSheet);
document.getElementById('reader-sheet-overlay').addEventListener('click', closeReaderSheet);

document.getElementById('reader-fonts').addEventListener('click', (e) => {
  const chip = e.target.closest('.reader-chip');
  if (!chip) return;
  saveReaderPref('verba-reader-font', chip.dataset.font);
  syncReaderSheetChips();
  applyReaderStyle();
});

// The size slider here and the one in Settings write the same key.
const readerSizeRange = document.getElementById('reader-size-range');
const readerSizeLabel = document.getElementById('reader-size-label');
readerSizeRange.addEventListener('input', () => {
  const scale = parseInt(readerSizeRange.value, 10);
  readerSizeLabel.textContent = scale + '%';
  saveFontScale(scale);
  applyFontScale(scale);
  if (fontScaleRange) fontScaleRange.value = String(scale);
  if (fontScaleLabel) fontScaleLabel.textContent = scale + '%';
});

document.getElementById('reader-backgrounds').addEventListener('click', (e) => {
  const chip = e.target.closest('.reader-bgchip');
  if (!chip) return;
  saveReaderPref('verba-reader-bg', chip.dataset.bg || '');
  syncReaderSheetChips();
  applyReaderStyle();
});

(function initReaderStyle() {
  applyReaderStyle();
  const scale = loadFontScale();
  readerSizeRange.value = String(scale);
  readerSizeLabel.textContent = scale + '%';
})();

// Keep both size sliders in sync when the Settings one moves.
fontScaleRange.addEventListener('input', () => {
  readerSizeRange.value = fontScaleRange.value;
  readerSizeLabel.textContent = fontScaleRange.value + '%';
});

// Chunk size ceiling shared with the backend's provider cap.

// "1.2 MB" style, for the cache-clear toast.
function fmtBytes(n) {
  if (!n) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i ? 1 : 0)} ${units[i]}`;
}

// ── Listen: library + reading view ──

function showPanel(id) {
  document.querySelectorAll('.tab-panel').forEach(p => p.classList.add('hidden'));
  const el = document.getElementById(id);
  if (el) el.classList.remove('hidden');
}

// ── Reading view (plain text on screen; scroll position is the progress) ──

function renderReading(text) {
  const el = document.getElementById('reading-text');
  // Body is plain text carrying markers where structure sat: `![img](url)`
  // images, `[text](url)` links, markdown-style `## ` headings. Escape the
  // text runs; each marker becomes its element (lazy full-width image,
  // clickable link, demoted heading — one size, sections never dwarf the
  // article title). Newlines around headings are swallowed there: the
  // container is whitespace-pre-wrap, so left-behind blank lines would stack
  // on the heading's margins and open a visible gap.
  let html = '';
  let cursor = 0;
  const emitText = (chunk) => {
    html += escapeHtml(chunk)
      .replace(/\n*(#{1,6}) ([^\n]+)(?:\n+|$)/g, (_, hashes, title) => {
        const level = Math.min(6, hashes.length + 1);
        return `<h${level} class="text-primary/90 font-bold tracking-tight my-3" style="font-size:1.1em">${title}</h${level}>`;
      })
      .replace(LINK_MARKER, (m, label, url) =>
        // label and url came out of the already-escaped chunk; escaping again
        // would double-encode (& -> &amp; -> &amp;amp;).
        `<a data-link="${url}" class="text-primary underline">${label}</a>`);
  };
  text.replace(IMG_MARKER, (match, url, offset) => {
    emitText(text.slice(cursor, offset));
    html += `<img src="${escapeHtml(url)}" alt="" loading="lazy" class="w-full rounded-xl my-4">`;
    cursor = offset + match.length;
    return match;
  });
  emitText(text.slice(cursor));
  el.innerHTML = html + readingEndActionsHtml();
  // Opening a never-read article after a part-read one must land at the top;
  // both callers set their own resume scroll afterwards.
  el.scrollTop = 0;
}

// Actions at the natural end of the piece.
function readingEndActionsHtml() {
  // The natural "done" once you reach the end. Already-archived articles
  // skip it; their menu has Unarchive.
  const archive = readingItem && !readingItem.archived ? `
        <button data-reading-act="archive" class="inline-flex items-center gap-1.5 text-xs font-medium px-3 min-h-9 rounded-full bg-primary/15 text-primary hover:bg-primary/25 active:scale-[0.97] transition-all cursor-pointer">
          <span class="material-symbols-outlined self-center" style="font-size:18px;line-height:1;width:18px;height:18px">archive</span><span class="self-center leading-none">Archive</span>
        </button>` : '';
  // The buttons wrap as one group, so a narrow phone drops them under the
  // label instead of squeezing their text.
  return `
    <div class="mt-10 pt-5 border-t border-outline-variant/20 flex flex-wrap items-center justify-between gap-x-1 gap-y-3 select-none">
      <span class="text-xs text-on-surface-variant">End of article</span>
      <div class="flex items-center gap-1">${archive}
        <button data-reading-act="tags" class="inline-flex items-center gap-1.5 text-xs font-medium px-3 min-h-9 rounded-full bg-surface-container-highest text-on-surface hover:brightness-125 active:scale-[0.97] transition-all cursor-pointer">
          <span class="material-symbols-outlined self-center" style="font-size:18px;line-height:1;width:18px;height:18px">sell</span><span class="self-center leading-none">Tags</span>
        </button>
        <button data-reading-act="delete" class="inline-flex items-center gap-1.5 text-xs font-medium px-3 min-h-9 rounded-full bg-error/15 text-error hover:bg-error/25 active:scale-[0.97] transition-all cursor-pointer">
          <span class="material-symbols-outlined self-center" style="font-size:18px;line-height:1;width:18px;height:18px">delete</span><span class="self-center leading-none">Delete</span>
        </button>
      </div>
    </div>`;
}

// ── Reading progress (resume + library percentage) ──

// Char offset matching the current scroll position — the stable resume anchor
// and the basis for the library percentage.
function currentReadingChar() {
  const el = document.getElementById('reading-text');
  if (!el || !readingText.length) return 0;
  const range = el.scrollHeight - el.clientHeight;
  if (range <= 0) return el.scrollTop > 0 ? readingText.length : 0;
  return Math.round((el.scrollTop / range) * readingText.length);
}

// Persist reading progress (a char offset into the body) for the open item.
// Throttled to ~4s unless `force` (back, dismiss, finish). Driven by the
// reader's scroll events; also fires once on close.
let lastProgressSaveMs = 0;
function saveProgress(force) {
  if (!readingItem) return;
  const now = Date.now();
  if (!force && now - lastProgressSaveMs < 4000) return;
  lastProgressSaveMs = now;
  const offset = currentReadingChar();
  readingItem.progress = offset;
  invoke('library_set_progress', { id: readingItem.id, progress: offset }).catch(() => {});
}

// ── Opening items ──

let readingItem = null;  // LibraryItem currently open in the reading view
let readingText = '';    // body text currently on screen

// Scroll the reading view so a saved char offset is in view.
function resumeScroll(offset) {
  const el = document.getElementById('reading-text');
  if (!el || !offset) return;
  const range = el.scrollHeight - el.clientHeight;
  if (range > 0) el.scrollTop = Math.min(range, (offset / Math.max(1, readingText.length)) * range);
}

async function openReading(id) {
  const item = await invoke('library_get', { id });
  if (!item) return;
  readingItem = item;
  readingText = item.body || '';
  document.getElementById('reading-title').textContent = item.title || 'Untitled';
  // Show the panel BEFORE rendering: renderReading resets scrollTop, but a
  // scrollTop write on a display:none element is a no-op, so rendering into
  // the hidden panel would inherit the previous article's scroll position.
  showPanel('reading');
  renderReading(readingText);
  setBottomNavVisible(false);
  // Zero progress always means the top (renderReading already put it there).
  if (item.progress > 0) resumeScroll(item.progress);
  if (!item.seen) invoke('library_mark_seen', { id }).catch(() => {});
}

function closeReading() {
  saveProgress(true);
  readingItem = null;
  readingText = '';
  navigateTo('library');
}

document.getElementById('reading-back').addEventListener('click', closeReading);

// Scroll-driven progress: persist position as you read. Also drives the
// scrollbar's fade-out: `.scrolling` goes on while the finger is moving and
// drops off after a beat of stillness.
(function () {
  const el = document.getElementById('reading-text');
  if (!el) return;
  let idleTimer = null;
  el.addEventListener('scroll', () => {
    el.classList.add('scrolling');
    clearTimeout(idleTimer);
    idleTimer = setTimeout(() => el.classList.remove('scrolling'), 700);
    if (!readingItem) return;
    saveProgress(false);
  }, { passive: true });
})();

// Header menu on an open item routes into the same sheet the library list uses.
document.getElementById('reading-menu').addEventListener('click', () => {
  if (readingItem) openLibActionSheet(readingItem);
});

// End-of-article actions rendered inside #reading-text, plus saved links:
// opened in the system browser via the opener plugin (a raw href would try to
// navigate the app webview itself).
document.getElementById('reading-text').addEventListener('click', async (e) => {
  const link = e.target.closest('[data-link]');
  if (link) {
    e.preventDefault();
    invoke('plugin:opener|open_url', { url: link.dataset.link }).catch(err => showToast(err));
    return;
  }
  const btn = e.target.closest('[data-reading-act]');
  if (!btn || !readingItem) return;
  if (btn.dataset.readingAct === 'tags') openLibraryTagsSheet(readingItem.id);
  else if (btn.dataset.readingAct === 'archive') setLibraryArchived(readingItem.id, true);
  else if (btn.dataset.readingAct === 'delete') deleteLibraryItem(readingItem.id);
});

// Selected Library tab ('articles' | 'archive'). The fetched items (and feed
// titles used for the source badge) are cached in module vars so switching
// tabs re-renders instantly without hitting the backend again.
let libTab = 'articles';
let libItems = [];
let libFeedsById = {};
// Library filters, all independent and combined by visibleLibItems():
// source narrows by where an item came from, tag by label, search by text.
let libSource = 'all';   // all | feeds | web | text
let libTagFilter = '';   // lowercased tag, '' = any
let libSearch = '';      // lowercased query
let libSearchTimer = null;

// Where an item came from, from the provenance already on the record.
function libItemSource(it) {
  if (it.feed_id) return 'feeds';
  if (it.url) return 'web';
  return 'text';
}

// Every tag in use across the library, deduplicated case-insensitively but
// shown as first written. Alphabetical — this feeds
// the editors, where finding a specific tag matters.
function allLibraryTags() {
  const seen = new Map();
  for (const it of libItems) {
    for (const t of it.tags || []) {
      if (!seen.has(t.toLowerCase())) seen.set(t.toLowerCase(), t);
    }
  }
  return [...seen.values()].sort((a, b) => a.localeCompare(b));
}

/// A tag has to appear on this many articles before it earns a filter chip —
/// but ONLY once there are more tags than fit. Feeds supply ~6 categories per
/// article with a long tail of one-offs (author names and the like) that would
/// push the useful chips off-screen. In a lightly-tagged library there's
/// nothing to crowd out, and hiding a tag you just applied by hand reads as
/// the feature being broken, so below the cap every tag gets a chip.
const LIB_TAG_CHIP_MIN = 2;
const LIB_TAG_CHIP_MAX = 12;

// Tags worth filtering by: most-used first, so the row leads with whatever
// actually splits the library. The active tag is always included, even if an
// edit just dropped it below the threshold, so the UI can't strand itself.
function libFilterTags() {
  const counts = new Map(); // lower -> { label, n }
  // Counted within the current tab, so Articles never offers a chip whose
  // only articles are archived (and vice versa).
  const archivedView = libTab === 'archive';
  for (const it of libItems) {
    if (!!it.archived !== archivedView) continue;
    for (const t of it.tags || []) {
      const k = t.toLowerCase();
      const e = counts.get(k) || { label: t, n: 0 };
      e.n += 1;
      counts.set(k, e);
    }
  }
  // Only start demanding a minimum once the row would overflow anyway.
  const crowded = counts.size > LIB_TAG_CHIP_MAX;
  const ranked = [...counts.entries()]
    .filter(([, v]) => !crowded || v.n >= LIB_TAG_CHIP_MIN)
    .sort((a, b) => b[1].n - a[1].n || a[1].label.localeCompare(b[1].label))
    .slice(0, LIB_TAG_CHIP_MAX);
  const out = ranked.map(([k, v]) => ({ key: k, label: v.label, n: v.n }));
  // The selected tag always shows, even when it's rare or ranked past the cap
  // — otherwise filtering by it makes its own chip disappear. Leads the row,
  // since it's the one currently doing something.
  if (libTagFilter && !out.some(t => t.key === libTagFilter)) {
    // Zero when the tag only exists on the other tab.
    const active = counts.get(libTagFilter) || {
      label: allLibraryTags().find(t => t.toLowerCase() === libTagFilter) || libTagFilter,
      n: 0,
    };
    out.unshift({ key: libTagFilter, label: active.label, n: active.n });
  }
  return out;
}

// Search runs over the text already in memory (library_list returns bodies),
// so there's no backend round trip — titles first-class, body as a fallback.
function libItemMatchesSearch(it) {
  if (!libSearch) return true;
  if ((it.title || '').toLowerCase().includes(libSearch)) return true;
  if ((it.tags || []).some(t => t.toLowerCase().includes(libSearch))) return true;
  return stripImageMarkers(it.body || '').toLowerCase().includes(libSearch);
}

function libItemMatchesFilters(it) {
  if (!!it.archived !== (libTab === 'archive')) return false;
  if (libSource !== 'all' && libItemSource(it) !== libSource) return false;
  if (libTagFilter && !(it.tags || []).some(t => t.toLowerCase() === libTagFilter)) return false;
  return libItemMatchesSearch(it);
}

const libThumb = document.getElementById('lib-thumb');

function setLibTab(tab) {
  libTab = tab;
  if (libThumb) libThumb.style.transform = tab === 'archive' ? 'translateX(100%)' : 'translateX(0)';
  document.querySelectorAll('.lib-tab-btn').forEach(b => {
    const active = b.dataset.lib === tab;
    b.classList.toggle('text-primary', active);
    b.classList.toggle('text-on-surface-variant', !active);
  });
  renderLibraryList();
}

document.querySelectorAll('.lib-tab-btn').forEach(b => {
  b.addEventListener('click', () => setLibTab(b.dataset.lib));
});

document.getElementById('lib-filters').addEventListener('click', (e) => {
  const tagBtn = e.target.closest('.lib-tag-filter');
  if (tagBtn) {
    // Tapping the active tag clears it — no separate "any tag" chip needed.
    libTagFilter = tagBtn.dataset.tag === libTagFilter ? '' : tagBtn.dataset.tag;
    renderLibraryList();
    return;
  }
  const srcBtn = e.target.closest('.lib-filter');
  if (!srcBtn) return;
  libSource = srcBtn.dataset.source;
  renderLibraryList();
});

// Search is over in-memory text, so it's cheap — a short debounce just avoids
// re-rendering the whole list on every keystroke.
const libSearchInput = document.getElementById('lib-search');
libSearchInput.addEventListener('input', () => {
  const q = libSearchInput.value.trim();
  document.getElementById('lib-search-clear').hidden = !q;
  clearTimeout(libSearchTimer);
  libSearchTimer = setTimeout(() => {
    libSearch = q.toLowerCase();
    renderLibraryList();
  }, 120);
});
document.getElementById('lib-search-clear').addEventListener('click', () => {
  libSearchInput.value = '';
  libSearch = '';
  document.getElementById('lib-search-clear').hidden = true;
  renderLibraryList();
  libSearchInput.focus();
});

// Long-press (500ms) gesture for a row's action sheet, without eating the
// normal tap-to-open click. Movement past a small threshold, or an early
// release/cancel, aborts the timer. Firing sets `el._longPressed`, which the
// row's own click handler checks (and clears) so the synthetic click that
// follows a long-press doesn't ALSO open the item; a fresh pointerdown also
// clears it, in case a 'contextmenu' fired without a click after it.
// `contextmenu` (desktop right-click, some Android long-press paths) is
// wired as a fallback trigger.
function attachLongPress(el, handler) {
  let timer = null;
  let startX = 0, startY = 0;
  const clear = () => { if (timer) { clearTimeout(timer); timer = null; } };

  el.addEventListener('pointerdown', (e) => {
    el._longPressed = false;
    startX = e.clientX; startY = e.clientY;
    clear();
    timer = setTimeout(() => {
      timer = null;
      el._longPressed = true;
      handler(e);
    }, 500);
  });
  el.addEventListener('pointermove', (e) => {
    if (timer && Math.hypot(e.clientX - startX, e.clientY - startY) > 10) clear();
  });
  el.addEventListener('pointerup', clear);
  el.addEventListener('pointercancel', clear);
  el.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    clear();
    el._longPressed = true;
    handler(e);
  });
}

// ── Library row long-press action sheet ──

const libActionSheet = document.getElementById('lib-action-sheet');
let libActionTarget = null; // { id }

function openLibActionSheet(item) {
  libActionTarget = { id: item.id };
  document.getElementById('lib-action-title').textContent = item.title;
  // Share only makes sense for things with a source URL.
  const shareAction = item.url
    ? [{ action: 'share', icon: 'share', label: 'Share link' }]
    : [];
  const actions = [
    item.archived
      ? { action: 'unarchive', icon: 'unarchive', label: 'Unarchive' }
      : { action: 'archive', icon: 'archive', label: 'Archive' },
    { action: 'reset-progress', icon: 'restart_alt', label: 'Reset progress' },
    { action: 'tags', icon: 'sell', label: (item.tags || []).length ? 'Edit tags' : 'Add tags' },
    ...shareAction,
    { action: 'delete', icon: 'delete', label: 'Delete', danger: true },
  ];
  document.getElementById('lib-action-list').innerHTML = actions.map(a =>
    `<button class="more-item w-full flex items-center gap-3 px-4 py-3 rounded-xl text-left cursor-pointer hover:bg-surface-container-highest ${a.danger ? 'text-error' : 'text-on-surface'}" data-action="${a.action}">
      <span class="material-symbols-outlined text-lg ${a.danger ? 'text-error/80' : 'text-on-surface-variant'}">${a.icon}</span>
      <span class="text-sm">${a.label}</span>
    </button>`).join('');
  libActionSheet.classList.remove('hidden');
}

function closeLibActionSheet() { libActionSheet.classList.add('hidden'); }
document.getElementById('lib-action-overlay').addEventListener('click', closeLibActionSheet);
document.getElementById('lib-action-list').addEventListener('click', async (e) => {
  const btn = e.target.closest('.more-item');
  if (!btn || !libActionTarget) return;
  const { id } = libActionTarget;
  const action = btn.dataset.action;
  closeLibActionSheet();
  if (action === 'reset-progress') await invoke('library_set_progress', { id, progress: 0 });
  else if (action === 'share') {
    try {
      await invoke('library_share_url', { id });
    } catch (err) {
      showToast(err);
    }
    return;
  }
  else if (action === 'archive' || action === 'unarchive') {
    await setLibraryArchived(id, action === 'archive');
    return;
  }
  else if (action === 'tags') { openLibraryTagsSheet(id); return; }
  else if (action === 'delete') { await deleteLibraryItem(id); return; }
  // Re-open the reader's context after a change made from the reading view.
  if (readingItem && readingItem.id === id) return;
  loadLibrary();
});

// Archive keeps an article but moves it out of the main list, under the
// Archived chip. Archiving the open article means you're done with it, so
// the reader closes.
async function setLibraryArchived(id, archived) {
  await invoke('library_set_archived', { id, archived });
  showToast(archived ? 'Archived' : 'Unarchived');
  if (readingItem && readingItem.id === id) {
    readingItem.archived = archived;
    if (archived) closeReading();
    return;
  }
  loadLibrary();
}

// Edit an existing item's tags, through the shared sheet.
function openLibraryTagsSheet(id) {
  const item = libItems.find(it => it.id === id);
  if (!item) return;
  openTagSheet({
    subtitle: item.title,
    tags: (item.tags || []).slice(),
    allTags: allLibraryTags(),
    onChange: async (tags) => {
      try {
        await invoke('library_set_tags', { id, tags });
      } catch (err) {
        showToast('Could not save tags: ' + err);
        return null; // keep what's on screen
      }
      item.tags = tags;
      // Don't strand the list on a filter whose last tag just went away.
      if (libTagFilter && !allLibraryTags().some(t => t.toLowerCase() === libTagFilter)) {
        libTagFilter = '';
      }
      renderLibraryList();
      return tags;
    },
  });
}

async function loadLibrary() {
  libItems = await invoke('library_list');
  // Feed titles for the source badge on imported articles (cheap local JSON).
  libFeedsById = {};
  try {
    (await invoke('feeds_list')).forEach(f => { libFeedsById[f.id] = f; });
  } catch (_) {}
  renderLibraryList();
}

// Renders the selected tab from the module-cached `libItems`/`libFeedsById` —
// no backend round trip, so switching tabs is instant.
// ── Cover tiles ──
//
// Deterministic artwork stands in for real cover art: the title hashes to
// one of eight palette-family gradients (see .cover-N in styles.css) and a
// monogram. Same title, same tile, every render, both themes.
function coverClass(seed) {
  let h = 2166136261;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return `cover cover-${(h >>> 0) % 8}`;
}

// A cover tile: gradient + monogram always render (instant, deterministic);
// the article's remote lead image layers on top when it has one (onerror
// peels the img off, exposing the gradient).
function coverTileHtml(it, sizeClasses, textClass) {
  const img = it.image_url
    ? `<img src="${escapeHtml(it.image_url)}" class="absolute inset-0 w-full h-full object-cover" loading="lazy" onerror="this.remove()">`
    : '';
  return `<div class="${coverClass(it.title)} ${sizeClasses} ${textClass} relative"><span>${escapeHtml(coverMonogram(it.title))}</span>${img}</div>`;
}

function coverMonogram(title) {
  const words = (title || '').trim().split(/\s+/).filter(w => /[\p{L}\p{N}]/u.test(w));
  const letter = (w) => (w.match(/[\p{L}\p{N}]/u) || ['?'])[0];
  if (!words.length) return '?';
  return words.length === 1 ? letter(words[0]) : letter(words[0]) + letter(words[1]);
}

// Source chips + one chip per tag in use, in a single row. Tags narrow
// whatever source is selected rather than replacing it.
function renderLibFilterChips() {
  const chipClass = on => `shrink-0 text-xs font-semibold px-3 py-1.5 rounded-full border transition-colors cursor-pointer ${
    on ? 'bg-primary/15 border-primary/40 text-primary' : 'border-outline-variant/30 text-on-surface-variant hover:bg-surface-container-high'}`;
  document.querySelectorAll('.lib-filter').forEach(btn => {
    btn.className = `lib-filter ${chipClass(btn.dataset.source === libSource)}`;
  });
  document.getElementById('lib-tag-chips').innerHTML = libFilterTags().map(t =>
    `<button class="lib-tag-filter ${chipClass(t.key === libTagFilter)}" data-tag="${escapeHtml(t.key)}">#${escapeHtml(t.label)}<span class="ml-1 opacity-50 tabular-nums">${t.n}</span></button>`).join('');
}

function renderLibraryList() {
  const items = libItems;
  const feedsById = libFeedsById;
  const list = document.getElementById('lib-list');
  const empty = document.getElementById('lib-empty');
  // The signature list treatment: listening progress lives on the card
  // itself — a hairline along its bottom edge — instead of "42%" text in the
  // meta line. The track only renders while something IS in progress;
  // finished cards drop the line and carry the tick alone (one state, one
  // signal). Delete moved off the rows into the long-press sheet: a
  // destructive control on every row invites accidental taps and repeats
  // what the sheet already offers.
  const progressEdge = (pct) => (pct > 0 && pct < 100)
    ? `<div class="absolute left-0 right-0 bottom-0 h-[3px] bg-primary/15"><div class="h-full bg-primary/70 rounded-r-full" style="width:${pct}%"></div></div>`
    : '';
  const doneTick = (pct) => pct >= 100
    ? '<span class="material-symbols-outlined text-lg text-success shrink-0">check_circle</span>'
    : '';
  const rowShell = (id, inner, pct, i) => `
    <div class="lib-item stagger-in relative overflow-hidden flex items-center justify-between gap-3 bg-surface-container rounded-xl px-3.5 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-id="${escapeHtml(id)}" style="--i:${i}">
      ${inner}
      ${doneTick(pct)}
      ${progressEdge(pct)}
    </div>`;
  const renderRow = (it, i) => {
    const len = (it.body || '').length || 1;
    const prog = Math.max(0, Math.min(len, it.progress || 0));
    const pct = Math.round(prog / len * 100);
    // The edge line carries the read fraction; words only give a sense of size.
    let meta = pct >= 100 ? 'Finished'
      : `${countWords(stripImageMarkers(it.body)).toLocaleString()} words`;
    // Publication date, when the source provided one.
    const pub = fmtPubDate(it.published);
    if (pub) meta += ` · ${pub}`;
    // Feed provenance gets its own line under the title: feed title, or the
    // hostname once the feed is deleted. Web/text imports have no line.
    let source = '';
    if (it.feed_id || it.url) {
      const feed = feedsById[it.feed_id];
      source = (feed && feed.title) || '';
      if (!source && it.url) { try { source = new URL(it.url).hostname; } catch (_) {} }
    }
    const newChip = (it.feed_id && !it.seen && !(it.progress > 0))
      ? '<span class="inline-block align-[2px] text-[10px] font-semibold text-primary bg-primary/10 rounded-full px-2 py-0.5 mr-1.5">NEW</span>'
      : '';
    const tags = (it.tags || []).length
      ? `<div class="flex flex-wrap gap-1 mt-1">${it.tags.map(t =>
          `<span class="text-[10px] font-medium px-1.5 py-0.5 rounded bg-surface-container-highest text-on-surface-variant">#${escapeHtml(t)}</span>`).join('')}</div>`
      : '';
    return rowShell(it.id, `
      ${coverTileHtml(it, 'w-11 h-11 rounded-lg self-start mt-0.5', 'text-sm')}
      <div class="min-w-0 flex-1">
        <div class="text-[15px] font-semibold leading-snug text-on-surface line-clamp-2">${newChip}${escapeHtml(it.title)}</div>
        ${source ? `<div class="text-xs font-medium text-primary/90 truncate mt-0.5">${escapeHtml(source)}</div>` : ''}
        <div class="text-xs text-on-surface-variant tabular-nums mt-0.5">${meta}</div>
        ${tags}
      </div>`, pct, i);
  };
  const shown = items.slice().reverse().filter(libItemMatchesFilters);
  document.getElementById('lib-count').textContent = `${shown.length.toLocaleString()} article${shown.length === 1 ? '' : 's'}`;
  renderLibFilterChips();
  const filtering = libSource !== 'all' || libTagFilter || libSearch;
  const archiveTab = libTab === 'archive';
  // An empty Articles tab with archived articles is caught up, not empty.
  const caughtUp = !archiveTab && !filtering && items.some(it => it.archived);
  empty.classList.toggle('hidden', shown.length > 0);
  document.getElementById('lib-empty-title').textContent = caughtUp ? 'All caught up'
    : filtering ? 'Nothing matches'
    : archiveTab ? 'Nothing archived yet' : 'No saved texts yet';
  document.getElementById('lib-empty-sub').textContent = archiveTab
    ? "Archive what you've read to keep it here"
    : 'Articles and pasted text all live here';
  // The "add your first" CTA only fits a library that's genuinely empty.
  document.getElementById('lib-empty-add').hidden = filtering || caughtUp || archiveTab;
  list.innerHTML = shown.map(renderRow).join('');
  list.querySelectorAll('.lib-item').forEach(el => {
    const id = el.dataset.id;
    el.addEventListener('click', (e) => {
      if (el._longPressed) { el._longPressed = false; return; }
      openReading(id);
    });
    attachLongPress(el, () => {
      const item = items.find(it => it.id === id);
      if (item) openLibActionSheet(item);
    });
  });
}

// Delete a library item, invoked from the long-press sheet.
async function deleteLibraryItem(id) {
  const item = libItems.find(it => it.id === id);
  const ok = await showConfirm(`Delete "${item ? item.title : 'this item'}"?`);
  if (!ok) return;
  // Deleting what's on screen: leave, or the reader keeps showing a deleted item.
  const wasOpen = readingItem && readingItem.id === id;
  if (wasOpen) {
    readingItem = null;
    readingText = '';
  }
  await invoke('library_delete', { id });
  if (wasOpen) navigateTo('library');
  loadLibrary();
}

// ── Add-text modal ──

const libAddModal = document.getElementById('lib-add-modal');
const addChooserModal = document.getElementById('add-chooser-modal');
const addUrlModal = document.getElementById('add-url-modal');
function openModal(el) { el.classList.remove('hidden'); el.classList.add('flex'); }
function closeModal(el) { el.classList.add('hidden'); el.classList.remove('flex'); }

// The bottom-nav + button opens a source chooser (Text, URL, File).
function openAddModal() { openModal(addChooserModal); }

// ── Tag picker inside the add modals ──
//
// Tags chosen before the item exists, so they're held here and passed to
// library_add. Keyed by modal ('text' | 'url') since both can hold their own
// pending selection. Existing tags are one-tap chips; free text covers the rest.
const pendingTags = { text: [], url: [] };

function renderPendingTags(kind) {
  const host = document.querySelector(`[data-tagpicker="${kind}"]`);
  if (!host) return;
  const chosen = pendingTags[kind];
  const suggestions = allLibraryTags()
    .filter(t => !chosen.some(x => x.toLowerCase() === t.toLowerCase()))
    .slice(0, 8);
  host.innerHTML = `
    <div class="flex items-center gap-2">
      <input class="tp-input flex-1 min-w-0 bg-surface-container-highest border border-outline-variant/30 rounded-lg px-3 py-2 text-sm text-on-surface placeholder:text-on-surface-variant/50 focus:outline-none focus:ring-1 focus:ring-primary/50"
        type="text" maxlength="32" placeholder="Add a tag (optional)" />
      <button class="tp-add shrink-0 text-xs font-semibold px-3 py-2 rounded-lg border border-outline-variant/30 text-on-surface-variant hover:bg-surface-container-highest cursor-pointer">Add</button>
    </div>
    ${chosen.length ? `<div class="flex flex-wrap gap-1.5 mt-2">${chosen.map(t => `
      <span class="inline-flex items-center gap-1 text-xs font-medium pl-2.5 pr-1 py-1 rounded-full bg-primary/15 text-primary">
        #${escapeHtml(t)}
        <button class="tp-remove w-4 h-4 flex items-center justify-center rounded-full hover:bg-primary/20 cursor-pointer" data-tag="${escapeHtml(t)}" aria-label="Remove ${escapeHtml(t)}">
          <span class="material-symbols-outlined text-[13px]">close</span>
        </button>
      </span>`).join('')}</div>` : ''}
    ${suggestions.length ? `<div class="flex flex-wrap gap-1.5 mt-2">${suggestions.map(t =>
      `<button class="tp-suggest text-xs font-medium px-2 py-1 rounded-full border border-outline-variant/30 text-on-surface-variant hover:bg-surface-container-highest cursor-pointer" data-tag="${escapeHtml(t)}">#${escapeHtml(t)}</button>`).join('')}</div>` : ''}`;
}

function addPendingTag(kind, value) {
  const t = (value || '').trim();
  if (!t) return;
  if (!pendingTags[kind].some(x => x.toLowerCase() === t.toLowerCase())) {
    pendingTags[kind].push(t);
  }
  renderPendingTags(kind);
}

// One delegated handler per picker host, bound once.
for (const kind of ['text', 'url']) {
  const host = document.querySelector(`[data-tagpicker="${kind}"]`);
  if (!host) continue;
  host.addEventListener('click', (e) => {
    const remove = e.target.closest('.tp-remove');
    if (remove) {
      pendingTags[kind] = pendingTags[kind].filter(t => t !== remove.dataset.tag);
      renderPendingTags(kind);
      return;
    }
    const suggest = e.target.closest('.tp-suggest');
    if (suggest) { addPendingTag(kind, suggest.dataset.tag); return; }
    if (e.target.closest('.tp-add')) addPendingTag(kind, host.querySelector('.tp-input').value);
  });
  host.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && e.target.classList.contains('tp-input')) {
      e.preventDefault(); // Enter adds the tag, never submits the modal
      addPendingTag(kind, e.target.value);
    }
  });
}

function openTextModal() {
  document.getElementById('lib-add-title').value = '';
  document.getElementById('lib-add-body').value = '';
  pendingTags.text = [];
  renderPendingTags('text');
  openModal(libAddModal);
}
function closeAddModal() { closeModal(libAddModal); }

function openUrlModal() {
  document.getElementById('add-url-input').value = '';
  pendingTags.url = [];
  renderPendingTags('url');
  openModal(addUrlModal);
}
function closeUrlModal() { closeModal(addUrlModal); }

document.getElementById('add-chooser-cancel').addEventListener('click', () => closeModal(addChooserModal));
addChooserModal.addEventListener('click', (e) => {
  if (e.target === addChooserModal) { closeModal(addChooserModal); return; } // backdrop
  const btn = e.target.closest('.add-source');
  if (!btn) return;
  const src = btn.dataset.source;
  if (src === 'text') { closeModal(addChooserModal); openTextModal(); }
  else if (src === 'url') { closeModal(addChooserModal); openUrlModal(); }
  else if (src === 'file') { closeModal(addChooserModal); pickImportFile(); }
});

document.getElementById('lib-add-cancel').addEventListener('click', closeAddModal);
document.getElementById('lib-add-save').addEventListener('click', async () => {
  const title = document.getElementById('lib-add-title').value;
  const body = document.getElementById('lib-add-body').value;
  if (!body.trim()) { showToast('Paste some text first'); return; }
  try {
    await invoke('library_add', { title: title || null, body, tags: pendingTags.text });
    closeAddModal();
    loadLibrary();
  } catch (err) { showToast('Save failed: ' + err); }
});

document.getElementById('add-url-cancel').addEventListener('click', closeUrlModal);
document.getElementById('add-url-save').addEventListener('click', () => {
  let url = document.getElementById('add-url-input').value.trim();
  if (!url) { showToast('Paste a URL first'); return; }
  if (!/^https?:\/\//i.test(url)) url = 'https://' + url;
  const tags = pendingTags.url.slice(); // closeUrlModal resets the picker
  closeUrlModal();
  // importSharedText extracts the URL, fetches + Readability-parses in Rust,
  // adds to the library, switches to Listen and opens the reader.
  importSharedText(url, tags, true);
});

// ── File import ──

const importFileInput = document.getElementById('import-file-input');

// Open the OS file/document picker for .txt/.md/.pdf. Both extensions AND
// MIME types are listed in `accept`: on some devices a bare extension whose
// MIME can't be resolved gets silently dropped by RustWebChromeClient's
// getValidTypes, narrowing the picker to nothing.
function pickImportFile() {
  importFileInput.accept = '.txt,.md,.markdown,.pdf,text/plain,text/markdown,application/pdf,application/octet-stream';
  importFileInput.click();
}

importFileInput.addEventListener('change', () => {
  const file = importFileInput.files && importFileInput.files[0];
  importFileInput.value = ''; // let picking the same file again still fire 'change'
  if (file) importFile(file);
});

// Import a picked file as one article, however long. SAF's reported MIME
// type is unreliable, so parsing is routed by the filename's own extension,
// not file.type; an extensionless name reads as plain text.
async function importFile(file) {
  const name = file.name || 'Untitled';
  const extMatch = name.match(/\.([^.]+)$/);
  const ext = extMatch ? extMatch[1].toLowerCase() : 'txt';
  const titleFromName = name.replace(/\.[^.]+$/, '') || 'Untitled';
  showToast('Importing…');
  try {
    let text;
    if (ext === 'pdf') {
      const buf = await file.arrayBuffer();
      try {
        text = (await parsePdf(buf)).text;
      } catch (err) {
        showToast('Could not read PDF: ' + err);
        return;
      }
    } else {
      const raw = (await file.text()).replace(/\r\n/g, '\n');
      text = (ext === 'md' || ext === 'markdown') ? markdownToText(raw) : raw;
    }
    if (!text.trim()) { showToast('No text found in file'); return; }

    const item = await invoke('library_add', { title: titleFromName, body: text });
    await loadLibrary();
    openReading(item.id);
    showToast('Text added');
  } catch (err) {
    showToast('Import failed: ' + err);
  }
}

// ── Feeds (RSS subscriptions) ──

let pollingFeeds = false;
let browsingFeed = null;
// Entries parsed this session, per feed id, backing the browse view.
const sessionEntries = {};
// Browse-view pagination state per feed id: {page, nextUrl, exhausted}.
// Historical pages are display + manual-add only — they never touch
// seen-tracking or auto-import (those work on the feed's first page).
const feedPaging = {};

// Cloudflare-style browser checks 403 the app's HTTP client outright (the TLS
// fingerprint gives it away, headers don't help). Fall back to a hidden
// Android WebView — a real browser engine that passes the JS challenge. Slow
// (seconds on first hit while the challenge runs), so only on 403/503.
function isBotBlocked(err) {
  return /HTTP (403|503)/.test(String(err));
}

async function fetchFeedBody(url, etag, lastModified) {
  try {
    return await invoke('fetch_feed', { url, etag, lastModified });
  } catch (err) {
    if (!isBotBlocked(err)) throw err;
    const body = await invoke('webview_fetch', { url });
    return { not_modified: false, body, etag: '', last_modified: '' };
  }
}

async function fetchArticleHtml(url) {
  try {
    return await invoke('fetch_article', { url });
  } catch (err) {
    if (!isBotBlocked(err)) throw err;
    return await invoke('webview_fetch', { url });
  }
}

// Canonical form for matching an article URL against the library: drop the
// fragment and a trailing slash so cosmetic variants of the same link match.
function normArticleUrl(u) {
  if (!u) return '';
  try {
    const x = new URL(u);
    x.hash = '';
    return x.href.replace(/\/$/, '');
  } catch (_) {
    return u.trim().replace(/\/$/, '');
  }
}

// Parse RSS 2.0 / Atom XML into {title, entries: [{key, title, link, date,
// dateMs, contentHtml}]}, or null when it isn't a recognizable feed. CSS type
// selectors match any namespace in XML documents, so Atom elements resolve
// without namespace plumbing; only content:encoded needs getElementsByTagNameNS.
function parseFeedXml(xml, feedUrl) {
  const doc = new DOMParser().parseFromString(xml, 'text/xml');
  if (doc.querySelector('parsererror')) return null;

  const text = (el, sel) => {
    const n = el.querySelector(sel);
    return n ? (n.textContent || '').trim() : '';
  };
  // Inner HTML of an XML node: element children (Atom xhtml content) get
  // serialized; plain or entity-escaped content comes back as text.
  const nodeHtml = (n) => {
    if (!n) return '';
    if (n.children && n.children.length) {
      const s = new XMLSerializer();
      return Array.from(n.childNodes).map(c => s.serializeToString(c)).join('');
    }
    return (n.textContent || '').trim();
  };
  const resolveLink = (link) => {
    if (!link) return '';
    try { return new URL(link, feedUrl).href; } catch (_) { return link; }
  };
  const entryOf = (el, isAtom) => {
    let link = '';
    if (isAtom) {
      const links = Array.from(el.querySelectorAll('link'));
      const alt = links.find(l => (l.getAttribute('rel') || 'alternate') === 'alternate');
      link = (alt || links[0]) ? ((alt || links[0]).getAttribute('href') || '') : '';
    } else {
      link = text(el, 'link');
      if (!link) {
        // Some RSS items carry an atom:link (href attribute) instead.
        const l = el.querySelector('link');
        link = l ? (l.getAttribute('href') || '') : '';
      }
    }
    link = resolveLink(link);
    const guid = isAtom ? text(el, 'id') : text(el, 'guid');
    const title = text(el, 'title');
    const date = isAtom
      ? (text(el, 'published') || text(el, 'updated'))
      : text(el, 'pubDate');
    const dateMs = date ? (Date.parse(date) || 0) : 0;
    const contentHtml = isAtom
      ? (nodeHtml(el.querySelector('content')) || nodeHtml(el.querySelector('summary')))
      : (nodeHtml(el.getElementsByTagNameNS('*', 'encoded')[0]) || nodeHtml(el.querySelector('description')));
    // Categories become article tags. Atom puts the label in a `term`
    // attribute, RSS in the element text; a few feeds use both spellings, so
    // read whichever is present. Bounded per entry — a feed that tags an
    // article twenty ways shouldn't dominate the library's tag set.
    const categories = [];
    for (const c of el.querySelectorAll('category')) {
      const label = (c.getAttribute('term') || c.textContent || '').trim();
      // Skip URL-ish schemes some feeds put in `term`, and anything long
      // enough to be a sentence rather than a label.
      if (!label || label.length > 32 || /^https?:\/\//i.test(label)) continue;
      if (!categories.some(t => t.toLowerCase() === label.toLowerCase())) categories.push(label);
      if (categories.length >= 8) break;
    }
    // Stable identity for seen-tracking; capped so a pathological guid can't
    // bloat the stored list.
    const key = (guid || link || `${title}|${date}`).slice(0, 500);
    return { key, title, link, date, dateMs, contentHtml, categories };
  };

  // RFC 5005-style pagination: a feed-level link rel="next" pointing at the
  // next (older) page. Resolved against the feed URL.
  const nextLink = () => {
    for (const l of doc.querySelectorAll('link[rel="next"][href]')) {
      if (!l.closest('item') && !l.closest('entry')) {
        return resolveLink(l.getAttribute('href'));
      }
    }
    return '';
  };
  const rssItems = Array.from(doc.querySelectorAll('channel > item'));
  if (rssItems.length || doc.querySelector('channel > title')) {
    return {
      title: text(doc, 'channel > title'),
      entries: rssItems.map(el => entryOf(el, false)).filter(e => e.key),
      next: nextLink(),
    };
  }
  const atomEntries = Array.from(doc.querySelectorAll('entry'));
  if (atomEntries.length || (doc.documentElement && doc.documentElement.localName === 'feed')) {
    return {
      title: text(doc, 'feed > title'),
      entries: atomEntries.map(el => entryOf(el, true)).filter(e => e.key),
      next: nextLink(),
    };
  }
  return null;
}

// Discover a feed for a site URL that isn't itself a feed. Two passes,
// borrowed from rsslookup: (1) the page's <link rel="alternate"> feed tags —
// the reliable, site-declared signal; (2) common feed paths probed relative
// to the page and its origin. Every candidate is validated by actually
// parsing it as a feed (stronger than a content-type sniff, and we need the
// parse for feed_add's pre-seeding anyway). Returns {url, parsed} or null.
const COMMON_FEED_PATHS = [
  '/atom', '/atom.xml', '/feed', '/feed/', '/feed.rss', '/feed.xml',
  '/index.rss', '/index.xml', '/rss', '/rss/', '/rss.xml',
  'atom', 'atom.xml', 'feed', 'feed/', 'feed.rss', 'feed.xml',
  'index.rss', 'index.xml', 'rss', 'rss/', 'rss.xml',
];

async function discoverFeed(pageUrl, pageHtml) {
  // Pass 1: <link rel="alternate" type="application/rss+xml|atom+xml">.
  const linkCandidates = [];
  try {
    const doc = new DOMParser().parseFromString(pageHtml, 'text/html');
    doc.querySelectorAll('link[rel~="alternate"][href]').forEach(l => {
      const type = (l.getAttribute('type') || '').toLowerCase();
      if (!/(rss|atom)/.test(type) || !type.includes('xml')) return;
      let href = l.getAttribute('href');
      try { href = new URL(href, pageUrl).href; } catch (_) { return; }
      // Skip per-post comment feeds (WordPress emits one beside the real feed).
      if (/comment/i.test(href)) return;
      if (!linkCandidates.includes(href)) linkCandidates.push(href);
    });
  } catch (_) {}
  // Site-declared candidates may sit behind the same bot check as the page,
  // so they get the WebView fallback. Bounded: first 3.
  for (const cand of linkCandidates.slice(0, 3)) {
    try {
      const r = await fetchFeedBody(cand, '', '');
      const parsed = parseFeedXml(r.body, cand);
      if (parsed && parsed.entries.length) return { url: cand, parsed };
    } catch (_) {}
  }

  // Pass 2: common paths, deduped (at a site root the absolute and relative
  // variants collapse). Plain fetch only — no WebView fallback here, or a
  // bot-walled site would run the seconds-long challenge per probe.
  const probes = [];
  for (const path of COMMON_FEED_PATHS) {
    try {
      const u = new URL(path, pageUrl).href;
      if (u !== pageUrl && !probes.includes(u)) probes.push(u);
    } catch (_) {}
  }
  for (const cand of probes.slice(0, 12)) {
    try {
      const r = await invoke('fetch_feed', { url: cand, etag: '', lastModified: '' });
      const parsed = parseFeedXml(r.body, cand);
      if (parsed && parsed.entries.length) return { url: cand, parsed };
    } catch (_) {}
  }
  return null;
}

// Fetch the next (older) page of a feed for the browse view. Strategy: the
// feed's declared rel="next" archive link when present, else WordPress's
// ?paged=N convention. A backend that supports neither either 404s or
// returns the same entries again — both read as "no older articles".
async function loadMoreEntries(feed) {
  const paging = feedPaging[feed.id] || { page: 1, nextUrl: '', exhausted: false };
  feedPaging[feed.id] = paging;
  const known = new Set((sessionEntries[feed.id] || []).map(e => e.key));
  let url = paging.nextUrl;
  if (!url) {
    const sep = feed.url.includes('?') ? '&' : '?';
    url = `${feed.url}${sep}paged=${paging.page + 1}`;
  }
  try {
    const r = await fetchFeedBody(url, '', '');
    const parsed = parseFeedXml(r.body, url);
    const fresh = parsed ? parsed.entries.filter(e => !known.has(e.key)) : [];
    if (!fresh.length) {
      paging.exhausted = true;
      showToast('No older articles available');
      return;
    }
    sessionEntries[feed.id] = (sessionEntries[feed.id] || []).concat(fresh);
    paging.page += 1;
    paging.nextUrl = (parsed && parsed.next) || '';
  } catch (_) {
    // Out of pages (WordPress 404s past the end) or the site can't paginate.
    paging.exhausted = true;
    showToast('No older articles available');
  }
}

// Import one feed entry into the library. Prefers the feed's embedded HTML
// when it's substantial (full-content feeds like Substack); otherwise fetches
// the article page. Throws when nothing readable can be extracted.
async function importFeedEntry(feed, entry) {
  let article = null;
  if (entry.contentHtml) {
    const textLen = new DOMParser().parseFromString(entry.contentHtml, 'text/html')
      .body.textContent.trim().length;
    if (textLen >= 500) {
      try {
        article = extractArticle(
          `<html><head></head><body><article>${entry.contentHtml}</article></body></html>`,
          entry.link || feed.url
        );
      } catch (_) {}
    }
  }
  if (!article && entry.link) {
    const html = await fetchArticleHtml(entry.link);
    article = await extractArticleSmart(html, entry.link);
  }
  if (!article) throw 'no readable article';
  return await invoke('library_add', {
    title: entry.title || article.title || entry.link,
    body: article.body,
    url: entry.link || null,
    feedId: feed.id,
    guid: entry.key,
    // Feed entry date first (authoritative); page metadata as fallback.
    published: entry.date || article.published || null,
    imageUrl: article.imageUrl || null,
    tags: entry.categories || [],
  });
}

async function libraryUrlSet() {
  const set = new Set();
  try {
    (await invoke('library_list')).forEach(it => {
      const u = normArticleUrl(it.url);
      if (u) set.add(u);
    });
  } catch (_) {}
  return set;
}

// One feed's poll: fetch (conditional GET unless noValidators), parse, auto-
// import what's new, then commit seen keys and validators. Ordering matters
// for crash safety: state is only written after imports, so a crash mid-poll
// means the next check re-fetches and picks the entries up again. Returns the
// number of articles imported; throws on fetch/parse failure.
async function checkFeed(feed, libUrls, opts) {
  const useValidators = !(opts && opts.noValidators);
  const r = await fetchFeedBody(
    feed.url,
    useValidators ? (feed.etag || '') : '',
    useValidators ? (feed.last_modified || '') : ''
  );
  if (r.not_modified) {
    await invoke('feed_checked', { id: feed.id, etag: feed.etag || '', lastModified: feed.last_modified || '' });
    return 0;
  }
  const parsed = parseFeedXml(r.body, feed.url);
  if (!parsed) throw 'not a valid feed';
  sessionEntries[feed.id] = parsed.entries;
  feedPaging[feed.id] = { page: 1, nextUrl: parsed.next || '', exhausted: false };
  const seen = new Set(feed.seen || []);
  const fresh = parsed.entries.filter(en => !seen.has(en.key));
  let imported = 0;
  if (feed.auto_add && fresh.length) {
    // Belt and braces on top of seen-tracking: auto-import only entries
    // published after the feed was added (with a week's slack for feeds that
    // list late). Undated entries pass. This keeps any dedup regression from
    // resurrecting archive posts; older entries stay addable from browse.
    const addedMs = Date.parse(feed.added || '') || 0;
    const cutoffMs = addedMs ? addedMs - 7 * 24 * 3600 * 1000 : 0;
    const eligible = fresh.filter(en => !en.dateMs || en.dateMs >= cutoffMs);
    // Cap the auto-import at the 5 newest; import oldest-first so insertion
    // order keeps the library list newest-on-top. Entries beyond the cap are
    // marked seen below and stay reachable from the browse view.
    const newest = eligible.slice()
      .sort((a, b) => (b.dateMs || 0) - (a.dateMs || 0))
      .slice(0, 5)
      .reverse();
    for (const entry of newest) {
      if (entry.link && libUrls.has(normArticleUrl(entry.link))) continue;
      try {
        await importFeedEntry(feed, entry);
        if (entry.link) libUrls.add(normArticleUrl(entry.link));
        imported++;
      } catch (_) {
        // Unreadable entry: skip. It's still marked seen (never retry-loop a
        // broken page); manual add from the browse view remains possible.
      }
    }
  }
  // Seen = the feed's FULL current key set (the backend keeps a grace buffer
  // of recently-departed keys on top). Passing only fresh keys into a capped
  // FIFO churned on feeds listing more entries than the cap: live keys got
  // evicted, looked new again, and resurrected old posts every poll.
  await invoke('feed_mark_seen', { id: feed.id, keys: parsed.entries.map(en => en.key) });
  await invoke('feed_checked', { id: feed.id, etag: r.etag || '', lastModified: r.last_modified || '' });
  return imported;
}

// Check every feed for new articles. Runs at most once at a time; called on
// app open (silent) and from refresh button / pull-to-refresh (interactive).
async function pollFeeds(opts) {
  const interactive = !!(opts && opts.interactive);
  if (pollingFeeds) return;
  pollingFeeds = true;
  const btn = document.getElementById('feeds-refresh');
  btn.disabled = true;
  const btnLabel = btn.textContent;
  btn.textContent = 'Refreshing…';
  let imported = 0;
  let failures = 0;
  let hadFeeds = false;
  try {
    const feeds = await invoke('feeds_list');
    hadFeeds = feeds.length > 0;
    if (hadFeeds) {
      const libUrls = await libraryUrlSet();
      for (const feed of feeds) {
        try {
          imported += await checkFeed(feed, libUrls);
        } catch (_) {
          failures++;
        }
      }
      await backfillFeedTags();
    }
  } finally {
    pollingFeeds = false;
    btn.disabled = false;
    btn.textContent = btnLabel;
  }
  if (imported > 0) {
    showToast(imported === 1 ? '1 new article' : `${imported} new articles`);
    if (!document.getElementById('library').classList.contains('hidden')) loadLibrary();
  } else if (interactive && hadFeeds) {
    showToast(failures ? 'Some feeds failed to update' : 'No new articles');
  }
  if (!document.getElementById('feeds').classList.contains('hidden')) loadFeeds();
}

// Feed categories only land on articles imported since that shipped, so a
// library built up before it has no tags at all — and an empty filter row
// reads as a broken feature. Any already-imported article whose entry is still
// in the feed can be matched by guid and tagged after the fact. Runs after
// every poll and skips items that already have tags, so it costs nothing once
// caught up. Feeds answering 304 contribute no entries; those articles get
// picked up on the next poll that actually returns content.
async function backfillFeedTags() {
  const byGuid = new Map();
  for (const entries of Object.values(sessionEntries)) {
    for (const en of entries || []) {
      if (en.categories && en.categories.length) byGuid.set(en.key, en.categories);
    }
  }
  if (!byGuid.size) return;
  let items;
  try { items = await invoke('library_list'); } catch (_) { return; }
  let tagged = 0;
  for (const it of items) {
    if (!it.feed_id || !it.guid || (it.tags || []).length) continue;
    const tags = byGuid.get(it.guid);
    if (!tags) continue;
    try {
      await invoke('library_set_tags', { id: it.id, tags });
      tagged++;
    } catch (_) { /* one failure shouldn't stop the sweep */ }
  }
  if (tagged) loadLibrary();
}

async function loadFeeds() {
  const feeds = await invoke('feeds_list');
  const list = document.getElementById('feeds-list');
  const empty = document.getElementById('feeds-empty');
  empty.classList.toggle('hidden', feeds.length > 0);
  list.innerHTML = feeds.slice().reverse().map((f, i) => {
    const checked = f.last_checked ? formatTimestamp(f.last_checked) : 'never';
    const meta = `Checked ${checked} · Auto-add ${f.auto_add ? 'on' : 'off'}`;
    return `
    <div class="feed-item stagger-in flex items-center justify-between gap-3 bg-surface-container rounded-xl px-3.5 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-id="${escapeHtml(f.id)}" style="--i:${i}">
      <div class="${coverClass(f.title)} w-11 h-11 rounded-lg self-start mt-0.5 text-sm">${escapeHtml(coverMonogram(f.title))}</div>
      <div class="min-w-0 flex-1">
        <div class="text-[15px] font-semibold leading-snug text-on-surface truncate">${escapeHtml(f.title)}</div>
        <div class="text-xs text-on-surface-variant truncate mt-0.5">${escapeHtml(f.url)}</div>
        <div class="text-xs text-on-surface-variant tabular-nums mt-0.5">${meta}</div>
      </div>
      <button class="feed-edit shrink-0 text-on-surface-variant/50 hover:text-primary transition-colors p-1 cursor-pointer" data-id="${escapeHtml(f.id)}">
        <span class="material-symbols-outlined text-lg">edit</span>
      </button>
      <button class="feed-del shrink-0 text-on-surface-variant/50 hover:text-error transition-colors p-1 cursor-pointer" data-id="${escapeHtml(f.id)}">
        <span class="material-symbols-outlined text-lg">delete</span>
      </button>
    </div>`;
  }).join('');
  list.querySelectorAll('.feed-item').forEach(el => {
    el.addEventListener('click', (e) => {
      if (e.target.closest('.feed-del') || e.target.closest('.feed-edit')) return;
      openFeedEntries(el.dataset.id);
    });
  });
  list.querySelectorAll('.feed-edit').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const feed = feeds.find(f => f.id === btn.dataset.id);
      if (feed) openFeedEditModal(feed);
    });
  });
  list.querySelectorAll('.feed-del').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      await invoke('feed_delete', { id: btn.dataset.id });
      loadFeeds();
    });
  });
}

// Browse all entries currently in a feed. Browsing counts as a check: new
// entries auto-import (when enabled) and everything current is marked seen.
async function openFeedEntries(id) {
  const feeds = await invoke('feeds_list');
  const feed = feeds.find(f => f.id === id);
  if (!feed) return;
  browsingFeed = feed;
  document.getElementById('feed-entries-title').textContent = feed.title;
  document.getElementById('feed-auto-add').checked = !!feed.auto_add;
  showPanel('feed-entries');
  setBottomNavVisible(false); // detail view
  const listEl = document.getElementById('feed-entries-list');
  if (!sessionEntries[feed.id]) {
    listEl.innerHTML = '<div class="text-xs text-on-surface-variant px-1 py-4">Loading…</div>';
    if (!pollingFeeds) {
      pollingFeeds = true;
      try {
        // Empty validators: the browse view always needs the body, a 304
        // would leave it with nothing to show.
        await checkFeed(feed, await libraryUrlSet(), { noValidators: true });
      } catch (err) {
        listEl.innerHTML = `<div class="text-xs text-error px-1 py-4">Could not load feed: ${escapeHtml(String(err))}</div>`;
        return;
      } finally {
        pollingFeeds = false;
      }
    } else {
      // A poll is already running; fetch for display only, no state writes.
      try {
        const r = await fetchFeedBody(feed.url, '', '');
        const parsed = parseFeedXml(r.body, feed.url);
        if (parsed) {
          sessionEntries[feed.id] = parsed.entries;
          feedPaging[feed.id] = { page: 1, nextUrl: parsed.next || '', exhausted: false };
        }
      } catch (_) {}
    }
  }
  renderFeedEntries(feed);
}

async function renderFeedEntries(feed) {
  const entries = sessionEntries[feed.id] || [];
  const listEl = document.getElementById('feed-entries-list');
  if (!entries.length) {
    listEl.innerHTML = '<div class="text-xs text-on-surface-variant px-1 py-4">No entries in this feed</div>';
    return;
  }
  // "Added" state: exact provenance via the stored entry key, else the
  // normalized article URL (also matches articles imported via share).
  const byGuid = {};
  const byUrl = {};
  try {
    (await invoke('library_list')).forEach(it => {
      if (it.guid) byGuid[it.guid] = it.id;
      const u = normArticleUrl(it.url);
      if (u) byUrl[u] = it.id;
    });
  } catch (_) {}
  const paging = feedPaging[feed.id];
  const canLoadMore = !!paging && !paging.exhausted;
  listEl.innerHTML = entries.map((e, i) => {
    const itemId = byGuid[e.key] || (e.link ? byUrl[normArticleUrl(e.link)] : '') || '';
    // Date-only, with the year once it isn't current — a 2011 archive post
    // must not read like this week's.
    const date = e.date ? (fmtPubDate(e.date) || formatTimestamp(e.date)) : '';
    const right = itemId
      ? '<span class="shrink-0 flex items-center gap-1 text-xs text-on-surface-variant"><span class="material-symbols-outlined text-base">check</span>Added</span>'
      : `<button class="feed-entry-add shrink-0 w-10 h-10 flex items-center justify-center rounded-full text-on-surface-variant hover:text-primary hover:bg-primary/10 transition-colors cursor-pointer" data-i="${i}">
           <span class="material-symbols-outlined text-xl">add_circle</span>
         </button>`;
    return `
    <div class="feed-entry stagger-in flex items-center justify-between gap-3 bg-surface-container rounded-xl px-3.5 py-3 ${itemId ? 'cursor-pointer hover:bg-surface-container-high transition-colors' : ''}" data-item-id="${escapeHtml(itemId)}" style="--i:${i}">
      <div class="min-w-0 flex-1">
        <div class="text-[15px] font-semibold leading-snug text-on-surface line-clamp-2">${escapeHtml(e.title || e.link || 'Untitled')}</div>
        ${date ? `<div class="text-xs text-on-surface-variant tabular-nums mt-1">${escapeHtml(date)}</div>` : ''}
      </div>
      ${right}
    </div>`;
  }).join('') + (canLoadMore
    ? '<button id="feed-load-more" class="w-full text-xs font-semibold text-on-surface-variant hover:text-primary transition-colors px-4 py-3 rounded-xl bg-surface-container-low hover:bg-surface-container-high cursor-pointer">Load older articles</button>'
    : '');
  const moreBtn = document.getElementById('feed-load-more');
  if (moreBtn) {
    moreBtn.addEventListener('click', async () => {
      moreBtn.disabled = true;
      moreBtn.textContent = 'Loading…';
      await loadMoreEntries(feed);
      renderFeedEntries(feed);
    });
  }
  listEl.querySelectorAll('.feed-entry').forEach(el => {
    el.addEventListener('click', (e) => {
      if (e.target.closest('.feed-entry-add')) return;
      if (el.dataset.itemId) openReading(el.dataset.itemId);
    });
  });
  listEl.querySelectorAll('.feed-entry-add').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const entry = entries[Number(btn.dataset.i)];
      if (!entry) return;
      btn.disabled = true;
      try {
        await importFeedEntry(feed, entry);
        showToast('Article added');
        renderFeedEntries(feed);
      } catch (err) {
        showToast('Could not add: ' + err);
        btn.disabled = false;
        btn.innerHTML = '<span class="material-symbols-outlined text-xl">add_circle</span>';
      }
    });
  });
}

document.getElementById('feeds-refresh').addEventListener('click', () => {
  pollFeeds({ interactive: true });
});

const feedAddModal = document.getElementById('feed-add-modal');
function closeFeedAddModal() {
  feedAddModal.classList.add('hidden');
  feedAddModal.classList.remove('flex');
}
document.getElementById('feed-add-btn').addEventListener('click', () => {
  document.getElementById('feed-add-url').value = '';
  feedAddModal.classList.remove('hidden');
  feedAddModal.classList.add('flex');
});
document.getElementById('feed-add-cancel').addEventListener('click', closeFeedAddModal);
document.getElementById('feed-add-save').addEventListener('click', async () => {
  let url = document.getElementById('feed-add-url').value.trim();
  if (!url) { showToast('Enter a feed or site URL'); return; }
  if (!/^https?:\/\//i.test(url)) url = 'https://' + url;
  // Canonicalize (adds the root slash, lowercases the host) and reject garbage
  // before any network round-trip.
  try { url = new URL(url).href; } catch (_) { showToast('Not a valid URL'); return; }
  const btn = document.getElementById('feed-add-save');
  btn.disabled = true;
  btn.textContent = 'Adding…';
  try {
    // Validate by fetching and parsing before anything is persisted.
    const r = await fetchFeedBody(url, '', '');
    let feedUrl = url;
    let parsed = parseFeedXml(r.body, url);
    if (!parsed) {
      // Not feed XML — treat it as a site page and go find its feed.
      btn.textContent = 'Finding feed…';
      const found = await discoverFeed(url, r.body);
      if (!found) { showToast('No feed found on that site'); return; }
      feedUrl = found.url;
      parsed = found.parsed;
      showToast('Found feed: ' + feedUrl.replace(/^https?:\/\//, ''));
    }
    // Everything currently in the feed is pre-seen: only entries published
    // after this point auto-add. Existing ones stay reachable via browse.
    const feed = await invoke('feed_add', {
      url: feedUrl,
      title: parsed.title || '',
      seen: parsed.entries.map(e => e.key),
    });
    sessionEntries[feed.id] = parsed.entries;
    feedPaging[feed.id] = { page: 1, nextUrl: parsed.next || '', exhausted: false };
    closeFeedAddModal();
    loadFeeds();
  } catch (err) {
    showToast('Could not add feed: ' + err);
  } finally {
    btn.disabled = false;
    btn.textContent = 'Add';
  }
});

const feedEditModal = document.getElementById('feed-edit-modal');
let feedEditId = null;
function openFeedEditModal(feed) {
  feedEditId = feed.id;
  document.getElementById('feed-edit-title').value = feed.title;
  document.getElementById('feed-edit-url').value = feed.url;
  feedEditModal.classList.remove('hidden');
  feedEditModal.classList.add('flex');
}
function closeFeedEditModal() {
  feedEditModal.classList.add('hidden');
  feedEditModal.classList.remove('flex');
}
document.getElementById('feed-edit-cancel').addEventListener('click', closeFeedEditModal);
document.getElementById('feed-edit-save').addEventListener('click', async () => {
  const title = document.getElementById('feed-edit-title').value.trim();
  let url = document.getElementById('feed-edit-url').value.trim();
  if (!url) { showToast('Enter a feed URL'); return; }
  if (!/^https?:\/\//i.test(url)) url = 'https://' + url;
  try {
    await invoke('feed_update', { id: feedEditId, title, url });
    closeFeedEditModal();
    loadFeeds();
  } catch (err) {
    showToast('Could not save: ' + err);
  }
});

document.getElementById('feed-entries-back').addEventListener('click', () => {
  browsingFeed = null;
  navigateTo('feeds');
});

document.getElementById('feed-auto-add').addEventListener('change', (e) => {
  if (!browsingFeed) return;
  browsingFeed.auto_add = e.target.checked;
  invoke('feed_set_auto_add', { id: browsingFeed.id, autoAdd: e.target.checked }).catch(() => {});
});

// Android-style pull-to-refresh, scoped to the Feeds list (the Refresh button
// covers desktop). Dragging the list down from the top opens a gap holding an
// indicator; past THRESHOLD it arms ("Release to refresh"), and on release it
// spins while onRefresh() (a promise) runs. Only the Feeds page has it — other
// lists don't fetch anything, so a pull there would surprise.
function attachPullToRefresh(scrollEl, ptr, icon, text, onRefresh) {
  if (!scrollEl || !ptr) return;
  const H = 48; // indicator height (matches h-12)
  const THRESHOLD = 64; // drag past this to arm
  const MAX = 96; // rubber-band clamp
  let startY = 0, dragging = false, armed = false, busy = false;

  const anim = (on) => {
    const v = on ? 'transform 0.2s ease' : 'none';
    scrollEl.style.transition = v;
    ptr.style.transition = v;
  };
  const place = (pull) => {
    scrollEl.style.transform = `translateY(${pull}px)`;
    ptr.style.transform = `translateY(${pull - H}px)`; // rides just above the list
  };
  const label = (on) => {
    icon.style.transform = on ? 'rotate(180deg)' : 'rotate(0deg)';
    text.textContent = on ? 'Release to refresh' : 'Pull to refresh';
  };
  const home = () => { anim(true); place(0); };

  scrollEl.addEventListener('touchstart', (e) => {
    if (busy) return;
    dragging = scrollEl.scrollTop === 0;
    startY = dragging ? e.touches[0].clientY : 0;
  }, { passive: true });

  scrollEl.addEventListener('touchmove', (e) => {
    if (busy || !dragging) return;
    if (scrollEl.scrollTop > 0) { dragging = false; return; }
    const dy = e.touches[0].clientY - startY;
    if (dy <= 0) { // at/above the top: no pull, leave native scroll alone
      anim(false); place(0);
      if (armed) { armed = false; label(false); }
      return;
    }
    e.preventDefault(); // own the downward pull (listener is non-passive)
    const pull = Math.min(MAX, dy * 0.5); // damped
    anim(false); place(pull);
    const a = pull >= THRESHOLD;
    if (a !== armed) { armed = a; label(a); }
  }, { passive: false });

  scrollEl.addEventListener('touchend', () => {
    if (busy || !dragging) return;
    dragging = false;
    if (!armed) { home(); return; }
    busy = true; armed = false;
    anim(true); place(H); // hold the indicator open while refreshing
    icon.textContent = 'progress_activity';
    icon.style.transform = 'rotate(0deg)';
    text.textContent = 'Refreshing…';
    Promise.resolve(onRefresh()).catch(() => {}).finally(() => {
      icon.textContent = 'arrow_downward';
      text.textContent = 'Pull to refresh';
      home();
      busy = false;
    });
  }, { passive: true });
}
attachPullToRefresh(
  document.getElementById('feeds-scroll'),
  document.getElementById('feeds-ptr'),
  document.getElementById('feeds-ptr-icon'),
  document.getElementById('feeds-ptr-text'),
  () => pollFeeds({ interactive: true }),
);

// ── Tag sheet (library) ──
//
// One editor for both domains. The caller owns persistence: `onChange` gets
// the new tag list, saves it, and returns the list actually stored — the sheet
// only renders. Tags apply on every edit; there's no save button to forget.

const tagSheet = document.getElementById('tag-sheet');
let tagSheetCtx = null; // { subtitle, tags, allTags, onChange }

function openTagSheet(ctx) {
  tagSheetCtx = ctx;
  renderTagSheet();
  tagSheet.classList.remove('hidden');
}
function closeTagSheet() {
  tagSheet.classList.add('hidden');
  tagSheetCtx = null;
}

function renderTagSheet() {
  if (!tagSheetCtx) return;
  const tags = tagSheetCtx.tags || [];
  const suggestions = (tagSheetCtx.allTags || [])
    .filter(t => !tags.some(x => x.toLowerCase() === t.toLowerCase()));
  document.getElementById('tag-sheet-body').innerHTML = `
    <p class="text-sm font-bold text-on-surface mb-1">Tags</p>
    <p class="text-xs text-on-surface-variant mb-4">${escapeHtml(tagSheetCtx.subtitle || '')}</p>
    <div class="flex flex-wrap gap-2 mb-4">
      ${tags.length ? tags.map(t => `
        <span class="inline-flex items-center gap-1 text-xs font-medium pl-2.5 pr-1.5 py-1.5 rounded-full bg-primary/15 text-primary">
          #${escapeHtml(t)}
          <button class="tag-remove w-5 h-5 flex items-center justify-center rounded-full hover:bg-primary/20 cursor-pointer" data-tag="${escapeHtml(t)}" aria-label="Remove ${escapeHtml(t)}">
            <span class="material-symbols-outlined text-[14px]">close</span>
          </button>
        </span>`).join('')
        : '<p class="text-xs text-on-surface-variant/70">No tags yet</p>'}
    </div>
    <div class="flex items-center gap-2 mb-4">
      <input id="tag-input" type="text" placeholder="Add a tag" maxlength="32"
        class="flex-1 min-w-0 bg-surface-container-highest border border-outline-variant/30 rounded-lg px-3 py-2 text-sm text-on-surface placeholder:text-on-surface-variant/50 focus:outline-none focus:ring-1 focus:ring-primary/50" />
      <button id="tag-add" class="shrink-0 text-xs font-semibold px-4 py-2 bg-primary text-on-primary rounded-lg hover:brightness-110 transition-all cursor-pointer">Add</button>
    </div>
    ${suggestions.length ? `
      <p class="text-[11px] font-semibold text-on-surface-variant uppercase tracking-wider mb-2">Already in use</p>
      <div class="flex flex-wrap gap-2">
        ${suggestions.map(t => `<button class="tag-suggest text-xs font-medium px-2.5 py-1.5 rounded-full border border-outline-variant/30 text-on-surface-variant hover:bg-surface-container-highest cursor-pointer" data-tag="${escapeHtml(t)}">#${escapeHtml(t)}</button>`).join('')}
      </div>` : ''}`;
}

async function applyTagChange(tags) {
  if (!tagSheetCtx) return;
  const saved = await tagSheetCtx.onChange(tags);
  if (!tagSheetCtx) return; // closed while saving
  if (saved) tagSheetCtx.tags = saved;
  renderTagSheet();
}

document.getElementById('tag-sheet-overlay').addEventListener('click', closeTagSheet);
document.getElementById('tag-sheet-body').addEventListener('click', (e) => {
  if (!tagSheetCtx) return;
  const tags = (tagSheetCtx.tags || []).slice();
  const remove = e.target.closest('.tag-remove');
  if (remove) {
    applyTagChange(tags.filter(t => t !== remove.dataset.tag));
    return;
  }
  const suggest = e.target.closest('.tag-suggest');
  if (suggest) {
    applyTagChange(tags.concat(suggest.dataset.tag));
    return;
  }
  if (e.target.closest('#tag-add')) {
    const input = document.getElementById('tag-input');
    const value = input.value.trim();
    if (value) applyTagChange(tags.concat(value));
  }
});
document.getElementById('tag-sheet-body').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && e.target.id === 'tag-input') {
    e.preventDefault();
    document.getElementById('tag-add').click();
  }
});

// ── Debug tab ──

const logOutput = document.getElementById('log-output');

// Frontend lines for the Debug tab. On Android a console.log needs adb to
// read, so anything the frontend needs to say for diagnosis goes here.
function debugLog(line) {
  if (!logOutput) return;
  const el = document.createElement('div');
  el.textContent = line;
  el.className = 'text-sky-400';
  logOutput.appendChild(el);
  logOutput.scrollTop = logOutput.scrollHeight;
}

// Rust-side logs (INFO and up) mirrored in by the backend logger.
listen('log-message', (event) => {
  const { level, line } = event.payload;
  const el = document.createElement('div');
  el.textContent = line;
  if (level === 'ERROR') el.className = 'text-red-400';
  else if (level === 'WARN') el.className = 'text-yellow-400';
  else if (level === 'DEBUG') el.className = 'text-gray-500';
  else el.className = 'text-gray-300';
  logOutput.appendChild(el);
  logOutput.scrollTop = logOutput.scrollHeight;
});

document.getElementById('clear-logs').addEventListener('click', () => {
  logOutput.innerHTML = '';
});

document.getElementById('copy-logs').addEventListener('click', () => {
  const text = logOutput.innerText;
  const btn = document.getElementById('copy-logs');
  invoke('copy_to_clipboard', { text })
    .then(() => {
      btn.textContent = 'Copied!';
      setTimeout(() => { btn.textContent = 'Copy'; }, 1500);
    })
    .catch((err) => {
      showToast('Copy failed: ' + err);
    });
});

// ── Native back button (Android) ──
//
// MainActivity's OnBackPressedCallback evaluates window.verbaHandleBack() on
// every back press (gesture or button) and only backgrounds the app when it
// returns false — see MainActivity.kt. main.js is a module, so a top-level
// `function verbaHandleBack` would be module-scoped, not a global the native
// side can reach; it must be assigned onto `window` explicitly.
//
// Checked in order: any open overlay (closed with its own existing close
// function), then which detail panel is on screen. The detail-panel checks
// read the DOM directly rather than `activeTab` — openReading/
// openFeedEntries call showPanel() directly and never update `activeTab`
// (only navigateTo does), so it does not track these views.
const BACK_OVERLAYS = [
  { el: document.getElementById('confirm-dialog'), close: () => document.getElementById('confirm-cancel').click() },
  { el: document.getElementById('prompt-dialog'), close: () => document.getElementById('prompt-cancel').click() },
  { el: readerSheet, close: closeReaderSheet },
  { el: libActionSheet, close: closeLibActionSheet },
  { el: moreSheet, close: closeMoreSheet },
  { el: addChooserModal, close: () => closeModal(addChooserModal) },
  { el: libAddModal, close: closeAddModal },
  { el: addUrlModal, close: closeUrlModal },
  { el: feedAddModal, close: closeFeedAddModal },
  { el: feedEditModal, close: closeFeedEditModal },
  { el: tagSheet, close: closeTagSheet },
];

window.verbaHandleBack = function () {
  for (const overlay of BACK_OVERLAYS) {
    if (overlay.el && !overlay.el.classList.contains('hidden')) {
      overlay.close();
      return true;
    }
  }
  if (!document.getElementById('reading').classList.contains('hidden')) {
    document.getElementById('reading-back').click();
    return true;
  }
  if (!document.getElementById('feed-entries').classList.contains('hidden')) {
    navigateTo('feeds');
    return true;
  }
  return false;
};

// ── Init ──

document.addEventListener('DOMContentLoaded', async () => {
  navigateTo('library');

  // A URL/text shared to the app before the webview was ready waits in Rust;
  // pull it now that everything (incl. the library) is loaded.
  try {
    const shared = await invoke('take_shared_text');
    if (shared) importSharedText(shared);
  } catch (_) {}

  // Check RSS feeds for new articles. Fire-and-forget: never blocks startup,
  // silent when offline.
  pollFeeds({}).catch(() => {});
});

// A share arriving while the app is already open: Rust emits this once it has
// stashed the text; go back through take_shared_text so cold and warm shares
// use one consumption path (delivered exactly once).
listen('shared-text', () => {
  invoke('take_shared_text').then(t => { if (t) importSharedText(t); }).catch(() => {});
});
