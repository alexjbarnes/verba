// Text/file import parsing: Markdown stripping and PDF text extraction.
//
// Pure ES module: no window.__TAURI__, invoke, or app-state references. That
// is what lets it load and run headlessly (Node, a plain static server) for
// testing — main.js is the only caller and owns all Tauri/app-state glue.

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
// it's ~1.8MB with its worker and most imports are txt/md).
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
