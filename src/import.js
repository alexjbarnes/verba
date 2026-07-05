// Text/file import parsing: Markdown stripping, paragraph/sentence splitting
// into chapter-sized parts, and EPUB/PDF text extraction.
//
// Pure ES module: no window.__TAURI__, invoke, or app-state references. That
// is what lets it load and run headlessly (Node, a plain static server) for
// testing — main.js is the only caller and owns all Tauri/app-state glue.

import { unzipSync } from './fflate.js';

// ── Word counting ──

export function countWords(text) {
  if (!text) return 0;
  return text.trim().split(/\s+/).filter(Boolean).length;
}

// ── Markdown -> plain text ──

// Strip common Markdown markup down to the text a reader would hear, for
// files whose extension says "markdown" but whose content is otherwise plain
// prose. Deliberately simple (not a CommonMark parser): a fixed sequence of
// substitutions, ordered so each one can't be undone by a later one (images
// before links, so a link regex can't turn "![alt](img)" into "!alt"; rules
// before list markers, so "- - -" isn't half-eaten as a bullet first).
export function markdownToText(md) {
  if (!md) return '';
  let text = md;

  // Fenced code blocks: drop the whole block, fences and language tag too.
  text = text.replace(/```[\s\S]*?```/g, '');
  // Inline code: keep the content, drop the backticks.
  text = text.replace(/`([^`]*)`/g, '$1');
  // Images: alt text isn't useful read aloud — drop the whole markup.
  text = text.replace(/!\[[^\]]*\]\([^)]*\)/g, '');
  // Links: keep the visible text, drop the URL.
  text = text.replace(/\[([^\]]*)\]\([^)]*\)/g, '$1');
  // Horizontal rules: a line of only -, *, or _ (3+, optionally spaced).
  text = text.replace(/^[ \t]*([-*_])[ \t]*(\1[ \t]*){2,}$/gm, '');
  // Table separator rows ("|---|:--:|"); real rows still have letters/digits
  // and survive this, then get their pipes turned into spaces below.
  text = text.replace(/^[ \t]*\|?[ \t:|-]*-[ \t:|-]*\|?[ \t]*$/gm, '');
  // Headings: drop the leading #'s, keep the text.
  text = text.replace(/^[ \t]*#{1,6}[ \t]+/gm, '');
  // Blockquotes: drop the leading '>' markers.
  text = text.replace(/^[ \t]*>+[ \t]?/gm, '');
  // List markers (-, *, + or "1." / "1)") at the start of a line.
  text = text.replace(/^[ \t]*[-*+][ \t]+/gm, '');
  text = text.replace(/^[ \t]*\d+[.)][ \t]+/gm, '');
  // Remaining table pipes -> spaces, so cells read as separate words.
  text = text.replace(/\|/g, ' ');
  // Emphasis/strikethrough: keep the wrapped text, drop the markers. Longest
  // markers first so "**bold**" isn't left with stray single asterisks by the
  // italic pass.
  text = text.replace(/(\*\*\*|___)(.+?)\1/g, '$2');
  text = text.replace(/(\*\*|__)(.+?)\1/g, '$2');
  text = text.replace(/(\*|_)(.+?)\1/g, '$2');
  text = text.replace(/~~(.+?)~~/g, '$1');

  text = text.replace(/[ \t]+$/gm, ''); // trailing space left by a stripped marker
  text = text.replace(/\n{3,}/g, '\n\n'); // collapse blank-line runs left by removed lines
  return text.trim();
}

// ── Splitting long text into chapter-sized parts ──

// Scan for sentence-ending punctuation followed by whitespace/end, so the
// whole input is always covered (unlike a single greedy regex + match(),
// which silently drops a trailing sentence that has no terminal punctuation).
function splitSentencesLoose(text) {
  const out = [];
  let last = 0;
  const re = /[.!?]+(?=\s|$)/g;
  let m;
  while ((m = re.exec(text)) !== null) {
    const end = m.index + m[0].length;
    out.push(text.slice(last, end));
    last = end;
  }
  if (last < text.length) out.push(text.slice(last));
  return out.length ? out : [text];
}

// splitIntoParts's fallback for a single paragraph that alone exceeds `max`
// words (a wall-of-text chapter with no blank-line breaks): split on sentence
// boundaries instead, using the same balanced-target approach.
function splitParagraphBySentence(para, target, max) {
  const sentences = splitSentencesLoose(para);
  const total = countWords(para);
  const nParts = Math.max(1, Math.ceil(total / target));
  const perPart = total / nParts;
  const parts = [];
  let cur = '';
  let curWords = 0;
  for (const sent of sentences) {
    const sentWords = countWords(sent);
    if (curWords > 0 && curWords + sentWords > perPart && parts.length < nParts - 1) {
      parts.push(cur.trim());
      cur = '';
      curWords = 0;
    }
    cur += sent;
    curWords += sentWords;
  }
  if (cur.trim()) parts.push(cur.trim());
  return parts.length ? parts : [para];
}

// Split `text` into roughly `target`-word parts, never leaving the whole
// thing intact once it exceeds `max`. Used both for a long imported file
// (parts titled "Part i") and for one oversized EPUB chapter (titled
// "Title (i/n)"). Greedy paragraph accumulation keeps whole paragraphs
// together; a single paragraph bigger than `max` falls back to sentence
// boundaries so it still splits.
export function splitIntoParts(text, target = 2500, max = 3500) {
  const total = countWords(text);
  if (total <= max) return [text];

  const nParts = Math.ceil(total / target);
  const perPart = total / nParts;
  const paragraphs = text.split(/\n{2,}/);

  const parts = [];
  let cur = [];
  let curWords = 0;
  const flush = () => {
    if (cur.length) parts.push(cur.join('\n\n'));
    cur = [];
    curWords = 0;
  };
  for (const para of paragraphs) {
    const paraWords = countWords(para);
    if (paraWords > max) {
      flush();
      parts.push(...splitParagraphBySentence(para, target, max));
      continue;
    }
    if (curWords > 0 && curWords + paraWords > perPart && parts.length < nParts - 1) {
      flush();
    }
    cur.push(para);
    curWords += paraWords;
  }
  flush();
  return parts;
}

// ── EPUB ──

// Resolve a zip-entry-relative href (POSIX-style, possibly with '.'/'..'
// segments and a URI-encoded or fragment suffix) against a base directory,
// returning the flat key `unzipSync`'s output is indexed by.
function resolveZipPath(baseDir, href) {
  let clean = href.split('#')[0];
  try { clean = decodeURIComponent(clean); } catch (_) { /* leave as-is */ }
  const parts = (baseDir ? baseDir.split('/') : []).concat(clean.split('/'));
  const stack = [];
  for (const part of parts) {
    if (part === '' || part === '.') continue;
    if (part === '..') stack.pop();
    else stack.push(part);
  }
  return stack.join('/');
}

function dirOf(path) {
  return path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
}

// Parse one spine item as strict XHTML, retrying as regular HTML on a parse
// error — some EPUBs ship XHTML that isn't actually well-formed XML.
function parseXhtml(str) {
  let doc = new DOMParser().parseFromString(str, 'application/xhtml+xml');
  if (doc.querySelector('parsererror')) {
    doc = new DOMParser().parseFromString(str, 'text/html');
  }
  return doc;
}

const BLOCK_SELECTOR = 'p, h1, h2, h3, h4, h5, h6, li, blockquote, td, figcaption';
// Calibre-style conversions wrap every paragraph in <div><span>..</span></div>
// with no semantic block elements at all; a second pass with leaf divs as the
// paragraph unit catches those.
const DIV_BLOCK_SELECTOR = BLOCK_SELECTOR + ', div';

function collectBlocks(doc, selector) {
  const parts = [];
  doc.querySelectorAll(selector).forEach(el => {
    // Skip a block that itself wraps another block from this list (e.g.
    // "<li><p>...</p></li>") so its text isn't captured twice: once via the
    // outer element, again via the inner one.
    if (el.querySelector(selector)) return;
    const t = (el.textContent || '').replace(/\s+/g, ' ').trim();
    if (t) parts.push(t);
  });
  return parts.join('\n\n');
}

function extractChapterText(doc) {
  doc.querySelectorAll('script, style').forEach(n => n.remove());
  let text = collectBlocks(doc, BLOCK_SELECTOR);
  if (text.length < 50) {
    const divText = collectBlocks(doc, DIV_BLOCK_SELECTOR);
    if (divText.length > text.length) text = divText;
  }
  // Last resort: flat body text. Loses paragraph breaks but keeps a book
  // whose markup defeats both block walks readable.
  if (text.length < 50) {
    const root = doc.body || doc.documentElement;
    const flat = root ? (root.textContent || '').replace(/\s+/g, ' ').trim() : '';
    if (flat.length > text.length) text = flat;
  }
  return text;
}

function firstHeading(doc) {
  const h = doc.querySelector('h1, h2');
  return h ? (h.textContent || '').trim() : '';
}

// Parse an EPUB (fflate `unzipSync` + DOMParser) into a book title and its
// chapters, one per spine item. Chapter titles prefer the EPUB3 nav doc, then
// fall back to toc.ncx, then the chapter's own first heading, then "Chapter
// N" — TOC weirdness should never fail the import, only degrade the title.
export async function parseEpub(arrayBuffer) {
  const files = unzipSync(new Uint8Array(arrayBuffer));

  const utf8 = new TextDecoder('utf-8');
  const readText = (path) => {
    const bytes = files[path];
    return bytes ? utf8.decode(bytes) : null;
  };

  // encryption.xml also covers mere font obfuscation (Adobe/IDPF mangling),
  // which leaves the text fully readable — only refuse when a non-font
  // resource is encrypted (actual DRM).
  const encXml = readText('META-INF/encryption.xml');
  if (encXml) {
    const encDoc = new DOMParser().parseFromString(encXml, 'application/xml');
    const refs = Array.from(encDoc.getElementsByTagNameNS('*', 'CipherReference'));
    const fontRe = /\.(ttf|otf|woff2?)$/i;
    const allFonts = refs.length > 0 &&
      refs.every(r => fontRe.test((r.getAttribute('URI') || '').split(/[?#]/)[0]));
    if (!allFonts) {
      throw new Error('This EPUB is protected (DRM) and cannot be imported');
    }
  }

  const containerXml = readText('META-INF/container.xml');
  if (!containerXml) throw new Error('Not a valid EPUB (missing META-INF/container.xml)');
  const containerDoc = new DOMParser().parseFromString(containerXml, 'application/xml');
  const opfPath = containerDoc.querySelector('rootfile') &&
    containerDoc.querySelector('rootfile').getAttribute('full-path');
  if (!opfPath) throw new Error('Not a valid EPUB (no rootfile in container.xml)');
  const opfXml = readText(opfPath);
  if (!opfXml) throw new Error('Not a valid EPUB (missing content file: ' + opfPath + ')');
  const opfDoc = new DOMParser().parseFromString(opfXml, 'application/xml');
  const opfDir = dirOf(opfPath);

  // dc:title by local name, not a hardcoded "dc:" prefix — some producers use
  // a different namespace prefix for the same Dublin Core element. A title
  // with no letters at all (Calibre placeholder ids like "540467868") is
  // worse than the filename the caller falls back to, so report none.
  const titleEl = opfDoc.getElementsByTagNameNS('*', 'title')[0];
  const rawTitle = titleEl ? titleEl.textContent.trim() : '';
  const bookTitle = /[a-zA-Z]/.test(rawTitle) ? rawTitle : '';

  const manifest = {}; // id -> { href, mediaType, properties }
  for (const item of opfDoc.getElementsByTagName('item')) {
    const id = item.getAttribute('id');
    const href = item.getAttribute('href');
    if (id && href) {
      manifest[id] = {
        href,
        mediaType: item.getAttribute('media-type') || '',
        properties: item.getAttribute('properties') || '',
      };
    }
  }

  const spineEl = opfDoc.getElementsByTagName('spine')[0];
  const itemrefs = spineEl ? Array.from(spineEl.getElementsByTagName('itemref')) : [];
  const spineHrefs = itemrefs
    // linear="no" marks auxiliary content (endnote docs, covers) outside the
    // reading flow.
    .filter(ir => ir.getAttribute('linear') !== 'no')
    .map(ir => manifest[ir.getAttribute('idref')])
    .filter(Boolean)
    .map(m => resolveZipPath(opfDir, m.href));

  // Chapter titles: EPUB3 nav doc first (resolved href, fragment dropped, ->
  // link text).
  const navTitles = new Map();
  const navItem = Object.values(manifest).find(m => m.properties.split(/\s+/).includes('nav'));
  if (navItem) {
    const navPath = resolveZipPath(opfDir, navItem.href);
    const navHtml = readText(navPath);
    if (navHtml) {
      const navDoc = new DOMParser().parseFromString(navHtml, 'application/xhtml+xml');
      const navDir = dirOf(navPath);
      const navs = Array.from(navDoc.getElementsByTagName('nav'));
      const tocNav = navs.find(n => {
        const type = n.getAttribute('epub:type') ||
          n.getAttributeNS('http://www.idpf.org/2007/ops', 'type') || '';
        return type.split(/\s+/).includes('toc');
      }) || navs[0];
      if (tocNav) {
        for (const a of tocNav.getElementsByTagName('a')) {
          const href = a.getAttribute('href');
          const title = (a.textContent || '').trim();
          if (!href || !title) continue;
          const resolved = resolveZipPath(navDir, href);
          if (!navTitles.has(resolved)) navTitles.set(resolved, title);
        }
      }
    }
  }
  // Fall back to toc.ncx (EPUB2) only when the nav doc gave nothing usable.
  if (navTitles.size === 0) {
    const ncxItem = Object.values(manifest).find(m => m.mediaType === 'application/x-dtbncx+xml') ||
      Object.values(manifest).find(m => m.href.toLowerCase().endsWith('.ncx'));
    if (ncxItem) {
      const ncxPath = resolveZipPath(opfDir, ncxItem.href);
      const ncxDir = dirOf(ncxPath);
      const ncxXml = readText(ncxPath);
      if (ncxXml) {
        const ncxDoc = new DOMParser().parseFromString(ncxXml, 'application/xml');
        for (const navPoint of ncxDoc.getElementsByTagName('navPoint')) {
          const content = navPoint.getElementsByTagName('content')[0];
          const label = navPoint.getElementsByTagName('text')[0];
          const src = content && content.getAttribute('src');
          const title = label && label.textContent.trim();
          if (src && title) {
            const resolved = resolveZipPath(ncxDir, src);
            if (!navTitles.has(resolved)) navTitles.set(resolved, title);
          }
        }
      }
    }
  }

  const rawChapters = [];
  for (const path of spineHrefs) {
    const bytes = files[path];
    if (!bytes) continue; // referenced by the spine but missing from the zip
    const doc = parseXhtml(utf8.decode(bytes));
    const body = extractChapterText(doc);
    if (body.length < 50) continue; // covers/blank/nav-only pages
    const title = navTitles.get(path) || firstHeading(doc) || '';
    rawChapters.push({ title, body });
  }
  if (rawChapters.length === 0) {
    throw new Error('No readable chapters found in this EPUB');
  }

  // Calibre conversions split on print page breaks, producing hundreds of
  // tiny untitled files. Merge an UNTITLED chapter into its predecessor while
  // the combined size stays chapter-sane; a titled chapter always starts
  // fresh, so real TOC structure is never buried. Properly-authored books
  // (every spine item titled) pass through untouched.
  const mergedChapters = [];
  let prevWords = 0;
  for (const ch of rawChapters) {
    const chWords = countWords(ch.body);
    const prev = mergedChapters[mergedChapters.length - 1];
    if (prev && !ch.title && prevWords + chWords <= 3500) {
      prev.body += '\n\n' + ch.body;
      prevWords += chWords;
    } else {
      mergedChapters.push({ title: ch.title, body: ch.body });
      prevWords = chWords;
    }
  }

  const chapters = [];
  mergedChapters.forEach((ch, i) => {
    const title = ch.title || `Chapter ${i + 1}`;
    if (countWords(ch.body) > 3500) {
      const parts = splitIntoParts(ch.body, 2500, 3500);
      parts.forEach((body, pi) => {
        chapters.push({ title: parts.length > 1 ? `${title} (${pi + 1}/${parts.length})` : title, body });
      });
    } else {
      chapters.push({ title, body: ch.body });
    }
  });

  return { title: bookTitle, chapters };
}

// ── PDF ──

// Rebuild one page's lines from pdf.js's flat text-item list: group items
// into lines by their Y position (transform[5]) and the item's own hasEOL
// flag, inserting a space where positioning (not a space glyph) created a
// gap between items. Returns trimmed, non-empty lines in stream order.
function pageLines(items) {
  const lines = [];
  let curLine = [];
  let curY = null;
  let prevRight = null;
  const flushLine = () => {
    if (curLine.length) lines.push({ y: curY, text: curLine.join('') });
    curLine = [];
    prevRight = null;
  };
  for (const item of items) {
    const y = item.transform[5];
    if (curY !== null && Math.abs(y - curY) > 1) flushLine();
    const x = item.transform[4];
    const fontSize = Math.abs(item.transform[0]) || 1;
    // A gap wider than ~1/4 the font size from the previous item's right edge
    // means the PDF used positioning instead of a space glyph between words
    // (common with justified/kerned text) — insert one so words don't glue.
    if (prevRight !== null && x - prevRight > fontSize * 0.25 && curLine.length) {
      curLine.push(' ');
    }
    curLine.push(item.str);
    curY = y;
    prevRight = x + (item.width || 0);
    if (item.hasEOL) flushLine();
  }
  flushLine();
  return lines
    .map(l => ({ y: l.y, text: l.text.trim() }))
    .filter(l => l.text);
}

// Join one page's lines into text: break a paragraph where the line-to-line Y
// gap is much bigger than the page's typical line height; join lines WITHIN a
// paragraph with a space — a raw '\n' would read back as an 800ms paragraph
// pause in the TTS splitter (split_for_pauses), i.e. a stutter after every
// visual line — de-hyphenating a line-wrap break as it joins.
function joinPageLines(lines) {
  const gaps = [];
  for (let i = 1; i < lines.length; i++) gaps.push(Math.abs(lines[i - 1].y - lines[i].y));
  const sortedGaps = gaps.slice().sort((a, b) => a - b);
  const medianGap = sortedGaps.length ? sortedGaps[Math.floor(sortedGaps.length / 2)] : 0;

  let out = '';
  let prevY = null;
  for (const line of lines) {
    if (prevY !== null && medianGap > 0 && Math.abs(prevY - line.y) > medianGap * 1.6) {
      out += '\n\n';
    } else if (out) {
      // "exam-" + "ple" -> "example": a letter-hyphen at a line wrap is the
      // typesetter's break, not a real compound.
      if (/[a-z]-$/i.test(out) && /^[a-z]/i.test(line.text)) out = out.slice(0, -1);
      else out += ' ';
    }
    out += line.text;
    prevY = line.y;
  }
  return out;
}

// Strip page furniture — running headers/footers and bare page numbers —
// which would otherwise be read aloud between every page. Conservative: only
// a page's first or last line is ever considered, and a repeating line must
// recur (digit-insensitively, so a page number inside it doesn't hide the
// repeat) on enough pages first. 30% not 60%: alternating verso/recto
// headers each appear on only ~half the pages.
function stripPageFurniture(pages) {
  const norm = t => t.replace(/\d+/g, '#').replace(/\s+/g, ' ').trim();
  const nonEmpty = pages.filter(p => p.length);
  const threshold = Math.max(3, Math.ceil(nonEmpty.length * 0.3));
  const firstCounts = new Map();
  const lastCounts = new Map();
  for (const lines of nonEmpty) {
    const f = norm(lines[0].text);
    firstCounts.set(f, (firstCounts.get(f) || 0) + 1);
    if (lines.length > 1) {
      const l = norm(lines[lines.length - 1].text);
      lastCounts.set(l, (lastCounts.get(l) || 0) + 1);
    }
  }
  const isFurniture = (line, counts) =>
    /^\d{1,4}$/.test(line.text) || (counts.get(norm(line.text)) || 0) >= threshold;
  return pages.map(lines => {
    let ls = lines;
    if (ls.length && isFurniture(ls[0], firstCounts)) ls = ls.slice(1);
    if (ls.length && isFurniture(ls[ls.length - 1], lastCounts)) ls = ls.slice(0, -1);
    return ls;
  });
}

// Extract the text layer of a PDF via pdf.js (vendored; loaded lazily since
// it's ~1.8MB with its worker and most imports are txt/md/epub).
export async function parsePdf(arrayBuffer) {
  let pdfjs;
  try {
    pdfjs = await import('/pdfjs/pdf.min.mjs');
  } catch (err) {
    throw new Error('PDF support is unavailable (pdf.js failed to load): ' + err);
  }
  pdfjs.GlobalWorkerOptions.workerSrc = '/pdfjs/pdf.worker.min.mjs';

  // pdf.js wants a typed array (raw ArrayBuffer input is deprecated).
  const doc = await pdfjs.getDocument({ data: new Uint8Array(arrayBuffer) }).promise;
  let pages = [];
  try {
    for (let p = 1; p <= doc.numPages; p++) {
      const page = await doc.getPage(p);
      const content = await page.getTextContent();
      pages.push(pageLines(content.items));
    }
  } finally {
    doc.destroy().catch(() => {}); // releases the worker's copy of the document
  }
  // Furniture only exists in multi-page docs; a 1-2 pager keeps every line
  // (its first line could legitimately be a bare year or number).
  if (pages.length >= 3) pages = stripPageFurniture(pages);
  const text = pages.map(joinPageLines).filter(Boolean).join('\n\n').trim();
  if (text.length < 200) {
    throw new Error('No text layer found (scanned PDF?)');
  }
  return { text };
}
