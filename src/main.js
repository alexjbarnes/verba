import { markdownToText, countWords, splitIntoParts, parseEpub, parsePdf } from './import.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let engineReady = false;

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
function showPrompt(message, { value = '', okLabel = 'Save', placeholder = '' } = {}) {
  return new Promise((resolve) => {
    const dialog = document.getElementById('prompt-dialog');
    document.getElementById('prompt-msg').textContent = message;
    const input = document.getElementById('prompt-input');
    input.value = value;
    input.placeholder = placeholder;
    document.getElementById('prompt-ok').textContent = okLabel;
    dialog.classList.remove('hidden');
    dialog.classList.add('flex');
    input.focus();
    input.select();

    const cleanup = (result) => {
      dialog.classList.add('hidden');
      dialog.classList.remove('flex');
      document.getElementById('prompt-ok').onclick = null;
      document.getElementById('prompt-cancel').onclick = null;
      input.onkeydown = null;
      resolve(result);
    };
    document.getElementById('prompt-ok').onclick = () => cleanup(input.value.trim());
    document.getElementById('prompt-cancel').onclick = () => cleanup(null);
    input.onkeydown = (e) => {
      if (e.key === 'Enter') { e.preventDefault(); cleanup(input.value.trim()); }
      else if (e.key === 'Escape') { e.preventDefault(); cleanup(null); }
    };
  });
}

// Gate a voice's (possibly large) model download behind a prompt. Returns true
// if the model is already on disk or the user agreed to fetch it.
async function confirmVoiceDownload(model) {
  const m = modelById(model);
  if (isDownloaded(m)) return true;
  const name = (m && m.name) || 'this voice';
  const size = m && m.size ? ` (${m.size})` : '';
  return showConfirm(`Download ${name}${size}? It downloads once, then works offline.`,
    { okLabel: 'Download', danger: false });
}

// ── Sidebar & navigation ──

const isDesktop = !navigator.userAgent.includes('Android');
const modeDefaultTab = { speak: 'history', listen: 'library', meeting: 'meetings' };
let currentMode = 'speak';
let activeTab = null;

// Detail views hide the bottom nav (and drop the player bar to the screen edge).
const DETAIL_PANELS = new Set(['reading', 'feed-entries', 'book-chapters', 'meeting-live', 'meeting-view']);

// Bottom-nav layout per mode. `center` is the prominent circular action button
// (Listen's Add); `overflow` opens the More sheet.
const BOTTOM_NAV = {
  listen: [
    { tab: 'library', label: 'Library', icon: 'library_books' },
    { tab: 'feeds', label: 'Feeds', icon: 'rss_feed' },
    { action: 'add', label: 'Add', icon: 'add', center: true },
    { tab: 'voices', label: 'Voices', icon: 'graphic_eq' },
    { overflow: true, label: 'More', icon: 'more_horiz' },
  ],
  speak: [
    { tab: 'history', label: 'History', icon: 'history' },
    { tab: 'snippets', label: 'Snippets', icon: 'sticky_note_2' },
    { tab: 'general', label: 'Settings', icon: 'settings' },
    { overflow: true, label: 'More', icon: 'more_horiz' },
  ],
  // Desktop-only mode (see isDesktop gating at boot); the record button is
  // the center action, wired to startMeeting() in the bottomNav click handler.
  meeting: [
    { tab: 'meetings', label: 'Meetings', icon: 'event_note' },
    { action: 'meeting-start', label: 'Record', icon: 'fiber_manual_record', center: true },
    { overflow: true, label: 'More', icon: 'more_horiz' },
  ],
};
// Pages reachable only through the More sheet, per mode.
const MORE_ITEMS = {
  listen: [
    { tab: 'general', label: 'Settings', icon: 'settings' },
    { tab: 'reports', label: 'Reports', icon: 'flag' },
    { tab: 'debug', label: 'Debug', icon: 'bug_report' },
  ],
  speak: [
    { tab: 'debug', label: 'Debug', icon: 'bug_report' },
  ],
  meeting: [
    { tab: 'general', label: 'Settings', icon: 'settings' },
    { tab: 'debug', label: 'Debug', icon: 'bug_report' },
  ],
};

const bottomNav = document.getElementById('bottom-nav');
const moreSheet = document.getElementById('nav-more-sheet');

// Per-tab data refreshers, run when a tab is shown.
const TAB_LOADERS = {
  // Re-check the dictation-package banner condition every time History is
  // shown, not just at boot (state changes when the user installs it).
  history: () => { loadHistory(); loadPackagesStatus(); },
  general: () => { loadVocab(); loadPackagesStatus(); loadStorage(); if (isDesktop) { loadMeetingModels(); loadMeetingGallery(); } applySettingsDefaultExpand(); },
  snippets: () => loadSnippets(),
  library: () => loadLibrary(),
  feeds: () => loadFeeds(),
  voices: () => loadVoices(),
  reports: () => loadReports(),
  meetings: () => { loadMeetings(); loadPackagesStatus(); },
};

// Show a top-level tab: swap the panel, refresh its data, keep/hide the player
// bar as a mini-player, and light up the matching bottom-nav slot.
function navigateTo(tab) {
  activeTab = tab;
  showPanel(tab);
  setBottomNavVisible(!DETAIL_PANELS.has(tab));
  // Leaving any open reading: keep the player bar only while audio is actively
  // playing (mini-player); otherwise hide it.
  if (!(ttsStarted && !ttsState.finished && !ttsState.paused)) hidePlayerBar();
  renderBottomNav();
  const load = TAB_LOADERS[tab];
  if (load) load();
}

function renderBottomNav() {
  const items = BOTTOM_NAV[currentMode] || [];
  bottomNav.innerHTML = items.map(it => {
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

// Toggle the nav, then re-anchor the player bar relative to it.
function setBottomNavVisible(show) {
  bottomNav.classList.toggle('hidden', !show);
  positionPlayerBar();
}

bottomNav.addEventListener('click', (e) => {
  const slot = e.target.closest('.nav-slot');
  if (!slot) return;
  if (slot.dataset.action === 'add') { openAddModal(); return; }
  if (slot.dataset.action === 'meeting-start') { startMeeting(); return; }
  if (slot.dataset.overflow) { openMoreSheet(); return; }
  if (slot.dataset.tab) navigateTo(slot.dataset.tab);
});

// Empty-library call to action mirrors the nav's Add button.
document.getElementById('lib-empty-add').addEventListener('click', () => openAddModal());

// ── More sheet ──

function openMoreSheet() {
  document.getElementById('nav-more-list').innerHTML = (MORE_ITEMS[currentMode] || []).map(it =>
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

// ── Speak / Listen mode ──

const modeThumb = document.getElementById('mode-thumb');
// Fixed regardless of how many mode buttons are visible: translateX(N * 100%)
// moves the thumb N of its own widths, which lines up under button N whether
// the pill holds 2 slots (Android) or 3 (desktop, see the isDesktop boot check).
const MODE_ORDER = ['speak', 'listen', 'meeting'];

function setMode(mode) {
  // A recording meeting owns the mic; leaving it mid-session would strand the
  // session with no visible Stop/Cancel. Entering meeting mode is always fine.
  if (meetingRecording && currentMode === 'meeting' && mode !== 'meeting') {
    showToast('Stop or cancel the meeting before switching modes');
    return;
  }
  currentMode = mode;
  if (modeThumb) modeThumb.style.transform = `translateX(${MODE_ORDER.indexOf(mode) * 100}%)`;
  document.querySelectorAll('.mode-btn').forEach(b => {
    const active = b.dataset.mode === mode;
    b.classList.toggle('text-primary', active);
    b.classList.toggle('text-on-surface-variant', !active);
    const icon = b.querySelector('.material-symbols-outlined');
    if (icon) icon.style.fontVariationSettings = `'FILL' ${active ? 1 : 0}`;
  });
  renderBottomNav();
  // Re-entering meeting mode while its session is still recording (rehydrate
  // at boot, or switching back) goes straight to the live view, not the list.
  if (mode === 'meeting' && meetingRecording) {
    showPanel('meeting-live');
    setBottomNavVisible(false);
  } else {
    navigateTo(modeDefaultTab[mode]);
  }
}

document.querySelectorAll('.mode-btn').forEach(b => {
  b.addEventListener('click', () => setMode(b.dataset.mode));
});

// ── History tab ──

const historyList = document.getElementById('history-list');
const historyPlaceholder = document.getElementById('history-placeholder');

function formatDuration(ms) {
  if (ms < 1000) return ms + 'ms';
  return (ms / 1000).toFixed(1) + 's';
}

function formatTimestamp(iso) {
  try {
    const d = new Date(iso);
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' }) +
      ' ' + d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  } catch (_) {
    return iso;
  }
}

function formatSpeed(entry) {
  if (!entry.audio_duration_ms || !entry.duration_ms) return '';
  const rtf = entry.duration_ms / entry.audio_duration_ms;
  return rtf.toFixed(2) + 'x RTF';
}

function formatAudioDuration(ms) {
  if (!ms) return '';
  const secs = ms / 1000;
  if (secs < 60) return secs.toFixed(1) + 's spoken';
  const mins = Math.floor(secs / 60);
  const rem = (secs % 60).toFixed(0);
  return mins + 'm ' + rem + 's spoken';
}

function renderChunkTimings(chunks) {
  if (!chunks || chunks.length === 0) return '';
  let html = '<div class="mt-2">';
  html += '<span class="text-[10px] font-semibold uppercase tracking-wider text-primary/70">Transcription chunks</span>';
  html += '<div class="mt-1 space-y-0.5">';
  let total = 0;
  for (let i = 0; i < chunks.length; i++) {
    const c = chunks[i];
    total += c.transcribe_ms;
    const audioSec = (c.audio_ms / 1000).toFixed(1);
    html += `<p class="text-xs text-on-surface-variant font-mono">${c.transcribe_ms}ms (${audioSec}s audio)</p>`;
  }
  if (chunks.length > 1) {
    html += `<p class="text-xs text-on-surface-variant font-mono font-semibold">Total: ${total}ms</p>`;
  }
  html += '</div></div>';
  return html;
}

function lcsWordDiff(oldText, newText) {
  const ow = oldText.trim().split(/\s+/).filter(w => w);
  const nw = newText.trim().split(/\s+/).filter(w => w);
  const m = ow.length, n = nw.length;
  const dp = Array.from({length: m + 1}, () => new Int32Array(n + 1));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      dp[i][j] = ow[i-1] === nw[j-1]
        ? dp[i-1][j-1] + 1
        : Math.max(dp[i-1][j], dp[i][j-1]);
    }
  }
  const ops = [];
  let i = m, j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && ow[i-1] === nw[j-1]) {
      ops.unshift({t: 'eq', w: nw[j-1]}); i--; j--;
    } else if (j > 0 && (i === 0 || dp[i][j-1] >= dp[i-1][j])) {
      ops.unshift({t: 'ins', w: nw[j-1]}); j--;
    } else {
      ops.unshift({t: 'del', w: ow[i-1]}); i--;
    }
  }
  return ops;
}

function renderDiffHtml(oldText, newText) {
  return lcsWordDiff(oldText, newText).map(op => {
    if (op.t === 'eq') return escapeHtml(op.w);
    if (op.t === 'ins') return `<span style="color:#fbbf24">${escapeHtml(op.w)}</span>`;
    return `<span style="text-decoration:line-through;opacity:0.35">${escapeHtml(op.w)}</span>`;
  }).join(' ');
}

function renderPipelineStages(stages, chunkTimings) {
  const hasStages = stages && stages.length > 1;
  const hasChunks = chunkTimings && chunkTimings.length > 0;
  if (!hasStages && !hasChunks) return '';

  let html = '<div class="pipeline-stages hidden mt-3 pt-3 border-t border-outline-variant/20 space-y-2">';
  if (hasChunks) {
    html += renderChunkTimings(chunkTimings);
  }
  if (hasStages) {
    for (let idx = 0; idx < stages.length; idx++) {
      const stage = stages[idx];
      const isBaseline = idx === 0;
      const unchanged = !isBaseline && stage.changed === false;
      const dim = unchanged ? ' opacity-40' : '';
      const tag = unchanged ? ' (no change)' : '';
      const timing = stage.duration_ms ? ` ${stage.duration_ms}ms` : '';
      let colaHtml = '';
      if (stage.grammar_score != null) {
        const pct = Math.round(stage.grammar_score * 100);
        const routed = stage.grammar_score < 0.75;
        const color = routed ? '#f87171' : '#4ade80';
        colaHtml = ` <span style="color:${color};font-variant-numeric:tabular-nums">Score ${pct}%${routed ? ' → corrected' : ''}</span>`;
      }
      const textHtml = (!isBaseline && stage.changed)
        ? renderDiffHtml(stages[idx - 1].text, stage.text)
        : escapeHtml(stage.text);
      let sentencesHtml = '';
      if (stage.grammar_sentences && stage.grammar_sentences.length > 1) {
        sentencesHtml = '<div class="mt-1 mb-0.5 space-y-0.5 pl-2 border-l-2 border-outline-variant/30">';
        for (const sent of stage.grammar_sentences) {
          const p = sent.score != null ? Math.round(sent.score * 100) : null;
          const scoreStr = p != null
            ? `<span style="color:${p < 75 ? '#f87171' : '#4ade80'};font-variant-numeric:tabular-nums">${p}%</span> `
            : '';
          const action = sent.corrected ? '<span style="color:#f87171">corrected</span>' : '<span style="color:#4ade80">passed</span>';
          const preview = sent.text.length > 70 ? sent.text.slice(0, 67) + '…' : sent.text;
          sentencesHtml += `<p class="text-[10px] text-on-surface-variant font-mono leading-relaxed">${scoreStr}${action} — ${escapeHtml(preview)}</p>`;
        }
        sentencesHtml += '</div>';
      }
      html += `
        <div class="${dim}">
          <span class="text-[10px] font-semibold uppercase tracking-wider text-primary/70">${escapeHtml(stage.name)}${tag}${timing}${colaHtml}</span>
          ${sentencesHtml}<p class="text-xs text-on-surface-variant leading-relaxed mt-0.5 select-text">${textHtml}</p>
        </div>`;
    }
  }
  html += '</div>';
  return html;
}

function formatEntryForCopy(entry) {
  return JSON.stringify(entry, null, 2);
}

function renderHistory(entries) {
  historyList.innerHTML = '';
  if (!entries || entries.length === 0) {
    historyList.innerHTML = `
      <div class="flex flex-col items-center justify-center pt-16 text-on-surface-variant">
        <span class="material-symbols-outlined text-4xl mb-3 opacity-30">history</span>
        <p class="text-sm">No transcriptions yet</p>
      </div>`;
    return;
  }
  let staggerIdx = 0;
  for (const entry of [...entries].reverse()) {
    const card = document.createElement('div');
    card.className = 'bg-surface-container rounded-xl p-4 stagger-in';
    card.style.setProperty('--i', staggerIdx++);

    const hasStages = entry.pipeline_stages && entry.pipeline_stages.length > 1;
    const hasChunks = entry.chunk_timings && entry.chunk_timings.length > 0;
    const hasDetails = hasStages || hasChunks;

    // The default card is for the USER: their words, when, how long they
    // spoke. Engine telemetry (transcribe ms, postprocess ms, RTF, model id)
    // is developer detail — it lives behind the Details toggle with the
    // pipeline stages, not on every card.
    const meta = [
      formatTimestamp(entry.timestamp),
      formatAudioDuration(entry.audio_duration_ms),
    ].filter(Boolean).join(' · ');
    const telemetry = [
      formatDuration(entry.duration_ms) + ' to transcribe',
      entry.postprocess_ms ? entry.postprocess_ms + 'ms postprocess' : null,
      formatSpeed(entry),
      escapeHtml(entry.model_id),
    ].filter(Boolean);

    const toggleBtn =
      '<button class="pipeline-toggle text-xs font-semibold text-on-surface-variant hover:text-primary transition-colors cursor-pointer min-h-8 px-1">Details</button>';

    card.innerHTML = `
      <div class="flex items-start gap-3">
        <p class="text-[15px] text-on-surface leading-relaxed select-text flex-1 min-w-0">${escapeHtml(entry.text)}</p>
        <button class="copy-entry-btn shrink-0 -mt-1 -mr-1 w-9 h-9 flex items-center justify-center rounded-lg text-on-surface-variant hover:text-primary hover:bg-primary/10 transition-colors cursor-pointer">
          <span class="material-symbols-outlined text-[18px]">content_copy</span>
        </button>
      </div>
      <div class="flex items-center gap-x-3 text-xs text-on-surface-variant mt-2">
        <span class="tabular-nums">${meta}</span>
        ${toggleBtn}
      </div>
      <div class="pipeline-telemetry hidden flex-wrap items-center gap-x-4 gap-y-1 text-xs text-on-surface-variant/80 mt-2">
        ${telemetry.map(s => '<span>' + s + '</span>').join('')}
      </div>
      ${renderPipelineStages(entry.pipeline_stages, entry.chunk_timings)}`;

    card.querySelector('.pipeline-toggle').addEventListener('click', (e) => {
      const telemetryEl = card.querySelector('.pipeline-telemetry');
      telemetryEl.classList.toggle('hidden');
      telemetryEl.classList.toggle('flex');
      const open = !telemetryEl.classList.contains('hidden');
      if (hasDetails) {
        card.querySelector('.pipeline-stages').classList.toggle('hidden', !open);
      }
      e.target.textContent = open ? 'Hide' : 'Details';
    });

    card.querySelector('.copy-entry-btn').addEventListener('click', (e) => {
      const text = formatEntryForCopy(entry);
      const icon = e.currentTarget.querySelector('.material-symbols-outlined');
      invoke('copy_to_clipboard', { text }).then(() => {
        icon.textContent = 'check';
        setTimeout(() => { icon.textContent = 'content_copy'; }, 1500);
      });
    });

    historyList.appendChild(card);
  }
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

async function loadHistory() {
  try {
    const entries = await invoke('list_history');
    renderHistory(entries);
  } catch (err) {
    console.error('Failed to load history:', err);
  }
}

// Auto-refresh history when the app comes back to foreground
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') {
    loadHistory();
  }
});

// Refresh history when a transcription completes (in-app dictation path)
listen('transcription-result', () => {
  loadHistory();
});

document.getElementById('export-history').addEventListener('click', async () => {
  const btn = document.getElementById('export-history');
  try {
    const json = await invoke('export_history');
    await invoke('copy_to_clipboard', { text: json });
    btn.textContent = 'Copied!';
  } catch (err) {
    console.error('Export failed:', err);
    showToast('Export failed: ' + err);
  }
  setTimeout(() => { btn.textContent = 'Export'; }, 2000);
});

document.getElementById('clear-history').addEventListener('click', async () => {
  if (!await showConfirm('Clear all history?')) return;
  try {
    await invoke('clear_history');
    renderHistory([]);
  } catch (err) {
    console.error('Failed to clear history:', err);
  }
});

// ── Reports (mispronunciations) ──

function renderReports(entries) {
  const list = document.getElementById('reports-list');
  list.innerHTML = '';
  if (!entries || entries.length === 0) {
    list.innerHTML = `
      <div class="flex flex-col items-center justify-center pt-16 text-on-surface-variant">
        <span class="material-symbols-outlined text-4xl mb-3 opacity-30">flag</span>
        <p class="text-sm">No reports yet</p>
        <p class="text-xs mt-2 max-w-xs text-center opacity-80">Select a word in an article and choose "Report mispronunciation" from the text menu.</p>
      </div>`;
    return;
  }
  let i = 0;
  for (const entry of [...entries].reverse()) {
    const row = document.createElement('div');
    row.className = 'stagger-in bg-surface-container rounded-xl px-3.5 py-3';
    row.style.setProperty('--i', i++);
    const when = entry.reported_at_ms ? new Date(entry.reported_at_ms).toLocaleString() : '';
    const meta = [entry.voice, when].filter(Boolean).join(' \u{b7} ');
    row.innerHTML = `
      <span class="text-[15px] font-semibold leading-snug text-on-surface truncate select-text block">${escapeHtml(entry.word)}</span>
      ${meta ? `<span class="text-xs text-on-surface-variant tabular-nums truncate block mt-0.5">${escapeHtml(meta)}</span>` : ''}`;
    list.appendChild(row);
  }
}

async function loadReports() {
  try {
    renderReports(await invoke('mispronunciations_list'));
  } catch (err) {
    console.error('Failed to load reports:', err);
  }
}

document.getElementById('export-reports').addEventListener('click', async () => {
  const btn = document.getElementById('export-reports');
  try {
    const json = await invoke('export_mispronunciations');
    await invoke('copy_to_clipboard', { text: json });
    btn.textContent = 'Copied!';
  } catch (err) {
    showToast('Export failed: ' + err);
  }
  setTimeout(() => { btn.textContent = 'Export'; }, 2000);
});

document.getElementById('clear-reports').addEventListener('click', async () => {
  if (!await showConfirm('Clear all reports?')) return;
  try {
    await invoke('clear_mispronunciations');
    renderReports([]);
  } catch (err) {
    console.error('Failed to clear reports:', err);
  }
});

// ── Dictation package status (Settings > Updates, History banner) ──
//
// One package covers the ASR model, VAD, and grammar correction (see
// MODEL_PACKAGES.md) — packages_status() is the source of truth for both the
// Settings row and the History banner nudging an uninstalled user to fetch it.

// Human-readable byte size (binary units).
function fmtBytes(bytes) {
  if (!bytes || bytes <= 0) return '0 MB';
  const units = ['B', 'KB', 'MB', 'GB'];
  let n = bytes, i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(n < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}

let pkgStatus = null; // last packages_status()/packages_check_updates() result
// Session-only: hides the History banner once tapped even if the package is
// still missing on the next tab load. Reset on a fresh app launch.
let dictationBannerDismissed = false;

function pkgDictationStatusLine(d) {
  if (d.state === 'downloading') return `Downloading… ${Math.round((d.progress || 0) * 100)}%`;
  if (d.state === 'installed') return `Installed - v${d.installed_version}`;
  if (d.state === 'update_available') return `Update available - v${d.installed_version} -> v${d.available_version}`;
  return `Not downloaded - ${fmtBytes(d.pending_bytes)}`;
}

function renderPkgDictation() {
  if (!pkgStatus) return;
  const d = pkgStatus.dictation;
  const btn = document.getElementById('pkg-dictation-btn');
  const progress = document.getElementById('pkg-dictation-progress');
  document.getElementById('pkg-dictation-status').textContent = pkgDictationStatusLine(d);
  progress.classList.toggle('hidden', d.state !== 'downloading');
  if (d.state === 'downloading') {
    document.getElementById('pkg-dictation-fill').style.width = `${Math.round((d.progress || 0) * 100)}%`;
    btn.disabled = true;
    btn.textContent = `${Math.round((d.progress || 0) * 100)}%`;
  } else if (d.state === 'installed') {
    btn.disabled = true;
    btn.textContent = 'Installed';
  } else {
    btn.disabled = false;
    btn.textContent = d.state === 'update_available' ? 'Update' : 'Download';
  }
}

function renderDictationBanner() {
  const banner = document.getElementById('dictation-banner');
  if (!banner) return;
  const show = !!pkgStatus && pkgStatus.dictation.state === 'not_installed' && !dictationBannerDismissed;
  banner.classList.toggle('hidden', !show);
  banner.classList.toggle('flex', show);
}

async function loadPackagesStatus() {
  try {
    pkgStatus = await invoke('packages_status');
    renderPkgDictation();
    renderDictationBanner();
    updateModelsBadge();
  } catch (err) {
    console.error('Failed to load package status:', err);
  }
}

// Packages whose manifest offers a new or missing model. Covers a version bump
// AND a component the manifest gained since install (e.g. the meeting speaker-
// diarization model added server-side).
function packagesWithUpdates(status) {
  const out = [];
  if (status && status.dictation && status.dictation.state === 'update_available') out.push('Dictation');
  if (status && status.meeting && status.meeting.state === 'update_available') out.push('Meeting');
  return out;
}

// Boot-time update check: refetch the manifest (bypasses the 24h cache) and
// highlight any installed package with an update waiting. Fire-and-forget:
// never blocks startup, silent when offline.
async function checkPackageUpdatesOnStartup() {
  try {
    const status = await invoke('packages_check_updates');
    if (status && !status.check_error) {
      pkgStatus = status;
      renderPkgDictation();
      renderDictationBanner();
      renderPkgMeeting();
    }
  } catch (_) {}
  // Ensure a status even if the network check failed (offline boot), then decide
  // whether to surface the first-launch models modal.
  if (!pkgStatus) { try { pkgStatus = await invoke('packages_status'); } catch (_) {} }
  updateModelsBadge();
  maybeAutoOpenModels();
}

// ── Models modal: one clear place to see what's downloaded and what's missing,
// with per-package download. Auto-shown on first launch when something needs
// attention, and openable from Settings > Updates > Model downloads.
let modelsModalOpen = false;
let modelsModalDismissed = false; // session-only, so a dismissed modal stops nagging

const MODEL_PACKAGES = {
  dictation: { name: 'Dictation', purpose: 'Voice typing and transcription. Needed for Speak and for meeting transcripts.' },
  meeting: { name: 'Meeting', purpose: 'On-device meeting transcription, speaker labels, and summaries.' },
};

function pkgStateLabel(state) {
  return { installed: 'Installed', downloading: 'Downloading', update_available: 'Update available', not_installed: 'Not downloaded' }[state] || 'Unknown';
}

// Packages that genuinely need attention: dictation missing or updatable (it is
// essential), and meeting updatable (you have it, a piece is missing). Meeting
// not being downloaded is optional, so it does not count as "needs attention".
function modelsNeedingAttention(status) {
  const out = [];
  if (!status) return out;
  const d = status.dictation;
  if (d && (d.state === 'not_installed' || d.state === 'update_available')) out.push('dictation');
  if (isDesktop && status.meeting && status.meeting.state === 'update_available') out.push('meeting');
  return out;
}

function updateModelsBadge() {
  const badge = document.getElementById('models-summary-badge');
  if (!badge) return;
  const n = modelsNeedingAttention(pkgStatus).length;
  badge.textContent = n ? `${n} to download` : 'All installed';
  badge.className = 'text-xs font-semibold ' + (n ? 'text-primary' : 'text-on-surface-variant');
}

function maybeAutoOpenModels() {
  if (modelsModalDismissed || modelsModalOpen) return;
  if (modelsNeedingAttention(pkgStatus).length) openModelsModal();
}

function modelCardHtml(key, d, model) {
  const meta = MODEL_PACKAGES[key];
  const state = d ? d.state : 'not_installed';
  const downloading = state === 'downloading';
  const pct = downloading ? Math.round((d.progress || 0) * 100) : 0;
  let size = '';
  if (key === 'dictation') size = d && d.pending_bytes ? fmtBytes(d.pending_bytes) : '';
  else if (key === 'meeting' && model) size = model.size || '';
  const statusText = downloading
    ? `Downloading… ${pct}%`
    : pkgStateLabel(state) + (size && state !== 'installed' ? ` · ${size}` : '');
  const statusColor = state === 'installed' || state === 'update_available' ? 'text-primary' : 'text-on-surface-variant';
  let action;
  if (state === 'installed') action = '<span class="material-symbols-outlined text-primary">check_circle</span>';
  else if (downloading) action = `<span class="text-xs font-semibold text-on-surface-variant tabular-nums">${pct}%</span>`;
  else action = `<button class="model-dl-btn text-xs font-semibold px-4 py-2 bg-primary text-on-primary rounded-lg hover:brightness-110 transition cursor-pointer" data-pkg="${key}">${state === 'update_available' ? 'Update' : 'Download'}</button>`;
  return `<div class="model-card bg-surface-container rounded-xl p-4" data-pkg="${key}">
    <div class="flex items-start justify-between gap-3">
      <div class="min-w-0">
        <p class="text-sm font-semibold text-on-surface">${meta.name}</p>
        <p class="text-xs text-on-surface-variant mt-0.5">${meta.purpose}</p>
        <p class="model-card-status text-xs font-medium mt-1.5 ${statusColor}">${statusText}</p>
      </div>
      <div class="shrink-0 flex items-center">${action}</div>
    </div>
    <div class="model-card-progress ${downloading ? '' : 'hidden'} mt-3"><div class="progress-bar"><div class="model-card-fill progress-bar-fill" style="width:${pct}%"></div></div></div>
  </div>`;
}

function renderModelsModal() {
  const list = document.getElementById('models-list');
  if (!list || !pkgStatus) return;
  const rec = meetingModels.find(m => m.recommended) || meetingModels.find(m => m.id === meetingSelectedSummarizer) || meetingModels[0];
  const cards = [modelCardHtml('dictation', pkgStatus.dictation)];
  if (isDesktop) cards.push(modelCardHtml('meeting', pkgStatus.meeting, rec));
  list.innerHTML = cards.join('');
  list.querySelectorAll('.model-dl-btn').forEach(b => b.addEventListener('click', () => downloadPackage(b.dataset.pkg)));
  const n = modelsNeedingAttention(pkgStatus).length;
  document.getElementById('models-modal-sub').textContent = n ? `${n} model${n > 1 ? 's' : ''} to download` : 'Everything is downloaded.';
}

async function downloadPackage(pkg) {
  try {
    if (pkg === 'dictation') {
      await invoke('package_install_dictation');
    } else if (pkg === 'meeting') {
      const rec = meetingModels.find(m => m.id === meetingSelectedSummarizer)
        || meetingModels.find(m => m.recommended) || meetingModels[0];
      if (!rec) { showToast('No summarizer available yet'); return; }
      meetingSelectedSummarizer = rec.id;
      await invoke('package_install_meeting', { summarizerId: rec.id });
    }
  } catch (err) {
    showToast('Download failed: ' + err);
  }
  await loadPackagesStatus();
  if (isDesktop) await loadMeetingModels();
  updateModelsBadge();
  if (modelsModalOpen) renderModelsModal();
}

function openModelsModal() {
  if (isDesktop && !meetingModels.length) loadMeetingModels().then(() => { if (modelsModalOpen) renderModelsModal(); });
  renderModelsModal();
  const m = document.getElementById('models-modal');
  m.classList.remove('hidden');
  m.classList.add('flex');
  modelsModalOpen = true;
}

function closeModelsModal() {
  const m = document.getElementById('models-modal');
  m.classList.add('hidden');
  m.classList.remove('flex');
  modelsModalOpen = false;
  modelsModalDismissed = true;
}

document.getElementById('models-modal-close').addEventListener('click', closeModelsModal);
document.getElementById('models-modal-done').addEventListener('click', closeModelsModal);
document.getElementById('models-modal-overlay').addEventListener('click', closeModelsModal);
document.getElementById('open-models-modal').addEventListener('click', () => { modelsModalDismissed = false; openModelsModal(); });

document.getElementById('pkg-dictation-btn').addEventListener('click', async () => {
  const btn = document.getElementById('pkg-dictation-btn');
  btn.disabled = true;
  try {
    await invoke('package_install_dictation');
  } catch (err) {
    showToast('Install failed: ' + err);
  }
  await loadPackagesStatus();
});

document.getElementById('check-updates-btn').addEventListener('click', async () => {
  const btn = document.getElementById('check-updates-btn');
  btn.disabled = true;
  btn.textContent = 'Checking…';
  try {
    pkgStatus = await invoke('packages_check_updates');
    renderPkgDictation();
    renderDictationBanner();
    renderPkgMeeting();
    if (pkgStatus.check_error) {
      showToast('Check failed: ' + pkgStatus.check_error);
    } else {
      const updates = packagesWithUpdates(pkgStatus);
      showToast(updates.length ? `Update available: ${updates.join(' & ')}` : 'Up to date');
    }
    // The manifest refetch also covers the voices list.
    if (!document.getElementById('voices').classList.contains('hidden')) loadVoices();
  } catch (err) {
    showToast('Check failed: ' + err);
  } finally {
    btn.disabled = false;
    btn.textContent = 'Check for updates';
  }
});

document.getElementById('dictation-banner-dismiss').addEventListener('click', () => {
  dictationBannerDismissed = true;
  renderDictationBanner();
});
document.getElementById('dictation-banner-action').addEventListener('click', () => {
  navigateTo('general');
});

// ── Storage (Settings > General) ──

const STORAGE_ROWS = [
  { key: 'dictation', label: 'Dictation models', category: 'dictation',
    warn: 'Clear downloaded dictation models? Voice typing and grammar correction stop working until you download them again.' },
  { key: 'voices', label: 'Voices', category: 'voices',
    warn: 'Clearing voices deletes downloaded voice models; they can be re-downloaded.' },
  { key: 'custom_voices', label: 'Custom voices', category: null },
  { key: 'tts_cache', label: 'Generated audio cache', category: 'tts_cache',
    warn: 'Clear the generated audio cache? Playback regenerates audio as needed, using more data next time you listen.' },
  { key: 'unclaimed', label: 'Legacy files', category: 'unclaimed',
    warn: "Delete files left over from removed models? Nothing on this device uses them." },
];

// ── Settings collapsible groups (findability) ──
//
// Each group (General / Speak / Listen / Meeting) folds behind its header, so
// the headers act as a scannable index instead of one long scroll. Opening
// Settings expands only the group for the mode you came from and collapses the
// rest, so what you came for sits up top with General one tap away. Clicking a
// header toggles that group.
function applySettingsDefaultExpand() {
  const open = new Set([currentMode]);
  document.querySelectorAll('#general .settings-group').forEach(g => {
    g.classList.toggle('collapsed', !open.has(g.dataset.group));
  });
}

document.getElementById('general').addEventListener('click', (e) => {
  const toggle = e.target.closest('.settings-group-toggle');
  if (toggle) toggle.closest('.settings-group')?.classList.toggle('collapsed');
});

async function loadStorage() {
  let summary;
  try {
    summary = await invoke('storage_summary');
  } catch (err) {
    console.error('Failed to load storage summary:', err);
    return;
  }
  const list = document.getElementById('storage-list');
  list.innerHTML = STORAGE_ROWS
    .filter(r => r.key !== 'unclaimed' || summary[r.key] > 0)
    .map(r => `
      <div class="flex items-center justify-between gap-3 px-4 py-3">
        <div class="min-w-0">
          <p class="text-sm font-medium text-on-surface">${r.label}</p>
          <p class="text-xs text-on-surface-variant mt-0.5">${fmtBytes(summary[r.key])}</p>
        </div>
        ${r.category ? `<button class="storage-clear-btn shrink-0 text-xs font-semibold text-on-surface-variant hover:text-error transition-colors px-3 py-1.5 rounded-lg hover:bg-error/10 cursor-pointer" data-category="${r.category}">Clear</button>` : ''}
      </div>`)
    .join('');
  list.querySelectorAll('.storage-clear-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      const row = STORAGE_ROWS.find(r => r.category === btn.dataset.category);
      if (!await showConfirm(row.warn, { okLabel: 'Clear' })) return;
      try {
        const freed = await invoke('storage_clear', { category: row.category });
        showToast(`Freed ${fmtBytes(freed)}`);
        await loadStorage();
        if (row.category === 'dictation') await loadPackagesStatus();
        if (row.category === 'voices' && !document.getElementById('voices').classList.contains('hidden')) loadVoices();
      } catch (err) {
        showToast('Clear failed: ' + err);
      }
    });
  });
}

// ── Download progress events ──

listen('download-progress', (event) => {
  const { id, progress } = event.payload;
  const pct = Math.round(progress * 100);

  // Reader's inline voice-download bar (no model-row container).
  if (id === ttsModelId) {
    const f = document.getElementById('tts-dl-fill');
    const p = document.getElementById('tts-dl-pct');
    if (f) f.style.width = `${pct}%`;
    if (p) p.textContent = `${pct}%`;
  }

  // Voices-page catalogue: inline percent on the row being fetched via Use.
  document.querySelectorAll(`[data-voice-progress="${id}"]`).forEach(el => {
    el.textContent = `${pct}%`;
  });

  // Settings > Updates: the dictation package's own progress bar/button.
  if (id === 'pkg-dictation') {
    document.getElementById('pkg-dictation-progress').classList.remove('hidden');
    document.getElementById('pkg-dictation-fill').style.width = `${pct}%`;
    document.getElementById('pkg-dictation-status').textContent = `Downloading… ${pct}%`;
    const btn = document.getElementById('pkg-dictation-btn');
    btn.disabled = true;
    btn.textContent = `${pct}%`;
  }

  // Settings > Meeting: the summarizer package's download row.
  if (id === 'pkg-meeting') {
    document.getElementById('pkg-meeting-progress').classList.remove('hidden');
    document.getElementById('pkg-meeting-fill').style.width = `${pct}%`;
    document.getElementById('pkg-meeting-status').textContent = `Downloading… ${pct}%`;
    const btn = document.getElementById('pkg-meeting-btn');
    btn.disabled = true;
    btn.textContent = `${pct}%`;
  }

  // Models modal: live progress on the matching package card.
  if (modelsModalOpen && (id === 'pkg-dictation' || id === 'pkg-meeting')) {
    const card = document.querySelector(`.model-card[data-pkg="${id === 'pkg-dictation' ? 'dictation' : 'meeting'}"]`);
    if (card) {
      const s = card.querySelector('.model-card-status'); if (s) s.textContent = `Downloading… ${pct}%`;
      const p = card.querySelector('.model-card-progress'); if (p) p.classList.remove('hidden');
      const f = card.querySelector('.model-card-fill'); if (f) f.style.width = `${pct}%`;
    }
  }
});

listen('download-complete', async (event) => {
  if (event.payload?.id === 'pkg-dictation') {
    await loadPackagesStatus();
    updateModelsBadge();
    if (modelsModalOpen) renderModelsModal();
    showToast('Dictation models installed');
    return;
  }
  if (event.payload?.id === 'pkg-meeting') {
    await loadPackagesStatus();
    if (isDesktop) await loadMeetingModels();
    updateModelsBadge();
    if (modelsModalOpen) renderModelsModal();
    showToast('Summarizer installed');
    return;
  }
  await updateTtsPanel();
  if (!document.getElementById('voices').classList.contains('hidden')) loadVoices();
});

listen('engine-ready', () => {
  engineReady = true;
});

listen('model-loading', (event) => {
  showToast('Loading model...');
});

listen('model-loaded', (event) => {
  if (!event.payload?.native_toast) {
    showToast(`Model ready: ${event.payload?.id || ''}`);
  }
});

listen('model-error', (event) => {
  if (!event.payload?.native_toast) {
    showToast('Model load failed: ' + (event.payload?.error || 'unknown error'));
  }
});

// ── Dictation input device (Settings > Speak) ──

async function loadAudioDevices() {
  const devices = await invoke('list_audio_devices');
  const sel = document.getElementById('audio-device');
  // Keep "System Default" as first option, add real devices
  sel.innerHTML = '<option value="-1">System Default</option>';
  for (const dev of devices) {
    const opt = document.createElement('option');
    opt.value = dev.index;
    opt.textContent = dev.name;
    sel.appendChild(opt);
  }
}

// Meeting-mode device pickers (desktop). Values are device NAMES ("" = system
// default), matching the backend's InputByName / LoopbackByName specs.
async function loadMeetingDevices() {
  const [inputs, outputs] = await Promise.all([
    invoke('list_audio_devices'),
    invoke('list_audio_output_devices'),
  ]);
  const fill = (sel, devices) => {
    sel.innerHTML = '<option value="">System Default</option>';
    for (const dev of devices) {
      const opt = document.createElement('option');
      opt.value = dev.name;
      opt.textContent = dev.name;
      sel.appendChild(opt);
    }
  };
  fill(document.getElementById('cfg-meeting-mic'), inputs);
  fill(document.getElementById('cfg-meeting-speaker'), outputs);
}

// Select a device by name, adding a placeholder option first if it isn't
// currently connected, so a saved choice survives the device being unplugged.
function selectDeviceOption(sel, value) {
  if (value && !Array.from(sel.options).some((o) => o.value === value)) {
    const opt = document.createElement('option');
    opt.value = value;
    opt.textContent = `${value} (not connected)`;
    sel.appendChild(opt);
  }
  sel.value = value || '';
}

// ── Dictation hotkey (desktop) ──
//
// Accelerators are "Modifier+…+Code" with the key part as a raw W3C
// KeyboardEvent `code` ("Alt+KeyD"), which is exactly what global-hotkey's
// parser accepts on the Rust side — so capture emits e.code untranslated.

const isMac = navigator.userAgent.includes('Mac');
const HOTKEY_MOD_LABELS = isMac
  ? { Control: '⌃', Alt: '⌥', Shift: '⇧', Super: '⌘' }
  : { Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Super: 'Win' };
const HOTKEY_SEP = isMac ? ' ' : ' + ';
// Held-down modifiers, which can't themselves be the shortcut's key.
const HOTKEY_MODIFIER_CODES = new Set(['ControlLeft', 'ControlRight', 'AltLeft', 'AltRight',
  'ShiftLeft', 'ShiftRight', 'MetaLeft', 'MetaRight', 'CapsLock']);
const HOTKEY_KEY_LABELS = {
  Space: 'Space', Backquote: '`', Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']',
  Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/',
  ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→',
};

function hotkeyKeyLabel(code) {
  if (code.startsWith('Key')) return code.slice(3);
  if (code.startsWith('Digit')) return code.slice(5);
  if (code.startsWith('Numpad')) return 'Num ' + code.slice(6);
  return HOTKEY_KEY_LABELS[code] || code;
}

// "Alt+KeyD" -> "⌥ D" on macOS, "Alt + D" elsewhere.
function formatHotkey(accel) {
  if (!accel) return 'Not set';
  const parts = accel.split('+');
  const key = parts.pop();
  return [...parts.map(m => HOTKEY_MOD_LABELS[m] || m), hotkeyKeyLabel(key)].join(HOTKEY_SEP);
}

let dictationHotkey = '';
let hotkeyCapturing = false;

function renderHotkeyBtn() {
  const btn = document.getElementById('hotkey-btn');
  if (btn) btn.textContent = formatHotkey(dictationHotkey);
}

function startHotkeyCapture() {
  if (hotkeyCapturing) return;
  hotkeyCapturing = true;
  const btn = document.getElementById('hotkey-btn');
  btn.textContent = 'Press keys…';
  btn.classList.add('ring-1', 'ring-primary', 'text-primary');
  document.getElementById('hotkey-hint').textContent =
    'Hold a modifier and press a key. Esc cancels.';
  // Capture phase so the combination never reaches the page's own handlers.
  window.addEventListener('keydown', onHotkeyKeydown, true);
  window.addEventListener('blur', endHotkeyCapture);
  window.addEventListener('pointerdown', onHotkeyPointerDown, true);
}

function endHotkeyCapture() {
  if (!hotkeyCapturing) return;
  hotkeyCapturing = false;
  window.removeEventListener('keydown', onHotkeyKeydown, true);
  window.removeEventListener('blur', endHotkeyCapture);
  window.removeEventListener('pointerdown', onHotkeyPointerDown, true);
  const btn = document.getElementById('hotkey-btn');
  btn.classList.remove('ring-1', 'ring-primary', 'text-primary');
  document.getElementById('hotkey-hint').textContent = 'Press and hold to dictate';
  renderHotkeyBtn();
}

function onHotkeyPointerDown(e) {
  if (!e.target.closest('#hotkey-btn')) endHotkeyCapture();
}

async function onHotkeyKeydown(e) {
  e.preventDefault();
  e.stopPropagation();
  if (e.code === 'Escape') { endHotkeyCapture(); return; }
  const mods = [];
  if (e.ctrlKey) mods.push('Control');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Super');
  const btn = document.getElementById('hotkey-btn');
  if (HOTKEY_MODIFIER_CODES.has(e.code)) {
    // Still holding modifiers — show what's down so far.
    btn.textContent = [...mods.map(m => HOTKEY_MOD_LABELS[m]), '…'].join(HOTKEY_SEP);
    return;
  }
  if (!mods.length) {
    // A bare key would fire globally while typing anywhere.
    btn.textContent = 'Add a modifier';
    return;
  }
  const accel = [...mods, e.code].join('+');
  const previous = dictationHotkey;
  dictationHotkey = accel;
  endHotkeyCapture();
  try {
    await invoke('set_dictation_hotkey', { accelerator: accel });
    showToast(`Hotkey set to ${formatHotkey(accel)}`);
  } catch (err) {
    dictationHotkey = previous;
    renderHotkeyBtn();
    showToast(String(err));
  }
}

document.getElementById('hotkey-btn').addEventListener('click', startHotkeyCapture);

// ── General tab ──

async function loadConfig() {
  const cfg = await invoke('get_config');
  document.getElementById('cfg-haptic').checked = cfg.haptic_feedback;
  dictationHotkey = cfg.dictation_hotkey || '';
  renderHotkeyBtn();
  // Restore audio device selection
  document.getElementById('audio-device').value = cfg.device_index;
  document.getElementById('cfg-threads').value = String(cfg.threads);

  // Restore TTS voice favourites, last-selected voice, and per-voice speeds.
  // Favourites are "model:sid" keys. Old configs stored bare sids, but those
  // only ever referred to LibriTTS speakers and that model left the
  // catalogue, so there is nothing to migrate them onto.
  ttsFavourites = Array.isArray(cfg.tts_favourite_voices) ? cfg.tts_favourite_voices.slice() : [];
  ttsVoiceSpeeds = (cfg.tts_voice_speeds && typeof cfg.tts_voice_speeds === 'object') ? cfg.tts_voice_speeds : {};
  ttsActiveModel = cfg.tts_model || '';
  ttsVoice = cfg.tts_voice || '0';
  updateVoiceBtnLabel();
  // Apply the saved speed for the restored voice (default 1x).
  setTtsSpeed(speedForVoice(ttsVoice));

  // Auto-save on change
  document.getElementById('cfg-haptic').addEventListener('change', saveConfig);
  document.getElementById('audio-device').addEventListener('change', saveConfig);
  document.getElementById('cfg-threads').addEventListener('change', applyThreads);

  // Meeting settings (desktop only): diarization toggle + transcript/summary
  // folders. Persisted through the same saveConfig round-trip.
  if (isDesktop) {
    await loadMeetingDevices();
    document.getElementById('cfg-meeting-diarize').checked = cfg.meeting_diarize !== false;
    document.getElementById('cfg-meeting-transcript-dir').value = cfg.meeting_transcript_dir || '';
    document.getElementById('cfg-meeting-summary-dir').value = cfg.meeting_summary_dir || '';
    selectDeviceOption(document.getElementById('cfg-meeting-mic'), cfg.meeting_mic_device || '');
    selectDeviceOption(document.getElementById('cfg-meeting-speaker'), cfg.meeting_output_device || '');
    document.getElementById('cfg-meeting-diarize').addEventListener('change', saveConfig);
    document.getElementById('cfg-meeting-transcript-dir').addEventListener('change', saveConfig);
    document.getElementById('cfg-meeting-summary-dir').addEventListener('change', saveConfig);
    document.getElementById('cfg-meeting-mic').addEventListener('change', saveConfig);
    document.getElementById('cfg-meeting-speaker').addEventListener('change', saveConfig);
  }
}

async function saveConfig() {
  const cfg = await invoke('get_config');
  cfg.device_index = parseInt(document.getElementById('audio-device').value, 10);
  cfg.haptic_feedback = document.getElementById('cfg-haptic').checked;
  cfg.threads = parseInt(document.getElementById('cfg-threads').value, 10);
  if (isDesktop) {
    cfg.meeting_diarize = document.getElementById('cfg-meeting-diarize').checked;
    cfg.meeting_transcript_dir = document.getElementById('cfg-meeting-transcript-dir').value.trim();
    cfg.meeting_summary_dir = document.getElementById('cfg-meeting-summary-dir').value.trim();
    cfg.meeting_mic_device = document.getElementById('cfg-meeting-mic').value;
    cfg.meeting_output_device = document.getElementById('cfg-meeting-speaker').value;
  }
  try {
    await invoke('save_config', { cfg });
  } catch (err) {
    console.error('Save failed:', err);
    showToast(`Save failed: ${err}`);
  }
}

// The thread count is read when the TTS engine is created, so persist it and
// reload the loaded model to apply immediately (and surface the new count in
// the debug log's "TTS loading with N threads" line).
async function applyThreads() {
  await saveConfig();
  const n = document.getElementById('cfg-threads').value;
  if (!ttsLoadedModelId) {
    showToast(`Threads set to ${n} — applies when you load a TTS model`);
    return;
  }
  try {
    showToast(`Applying ${n} threads — reloading model...`);
    await invoke('tts_stop');
    await invoke('tts_load', { id: ttsLoadedModelId });
    showToast('Threads applied — generate again to compare RTF');
  } catch (err) {
    showToast('Reload failed: ' + err);
  }
}

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

// ── Vocabulary tab ──

async function loadVocab() {
  const entries = await invoke('get_vocab_entries');
  const list = document.getElementById('vocab-list');
  const empty = document.getElementById('vocab-empty');
  list.innerHTML = '';
  if (entries.length === 0) {
    empty.classList.remove('hidden');
    return;
  }
  empty.classList.add('hidden');
  for (const entry of entries) {
    const row = document.createElement('div');
    row.className = 'flex items-center justify-between gap-3 text-sm';
    row.innerHTML = `
      <span class="font-mono text-on-surface-variant">${escapeHtml(entry.from)}</span>
      <span class="text-on-surface-variant/40 text-xs">→</span>
      <span class="font-mono text-on-surface flex-1">${escapeHtml(entry.to)}</span>
      <button class="vocab-del-btn text-on-surface-variant hover:text-error transition-colors cursor-pointer p-1 rounded-lg hover:bg-error/10" data-from="${escapeHtml(entry.from)}" title="Remove">
        <span class="material-symbols-outlined text-base">delete</span>
      </button>`;
    list.appendChild(row);
  }
  list.querySelectorAll('.vocab-del-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      try {
        await invoke('remove_vocab_entry', { from: btn.dataset.from });
        await loadVocab();
      } catch (err) {
        showToast('Failed to remove: ' + err);
      }
    });
  });
}

document.getElementById('vocab-add-btn').addEventListener('click', async () => {
  const fromEl = document.getElementById('vocab-from');
  const toEl = document.getElementById('vocab-to');
  const from = fromEl.value.trim();
  const to = toEl.value.trim();
  if (!from || !to) {
    showToast('Both fields are required');
    return;
  }
  try {
    await invoke('add_vocab_entry', { from, to });
    fromEl.value = '';
    toEl.value = '';
    await loadVocab();
  } catch (err) {
    showToast('Failed to add: ' + err);
  }
});

document.getElementById('vocab-from').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') document.getElementById('vocab-to').focus();
});

document.getElementById('vocab-to').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') document.getElementById('vocab-add-btn').click();
});

// ── Snippets tab ──

async function loadSnippets() {
  const snippetList = document.getElementById('snippet-list');
  const snippetEmpty = document.getElementById('snippet-empty');
  let items;
  try {
    items = await invoke('list_snippets');
  } catch (err) {
    console.error('Failed to load snippets:', err);
    return;
  }

  snippetList.innerHTML = '';
  if (items.length === 0) {
    snippetEmpty.classList.remove('hidden');
    return;
  }
  snippetEmpty.classList.add('hidden');

  let i = 0;
  for (const snippet of items) {
    const row = document.createElement('div');
    row.className = 'stagger-in flex items-start justify-between gap-3 px-4 py-3';
    row.style.setProperty('--i', i++);
    row.innerHTML = `
      <div class="min-w-0 flex-1 snippet-edit-target cursor-pointer" data-id="${escapeHtml(snippet.id)}">
        <div class="flex flex-wrap gap-1 mb-1">
          ${snippet.triggers.map(t =>
            `<span class="text-[10px] font-mono font-semibold bg-primary/10 text-primary px-2 py-0.5 rounded-full">${escapeHtml(t)}</span>`
          ).join('')}
        </div>
        <p class="text-[15px] font-semibold leading-snug text-on-surface line-clamp-2">${escapeHtml(snippet.body)}</p>
      </div>
      <button class="snippet-del-btn shrink-0 text-on-surface-variant hover:text-error transition-colors cursor-pointer p-1.5 rounded-lg hover:bg-error/10 mt-0.5" data-id="${escapeHtml(snippet.id)}" title="Delete">
        <span class="material-symbols-outlined text-base">delete</span>
      </button>`;
    snippetList.appendChild(row);
  }

  snippetList.querySelectorAll('.snippet-edit-target').forEach(el => {
    el.addEventListener('click', () => {
      const snippet = items.find(s => s.id === el.dataset.id);
      if (snippet) wizShowEdit(snippet);
    });
  });

  snippetList.querySelectorAll('.snippet-del-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      if (!await showConfirm('Delete this snippet?')) return;
      try {
        await invoke('delete_snippet', { id: btn.dataset.id });
        await loadSnippets();
      } catch (err) {
        showToast('Failed to delete: ' + err);
      }
    });
  });
}

// ── Snippet creation/edit wizard ──

let wizRecording = false;
let wizEditId = null; // null = create mode, string = edit mode
let wizTriggers = []; // current list of trigger phrases

function wizShow() {
  wizRecording = false;
  wizEditId = null;
  wizTriggers = [];
  document.getElementById('wiz-title').textContent = 'New Snippet';
  document.getElementById('wiz-body-section').classList.add('hidden');
  document.getElementById('wiz-body-text').value = '';
  document.getElementById('wiz-save').classList.add('hidden');
  wizSetRecordBtn('idle');
  wizRenderTriggers();
  const wiz = document.getElementById('snippet-wizard');
  wiz.classList.remove('hidden');
  wiz.classList.add('flex');
}

function wizShowEdit(snippet) {
  wizRecording = false;
  wizEditId = snippet.id;
  wizTriggers = [...snippet.triggers];
  document.getElementById('wiz-title').textContent = 'Edit Snippet';
  document.getElementById('wiz-body-text').value = snippet.body;
  document.getElementById('wiz-body-section').classList.remove('hidden');
  document.getElementById('wiz-save').classList.remove('hidden');
  wizSetRecordBtn('idle');
  wizRenderTriggers();
  const wiz = document.getElementById('snippet-wizard');
  wiz.classList.remove('hidden');
  wiz.classList.add('flex');
}

function wizHide() {
  const wiz = document.getElementById('snippet-wizard');
  wiz.classList.add('hidden');
  wiz.classList.remove('flex');
}

function wizAddTrigger(text) {
  const t = text.trim();
  if (!t) return;
  if (!wizTriggers.some(x => x.toLowerCase() === t.toLowerCase())) {
    wizTriggers.push(t);
  }
  wizRenderTriggers();
  wizUpdateSections();
}

function wizRemoveTrigger(index) {
  wizTriggers.splice(index, 1);
  wizRenderTriggers();
  wizUpdateSections();
}

function wizRenderTriggers() {
  const list = document.getElementById('wiz-trigger-list');
  list.innerHTML = wizTriggers.map((t, i) =>
    `<span class="inline-flex items-center gap-1 text-xs font-mono bg-primary/10 text-primary px-2 py-0.5 rounded">
      ${escapeHtml(t)}
      <button class="wiz-trigger-del hover:text-error cursor-pointer" data-index="${i}">
        <span class="material-symbols-outlined" style="font-size:14px">close</span>
      </button>
    </span>`
  ).join('');
  list.querySelectorAll('.wiz-trigger-del').forEach(btn => {
    btn.addEventListener('click', () => wizRemoveTrigger(parseInt(btn.dataset.index)));
  });
}

function wizUpdateSections() {
  const hasTriggers = wizTriggers.length > 0;
  document.getElementById('wiz-body-section').classList.toggle('hidden', !hasTriggers);
  const hasBody = document.getElementById('wiz-body-text').value.trim();
  document.getElementById('wiz-save').classList.toggle('hidden', !hasTriggers);
}

function wizSetRecordBtn(state) {
  const btn = document.getElementById('wiz-record-btn');
  if (state === 'idle') {
    btn.innerHTML = '<span class="material-symbols-outlined text-base align-middle mr-1">mic</span> Tap to Record';
    btn.className = 'w-full py-3 rounded-xl text-xs font-bold uppercase tracking-widest transition-all active:scale-[0.98] cursor-pointer bg-surface-container-highest text-on-surface border border-outline-variant/30 hover:bg-surface-container-high';
    btn.disabled = false;
  } else if (state === 'recording') {
    btn.innerHTML = '<span class="material-symbols-outlined text-base align-middle mr-1 animate-pulse">graphic_eq</span> Recording... Tap to Stop';
    btn.className = 'w-full py-3 rounded-xl text-xs font-bold uppercase tracking-widest transition-all active:scale-[0.98] cursor-pointer bg-[#FF9944] text-white';
    btn.disabled = false;
  } else if (state === 'processing') {
    btn.innerHTML = '<span class="material-symbols-outlined text-base align-middle mr-1 animate-spin">progress_activity</span> Transcribing...';
    btn.className = 'w-full py-3 rounded-xl text-xs font-bold uppercase tracking-widest transition-all cursor-not-allowed bg-surface-container-highest text-on-surface-variant border border-outline-variant/30';
    btn.disabled = true;
  }
}

document.getElementById('wiz-record-btn').addEventListener('click', async () => {
  if (wizRecording) {
    wizRecording = false;
    wizSetRecordBtn('processing');
    try {
      const text = await invoke('ui_stop_and_transcribe_raw');
      wizAddTrigger(text);
      wizSetRecordBtn('idle');
    } catch (err) {
      showToast('Recording failed: ' + err);
      wizSetRecordBtn('idle');
    }
  } else {
    try {
      await invoke('ui_start_recording');
      wizRecording = true;
      wizSetRecordBtn('recording');
    } catch (err) {
      showToast('Failed to start: ' + err);
    }
  }
});

document.getElementById('wiz-save').addEventListener('click', async () => {
  const body = document.getElementById('wiz-body-text').value.trim();
  if (wizTriggers.length === 0) { showToast('Add at least one trigger'); return; }
  if (!body) { showToast('Body text cannot be empty'); return; }
  try {
    if (wizEditId) {
      await invoke('update_snippet', { id: wizEditId, triggers: wizTriggers, body });
      showToast('Snippet updated');
    } else {
      await invoke('save_snippet', { trigger: wizTriggers[0], body });
      // Add any extra triggers
      if (wizTriggers.length > 1) {
        const snippets = await invoke('list_snippets');
        const newest = snippets[snippets.length - 1];
        for (let i = 1; i < wizTriggers.length; i++) {
          await invoke('add_snippet_trigger', { id: newest.id, trigger: wizTriggers[i] });
        }
      }
      showToast('Snippet saved');
    }
    wizHide();
    await loadSnippets();
  } catch (err) {
    showToast('Failed to save: ' + err);
  }
});

document.getElementById('wiz-cancel').addEventListener('click', () => {
  if (wizRecording) {
    invoke('ui_stop_and_transcribe_raw').catch(() => {});
    wizRecording = false;
  }
  wizHide();
});

document.getElementById('snippet-create-btn').addEventListener('click', () => {
  if (!engineReady) {
    showToast('Engine still loading...');
    return;
  }
  wizShow();
});

// Snippet picker (no-match flow with self-healing)

let _snippetPickerSnippets = [];
let _snippetPickerText = '';

function showSnippetPicker(text, snippets) {
  _snippetPickerText = text;
  _snippetPickerSnippets = snippets;

  document.getElementById('snippet-picker-text').textContent = `"${text}"`;
  const list = document.getElementById('snippet-picker-list');
  list.innerHTML = '';

  if (snippets.length === 0) {
    list.innerHTML = '<p class="text-xs text-on-surface-variant">No snippets defined yet.</p>';
  } else {
    for (const snippet of snippets) {
      const btn = document.createElement('button');
      btn.className = 'w-full text-left px-3 py-2 rounded-lg hover:bg-surface-container-highest transition-colors cursor-pointer';
      btn.innerHTML = `
        <p class="text-xs font-mono text-primary">${escapeHtml(snippet.triggers[0])}</p>
        <p class="text-xs text-on-surface-variant truncate">${escapeHtml(snippet.body)}</p>`;
      btn.addEventListener('click', () => selectSnippetFromPicker(snippet));
      list.appendChild(btn);
    }
  }

  const picker = document.getElementById('snippet-picker');
  picker.classList.remove('hidden');
  picker.classList.add('flex');
}

async function selectSnippetFromPicker(snippet) {
  closeSnippetPicker();
  // Register the misheard text as an additional trigger (self-healing)
  try {
    await invoke('add_snippet_trigger', { id: snippet.id, trigger: _snippetPickerText });
  } catch (err) {
    console.error('Failed to add trigger:', err);
  }
  // Copy snippet body to clipboard so user can paste it
  try {
    await invoke('copy_to_clipboard', { text: snippet.body });
    showToast('Snippet copied to clipboard');
  } catch (_) {
    showToast('Snippet selected: ' + snippet.body.slice(0, 40));
  }
  await loadSnippets();
}

function closeSnippetPicker() {
  const picker = document.getElementById('snippet-picker');
  picker.classList.add('hidden');
  picker.classList.remove('flex');
}

document.getElementById('snippet-picker-cancel').addEventListener('click', closeSnippetPicker);

listen('snippet-matched', (event) => {
  showToast(`Snippet: "${event.payload?.trigger_text}"`);
});

listen('snippet-no-match', (event) => {
  const { text, snippets } = event.payload;
  showSnippetPicker(text, snippets || []);
});

// ── Reader (TTS) tab ──

const TTS_VOICE_PRESETS = {
  'tts-piper-alba': [
    { sid: 0, label: 'Alba (EN-GB F)' },
  ],
  'tts-kokoro-v1': [
    { sid: 0, label: 'Alloy (EN-US F)' },
    { sid: 1, label: 'Aoede (EN-US F)' },
    { sid: 2, label: 'Bella (EN-US F)' },
    { sid: 3, label: 'Heart (EN-US F)' },
    { sid: 4, label: 'Jessica (EN-US F)' },
    { sid: 5, label: 'Kore (EN-US F)' },
    { sid: 6, label: 'Nicole (EN-US F)' },
    { sid: 7, label: 'Nova (EN-US F)' },
    { sid: 8, label: 'River (EN-US F)' },
    { sid: 9, label: 'Sarah (EN-US F)' },
    { sid: 10, label: 'Sky (EN-US F)' },
    { sid: 11, label: 'Adam (EN-US M)' },
    { sid: 12, label: 'Echo (EN-US M)' },
    { sid: 13, label: 'Eric (EN-US M)' },
    { sid: 14, label: 'Fenrir (EN-US M)' },
    { sid: 15, label: 'Liam (EN-US M)' },
    { sid: 16, label: 'Michael (EN-US M)' },
    { sid: 17, label: 'Onyx (EN-US M)' },
    { sid: 18, label: 'Puck (EN-US M)' },
    { sid: 19, label: 'Santa (EN-US M)' },
    { sid: 20, label: 'Alice (EN-GB F)' },
    { sid: 21, label: 'Emma (EN-GB F)' },
    { sid: 22, label: 'Isabella (EN-GB F)' },
    { sid: 23, label: 'Lily (EN-GB F)' },
    { sid: 24, label: 'Daniel (EN-GB M)' },
    { sid: 25, label: 'Fable (EN-GB M)' },
    { sid: 26, label: 'George (EN-GB M)' },
    { sid: 27, label: 'Lewis (EN-GB M)' },
    { sid: 28, label: 'Dora (ES F)' },
    { sid: 29, label: 'Alex (ES M)' },
    { sid: 30, label: 'Siwis (FR F)' },
    { sid: 31, label: 'Alpha (HI F)' },
    { sid: 32, label: 'Beta (HI F)' },
    { sid: 33, label: 'Omega (HI M)' },
    { sid: 34, label: 'Psi (HI M)' },
    { sid: 35, label: 'Sara (IT F)' },
    { sid: 36, label: 'Nicola (IT M)' },
    { sid: 37, label: 'Alpha (JA F)' },
    { sid: 38, label: 'Gongitsune (JA F)' },
    { sid: 39, label: 'Nezumi (JA F)' },
    { sid: 40, label: 'Tebukuro (JA F)' },
    { sid: 41, label: 'Kumo (JA M)' },
    { sid: 42, label: 'Dora (PT F)' },
    { sid: 43, label: 'Alex (PT M)' },
    { sid: 44, label: 'Santa (PT M)' },
    { sid: 45, label: 'Xiaobei (ZH F)' },
    { sid: 46, label: 'Xiaoni (ZH F)' },
    { sid: 47, label: 'Xiaoxiao (ZH F)' },
    { sid: 48, label: 'Xiaoyi (ZH F)' },
    { sid: 49, label: 'Yunjian (ZH M)' },
    { sid: 50, label: 'Yunxi (ZH M)' },
    { sid: 51, label: 'Yunxia (ZH M)' },
    { sid: 52, label: 'Yunyang (ZH M)' },
  ],
};

let ttsModelId = null;       // active Piper TTS model id, resolved from the registry
let ttsLoadedModelId = null;
let ttsLoadedEngine = null;
// Set when the voice/model changes; a paused player must re-synthesize under
// the new voice on resume instead of resuming the old audio. Cleared whenever
// a fresh synth starts (startSpeak).
let voiceDirty = false;
// User-chosen TTS model (config.tts_model); '' = first registry TTS model.
let ttsActiveModel = '';

// Resolve the active TTS model from the registry list: the configured choice
// when it exists, else the first TTS entry (original single-model behavior).
function pickTtsModel(models) {
  const all = models.filter(m => m.engine.startsWith('tts_'));
  return all.find(m => m.id === ttsActiveModel) || all[0] || null;
}

// Friendly label for a speaker id of the ACTIVE model (catalogue names, then
// legacy presets, then "Voice N").
function voiceLabel(sid) {
  return catalogLabel(ttsModelId, sid);
}

async function updateTtsPanel() {
  const models = await invoke('list_models');
  const tts = pickTtsModel(models);
  const block = document.getElementById('tts-model-block');
  const dlRow = document.getElementById('tts-download-row');
  const dlLabel = document.getElementById('tts-download-label');

  if (!tts) {
    ttsModelId = null;
    ttsReady = false;
    block.classList.add('hidden');
    setPlayEnabled(false);
    return;
  }

  ttsModelId = tts.id;
  ttsLoadedEngine = tts.engine;
  // The player-bar voice label resolves its name through ttsModelId, which
  // was null at the boot-time render — re-render now that it's known, or the
  // bar says "Voice 0" until the user happens to open the voice sheet.
  updateVoiceBtnLabel();
  const info = await invoke('tts_info');
  if (info.loaded) ttsLoadedModelId = tts.id;

  const downloaded = tts.status === 'downloaded' || tts.status === 'active';
  const downloading = tts.status === 'downloading';

  dlLabel.textContent = `${tts.name}, ${tts.size}`;
  dlRow.classList.toggle('hidden', downloaded || downloading);

  // The block carries the download prompt; show it only while the voice isn't
  // ready. Playback (the player bar) is enabled once the voice is on disk; the
  // model loads lazily on first play.
  ttsReady = downloaded;
  setPlayEnabled(downloaded);
  block.classList.toggle('hidden', downloaded && !downloading);
}

// ── Voices page: catalogue (listen, favourite, download) ──

// Remote audition clips from the piper-samples site (playable before the
// model is downloaded); local synthesis is the fallback once it is.
const SAMPLE_BASE = 'https://rhasspy.github.io/piper-samples/samples/';

// Internal plumbing only: sample-clip path builders per backing file. The
// UI never surfaces "models" — users see voices, and the shared file a
// voice needs is fetched transparently on first Use.
const MODEL_META = {
  'tts-piper-alba': { sample: () => 'en/en_GB/alba/medium/speaker_0.mp3' },
  'tts-piper-alan': { sample: () => 'en/en_GB/alan/medium/speaker_0.mp3' },
  'tts-piper-aru': { sample: sid => `en/en_GB/aru/medium/speaker_${sid}.mp3` },
  'tts-piper-cori': { sample: () => 'en/en_GB/cori/medium/speaker_0.mp3' },
  'tts-piper-cori-high': { sample: () => 'en/en_GB/cori/high/speaker_0.mp3' },
  'tts-piper-jenny': { sample: () => 'en/en_GB/jenny_dioco/medium/speaker_0.mp3' },
  'tts-piper-northern-male': { sample: () => 'en/en_GB/northern_english_male/medium/speaker_0.mp3' },
  'tts-piper-semaine': { sample: sid => `en/en_GB/semaine/medium/speaker_${sid}.mp3` },
  'tts-piper-southern-female': { sample: () => 'en/en_GB/southern_english_female/low/speaker_0.mp3' },
  'tts-piper-vctk': { sample: sid => `en/en_GB/vctk/medium/speaker_${sid}.mp3` },
};

// The catalogue as users see it: named voices first, the big generic packs
// under "More voices". Keys stay model-scoped internally.
const VOICES = [
  { model: 'tts-piper-alba', sid: 0, label: 'Alba', hint: 'Scottish female' },
  { model: 'tts-piper-alan', sid: 0, label: 'Alan', hint: 'Male' },
  { model: 'tts-piper-cori', sid: 0, label: 'Cori', hint: 'Female' },
  { model: 'tts-piper-cori-high', sid: 0, label: 'Cori HQ', hint: 'Female, highest quality' },
  { model: 'tts-piper-jenny', sid: 0, label: 'Jenny', hint: 'Female' },
  { model: 'tts-piper-northern-male', sid: 0, label: 'Northern English Male', hint: 'Male, northern accent' },
  { model: 'tts-piper-southern-female', sid: 0, label: 'Southern English Female', hint: 'Female, southern accent' },
  { model: 'tts-piper-semaine', sid: 0, label: 'Prudence', hint: 'Expressive female' },
  { model: 'tts-piper-semaine', sid: 1, label: 'Spike', hint: 'Expressive male' },
  { model: 'tts-piper-semaine', sid: 2, label: 'Obadiah', hint: 'Expressive male' },
  { model: 'tts-piper-semaine', sid: 3, label: 'Poppy', hint: 'Expressive female' },
  ...Array.from({ length: 12 }, (_, i) => ({ model: 'tts-piper-aru', sid: i, label: `Aru ${i + 1}`, group: 1 })),
  // VCTK studio corpus: the 70 UK speakers (of 109) by documented accent.
  // Under review via VOICES.md — trim after the listening pass.
  { model: 'tts-piper-vctk', sid: 107, label: 'VCTK p225', hint: 'Female · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 90, label: 'VCTK p228', hint: 'Female · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 85, label: 'VCTK p229', hint: 'Female · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 15, label: 'VCTK p231', hint: 'Female · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 86, label: 'VCTK p240', hint: 'Female · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 18, label: 'VCTK p257', hint: 'Female · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 66, label: 'VCTK p268', hint: 'Female · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 1, label: 'VCTK p236', hint: 'Female · Manchester', group: 1 },
  { model: 'tts-piper-vctk', sid: 48, label: 'VCTK p244', hint: 'Female · Manchester', group: 1 },
  { model: 'tts-piper-vctk', sid: 77, label: 'VCTK p269', hint: 'Female · Newcastle', group: 1 },
  { model: 'tts-piper-vctk', sid: 91, label: 'VCTK p282', hint: 'Female · Newcastle', group: 1 },
  { model: 'tts-piper-vctk', sid: 14, label: 'VCTK p277', hint: 'Female · Northeast England', group: 1 },
  { model: 'tts-piper-vctk', sid: 11, label: 'VCTK p276', hint: 'Female · Oxford', group: 1 },
  { model: 'tts-piper-vctk', sid: 3, label: 'VCTK p250', hint: 'Female · Southeast England', group: 1 },
  { model: 'tts-piper-vctk', sid: 0, label: 'VCTK p239', hint: 'Female · Southwest England', group: 1 },
  { model: 'tts-piper-vctk', sid: 89, label: 'VCTK p233', hint: 'Female · Staffordshire', group: 1 },
  { model: 'tts-piper-vctk', sid: 74, label: 'VCTK p230', hint: 'Female · Stockton-on-tees', group: 1 },
  { model: 'tts-piper-vctk', sid: 54, label: 'VCTK p267', hint: 'Female · Yorkshire', group: 1 },
  { model: 'tts-piper-vctk', sid: 94, label: 'VCTK p234', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 17, label: 'VCTK p238', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 103, label: 'VCTK p249', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 88, label: 'VCTK p253', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 6, label: 'VCTK p261', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 80, label: 'VCTK p262', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 2, label: 'VCTK p264', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 100, label: 'VCTK p265', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 41, label: 'VCTK p266', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 63, label: 'VCTK p280', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 58, label: 'VCTK p288', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 78, label: 'VCTK p293', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 56, label: 'VCTK p295', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 46, label: 'VCTK p313', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 42, label: 'VCTK p335', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 30, label: 'VCTK p340', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 44, label: 'VCTK p351', hint: 'Female · UK', group: 1 },
  { model: 'tts-piper-vctk', sid: 60, label: 'VCTK p232', hint: 'Male · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 57, label: 'VCTK p258', hint: 'Male · Southern England', group: 1 },
  { model: 'tts-piper-vctk', sid: 104, label: 'VCTK p256', hint: 'Male · Birmingham', group: 1 },
  { model: 'tts-piper-vctk', sid: 64, label: 'VCTK p278', hint: 'Male · Cheshire', group: 1 },
  { model: 'tts-piper-vctk', sid: 82, label: 'VCTK p227', hint: 'Male · Cumbria', group: 1 },
  { model: 'tts-piper-vctk', sid: 10, label: 'VCTK p274', hint: 'Male · Essex', group: 1 },
  { model: 'tts-piper-vctk', sid: 69, label: 'VCTK p279', hint: 'Male · Leicester', group: 1 },
  { model: 'tts-piper-vctk', sid: 81, label: 'VCTK p243', hint: 'Male · London', group: 1 },
  { model: 'tts-piper-vctk', sid: 9, label: 'VCTK p286', hint: 'Male · Newcastle', group: 1 },
  { model: 'tts-piper-vctk', sid: 4, label: 'VCTK p259', hint: 'Male · Nottingham', group: 1 },
  { model: 'tts-piper-vctk', sid: 19, label: 'VCTK p273', hint: 'Male · Suffolk', group: 1 },
  { model: 'tts-piper-vctk', sid: 95, label: 'VCTK p226', hint: 'Male · Surrey', group: 1 },
  { model: 'tts-piper-vctk', sid: 76, label: 'VCTK p254', hint: 'Male · Surrey', group: 1 },
  { model: 'tts-piper-vctk', sid: 23, label: 'VCTK p287', hint: 'Male · York', group: 1 },
  { model: 'tts-piper-vctk', sid: 12, label: 'VCTK p270', hint: 'Male · Yorkshire', group: 1 },
  { model: 'tts-piper-vctk', sid: 7, label: 'VCTK p263', hint: 'Male · Aberdeen', group: 1 },
  { model: 'tts-piper-vctk', sid: 5, label: 'VCTK p247', hint: 'Male · Argyll', group: 1 },
  { model: 'tts-piper-vctk', sid: 79, label: 'VCTK p252', hint: 'Male · Edinburgh', group: 1 },
  { model: 'tts-piper-vctk', sid: 62, label: 'VCTK p272', hint: 'Male · Edinburgh', group: 1 },
  { model: 'tts-piper-vctk', sid: 13, label: 'VCTK p281', hint: 'Male · Edinburgh', group: 1 },
  { model: 'tts-piper-vctk', sid: 70, label: 'VCTK p285', hint: 'Male · Edinburgh', group: 1 },
  { model: 'tts-piper-vctk', sid: 102, label: 'VCTK p237', hint: 'Male · Fife', group: 1 },
  { model: 'tts-piper-vctk', sid: 16, label: 'VCTK p271', hint: 'Male · Fife', group: 1 },
  { model: 'tts-piper-vctk', sid: 20, label: 'VCTK p284', hint: 'Male · Fife', group: 1 },
  { model: 'tts-piper-vctk', sid: 84, label: 'VCTK p255', hint: 'Male · Galloway', group: 1 },
  { model: 'tts-piper-vctk', sid: 55, label: 'VCTK p275', hint: 'Male · Midlothian', group: 1 },
  { model: 'tts-piper-vctk', sid: 96, label: 'VCTK p260', hint: 'Male · Orkney', group: 1 },
  { model: 'tts-piper-vctk', sid: 98, label: 'VCTK p241', hint: 'Male · Perth', group: 1 },
  { model: 'tts-piper-vctk', sid: 93, label: 'VCTK p246', hint: 'Male · Selkirk', group: 1 },
  { model: 'tts-piper-vctk', sid: 61, label: 'VCTK p292', hint: 'Male · Belfast', group: 1 },
  { model: 'tts-piper-vctk', sid: 28, label: 'VCTK p304', hint: 'Male · Belfast', group: 1 },
  { model: 'tts-piper-vctk', sid: 106, label: 'VCTK p364', hint: 'Male · Donegal', group: 1 },
  { model: 'tts-piper-vctk', sid: 97, label: 'VCTK p245', hint: 'Male · Dublin', group: 1 },
  { model: 'tts-piper-vctk', sid: 67, label: 'VCTK p298', hint: 'Male · Tipperary', group: 1 },
  { model: 'tts-piper-vctk', sid: 8, label: 'VCTK p283', hint: 'Male · UK', group: 1 },
];

function voiceByKey(key) {
  return VOICES.find(v => voiceKey(v.model, v.sid) === key);
}

let ttsModelsCache = [];
// Voice currently auditioning (drives the row's playing icon); null = none.
let samplingKey = null;
let sampleClearTimer = null;
let sampleAudio = null;

const voiceKey = (model, sid) => `${model}:${sid}`;

function splitKey(key) {
  const i = key.lastIndexOf(':');
  return [key.slice(0, i), key.slice(i + 1)];
}

function catalogLabel(model, sid) {
  const v = voiceByKey(voiceKey(model, sid));
  if (v) return v.label;
  const presets = TTS_VOICE_PRESETS[model] || [];
  const p = presets.find(x => x.sid === sid);
  if (p) return p.label;
  return `Voice ${sid}`;
}

function modelById(id) { return ttsModelsCache.find(m => m.id === id); }
function isDownloaded(m) { return !!m && (m.status === 'downloaded' || m.status === 'active'); }
function speakersOf(m) {
  return VOICES.filter(v => v.model === m.id).length || 1;
}

function voiceRowHtml(v, i) {
  const key = voiceKey(v.model, v.sid);
  const m = modelById(v.model);
  const fav = ttsFavourites.includes(key);
  const playing = key === samplingKey;
  const ready = isDownloaded(m);
  const inUse = ready && (pickTtsModel(ttsModelsCache) || {}).id === v.model && String(v.sid) === ttsVoice;
  const hintBits = [];
  if (v.hint) hintBits.push(escapeHtml(v.hint));
  if (!ready && m) hintBits.push(`${escapeHtml(m.size)} on first use`);
  const hint = hintBits.length
    ? `<span class="block text-xs text-on-surface-variant truncate mt-0.5">${hintBits.join(' \u{b7} ')}</span>`
    : '';
  const right = inUse
    ? '<span class="text-[11px] font-semibold text-primary shrink-0">In use</span>'
    : key === pendingUseKey
      ? `<span class="text-[11px] font-semibold text-on-surface-variant shrink-0" data-voice-progress="${escapeHtml(v.model)}">0%</span>`
      : `<button class="voice-use shrink-0 min-h-10 flex items-center justify-center text-xs font-semibold text-on-surface-variant hover:text-primary transition-colors px-3 rounded-lg hover:bg-primary/10 cursor-pointer" data-model="${escapeHtml(v.model)}" data-sid="${v.sid}">Use</button>`;
  // The sample icon stays a standalone glyph (not folded into the avatar) so
  // updateSampleIcon's direct DOM patch — it toggles this exact node on play/
  // stop without a re-render — keeps working untouched.
  return `
    <div class="voice-row stagger-in flex items-center justify-between gap-3 px-4 py-2.5" data-key="${escapeHtml(key)}" style="--i:${i || 0}">
      <button class="voice-sample flex items-center gap-3 min-w-0 flex-1 min-h-10 text-left cursor-pointer" data-model="${escapeHtml(v.model)}" data-sid="${v.sid}">
        <div class="${coverClass(v.label)} w-10 h-10 rounded-full text-xs shrink-0">${escapeHtml(coverMonogram(v.label))}</div>
        <span class="material-symbols-outlined text-xl shrink-0 ${playing ? 'text-primary' : 'text-on-surface-variant'}">${playing ? 'graphic_eq' : 'play_circle'}</span>
        <span class="min-w-0">
          <span class="block text-[15px] font-semibold leading-snug text-on-surface truncate">${escapeHtml(v.label)}</span>
          ${hint}
        </span>
      </button>
      ${right}
      <button class="voice-fav shrink-0 min-h-10 min-w-10 flex items-center justify-center cursor-pointer ${fav ? 'text-primary' : 'text-on-surface-variant/40'}" data-key="${escapeHtml(key)}">
        <span class="material-symbols-outlined text-lg" style="font-variation-settings:'FILL' ${fav ? 1 : 0}">star</span>
      </button>
    </div>`;
}

function rebuildFavSection() {
  const favSection = document.getElementById('voices-fav-section');
  const favList = document.getElementById('voices-fav-list');
  const rows = ttsFavourites
    .map((key, i) => {
      const v = voiceByKey(key);
      // Stale keys (unknown voice or its backing file left the registry)
      // stay in config but don't render.
      return v && modelById(v.model) ? voiceRowHtml(v, i) : '';
    })
    .filter(Boolean);
  favSection.classList.toggle('hidden', rows.length === 0);
  favList.innerHTML = rows.join('');
}

async function loadVoices() {
  const models = await invoke('list_models');
  ttsModelsCache = models.filter(m => m.engine.startsWith('tts_'));
  const active = pickTtsModel(models);
  if (active) ttsModelId = active.id;
  const section = (title, rows) => rows.length
    ? `<section>
        <p class="text-xs font-semibold text-on-surface-variant uppercase tracking-wider mb-2">${title}</p>
        <div class="bg-surface-container-low rounded-xl overflow-hidden divide-y divide-outline-variant/20">${rows.map(voiceRowHtml).join('')}</div>
      </section>`
    : '';
  document.getElementById('voices-catalogue').innerHTML =
    section('Voices', VOICES.filter(v => !v.group && modelById(v.model)))
    + section('More voices', VOICES.filter(v => v.group === 1 && modelById(v.model)));
  rebuildFavSection();
}

function stopSampleAudio() {
  if (sampleAudio) {
    try { sampleAudio.pause(); } catch (_) { /* already stopped */ }
    sampleAudio = null;
  }
}

async function localSample(model, sid) {
  if (ttsLoadedModelId !== model) {
    await invoke('tts_load', { id: model });
    ttsLoadedModelId = model;
  }
  await invoke('tts_sample', { sid, speed: ttsSpeed });
}

// Preview a voice: stream the piper-samples clip (works before download);
// fall back to on-device synthesis for downloaded models without a clip.
async function sampleVoiceRow(model, sid) {
  stopSampleAudio();
  setSamplingKey(voiceKey(model, sid));
  const meta = MODEL_META[model];
  const url = meta && meta.sample ? SAMPLE_BASE + meta.sample(sid) : null;
  const fallback = async () => {
    if (!isDownloaded(modelById(model))) {
      showToast('No sample available — download the model to preview');
      clearSampling();
      return;
    }
    try { await localSample(model, sid); } catch (err) {
      showToast('Sample failed: ' + err);
      clearSampling();
    }
  };
  if (!url) { await fallback(); return; }
  sampleAudio = new Audio(url);
  sampleAudio.onended = () => clearSampling();
  sampleAudio.onerror = () => { fallback(); };
  try { await sampleAudio.play(); } catch (_) { await fallback(); }
}

function setSamplingKey(key) {
  const prev = samplingKey;
  samplingKey = key;
  if (prev && prev !== key) updateSampleIcon(prev);
  updateSampleIcon(key);
  if (sampleClearTimer) clearTimeout(sampleClearTimer);
  // Local synth has no finished event back to us; clear after the clip-ish
  // duration unless another row starts first.
  sampleClearTimer = setTimeout(clearSampling, 7000);
}

function clearSampling() {
  const prev = samplingKey;
  samplingKey = null;
  if (prev) updateSampleIcon(prev);
  if (sampleClearTimer) { clearTimeout(sampleClearTimer); sampleClearTimer = null; }
}

function updateSampleIcon(key) {
  const playing = key === samplingKey;
  document.querySelectorAll(`.voice-row[data-key="${CSS.escape(key)}"] .voice-sample .material-symbols-outlined`).forEach(icon => {
    icon.textContent = playing ? 'graphic_eq' : 'play_circle';
    icon.classList.toggle('text-primary', playing);
    icon.classList.toggle('text-on-surface-variant', !playing);
  });
}

// Voice being fetched because the user tapped Use before its backing file
// was on disk; activation completes when the download does.
let pendingUseKey = null;

// Switch the reader to a voice. If its backing file isn't downloaded yet,
// ask first, then fetch — the row shows inline progress and the voice
// activates when the download completes.
async function useVoice(model, sid) {
  const key = voiceKey(model, sid);
  if (!isDownloaded(modelById(model))) {
    if (!await confirmVoiceDownload(model)) return;
    pendingUseKey = key;
    await loadVoices();
    try {
      await invoke('download_model', { id: model });
    } catch (err) {
      showToast('Voice download failed: ' + err);
      pendingUseKey = null;
      await loadVoices();
      return;
    }
    if (pendingUseKey !== key) return; // user picked another voice meanwhile
    pendingUseKey = null;
    const models = await invoke('list_models');
    ttsModelsCache = models.filter(m => m.engine.startsWith('tts_'));
  }
  const active = pickTtsModel(ttsModelsCache);
  if (!active || active.id !== model) {
    ttsActiveModel = model;
    try {
      const cfg = await invoke('get_config');
      cfg.tts_model = model;
      await invoke('save_config', { cfg });
    } catch (_) { /* non-fatal */ }
    try { await invoke('tts_stop'); } catch (_) { /* not playing */ }
    ttsLoadedModelId = null;
    ttsModelId = model;
  }
  setTtsVoice(String(sid));
  await loadVoices();
}

async function toggleFavourite(key) {
  const i = ttsFavourites.indexOf(key);
  const adding = i < 0;
  if (adding) ttsFavourites.push(key);
  else ttsFavourites.splice(i, 1);
  await persistFavourites();
  // Update stars in place across catalogue + favourites, then rebuild only
  // the small favourites list (avoids re-rendering ~1000 rows per tap).
  document.querySelectorAll(`.voice-fav[data-key="${CSS.escape(key)}"]`).forEach(btn => {
    btn.classList.toggle('text-primary', adding);
    btn.classList.toggle('text-on-surface-variant/40', !adding);
    const star = btn.querySelector('.material-symbols-outlined');
    if (star) star.style.fontVariationSettings = `'FILL' ${adding ? 1 : 0}`;
  });
  rebuildFavSection();
}

async function persistFavourites() {
  try {
    const cfg = await invoke('get_config');
    cfg.tts_favourite_voices = ttsFavourites;
    await invoke('save_config', { cfg });
  } catch (err) { showToast('Save failed: ' + err); }
}

function onCatalogueClick(e) {
  const use = e.target.closest('.voice-use');
  if (use) { useVoice(use.dataset.model, parseInt(use.dataset.sid, 10)); return; }
  const sampleBtn = e.target.closest('.voice-sample');
  if (sampleBtn) { sampleVoiceRow(sampleBtn.dataset.model, parseInt(sampleBtn.dataset.sid, 10)); return; }
  const favBtn = e.target.closest('.voice-fav');
  if (favBtn) toggleFavourite(favBtn.dataset.key);
}
document.getElementById('voices-catalogue').addEventListener('click', onCatalogueClick);
document.getElementById('voices-fav-list').addEventListener('click', onCatalogueClick);

document.getElementById('tts-download-btn').addEventListener('click', async () => {
  if (!ttsModelId) return;
  const btn = document.getElementById('tts-download-btn');
  const progress = document.getElementById('tts-dl-progress');
  btn.disabled = true;
  btn.textContent = 'Downloading...';
  progress.classList.remove('hidden');
  try {
    await invoke('download_model', { id: ttsModelId });
    progress.classList.add('hidden');
    await updateTtsPanel();
  } catch (err) {
    showToast('Download failed: ' + err);
    progress.classList.add('hidden');
    btn.disabled = false;
    btn.textContent = 'Download';
  }
});

// ── TTS Player Bar ──

let ttsState = { position_ms: 0, buffered_ms: 0, duration_ms: 0, paused: false, finished: false };
// True from when a render starts until the player actually begins emitting audio
// (drives the buffering spinner during the generate/pre-buffer gap, which has no
// position events). Cleared on the first non-rebuffering position event.
let ttsLoading = false;
// The spinner is delay-gated: every play press (fresh, resume, speed/voice/seek)
// has a short generate+pre-buffer gap, and showing the spinner instantly made it
// flash on every start. Only a wait past SPINNER_DELAY_MS arms it, so quick starts
// just go straight to playing. Mid-playback rebuffering is NOT gated (audio has
// genuinely stopped, so it should show at once).
let spinnerArmed = false;
let spinnerTimer = null;
const SPINNER_DELAY_MS = 350;
function setTtsLoading(on) {
  ttsLoading = on;
  if (spinnerTimer) { clearTimeout(spinnerTimer); spinnerTimer = null; }
  spinnerArmed = false;
  if (on) {
    spinnerTimer = setTimeout(() => {
      spinnerTimer = null;
      spinnerArmed = true;
      if (ttsLoading) updatePlayerBar(ttsState);
    }, SPINNER_DELAY_MS);
  }
}
// When a re-render is triggered while paused (a seek that can't be an in-buffer
// cursor move), keep it paused after the new render starts instead of playing.
let renderStartPaused = false;
// Latest fragment-relative buffered ms, updated on every position event even
// during a seek-drag (ttsState is frozen then). Used to decide whether a seek
// target is already generated (instant cursor move) or needs a re-render.
let liveBufferedMs = 0;
let ttsSeeking = false;
let ttsSpeed = 1.0;
// Canonical selected voice: a speaker-id string ("37") or "custom:<name>".
// Both the player voice sheet and a fresh load set this; startSpeak reads it.
let ttsVoice = '0';
// Starred speaker ids. The player voice sheet shows these (or all when empty).
let ttsFavourites = [];
// Per-voice playback speed (voice string -> speed). Selecting a voice restores
// its last speed; changing speed saves it against the current voice.
let ttsVoiceSpeeds = {};
// Reading view state (Listen library)
let readingItem = null;
let readingText = '';
// Set while a book chapter is open: {id, chapters, current}. `chapters` is the
// book's ChapterMeta list (word/char counts), `current` the open chapter's
// index. Null for a plain article — that's the flag openReading/tts-finished
// branch on to tell a book chapter apart from a whole-article read.
let bookState = null;
let readingWords = [];
let wordTimes = {};
let genBaseWord = 0;
let timingCursor = 0;
let activeWord = -1;
// Whether the highlight auto-scrolls the reading view. Disabled when the user
// scrolls manually (so they can read elsewhere); re-enabled when the highlight
// comes back into view.
let autoFollow = true;
// Word to resume from on the next Play (set when opening a part-read item); 0
// means start from the top. Cleared once consumed.
let resumeWord = 0;
// Throttle for persisting reading progress (Date.now() ms of last save).
let lastProgressSaveMs = 0;
// Whether playback has been started for the open article. False = the player
// bar's play button starts (resume or from 0) instead of toggling pause.
let ttsStarted = false;
// Whether the voice is downloaded (gates the player-bar play button).
let ttsReady = false;
// Estimated full-article duration (ms) at the current speed. Holds the player
// bar's total stable while generating; replaced by the real duration once
// generation completes (gen_done). Recomputed on open and on each (re)start.
let articleEstMs = 0;
// genId whose real duration has already been saved, so a full play measures the
// article's true length once (on gen_done) rather than repeatedly.
let measuredGenId = -1;
// genId already sampled into the per-voice calibration EMA (maybeLearnVoiceCal).
// Tracked separately from measuredGenId because calibration also learns from
// book chapters, which measuredGenId/maybeSaveMeasuredDuration deliberately skip.
let calibratedGenId = -1;
// The live duration estimate only refines the displayed total at these
// generation-progress milestones (fraction of the fragment generated), so the
// number changes a few discrete times rather than continuously. dynMilestoneIdx
// is the next one to fire; reset per (re)start.
const REFINE_MILESTONES = [0.25, 0.5, 0.75];
let dynMilestoneIdx = 0;
// Absolute ms offset of the current render fragment within the full text. The
// player plays one fragment [genBaseWord..end]; this is the time before it.
let timelineBaseMs = 0;
// Pending full-timeline seek target during a progress-bar drag (committed on release).
let pendingSeekMs = 0;
// Latest full-timeline total (ms), so a mid-listen re-render can keep the bar
// stable instead of briefly collapsing to the resume point.
let lastFullDurMs = 0;
// Generation id stamped on each tts_speak. Events from a superseded generation
// (the still-running old render after a speed/voice change) are ignored so their
// stale timings can't corrupt the new fragment's word map.
let genId = 0;

const playerBar = document.getElementById('tts-player');
const positionFill = document.getElementById('tts-position-fill');
const bufferFill = document.getElementById('tts-buffer-fill');
const seekThumb = document.getElementById('tts-seek-thumb');
const timeCurrent = document.getElementById('tts-time-current');
const timeTotal = document.getElementById('tts-time-total');
const playPauseBtn = document.getElementById('tts-play-pause');
const progressTrack = document.getElementById('tts-progress-track');
const cacheSegmentsEl = document.getElementById('tts-cache-segments');
const speedBtn = document.getElementById('tts-speed-btn');
// Disk-cache coverage for the open article at the current voice+speed: merged
// [startMs,endMs] blocks already synthesized, painted on the bar as buffered so
// what's instantly playable is visible before (and during) playback. cacheTotalMs
// is the article timeline those ranges sit on (real for cached segments, a
// word-rate estimate for the rest); it doubles as the duration when fully cached.
let cacheRanges = [];
let cacheTotalMs = 0;
let cacheStatusGen = -1; // last genId we refreshed coverage for (refresh once per gen_done)
const PLAYER_PAD = '8rem'; // bar measures ~117.5px with the grab handle; 128px keeps clearance

function fmtTime(ms) {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

// ── Per-voice duration calibration ──
//
// SPEAK_WPM below is one flat rate for every voice, but voices differ audibly
// in pace. Rather than a per-voice WPM table (which would duplicate the pause
// math per voice), a single multiplicative ratio is learned per voice from
// real measured playback (maybeLearnVoiceCal) and applied on top of the flat
// estimate, so the existing pause terms stay intact and nothing double-counts.
function loadVoiceCal() {
  try {
    const cal = JSON.parse(localStorage.getItem('verba-voice-cal') || '{}');
    return (cal && typeof cal === 'object') ? cal : {};
  } catch (_) {
    return {};
  }
}

function saveVoiceCal(cal) {
  try { localStorage.setItem('verba-voice-cal', JSON.stringify(cal)); } catch (_) { /* storage unavailable */ }
}

// Calibration key for the currently active voice — mirrors how loadCacheStatus
// derives sid from ttsVoice (a custom voice has no numeric sid, so it's keyed
// by name instead).
function activeVoiceCalKey() {
  const voiceVal = ttsVoice || '0';
  if (voiceVal.startsWith('custom:')) return `custom:${voiceVal.slice(7)}`;
  return `${ttsModelId}#${parseInt(voiceVal, 10) || 0}`;
}

function activeVoiceCalRatio() {
  const entry = loadVoiceCal()[activeVoiceCalKey()];
  return (entry && typeof entry.ratio === 'number') ? entry.ratio : 1;
}

// Pure EMA + clamp step, factored out from maybeLearnVoiceCal so it can be
// unit-tested headlessly. `prev` is the voice's existing {ratio, n} entry, or
// undefined/null for a never-calibrated voice.
function nextVoiceCalEntry(prev, observedRatio) {
  const n = prev ? prev.n : 0;
  const ratio = n === 0 ? observedRatio : 0.7 * prev.ratio + 0.3 * observedRatio;
  return { ratio: Math.min(2.0, Math.max(0.5, ratio)), n: n + 1 };
}

// Estimated spoken duration of `text` at a given speed, BEFORE per-voice
// calibration. Two parts: speech time scales with speed (length_scale ~
// 1/speed), but the silence spliced at punctuation (see piper.rs) is added in
// PCM at fixed lengths and does NOT scale with speed. Keeping them separate is
// what makes the estimate hold up at slow speeds (the old word-count/speed
// estimate doubled the pauses too). Approximate — used for the library
// time-left and the ready-state bar before generation. Calibrated against a
// real article (950 words, 0.75x measured 5:40): Piper reads faster than a
// naive ~165 wpm, and the spliced pauses overlap/collapse so they count for
// less than their raw splice lengths. These are rough — the real duration is
// measured and saved after the first full play (itemDurationMs). Exposed
// raw (uncalibrated) so maybeLearnVoiceCal can compare a real measurement
// against this baseline without the calibration ratio feeding back into itself.
const SPEAK_WPM = 255;
function estDurationMsForTextRaw(text, speed) {
  if (!text || !text.trim()) return 0;
  const words = text.trim().split(/\s+/).filter(Boolean).length;
  const spokenMs = words / SPEAK_WPM * 60000 / (speed || 1);
  const sentences = (text.match(/[.!?…]+/g) || []).length;
  const clauses = (text.match(/[,;:–—]/g) || []).length;
  const paragraphs = (text.match(/\n+/g) || []).length;
  const pauseMs = sentences * 300 + clauses * 150 + paragraphs * 500;
  return Math.round(spokenMs + pauseMs);
}

// Calibrated estimate used by every other call site: the raw estimate scaled
// by the active voice's learned ratio (default 1 until it has measurements).
function estDurationMsForText(text, speed) {
  return Math.round(estDurationMsForTextRaw(text, speed) * activeVoiceCalRatio());
}

// Word-count-only variant of estDurationMsForTextRaw, for a book chapter row:
// only ChapterMeta.words is available there (the chapter body isn't loaded
// just to render the list), so there's no punctuation to estimate pauses from.
function estMsForWordsRaw(words, speed) {
  if (!words) return 0;
  return Math.round(words / SPEAK_WPM * 60000 / (speed || 1));
}

function estMsForWords(words, speed) {
  return Math.round(estMsForWordsRaw(words, speed) * activeVoiceCalRatio());
}

// Real measured duration once the article has been generated (saved on the
// item), rescaled to the requested speed; falls back to the estimate for
// never-played items. Exact at the speed it was measured at.
function itemDurationMs(item, speed) {
  if (!item) return 0;
  if (item.duration_ms > 0 && item.duration_speed > 0) {
    return Math.round(item.duration_ms * item.duration_speed / (speed || 1));
  }
  return estDurationMsForText(item.body, speed);
}

function fmtMins(ms) {
  const mins = Math.round(ms / 60000);
  if (mins <= 0) return '<1 min';
  if (mins < 60) return `${mins} min`;
  const h = Math.floor(mins / 60), m = mins % 60;
  return m ? `${h}h ${m}m` : `${h}h`;
}

function showPlayerBar() {
  playerBar.classList.remove('translate-y-full');
  positionPlayerBar();
  document.querySelectorAll('.tab-panel').forEach(p => { p.style.paddingBottom = PLAYER_PAD; });
}

function hidePlayerBar() {
  playerBar.classList.add('translate-y-full');
  positionPlayerBar();
  document.querySelectorAll('.tab-panel').forEach(p => { p.style.paddingBottom = ''; });
}

// Keep the fixed player bar just above the bottom nav when BOTH are shown, so
// the mini-player and tab bar stack (Spotify-style) without overlapping. When
// the player is hidden or the nav is a detail view, it drops to the edge so
// translate-y-full slides it fully off-screen. Height is measured so safe-area
// insets never cause overlap.
function positionPlayerBar() {
  const navVisible = !bottomNav.classList.contains('hidden');
  const playerShown = !playerBar.classList.contains('translate-y-full');
  const lift = navVisible && playerShown;
  playerBar.style.bottom = lift ? `${bottomNav.offsetHeight}px` : '0px';
  playerBar.style.paddingBottom = lift ? '0px' : 'env(safe-area-inset-bottom)';
}

function resetTtsUI() {
  // The player bar is the only transport now; returning to a not-started state
  // means the next play press starts (resume or from 0) rather than pausing.
  ttsStarted = false;
  setTtsLoading(false);
  closeNowPlaying();
}

// Enable/disable the player-bar play control based on voice readiness.
function setPlayEnabled(enabled) {
  playPauseBtn.disabled = !enabled;
  playPauseBtn.classList.toggle('opacity-40', !enabled);
  playPauseBtn.classList.toggle('pointer-events-none', !enabled);
}

function updatePlayerBar(st) {
  ttsState = st;
  // Full-text timeline: this fragment sits at timelineBaseMs, so add it to the
  // player-relative position/buffer to get absolute values.
  const fullPos = timelineBaseMs + st.position_ms;
  const fullBuf = timelineBaseMs + (st.buffered_ms || 0);
  // Total duration: while still generating, hold the stable estimate (with the
  // buffered/played amount as a floor) so the number doesn't jump around as
  // chunks arrive; once generation is done, the buffered amount IS the real
  // duration. This keeps the library, the ready bar, and playback consistent.
  const fullDur = st.gen_done
    ? Math.max(fullBuf, fullPos, 1)
    : Math.max(articleEstMs, fullBuf, fullPos, 1);
  lastFullDurMs = fullDur;
  const posPct = Math.min(100, (fullPos / fullDur) * 100);
  const bufPct = Math.min(100, (fullBuf / fullDur) * 100);
  positionFill.style.width = `${posPct}%`;
  bufferFill.style.width = `${bufPct}%`;
  seekThumb.style.left = `${posPct}%`;
  seekThumb.style.opacity = '1';
  timeCurrent.textContent = fmtTime(fullPos);
  timeTotal.textContent = fmtTime(fullDur);
  const icon = playPauseBtn.querySelector('.material-symbols-outlined');
  // Spinner while generating the new render or rebuffering (and not paused), e.g.
  // right after a speed/voice change or a seek past the buffer — so it reads as
  // loading rather than stuck or paused.
  // !! is load-bearing: when st has no `rebuffering` field (the openReading and
  // startSpeak literals) and ttsLoading is false, the && chain short-circuits to
  // `undefined`, not false. classList.toggle('tts-spin', undefined) FLIPS the
  // class (Chromium treats an explicit undefined force as "toggle"), which spun
  // the play button on every article open. Coerce to a real boolean.
  const buffering = !!(((ttsLoading && spinnerArmed) || st.rebuffering) && !st.paused && !st.finished);
  icon.classList.toggle('tts-spin', buffering);
  if (st.finished) {
    icon.textContent = 'replay';
  } else if (buffering) {
    icon.textContent = 'progress_activity';
  } else {
    icon.textContent = st.paused ? 'play_arrow' : 'pause';
  }
  // Mirror onto the full-screen now-playing view when it's open — same
  // numbers, bigger face.
  if (npOpen) {
    document.getElementById('np-position-fill').style.width = `${posPct}%`;
    document.getElementById('np-buffer-fill').style.width = `${bufPct}%`;
    document.getElementById('np-seek-thumb').style.left = `${posPct}%`;
    document.getElementById('np-time-current').textContent = fmtTime(fullPos);
    document.getElementById('np-time-total').textContent = fmtTime(fullDur);
    document.getElementById('np-play-icon').textContent = icon.textContent;
    document.getElementById('np-play-icon').classList.toggle('tts-spin', buffering);
  }
  renderCacheSegments();
}

// Paint the cached blocks as a buffered underlay, positioned over the same total
// the bar displays (lastFullDurMs == cacheTotalMs once coverage is loaded), so a
// fully-cached article reads as fully buffered and partial caches show as gaps.
function renderCacheSegments() {
  if (!cacheSegmentsEl) return;
  const total = lastFullDurMs || cacheTotalMs || articleEstMs || 1;
  cacheSegmentsEl.replaceChildren();
  for (const r of cacheRanges) {
    const s = Math.max(0, r[0]);
    const e = Math.min(total, r[1]);
    if (e <= s) continue;
    const div = document.createElement('div');
    div.className = 'absolute top-0 h-full bg-primary/20 pointer-events-none';
    div.style.left = `${(s / total) * 100}%`;
    div.style.width = `${((e - s) / total) * 100}%`;
    cacheSegmentsEl.appendChild(div);
  }
}

// Ask the backend which segments of the open article are already on disk for the
// current voice+speed, and reflect it on the bar before any playback. When the
// whole article is cached, its summed real duration is the true length, so adopt
// it as the displayed total (fixes the bar showing an estimate on reopen).
async function loadCacheStatus() {
  cacheRanges = [];
  cacheTotalMs = 0;
  if (!readingItem || !ttsModelId || !readingText) { renderCacheSegments(); return; }
  const voiceVal = ttsVoice || '0';
  const sid = voiceVal.startsWith('custom:') ? 0 : (parseInt(voiceVal) || 0);
  const text = readingText;
  try {
    const st = await invoke('tts_cache_status', { id: ttsModelId, sid, speed: ttsSpeed, text });
    if (readingText !== text) return; // article/voice changed while awaiting
    if (!st || !st.supported) { renderCacheSegments(); return; }
    cacheRanges = Array.isArray(st.ranges) ? st.ranges : [];
    cacheTotalMs = st.total_ms || 0;
    // Only a FULLY cached article may set the displayed total: then cacheTotalMs
    // is the exact real duration at this speed. A partial (or empty) coverage
    // total is a real+estimate hybrid and must NOT overwrite the duration the
    // library list shows (itemDurationMs: the saved measured length, or estimate)
    // — doing so was loading a wrong duration on open. Partial coverage only
    // paints its cached blocks; the total stays whatever openReading chose.
    if (st.all_cached && cacheTotalMs > 0) {
      articleEstMs = cacheTotalMs;
      if (!ttsStarted) {
        updatePlayerBar({ position_ms: 0, buffered_ms: 0, gen_done: false, paused: true, finished: false });
      }
    }
    renderCacheSegments();
  } catch (_) { renderCacheSegments(); }
}

listen('tts-position', (event) => {
  if (event.payload.gen !== genId) return;
  liveBufferedMs = event.payload.buffered_ms || 0;
  // Playback has begun once we get a non-rebuffering event; stop the spinner.
  if (!event.payload.rebuffering) setTtsLoading(false);
  refineDuration();
  if (!ttsSeeking) updatePlayerBar(event.payload);
  showPlayerBar();
  highlightAt(timelineBaseMs + event.payload.position_ms);
  if (!event.payload.paused) saveProgress(false);
  maybeSaveMeasuredDuration(event.payload);
  maybeLearnVoiceCal(event.payload);
  // Generation just cached more of the article — refresh coverage once per gen.
  if (event.payload.gen_done && cacheStatusGen !== genId) {
    cacheStatusGen = genId;
    loadCacheStatus();
  }
});

// Once a full-article play finishes generating, the buffered amount IS the true
// duration at this speed. Save it so the library + ready bar show the real
// length next time instead of the estimate. Only a play from the top (genBaseWord
// 0) measures the whole article; resumes generate just the remainder.
function maybeSaveMeasuredDuration(p) {
  if (bookState) return; // duration_ms is whole-item semantics; books use word estimates
  if (!p.gen_done || genBaseWord !== 0 || !readingItem) return;
  if (measuredGenId === genId) return;
  const dur = p.buffered_ms;
  if (!dur || dur < 1000) return;
  measuredGenId = genId;
  readingItem.duration_ms = dur;
  readingItem.duration_speed = ttsSpeed;
  invoke('library_set_duration', { id: readingItem.id, durationMs: dur, speed: ttsSpeed }).catch(() => {});
}

// Learn how the active voice's real pace compares to the flat SPEAK_WPM
// estimate, across both articles and book chapters — unlike
// maybeSaveMeasuredDuration (which only saves whole-article durations and
// early-returns for books), this must fire for chapters too. Same "full play"
// guard (genBaseWord 0, gen_done), sitting next to that function rather than
// inside it since it can't share its books early-return.
function maybeLearnVoiceCal(p) {
  if (!p.gen_done || genBaseWord !== 0 || !readingText) return;
  if (calibratedGenId === genId) return;
  const dur = p.buffered_ms;
  if (!dur || dur < 5000) return; // too short a sample to be signal
  calibratedGenId = genId;
  const estRaw = estDurationMsForTextRaw(readingText, ttsSpeed);
  if (!estRaw) return;
  const key = activeVoiceCalKey();
  const cal = loadVoiceCal();
  cal[key] = nextVoiceCalEntry(cal[key], dur / estRaw);
  saveVoiceCal(cal);
}

// While generating (and only for items without a measured duration), extrapolate
// the real per-character rate of the audio produced so far to the rest of the
// fragment. Updates the displayed total only when generation crosses the next
// progress milestone (25/50/75%), so it changes a few discrete times rather than
// continuously. Driven by the timing map (a single monotonic source).
function refineDuration() {
  // Never override a saved true duration with a live estimate; nothing left once
  // all milestones have fired.
  if (readingItem && readingItem.duration_ms > 0) return;
  if (dynMilestoneIdx >= REFINE_MILESTONES.length) return;
  if (!readingText || timingCursor <= genBaseWord) return;
  const lastWord = timingCursor - 1;
  if (!readingWords[lastWord] || !readingWords[genBaseWord] || !wordTimes[lastWord]) return;
  const fragStartChar = readingWords[genBaseWord].start;
  const genChars = readingWords[lastWord].end - fragStartChar;
  const fragChars = readingText.length - fragStartChar;
  if (fragChars <= 0) return;
  const frac = genChars / fragChars;
  if (frac < REFINE_MILESTONES[dynMilestoneIdx]) return;
  // Crossed at least one milestone — advance past any we've passed and refine once.
  while (dynMilestoneIdx < REFINE_MILESTONES.length && frac >= REFINE_MILESTONES[dynMilestoneIdx]) {
    dynMilestoneIdx++;
  }
  const genMs = wordTimes[lastWord].e - timelineBaseMs; // fragment-relative audio so far
  if (genMs < 1000) return;
  articleEstMs = Math.round(timelineBaseMs + genMs * fragChars / genChars);
}

listen('tts-timing', (event) => {
  if (event.payload.gen !== genId) return;
  const { start_ms, duration_ms, text, word_ms } = event.payload;
  const words = (text || '').trim().split(/\s+/).filter(Boolean);
  const n = words.length;
  if (n === 0) return;
  // Anchor this segment to readingWords by matching its first word near the
  // running cursor. The backend splits at punctuation, so a token like
  // "death,4" becomes two backend words where the reader has one — without
  // re-anchoring, each such miscount accumulates into a growing highlight drift.
  timingCursor = anchorTiming(words, timingCursor);
  // Exact per-word durations from the model (w_ceil) when present and aligned;
  // otherwise distribute the segment duration by word character length.
  let durs;
  if (Array.isArray(word_ms) && word_ms.length === n) {
    durs = word_ms;
  } else {
    let totalLen = 0;
    const lens = words.map(w => { const l = w.length || 1; totalLen += l; return l; });
    durs = lens.map(l => duration_ms * (l / totalLen));
  }
  let acc = start_ms;
  for (let k = 0; k < n; k++) {
    // Absolute timeline position = fragment base + offset within the fragment.
    wordTimes[timingCursor + k] = { s: timelineBaseMs + acc, e: timelineBaseMs + acc + durs[k] };
    acc += durs[k];
  }
  timingCursor += n;
});

// Loose equality for anchoring: the timing spans carry piper's NORMALIZED text
// (numbers spelled out, dashes rewritten, "Dr." expanded), while readingWords
// holds the raw article. Case and leading/trailing punctuation must not block
// a re-anchor, or number-heavy text never resynchronizes after a divergence.
function anchorKey(w) {
  return w.toLowerCase().replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, '');
}

// Find where a segment's words line up in readingWords, searching outward from
// the expected cursor. Returns the matched start index (or the cursor unchanged
// if no confident match), so per-segment word-count mismatches self-correct
// each segment instead of accumulating. R must comfortably exceed the raw-vs-
// normalized word-count divergence a single segment can produce (a date-heavy
// sentence expands ~1 raw token into 5+ spoken words); the old 12 let drift
// outrun the search on number-dense articles, leaving the highlight stuck.
function anchorTiming(words, cursor) {
  const R = 40;
  const k0 = anchorKey(words[0]);
  if (!k0) return cursor; // punctuation-only token can't anchor
  for (let d = 0; d <= R; d++) {
    for (const cand of (d === 0 ? [cursor] : [cursor + d, cursor - d])) {
      if (cand < genBaseWord || cand >= readingWords.length || !readingWords[cand]) continue;
      // Match the first word; for a 1-word segment also require a non-ambiguous
      // hit by checking the next word, to avoid latching onto a common word.
      if (anchorKey(readingWords[cand].text) === k0 &&
          (words.length < 2 || !readingWords[cand + 1] ||
           anchorKey(readingWords[cand + 1].text) === anchorKey(words[1]))) {
        return cand;
      }
    }
  }
  return cursor;
}

listen('tts-finished', async (event) => {
  if (event.payload && event.payload.gen !== genId) return;
  // A chapter's tts-finished always means chapter end (every startSpeak
  // slices to the end of readingText) — auto-advance into the next chapter,
  // or mark the book done on its last one.
  if (bookState) {
    // The chapter actually played to its end — record it as completed. This
    // is the ONLY place chapters get marked done; navigation never does.
    invoke('book_chapter_completed', { id: bookState.id, chapter: bookState.current }).catch(() => {});
    if (readingItem && Array.isArray(readingItem.completed) && !readingItem.completed.includes(bookState.current)) {
      readingItem.completed.push(bookState.current);
    }
    const next = bookState.current + 1;
    if (next < bookState.chapters.length) {
      // readingItem is still the book's item (openBookChapter set it), so its
      // title carries over; only current_chapter/progress need to reflect the
      // chapter that just finished.
      const item = {
        id: bookState.id,
        title: readingItem ? readingItem.title : '',
        chapters: bookState.chapters,
        current_chapter: bookState.current,
        progress: 0,
      };
      await openBookChapter(item, next, { autoplay: true });
    } else {
      if (readingItem) {
        readingItem.progress = readingText.length;
        readingItem.current_chapter = bookState.current;
      }
      invoke('book_set_position', { id: bookState.id, chapter: bookState.current, offset: readingText.length }).catch(() => {});
      resetTtsUI();
      // The book has nothing left to advance into — hand off to the queue.
      await playNextInQueue();
    }
    return;
  }
  // Mark the item fully read (100%) and clear the resume point.
  if (readingItem) {
    readingItem.progress = readingText.length;
    resumeWord = 0;
    invoke('library_set_progress', { id: readingItem.id, progress: readingText.length }).catch(() => {});
  }
  resetTtsUI();
  // The article just finished — hand off to whatever's queued next.
  await playNextInQueue();
});

playPauseBtn.addEventListener('click', () => {
  if (playPauseBtn.disabled) return;
  if (!ttsStarted) {
    // Article loaded but not yet playing: start generating + playing, resuming
    // from the saved position or from the top.
    startPlaybackFromResume();
  } else if (ttsState.finished) {
    ttsState.finished = false;
    // Replay from the very start of the article.
    if (timelineBaseMs === 0) {
      invoke('tts_seek', { positionMs: 0 });
      invoke('tts_resume');
    } else {
      startSpeak(readingText, 0, 0);
    }
  } else if (ttsState.paused) {
    // Voice changed while paused: tear down the old (paused) generation, then
    // re-synthesize from here under the new voice instead of resuming the old
    // audio. Stopping first prevents the old worker racing tts_load's engine
    // swap (same hazard as the mid-play switch).
    if (voiceDirty) {
      const r = currentResumePoint();
      if (r) {
        ttsState.paused = false;
        invoke('tts_stop').finally(() => startSpeak(r.text, r.word, r.ms));
        return;
      }
    }
    invoke('tts_resume');
  } else {
    invoke('tts_pause');
    saveProgress(true);
  }
});

// Begin playback for the open article: resume from the saved word once if there
// is one, else start from the top. Used by the player-bar play button.
function startPlaybackFromResume() {
  if (resumeWord > 0 && readingWords[resumeWord]) {
    const w = resumeWord;
    resumeWord = 0;
    startSpeak(readingText.slice(readingWords[w].start), w, timelineBaseMs);
  } else {
    startSpeak(readingText, 0, 0);
  }
}

document.getElementById('tts-skip-back').addEventListener('click', () => {
  seekToFull(timelineBaseMs + ttsState.position_ms - 10000);
});

document.getElementById('tts-skip-fwd').addEventListener('click', () => {
  seekToFull(timelineBaseMs + ttsState.position_ms + 10000);
});

// ── Full-screen now-playing ──
//
// A bigger face on the same clock: transport clicks delegate to the bar's
// own buttons and the display mirrors updatePlayerBar's numbers, so there
// is exactly one playback state machine. Opened from the bar's grab
// handle; the back gesture closes it before anything else.
const nowPlaying = document.getElementById('now-playing');
let npOpen = false;

// Title + artwork for the expanded view. Called on open AND whenever the
// open item changes underneath it (book chapter auto-advance) — the display
// title is the composite header text, but the gradient/monogram seed is the
// ITEM title so the placeholder matches the same book's tile everywhere
// else in the app.
function refreshNowPlayingMeta() {
  const headerTitle = document.getElementById('reading-title').textContent || '';
  const seed = (readingItem && readingItem.title) || headerTitle;
  document.getElementById('np-title').textContent = headerTitle;
  const cover = document.getElementById('np-cover');
  cover.className = `${coverClass(seed)} w-36 h-36 rounded-2xl text-4xl shadow-xl relative overflow-hidden`;
  cover.innerHTML = `<span>${escapeHtml(coverMonogram(seed))}</span>`;
  if (readingItem) {
    if (readingItem.image_url) {
      cover.insertAdjacentHTML('beforeend',
        `<img src="${escapeHtml(readingItem.image_url)}" class="absolute inset-0 w-full h-full object-cover" onerror="this.remove()">`);
    } else if (readingItem.chapters && readingItem.chapters.length) {
      const id = readingItem.id;
      localCoverUrl(id).then(url => {
        if (url && npOpen && readingItem && readingItem.id === id && !cover.querySelector('img')) {
          cover.insertAdjacentHTML('beforeend',
            `<img src="${url}" class="absolute inset-0 w-full h-full object-cover" onerror="this.remove()">`);
        }
      });
    }
  }
}

function openNowPlaying() {
  refreshNowPlayingMeta();
  document.getElementById('np-speed').textContent = speedBtn.textContent;
  document.getElementById('np-voice-label').textContent =
    document.getElementById('tts-voice-btn-label').textContent;
  npOpen = true;
  nowPlaying.classList.remove('hidden');
  nowPlaying.classList.add('flex', 'sheet-anim');
  renderNpExcerpt();
  updatePlayerBar(ttsState);
}

function closeNowPlaying() {
  npOpen = false;
  nowPlaying.classList.add('hidden');
  nowPlaying.classList.remove('flex', 'sheet-anim');
}

// The sentence being spoken, active word highlighted, as centered context.
function renderNpExcerpt() {
  if (!npOpen) return;
  const el = document.getElementById('np-excerpt');
  if (activeWord < 0 || !readingWords.length) { el.textContent = ''; return; }
  const from = Math.max(0, activeWord - 9);
  const to = Math.min(readingWords.length, activeWord + 11);
  let html = from > 0 ? '… ' : '';
  for (let i = from; i < to; i++) {
    const w = escapeHtml(readingWords[i].text);
    html += (i === activeWord ? `<span class="text-on-surface font-semibold">${w}</span>` : w) + ' ';
  }
  if (to < readingWords.length) html += '…';
  el.innerHTML = html;
}

document.getElementById('tts-expand').addEventListener('click', openNowPlaying);
document.getElementById('np-collapse').addEventListener('click', closeNowPlaying);
document.getElementById('np-play-pause').addEventListener('click', () => playPauseBtn.click());
document.getElementById('np-skip-back').addEventListener('click', () => document.getElementById('tts-skip-back').click());
document.getElementById('np-skip-fwd').addEventListener('click', () => document.getElementById('tts-skip-fwd').click());
document.getElementById('np-voice').addEventListener('click', openVoiceSheet);
document.getElementById('np-speed').addEventListener('click', openSpeedSheet);

// Tap or drag to seek on the big track, mapped against the displayed total
// exactly like the bar's own track.
(() => {
  const track = document.getElementById('np-track');
  const preview = (e) => {
    const rect = track.getBoundingClientRect();
    const pct = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    pendingSeekMs = Math.floor(pct * (lastFullDurMs || 1));
    const displayPct = pct * 100;
    document.getElementById('np-position-fill').style.width = `${displayPct}%`;
    document.getElementById('np-seek-thumb').style.left = `${displayPct}%`;
    document.getElementById('np-time-current').textContent = fmtTime(pendingSeekMs);
  };
  track.addEventListener('pointerdown', (e) => {
    e.preventDefault(); // suppress compatibility mouse events on top of the source gate
    ttsSeeking = true;
    seekSource = 'np';
    track.setPointerCapture(e.pointerId);
    preview(e);
  });
  track.addEventListener('pointermove', (e) => { if (seekSource === 'np') preview(e); });
  track.addEventListener('pointerup', () => {
    if (seekSource !== 'np') return;
    ttsSeeking = false;
    seekSource = null;
    seekToFull(pendingSeekMs);
  });
})();

function seekFromPointer(e) {
  const rect = progressTrack.getBoundingClientRect();
  const x = (e.touches ? e.touches[0].clientX : e.clientX) - rect.left;
  const pct = Math.max(0, Math.min(1, x / rect.width));
  // Map against the displayed total (held estimate or real), so the drag lands
  // where the bar shows — not the raw backend duration.
  const fullDur = lastFullDurMs || 1;
  // Defer the actual seek to release: a drag across the fragment start could
  // otherwise trigger repeated re-renders. Track the target and preview only.
  pendingSeekMs = Math.floor(pct * fullDur);
  const displayPct = (pendingSeekMs / fullDur) * 100;
  positionFill.style.width = `${displayPct}%`;
  seekThumb.style.left = `${displayPct}%`;
  timeCurrent.textContent = fmtTime(pendingSeekMs);
}

// Which track owns the live drag: the now-playing track's pointer events
// spawn compatibility mouse/touch events that bubble to the document
// handlers below — gating them on the SOURCE (not just "a drag is live")
// keeps them from recomputing the seek against the hidden mini-bar's rect
// and corrupting pendingSeekMs mid-drag.
let seekSource = null; // 'bar' | 'np' | null
progressTrack.addEventListener('mousedown', (e) => { ttsSeeking = true; seekSource = 'bar'; seekFromPointer(e); });
progressTrack.addEventListener('touchstart', (e) => { ttsSeeking = true; seekSource = 'bar'; seekFromPointer(e); }, { passive: true });
document.addEventListener('mousemove', (e) => { if (seekSource === 'bar') seekFromPointer(e); });
document.addEventListener('touchmove', (e) => { if (seekSource === 'bar') seekFromPointer(e); }, { passive: true });
document.addEventListener('mouseup', () => { if (seekSource === 'bar') { ttsSeeking = false; seekSource = null; seekToFull(pendingSeekMs); } });
document.addEventListener('touchend', () => { if (seekSource === 'bar') { ttsSeeking = false; seekSource = null; seekToFull(pendingSeekMs); } });

// ── Speed sheet ──

const speedSheet = document.getElementById('speed-sheet');
const speedSlider = document.getElementById('tts-speed-slider');
const speedSliderLabel = document.getElementById('speed-slider-label');
const speedPresets = document.querySelectorAll('#speed-presets .speed-chip');

function openSpeedSheet() {
  speedSheet.classList.remove('hidden');
  speedSlider.value = ttsSpeed;
  speedSliderLabel.textContent = ttsSpeed.toFixed(ttsSpeed % 1 === 0 ? 1 : 2) + 'x';
  updateSpeedChips();
}

function closeSpeedSheet() {
  speedSheet.classList.add('hidden');
}

function setTtsSpeed(val) {
  ttsSpeed = Math.round(val * 100) / 100;
  const label = ttsSpeed % 1 === 0 ? ttsSpeed.toFixed(1) : String(ttsSpeed);
  speedBtn.textContent = label + 'x';
  document.getElementById('np-speed').textContent = label + 'x';
  speedSlider.value = ttsSpeed;
  speedSliderLabel.textContent = label + 'x';
  updateSpeedChips();
}

// Save the current speed against the active voice so reselecting that voice
// restores it. Fire-and-forget; persistVoicePrefs is async.
function rememberSpeedForVoice() {
  ttsVoiceSpeeds[ttsVoice] = ttsSpeed;
  persistVoicePrefs();
  // Cache coverage is per speed — refresh what's shown as buffered.
  if (readingItem) loadCacheStatus();
}

function updateSpeedChips() {
  speedPresets.forEach(chip => {
    const v = parseFloat(chip.dataset.speed);
    if (Math.abs(v - ttsSpeed) < 0.01) {
      chip.className = 'speed-chip flex-1 py-2.5 rounded-xl text-xs font-semibold transition-colors cursor-pointer bg-primary text-on-primary';
    } else {
      chip.className = 'speed-chip flex-1 py-2.5 rounded-xl text-xs font-semibold transition-colors cursor-pointer bg-surface-container-highest text-on-surface-variant';
    }
  });
}

speedBtn.addEventListener('click', openSpeedSheet);
document.getElementById('speed-sheet-overlay').addEventListener('click', closeSpeedSheet);

// Speed is applied at generation time (Piper length_scale), so a change can't
// retro-edit already-buffered audio. The word timing map lets playback continue
// from the current word at the new speed/voice instead of restarting.

// Synthesize `text`, mapping its words onto the reading view starting at word
// index `baseWord` (0 for a fresh play, the resume point on speed/voice change).
async function startSpeak(text, baseWord, baseMs) {
  if (!text || !text.trim()) { showToast('Nothing to read'); return; }
  if (!ttsModelId) { showToast('No voice available'); return; }

  const myGen = ++genId;
  ttsStarted = true;
  voiceDirty = false; // this synth reflects the current voice
  setTtsLoading(true); // generating/pre-buffering until the first audio plays
  autoFollow = true;
  dynMilestoneIdx = 0;

  const voiceVal = ttsVoice || '0';
  const isCustom = voiceVal.startsWith('custom:');
  const sid = isCustom ? 0 : parseInt(voiceVal);

  liveBufferedMs = 0; // new fragment: nothing generated yet
  // Position this fragment on the full-text timeline. A fresh play (baseWord 0)
  // resets everything; a resume/seek keeps prior word timings so seeking back
  // into already-rendered text can still locate its position.
  genBaseWord = baseWord;
  timingCursor = baseWord;
  timelineBaseMs = baseMs;
  if (baseWord === 0) {
    wordTimes = {};
    activeWord = -1;
  } else {
    // Drop timings from the resume point onward; they get re-rendered (maybe at a
    // new speed). Keep earlier words for backward-seek lookup. Leaving stale
    // ahead-of-frontier timings makes the highlight jump ahead of the speech.
    for (const k in wordTimes) {
      if (Number(k) >= baseWord) delete wordTimes[k];
    }
  }

  // Show the bar in a starting state up front, before the voice load, so the
  // buffering spinner (not a toast) covers the first-run load as well as the
  // generate/pre-buffer gap. The 350ms delay-gate keeps a cached, instant load
  // from flashing anything. A seek-while-paused re-render renders paused, so no
  // spinner shows then.
  const keepPaused = renderStartPaused;
  renderStartPaused = false;
  showPlayerBar();
  // Recompute the full-article duration at the current speed (measured if known,
  // else estimated); updatePlayerBar holds this as the total until generation
  // completes, so the bar stays stable (and consistent with the library).
  articleEstMs = itemDurationMs(readingItem, ttsSpeed) || estDurationMsForText(readingText, ttsSpeed);
  updatePlayerBar({ position_ms: 0, buffered_ms: 0, gen_done: false, paused: keepPaused, finished: false });

  // Try playing straight from the persistent cache first: if every segment this
  // text needs is already on disk, this starts playback with no engine load at
  // all (the ONNX session load is the multi-second wait the spinner otherwise
  // covers). Backend re-verifies coverage itself on the exact text, so a stale
  // guess here just costs one extra round trip, not correctness. Falls through
  // to the normal load+speak path unchanged on a miss.
  try {
    const cached = await invoke('tts_speak_cached', {
      id: ttsModelId,
      customVoice: isCustom ? voiceVal.slice(7) : null,
      text, speed: ttsSpeed, sid, gen: myGen,
    });
    if (cached) {
      if (keepPaused) {
        setTtsLoading(false);
        ttsState.paused = true;
        await invoke('tts_pause');
        updatePlayerBar(ttsState);
      }
      return;
    }
  } catch (_) {
    // Cache-only attempt errored rather than just missing (backend already
    // logs it) — fall through to the normal path below rather than failing the
    // whole play.
  }

  // Lazy-load the voice at generation time, no manual load step. First load can
  // take a couple seconds; the buffering spinner covers it (no toast). A custom
  // voice loads the engine each time; a built-in voice loads once.
  try {
    if (isCustom) {
      await invoke('tts_load', { id: ttsModelId, customVoice: voiceVal.slice(7) });
      ttsLoadedModelId = ttsModelId;
    } else if (!ttsLoadedModelId) {
      await invoke('tts_load', { id: ttsModelId });
      ttsLoadedModelId = ttsModelId;
    }
  } catch (err) {
    showToast((isCustom ? 'Failed to load custom voice: ' : 'Voice load failed: ') + err);
    resetTtsUI();
    updatePlayerBar({ position_ms: 0, buffered_ms: 0, gen_done: false, paused: true, finished: false });
    return;
  }

  try {
    await invoke('tts_speak', { text, speed: ttsSpeed, sid, gen: myGen });
    // Re-render started while paused (a seek): pause the new player before it
    // pre-buffers so it stays paused at the new spot instead of auto-playing.
    if (keepPaused) {
      setTtsLoading(false);
      ttsState.paused = true;
      await invoke('tts_pause');
      updatePlayerBar(ttsState);
    }
  } catch (err) {
    showToast('TTS error: ' + err);
    resetTtsUI();
    hidePlayerBar();
  }
}

// True when audio is actively playing (bar shown, not paused/finished).
function isPlaying() {
  return !ttsState.finished && !ttsState.paused
    && !playerBar.classList.contains('translate-y-full');
}

// Where playback currently is, as a {text, word, ms} startSpeak can resume
// from. Derived from the PLAYBACK position, not the highlight, so it's valid
// even when highlighting hasn't caught up. Null when there's nothing to
// resume. Capture this BEFORE any voice/model switch that might reset state.
function currentResumePoint() {
  if (!readingText) return null;
  const curMs = timelineBaseMs + ttsState.position_ms;
  let w = wordAtTime(curMs);
  if (w == null) w = genBaseWord;
  if (!readingWords[w]) return null;
  const remaining = readingText.slice(readingWords[w].start);
  if (!remaining.trim()) return null;
  return { text: remaining, word: w, ms: wordTimes[w] ? wordTimes[w].s : curMs };
}

// On a speed or voice change mid-listen, continue from the current word at the
// new setting rather than restarting from the top.
async function resumeFromCurrentWord() {
  if (!isPlaying()) return;
  const r = currentResumePoint();
  if (r) await startSpeak(r.text, r.word, r.ms);
}

speedPresets.forEach(chip => {
  chip.addEventListener('click', () => {
    setTtsSpeed(parseFloat(chip.dataset.speed));
    rememberSpeedForVoice();
    closeSpeedSheet();
    resumeFromCurrentWord();
  });
});

// `input` updates the label live during the drag; `change` (release) commits,
// saves the speed for the current voice, and applies it to playback.
speedSlider.addEventListener('input', () => {
  setTtsSpeed(parseFloat(speedSlider.value));
});
speedSlider.addEventListener('change', () => {
  setTtsSpeed(parseFloat(speedSlider.value));
  rememberSpeedForVoice();
  resumeFromCurrentWord();
});

// ── Listen: library + reading view ──

function showPanel(id) {
  document.querySelectorAll('.tab-panel').forEach(p => p.classList.add('hidden'));
  const el = document.getElementById(id);
  if (el) el.classList.remove('hidden');
  // Android text-selection "Report mispronunciation" only makes sense over
  // the reading view's own text.
  invoke('set_report_menu_enabled', { enabled: id === 'reading' }).catch(() => {});
}

function tokenizeWords(text) {
  const words = [];
  const re = /\S+/g;
  let m;
  while ((m = re.exec(text)) !== null) {
    words.push({ text: m[0], start: m.index, end: m.index + m[0].length });
  }
  return words;
}

function renderReading(text) {
  readingWords = tokenizeWords(text);
  const el = document.getElementById('reading-text');
  let html = '';
  let cursor = 0;
  readingWords.forEach((w, i) => {
    html += escapeHtml(text.slice(cursor, w.start));
    html += `<span data-w="${i}" class="rw">${escapeHtml(w.text)}</span>`;
    cursor = w.end;
  });
  html += escapeHtml(text.slice(cursor));
  el.innerHTML = html;
}

function setActiveWord(i) {
  if (i === activeWord) return;
  const el = document.getElementById('reading-text');
  if (!el) return;
  const prev = el.querySelector('.rw-active');
  if (prev) prev.classList.remove('rw-active');
  activeWord = i;
  const span = el.querySelector(`[data-w="${i}"]`);
  renderNpExcerpt();
  if (span) {
    span.classList.add('rw-active');
    // Don't fight a manual scroll. If following is off but the highlight is back
    // in view (the user scrolled to it, or playback caught up), resume following
    // without yanking this frame; otherwise only scroll while following.
    if (!autoFollow) {
      if (wordInReadingView(span)) autoFollow = true;
    } else {
      span.scrollIntoView({ block: 'center', behavior: 'smooth' });
    }
  }
}

function wordInReadingView(span) {
  const el = document.getElementById('reading-text');
  if (!el) return false;
  const c = el.getBoundingClientRect();
  const r = span.getBoundingClientRect();
  return r.bottom > c.top && r.top < c.bottom;
}

// A manual scroll (wheel/touch drag — not our programmatic scrollIntoView) turns
// off auto-follow so the user can read elsewhere without being yanked back.
(function () {
  const el = document.getElementById('reading-text');
  if (!el) return;
  const stop = () => { autoFollow = false; };
  el.addEventListener('wheel', stop, { passive: true });
  el.addEventListener('touchmove', stop, { passive: true });
})();

function highlightAt(pos) {
  // Highlight the last rendered word whose start time is at/before `pos`. Scan
  // to the generation frontier (don't break early) so a gap or slight timing
  // drift can't leave the highlight permanently stuck on one word.
  let found = -1;
  for (let i = genBaseWord; i < readingWords.length; i++) {
    const t = wordTimes[i];
    if (!t) break;          // beyond the generation frontier
    if (t.s <= pos) found = i;
  }
  if (found >= 0) setActiveWord(found);
}

// Find the last rendered word whose absolute start is at/before `ms`. Skips
// gaps (continue, not break) so it still works when resuming mid-article, where
// words before the resume point have no timings yet.
function wordAtTime(ms) {
  let best = null;
  for (let i = 0; i < readingWords.length; i++) {
    const t = wordTimes[i];
    if (!t) continue;
    if (t.s <= ms) best = i; else break;
  }
  return best;
}

// Seek to a full-timeline target. If the target is inside the already-generated
// region of the current fragment, it's an instant cursor move. Otherwise — a
// backward seek before the fragment, OR a forward seek past the generation
// frontier — re-render from the word at that point so generation produces from
// there and playback starts in ~a second (rather than snapping to the frontier
// or waiting for sequential generation to grind up to the target).
function seekToFull(targetMs) {
  targetMs = Math.max(0, targetMs);
  const fragT = targetMs - timelineBaseMs;
  if (fragT >= 0 && fragT <= liveBufferedMs) {
    // Inside generated audio: instant cursor move.
    invoke('tts_seek', { positionMs: Math.floor(fragT) });
    return;
  }
  if (!readingText) return;
  // Ungenerated target -> re-render from there. Prefer a known timing (seeking
  // back into already-heard text); otherwise estimate the word from the target's
  // position in the article (the un-generated prefix on resume, or anything past
  // the frontier, has no timing yet).
  let w = (fragT >= 0) ? null : wordAtTime(targetMs);
  let baseMs = (w != null && wordTimes[w]) ? wordTimes[w].s : targetMs;
  if (w == null) {
    const frac = articleEstMs > 0 ? Math.min(1, Math.max(0, targetMs / articleEstMs)) : 0;
    w = wordIndexAtChar(Math.floor(frac * readingText.length));
    baseMs = targetMs;
  }
  if (!readingWords[w]) return;
  // Keep the paused state across a seek that has to re-render.
  renderStartPaused = ttsState.paused;
  startSpeak(readingText.slice(readingWords[w].start), w, baseMs);
}

// Jump playback to a tapped word. Keyed on the word index (no ms<->word
// guessing): start from it when idle, instant-seek when it's already generated,
// else re-render from it (exact — generation begins at that word).
function jumpToWord(i) {
  if (!readingWords[i] || !ttsReady) return;
  autoFollow = true;
  setActiveWord(i); // immediate feedback while audio (re)starts
  const estMs = readingText.length ? (readingWords[i].start / readingText.length) * articleEstMs : 0;
  const baseMs = wordTimes[i] ? wordTimes[i].s : estMs;
  if (!ttsStarted) {
    startSpeak(readingText.slice(readingWords[i].start), i, baseMs);
    return;
  }
  const fragT = baseMs - timelineBaseMs;
  if (wordTimes[i] && fragT >= 0 && fragT <= liveBufferedMs) {
    invoke('tts_seek', { positionMs: Math.floor(fragT) }); // already generated
  } else {
    startSpeak(readingText.slice(readingWords[i].start), i, baseMs);
  }
}

// ── Reading progress (resume + library percentage) ──

// Char offset of the word currently being read — the stable resume anchor and
// the basis for the library percentage. Derived from playback position, falling
// back to the highlighted word then the fragment start.
function currentReadingChar() {
  if (!readingWords.length) return 0;
  const curMs = timelineBaseMs + (ttsState.position_ms || 0);
  let w = wordAtTime(curMs);
  if (w == null) w = activeWord >= 0 ? activeWord : genBaseWord;
  if (w < 0 || !readingWords[w]) return 0;
  return readingWords[w].start;
}

// First word at/after a character offset, for resuming at a saved position.
function wordIndexAtChar(offset) {
  if (offset <= 0) return 0;
  for (let i = 0; i < readingWords.length; i++) {
    if (readingWords[i].start >= offset) return i;
  }
  return Math.max(0, readingWords.length - 1);
}

// Persist reading progress (a char offset into the body) for the open item.
// Throttled to ~4s unless `force` (pause, back, dismiss, finish).
function saveProgress(force) {
  if (!readingItem) return;
  const now = Date.now();
  if (!force && now - lastProgressSaveMs < 4000) return;
  lastProgressSaveMs = now;
  const offset = currentReadingChar();
  readingItem.progress = offset;
  if (bookState) {
    invoke('book_set_position', { id: bookState.id, chapter: bookState.current, offset }).catch(() => {});
  } else {
    invoke('library_set_progress', { id: readingItem.id, progress: offset }).catch(() => {});
  }
}

// A book's overall read fraction, word-weighted across chapters (chapter
// bodies aren't loaded in the list view, only their ChapterMeta word/char
// counts): chapters that actually played to the end (it.completed) count
// fully, the current one by its character-offset progress within itself.
// Chapters merely jumped past count nothing — navigation must not fabricate
// listening history.
function bookProgress(it) {
  const totalWords = it.chapters.reduce((sum, c) => sum + c.words, 0);
  const done = new Set(it.completed || []);
  let wordsRead = 0;
  it.chapters.forEach((c, i) => { if (done.has(i)) wordsRead += c.words; });
  const cur = Math.min(it.current_chapter, it.chapters.length - 1);
  const curChapter = it.chapters[cur];
  if (curChapter && !done.has(cur) && curChapter.chars) {
    wordsRead += Math.min(1, it.progress / curChapter.chars) * curChapter.words;
  }
  return { totalWords, wordsRead, pct: totalWords ? Math.round(wordsRead / totalWords * 100) : 0 };
}

// Selected Library tab ('articles' | 'books'). The fetched items (and feed
// titles used for the source badge) are cached in module vars so switching
// tabs re-renders instantly without hitting the backend again.
let libTab = 'articles';
let libItems = [];
let libFeedsById = {};

const libThumb = document.getElementById('lib-thumb');

function setLibTab(tab) {
  libTab = tab;
  if (libThumb) libThumb.style.transform = tab === 'books' ? 'translateX(100%)' : 'translateX(0)';
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
let libActionTarget = null; // { id, chapter } — chapter null unless opened from a book chapter row

function openLibActionSheet(item, isBook) {
  libActionTarget = { id: item.id, chapter: null };
  document.getElementById('lib-action-title').textContent = item.title;
  const actions = isBook
    ? [
        { action: 'mark-read', icon: 'check_circle', label: 'Mark as read' },
        { action: 'mark-unread', icon: 'radio_button_unchecked', label: 'Mark as unread' },
        { action: 'add-queue', icon: 'playlist_add', label: 'Add to queue' },
        { action: 'delete', icon: 'delete', label: 'Delete', danger: true },
      ]
    : [
        { action: 'reset-progress', icon: 'restart_alt', label: 'Reset progress' },
        { action: 'add-queue', icon: 'playlist_add', label: 'Add to queue' },
        { action: 'delete', icon: 'delete', label: 'Delete', danger: true },
      ];
  document.getElementById('lib-action-list').innerHTML = actions.map(a =>
    `<button class="more-item w-full flex items-center gap-3 px-4 py-3 rounded-xl text-left cursor-pointer hover:bg-surface-container-highest ${a.danger ? 'text-error' : 'text-on-surface'}" data-action="${a.action}">
      <span class="material-symbols-outlined text-lg ${a.danger ? 'text-error/80' : 'text-on-surface-variant'}">${a.icon}</span>
      <span class="text-sm">${a.label}</span>
    </button>`).join('');
  libActionSheet.classList.remove('hidden');
}

// One-action variant of openLibActionSheet for a single book chapter row
// (long-pressed from openBook's chapter list): only "Add to queue" applies,
// keyed to this specific chapter rather than the book as a whole.
function openChapterActionSheet(item, idx) {
  libActionTarget = { id: item.id, chapter: idx };
  const ch = item.chapters[idx];
  document.getElementById('lib-action-title').textContent = ch.title || `Chapter ${idx + 1}`;
  document.getElementById('lib-action-list').innerHTML =
    `<button class="more-item w-full flex items-center gap-3 px-4 py-3 rounded-xl text-left cursor-pointer hover:bg-surface-container-highest text-on-surface" data-action="add-queue">
      <span class="material-symbols-outlined text-lg text-on-surface-variant">playlist_add</span>
      <span class="text-sm">Add to queue</span>
    </button>`;
  libActionSheet.classList.remove('hidden');
}
function closeLibActionSheet() { libActionSheet.classList.add('hidden'); }
document.getElementById('lib-action-overlay').addEventListener('click', closeLibActionSheet);
document.getElementById('lib-action-list').addEventListener('click', async (e) => {
  const btn = e.target.closest('.more-item');
  if (!btn || !libActionTarget) return;
  const { id, chapter } = libActionTarget;
  const action = btn.dataset.action;
  closeLibActionSheet();
  if (action === 'reset-progress') await invoke('library_set_progress', { id, progress: 0 });
  else if (action === 'mark-read') await invoke('book_set_read', { id, read: true });
  else if (action === 'mark-unread') await invoke('book_set_read', { id, read: false });
  else if (action === 'add-queue') addToQueue(id, chapter);
  else if (action === 'delete') { await deleteLibraryItem(id); return; }
  loadLibrary();
});

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
// a real image layers on top when the item has one — remote article lead
// images immediately (onerror peels the img off, exposing the gradient),
// stored EPUB covers asynchronously via hydrateLocalCovers().
function coverTileHtml(it, sizeClasses, textClass) {
  const isBook = !!(it.chapters && it.chapters.length);
  let img = '';
  if (!isBook && it.image_url) {
    img = `<img src="${escapeHtml(it.image_url)}" class="absolute inset-0 w-full h-full object-cover" loading="lazy" onerror="this.remove()">`;
  }
  const hydrate = isBook ? ` data-cover-book="${escapeHtml(it.id)}"` : '';
  return `<div class="${coverClass(it.title)} ${sizeClasses} ${textClass} relative"${hydrate}><span>${escapeHtml(coverMonogram(it.title))}</span>${img}</div>`;
}

// Stored covers (EPUBs) fetched once per item and patched in after render.
const localCoverCache = new Map(); // id -> data URL | null
async function localCoverUrl(id) {
  if (!localCoverCache.has(id)) {
    let url = null;
    try { url = await invoke('library_cover', { id }); } catch (_) { /* none */ }
    localCoverCache.set(id, url);
  }
  return localCoverCache.get(id);
}

function hydrateLocalCovers(root) {
  root.querySelectorAll('[data-cover-book]').forEach(el => {
    localCoverUrl(el.dataset.coverBook).then(url => {
      if (url && el.isConnected && !el.querySelector('img')) {
        el.insertAdjacentHTML('beforeend',
          `<img src="${url}" class="absolute inset-0 w-full h-full object-cover" onerror="this.remove()">`);
      }
    });
  });
}

function coverMonogram(title) {
  const words = (title || '').trim().split(/\s+/).filter(w => /[\p{L}\p{N}]/u.test(w));
  const letter = (w) => (w.match(/[\p{L}\p{N}]/u) || ['?'])[0];
  if (!words.length) return '?';
  return words.length === 1 ? letter(words[0]) : letter(words[0]) + letter(words[1]);
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
    ? '<span class="material-symbols-outlined text-lg text-primary shrink-0">check_circle</span>'
    : '';
  const progressEdgeThick = (pct) => (pct > 0 && pct < 100)
    ? `<div class="absolute left-0 right-0 bottom-0 h-[4px] bg-primary/15"><div class="progress-edge-fill h-full bg-primary/80 rounded-r-full" style="width:${pct}%"></div></div>`
    : '';
  const rowShell = (id, inner, pct, i) => `
    <div class="lib-item stagger-in relative overflow-hidden flex items-center justify-between gap-3 bg-surface-container rounded-xl px-3.5 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-id="${escapeHtml(id)}" style="--i:${i}">
      ${inner}
      ${doneTick(pct)}
      ${progressEdge(pct)}
    </div>`;
  // Books render as a cover grid, not rows: covers give the shelf a spine
  // to scan, and a two-column grid earns the vertical space a book (a
  // long-lived item) deserves.
  const renderBookCard = (it, i) => {
    const { totalWords, wordsRead, pct } = bookProgress(it);
    const meta = pct >= 100 ? 'Finished'
      : pct > 0 ? `${fmtMins(estMsForWords(totalWords - wordsRead, ttsSpeed))} left`
      : `${it.chapters.length} chapters · ${fmtMins(estMsForWords(totalWords, ttsSpeed))}`;
    const tick = pct >= 100
      ? '<span class="material-symbols-outlined absolute top-2 right-2 text-xl text-white/90 drop-shadow">check_circle</span>'
      : '';
    return `
    <div class="lib-item stagger-in cursor-pointer" data-id="${escapeHtml(it.id)}" style="--i:${i}">
      <div class="relative overflow-hidden rounded-xl aspect-[3/4] ${coverClass(it.title)}" data-cover-book="${escapeHtml(it.id)}">
        <span class="text-4xl opacity-90">${escapeHtml(coverMonogram(it.title))}</span>
        <div class="absolute inset-x-0 bottom-0 pt-10 pb-2.5 px-3 bg-gradient-to-t from-black/70 to-transparent">
          <p class="normal-case text-[13px] font-semibold leading-tight text-white line-clamp-2">${escapeHtml(it.title)}</p>
        </div>
        ${tick}
        ${progressEdgeThick(pct)}
      </div>
      <p class="text-xs text-on-surface-variant tabular-nums mt-1.5 px-0.5 truncate">${meta}</p>
    </div>`;
  };
  const renderRow = (it, i) => {
    const len = (it.body || '').length || 1;
    const prog = Math.max(0, Math.min(len, it.progress || 0));
    const pct = Math.round(prog / len * 100);
    // Real measured duration if we have it, else an estimate; at the current
    // playback speed so the list matches the reader.
    const totalMs = itemDurationMs(it, ttsSpeed);
    const leftMs = totalMs * (1 - prog / len);
    // The edge line carries the fraction; words only say what time remains.
    let meta = pct >= 100 ? 'Finished'
      : pct > 0 ? `${fmtMins(leftMs)} left`
      : fmtMins(totalMs);
    // Publication date, when the source provided one.
    const pub = fmtPubDate(it.published);
    if (pub) meta += ` · ${pub}`;
    // Feed provenance: source badge (feed title, else hostname once the feed
    // is deleted) and a NEW chip until the article is first opened.
    if (it.feed_id) {
      const feed = feedsById[it.feed_id];
      let src = feed ? feed.title : '';
      if (!src && it.url) { try { src = new URL(it.url).hostname; } catch (_) {} }
      if (src) meta += ` · ${escapeHtml(src)}`;
    }
    const newChip = (it.feed_id && !it.seen && !(it.progress > 0))
      ? '<span class="inline-block align-[2px] text-[10px] font-semibold text-primary bg-primary/10 rounded-full px-2 py-0.5 mr-1.5">NEW</span>'
      : '';
    return rowShell(it.id, `
      ${coverTileHtml(it, 'w-11 h-11 rounded-lg self-start mt-0.5', 'text-sm')}
      <div class="min-w-0 flex-1">
        <div class="text-[15px] font-semibold leading-snug text-on-surface line-clamp-2">${newChip}${escapeHtml(it.title)}</div>
        <div class="text-xs text-on-surface-variant truncate mt-0.5">${escapeHtml(it.body.slice(0, 80))}</div>
        <div class="text-xs text-on-surface-variant tabular-nums mt-0.5">${meta}</div>
      </div>`, pct, i);
  };
  const rev = items.slice().reverse();
  const isBook = (it) => !!(it.chapters && it.chapters.length);
  // One kind at a time (Articles/Books pill) — no section headers.
  const shown = rev.filter(it => isBook(it) === (libTab === 'books'));
  empty.classList.toggle('hidden', shown.length > 0);
  document.getElementById('lib-empty-title').textContent = libTab === 'books' ? 'No books yet' : 'No saved texts yet';
  if (libTab === 'books') {
    list.className = 'grid grid-cols-2 gap-3';
    list.innerHTML = shown.map(renderBookCard).join('');
  } else {
    list.className = 'space-y-2';
    list.innerHTML = shown.map(renderRow).join('');
  }
  hydrateLocalCovers(list);
  const bookIds = new Set(items.filter(isBook).map(it => it.id));
  list.querySelectorAll('.lib-item').forEach(el => {
    const id = el.dataset.id;
    el.addEventListener('click', (e) => {
      if (el._longPressed) { el._longPressed = false; return; }
      if (bookIds.has(id)) openBook(id); else openReading(id);
    });
    attachLongPress(el, () => {
      const item = items.find(it => it.id === id);
      if (item) openLibActionSheet(item, bookIds.has(id));
    });
  });
}

// Delete a library item (article or book), invoked from the long-press
// sheet. A book's audio is keyed off its chapter bodies, which
// library_delete also removes (books/<id>.json) — forget the cache first or
// it leaks.
async function deleteLibraryItem(id) {
  const item = libItems.find(it => it.id === id);
  const isBook = !!(item && item.chapters && item.chapters.length);
  const ok = await showConfirm(`Delete "${item ? item.title : 'this item'}"?`);
  if (!ok) return;
  if (isBook && ttsModelId) {
    const voiceVal = ttsVoice || '0';
    const sid = voiceVal.startsWith('custom:') ? 0 : (parseInt(voiceVal) || 0);
    await invoke('book_forget_audio', { id, modelId: ttsModelId, sid, speed: ttsSpeed }).catch(() => {});
  }
  await invoke('library_delete', { id });
  localCoverCache.delete(id);
  loadLibrary();
}

// ── Playlist queue ──
//
// Device-local queue of articles/chapters to auto-play once the current item
// finishes (see playNextInQueue, hooked into the tts-finished listener).
// localStorage-only, never synced. An entry is {id, chapter}: chapter null
// queues an article, or a book from wherever it currently is; chapter is a
// chapter index to queue one specific chapter of a book.
function loadQueue() {
  try {
    const q = JSON.parse(localStorage.getItem('verba-queue') || '[]');
    return Array.isArray(q) ? q : [];
  } catch (_) {
    return [];
  }
}

function saveQueue(q) {
  try { localStorage.setItem('verba-queue', JSON.stringify(q)); } catch (_) { /* storage unavailable */ }
}

// Whether finishing an item auto-plays the next queued one. Defaults on;
// only an explicit '0' (the user flipped the switch) turns it off.
function loadQueueAutoplay() {
  try {
    return localStorage.getItem('verba-queue-autoplay') !== '0';
  } catch (_) {
    return true;
  }
}

function saveQueueAutoplay(on) {
  try { localStorage.setItem('verba-queue-autoplay', on ? '1' : '0'); } catch (_) { /* storage unavailable */ }
}

let queueAutoplay = loadQueueAutoplay();

function renderQueueBadge() {
  const badge = document.getElementById('queue-badge');
  if (!badge) return;
  const n = loadQueue().length;
  badge.textContent = String(n);
  badge.classList.toggle('hidden', n === 0);
}

// Queue one item/chapter. Refuses a duplicate id+chapter pair rather than
// queuing the same thing twice.
function addToQueue(id, chapter) {
  const q = loadQueue();
  if (q.some(e => e.id === id && e.chapter === chapter)) {
    showToast('Already in queue');
    return;
  }
  q.push({ id, chapter });
  saveQueue(q);
  renderQueueBadge();
  showToast(`Added to queue (${q.length})`);
}

// Display title for a queue row: the cached library item's title (from A1's
// libItems), plus the chapter title for a specific-chapter entry. A queued
// item can be deleted before its turn comes up — still shown (and removable)
// so the sheet doesn't just silently drop it.
function queueEntryLabel(entry) {
  const item = libItems.find(it => it.id === entry.id);
  if (!item) return 'Deleted item';
  if (entry.chapter == null) return item.title;
  const ch = item.chapters && item.chapters[entry.chapter];
  const chTitle = ch ? (ch.title || `Chapter ${entry.chapter + 1}`) : `Chapter ${entry.chapter + 1}`;
  return `${item.title} — ${chTitle}`;
}

const queueSheet = document.getElementById('queue-sheet');

function renderQueueSheet() {
  const q = loadQueue();
  const list = document.getElementById('queue-sheet-list');
  if (!q.length) {
    list.innerHTML = '<p class="text-sm text-on-surface-variant px-3 py-6 text-center">Queue is empty</p>';
    return;
  }
  const last = q.length - 1;
  list.innerHTML = q.map((entry, i) => {
    // A queued entry can outlive its library item (deleted before its turn
    // comes up); fall back to a plain '?' tile rather than skip the row.
    const item = libItems.find(it => it.id === entry.id);
    const tile = item
      ? coverTileHtml(item, 'w-10 h-10 rounded-lg', 'text-xs')
      : `<div class="${coverClass('?')} w-10 h-10 rounded-lg text-xs">?</div>`;
    return `
    <div class="stagger-in flex items-center gap-2.5 px-3 py-2.5" data-i="${i}" style="--i:${i}">
      ${tile}
      <div class="flex flex-col shrink-0 -my-1">
        <button class="queue-row-up p-0.5 text-on-surface-variant hover:text-primary transition-colors cursor-pointer disabled:opacity-25 disabled:pointer-events-none" data-i="${i}" ${i === 0 ? 'disabled' : ''}>
          <span class="material-symbols-outlined text-base">keyboard_arrow_up</span>
        </button>
        <button class="queue-row-down p-0.5 text-on-surface-variant hover:text-primary transition-colors cursor-pointer disabled:opacity-25 disabled:pointer-events-none" data-i="${i}" ${i === last ? 'disabled' : ''}>
          <span class="material-symbols-outlined text-base">keyboard_arrow_down</span>
        </button>
      </div>
      <button class="queue-row-play min-w-0 flex-1 text-left text-sm font-medium text-on-surface truncate cursor-pointer hover:text-primary transition-colors">${escapeHtml(queueEntryLabel(entry))}</button>
      <button class="queue-row-remove shrink-0 text-on-surface-variant hover:text-error transition-colors p-1 cursor-pointer" data-i="${i}">
        <span class="material-symbols-outlined text-lg">close</span>
      </button>
    </div>`;
  }).join('')
    + '<button id="queue-clear-btn" class="w-full text-xs font-semibold text-on-surface-variant hover:text-error transition-colors px-4 py-3 mt-1 rounded-xl hover:bg-error/10 cursor-pointer">Clear queue</button>';
  hydrateLocalCovers(list);
}

function openQueueSheet() {
  renderQueueSheet();
  const autoplayCb = document.getElementById('queue-autoplay');
  if (autoplayCb) autoplayCb.checked = queueAutoplay;
  queueSheet.classList.remove('hidden');
}
function closeQueueSheet() { queueSheet.classList.add('hidden'); }

document.getElementById('queue-btn').addEventListener('click', openQueueSheet);
document.getElementById('queue-sheet-overlay').addEventListener('click', closeQueueSheet);
document.getElementById('queue-autoplay').addEventListener('change', (e) => {
  queueAutoplay = e.target.checked;
  saveQueueAutoplay(queueAutoplay);
});
document.getElementById('queue-sheet-list').addEventListener('click', (e) => {
  if (e.target.closest('#queue-clear-btn')) {
    saveQueue([]);
    renderQueueBadge();
    renderQueueSheet();
    return;
  }
  const upBtn = e.target.closest('.queue-row-up');
  if (upBtn) {
    const i = Number(upBtn.dataset.i);
    const q = loadQueue();
    if (i > 0) { [q[i - 1], q[i]] = [q[i], q[i - 1]]; saveQueue(q); renderQueueSheet(); }
    return;
  }
  const downBtn = e.target.closest('.queue-row-down');
  if (downBtn) {
    const i = Number(downBtn.dataset.i);
    const q = loadQueue();
    if (i < q.length - 1) { [q[i + 1], q[i]] = [q[i], q[i + 1]]; saveQueue(q); renderQueueSheet(); }
    return;
  }
  const removeBtn = e.target.closest('.queue-row-remove');
  if (removeBtn) {
    const q = loadQueue();
    q.splice(Number(removeBtn.dataset.i), 1);
    saveQueue(q);
    renderQueueBadge();
    renderQueueSheet();
    return;
  }
  const playBtn = e.target.closest('.queue-row-play');
  if (playBtn) {
    const i = Number(playBtn.closest('[data-i]').dataset.i);
    const q = loadQueue();
    const [entry] = q.splice(i, 1);
    saveQueue(q);
    renderQueueBadge();
    closeQueueSheet();
    if (entry) {
      playQueueEntry(entry).then(ok => { if (!ok) showToast('Could not play — item may have been deleted'); });
    }
  }
});
renderQueueBadge();

// Play one queue entry: article -> openReading (resumes where it left off);
// book, any chapter -> openBookChapter for that chapter (chapter null means
// "wherever the book currently is"). Resolves fresh via library_get rather
// than trusting the libItems cache, since the item may have changed (or been
// deleted) since it was queued. Returns false when the item is gone.
async function playQueueEntry(entry) {
  let item;
  try { item = await invoke('library_get', { id: entry.id }); } catch (_) { item = null; }
  if (!item) return false;
  const isBook = item.chapters && item.chapters.length > 0;
  if (!isBook) {
    await openReading(entry.id, { autoplay: true });
  } else if (entry.chapter == null) {
    await openBookChapter(item, item.current_chapter, { autoplay: true });
  } else {
    await openBookChapter(item, entry.chapter, { autoplay: true });
  }
  return true;
}

// Auto-advance hand-off once the current item has nothing left to play (an
// article finishing, or a book's LAST chapter finishing — the mid-book
// chapter-to-chapter advance never reaches here, it has its own path). Pops
// entries until one actually plays, skipping any that fail to resolve (e.g. a
// queued item deleted before its turn), until the queue is empty.
async function playNextInQueue() {
  if (!queueAutoplay) return;
  const q = loadQueue();
  while (q.length) {
    const entry = q.shift();
    saveQueue(q);
    renderQueueBadge();
    if (await playQueueEntry(entry)) return;
  }
}

// Shared Readability pipeline: parse HTML, strip site chrome and footnote
// markers, extract the readable article. Used by the share-import flow and the
// feed importer. Returns {title, body} or null when nothing readable is found.
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
  // Drop semantic chrome up front. Some sites (e.g. a table-based masthead
  // inside the same wrapper as the article) confuse Readability into
  // climbing past the <article> and keeping the site header/nav; removing
  // these landmarks first keeps it on the actual content.
  doc.querySelectorAll('header, footer, nav, aside').forEach(n => n.remove());
  // Strip footnote/reference markers: the little superscript numbers linking
  // to notes end up glued to a word ("column1") and get read aloud as a
  // stray number. Covers the common markdown/Hugo/Pandoc/MediaWiki markup,
  // plus any bare superscript that only wraps an in-page anchor. Also drop
  // the endnotes section itself (orphaned once the markers are gone).
  doc.querySelectorAll('sup[id^="fnref"], sup.reference, a.footnote-ref, [role="doc-noteref"], .footnote-ref').forEach(n => n.remove());
  doc.querySelectorAll('sup > a[href^="#"]').forEach(a => a.parentElement && a.parentElement.remove());
  doc.querySelectorAll('.footnotes, section.footnotes, [role="doc-endnotes"], ol.references').forEach(n => n.remove());
  const article = (typeof Readability !== 'undefined')
    ? new Readability(doc).parse()
    : null;
  if (article && article.textContent && article.textContent.trim().length > 50) {
    return {
      title: (article.title || '').trim(),
      body: article.textContent.trim(),
      published: (article.publishedTime || '').trim(),
      imageUrl,
    };
  }
  return null;
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
// text is saved as-is. Either way it lands in the library and opens in Listen,
// ready to play.
let importingShared = false;
async function importSharedText(text) {
  if (importingShared) return; // ignore a duplicate trigger mid-import
  const raw = (text || '').trim();
  if (!raw) return;
  const url = (raw.match(/https?:\/\/[^\s]+/) || [])[0];
  importingShared = true;
  try {
    let title = null;
    let body = raw;
    let published = null;
    let imageUrl = null;
    if (url) {
      showToast('Fetching article…');
      let html;
      try {
        html = await fetchArticleHtml(url);
      } catch (err) {
        showToast('Could not fetch: ' + err);
        return;
      }
      try {
        const article = extractArticle(html, url);
        if (article) {
          title = article.title || url;
          body = article.body;
          published = article.published || null;
          imageUrl = article.imageUrl || null;
        } else {
          showToast('No readable article found');
          return;
        }
      } catch (err) {
        showToast('Parse failed: ' + err);
        return;
      }
    }
    let item;
    try {
      item = await invoke('library_add', { title, body, url: url || null, published, imageUrl });
    } catch (err) {
      showToast('Save failed: ' + err);
      return;
    }
    setMode('listen');
    await loadLibrary();
    if (item && item.id) openReading(item.id);
    showToast(url ? 'Article added' : 'Text added');
  } finally {
    importingShared = false;
  }
}

// A live TTS session from a previously open item must die before a reading
// view rebinds the transport state (genId++, wordTimes reset). Left alive, its
// events carry the old gen — filtered, so the bar and highlight go dead while
// its audio keeps playing — and the next play layers a second generation over
// it. Reachable via share-import landing mid-listen (and any future path that
// leaves reading without the back button). Awaited so the backend stop lands
// before any new speak can race it.
async function stopLiveSession() {
  if (!ttsStarted) return;
  ttsStarted = false;
  try { await invoke('tts_stop'); } catch (_) { /* nothing playing */ }
}

async function openReading(id, { autoplay = false } = {}) {
  bookState = null; // opening a plain article, not continuing a book chapter
  let item;
  try { item = await invoke('library_get', { id }); }
  catch (err) { showToast('Open failed: ' + err); return; }
  if (!item) { showToast('Text not found'); return; }
  await stopLiveSession();
  invoke('library_mark_seen', { id }).catch(() => {});
  readingItem = item;
  readingText = item.body;
  activeWord = -1; genBaseWord = 0; timingCursor = 0; wordTimes = {};
  document.getElementById('reading-title').textContent = item.title;
  invoke('media_set_title', { title: item.title }).catch(() => {});
  if (npOpen) refreshNowPlayingMeta();
  renderReading(readingText);
  showPanel('reading');
  setBottomNavVisible(false); // detail view: hide the tab bar, player bar to edge

  // Resume point: a part-read item (progress between 0 and the end) reopens at
  // that word and highlights it; a finished or fresh item starts at the top.
  const prog = item.progress || 0;
  resumeWord = (prog > 0 && prog < readingText.length) ? wordIndexAtChar(prog) : 0;
  autoFollow = true;
  if (resumeWord > 0) setActiveWord(resumeWord);

  // Show the player bar in a ready (paused, not started) state, positioned at
  // the resume point. timelineBaseMs is an estimate of the skipped prefix so the
  // bar reflects the saved progress before generation; pressing play continues
  // from there (startSpeak reuses this base), keeping the bar continuous.
  // Fully reset transport state first: a previously-played article can leave
  // ttsLoading true (with a pending spinner timer) or an in-flight tts-position
  // event still tagged with the old genId, either of which would flip the ready
  // button into the spinning buffering state before play is ever pressed.
  ttsStarted = false;
  setTtsLoading(false);
  genId++;
  articleEstMs = itemDurationMs(item, ttsSpeed);
  const frac = readingText.length ? Math.min(1, prog / readingText.length) : 0;
  timelineBaseMs = articleEstMs > 0 ? frac * articleEstMs : 0;
  showPlayerBar();
  updatePlayerBar({ position_ms: 0, buffered_ms: 0, gen_done: false, paused: true, finished: false });

  await updateTtsPanel();
  // Reflect what's already cached for this voice+speed on the bar (and adopt the
  // real duration if fully cached) before any play. Fire-and-forget.
  loadCacheStatus();
  // Queue/auto-advance hand-off: start exactly like the player-bar play button
  // would for a not-yet-started item (resume point if there is one, else top).
  if (autoplay) startPlaybackFromResume();
}

// Open a book's chapter list (from a Library row tap).
async function openBook(id) {
  let item;
  try { item = await invoke('library_get', { id }); }
  catch (err) { showToast('Open failed: ' + err); return; }
  if (!item) { showToast('Book not found'); return; }
  document.getElementById('book-chapters-title').textContent = item.title;
  showPanel('book-chapters');
  setBottomNavVisible(false); // detail view

  const listEl = document.getElementById('book-chapters-list');
  const completed = new Set(item.completed || []);
  listEl.innerHTML = item.chapters.map((ch, idx) => {
    // Done = actually played to the end, never inferred from position:
    // jumping into chapter 8 must not tick off 1-7.
    const done = completed.has(idx);
    const current = idx === item.current_chapter;
    const currentChip = current
      ? '<span class="text-[10px] font-semibold text-primary bg-primary/10 rounded px-1.5 py-0.5">CURRENT</span>'
      : '';
    const doneIcon = done
      ? '<span class="material-symbols-outlined text-lg text-primary shrink-0">check_circle</span>'
      : '';
    // Per-chapter completion, drawn as the same bottom-edge progress line the
    // library cards use (ch.chars comes from ChapterMeta, so no chapter body
    // needs loading just to render this list). Finished chapters carry the
    // tick alone; chapters merely jumped past show nothing.
    let pctNum = 0;
    if (done) pctNum = 100;
    else if (current && ch.chars > 0) pctNum = Math.round(Math.min(1, item.progress / ch.chars) * 100);
    const edge = (pctNum > 0 && pctNum < 100)
      ? `<div class="absolute left-0 right-0 bottom-0 h-[3px] bg-primary/15"><div class="h-full bg-primary/70 rounded-r-full" style="width:${pctNum}%"></div></div>`
      : '';
    return `
    <div class="lib-item book-chapter-row relative overflow-hidden flex items-center justify-between gap-3 bg-surface-container rounded-xl px-4 py-3.5 cursor-pointer hover:bg-surface-container-high transition-colors" data-idx="${idx}">
      <div class="min-w-0 flex-1">
        <div class="text-[15px] font-semibold leading-snug text-on-surface truncate">${idx + 1}. ${escapeHtml(ch.title || `Chapter ${idx + 1}`)}</div>
        <div class="flex items-center justify-between mt-1">
          <span class="text-xs text-on-surface-variant tabular-nums">${fmtMins(estMsForWords(ch.words, ttsSpeed))}</span>
          ${currentChip}
        </div>
      </div>
      ${doneIcon}
      ${edge}
    </div>`;
  }).join('');
  listEl.querySelectorAll('.book-chapter-row').forEach(el => {
    el.addEventListener('click', () => {
      if (el._longPressed) { el._longPressed = false; return; }
      openBookChapter(item, Number(el.dataset.idx));
    });
    attachLongPress(el, () => openChapterActionSheet(item, Number(el.dataset.idx)));
  });
}

document.getElementById('book-chapters-back').addEventListener('click', () => {
  navigateTo('library');
});

// Open one chapter of a book: fetch its body and wire up the reading view
// (mirrors openReading). `item` is the book's LibraryItem (chapters + the
// current_chapter/progress it had BEFORE this pick); `idx` the chapter to
// open. A forward pick (idx > current_chapter) records the new position and
// trims audio for chapters now more than one behind; a backward pick only
// records the position (never trims — see book_trim_audio's cache-key note).
async function openBookChapter(item, idx, { autoplay = false } = {}) {
  let body;
  try { body = await invoke('book_chapter', { id: item.id, idx }); }
  catch (err) { showToast('Could not open chapter: ' + err); return; }
  await stopLiveSession();

  const forward = idx > item.current_chapter;
  const changedChapter = idx !== item.current_chapter;
  bookState = { id: item.id, chapters: item.chapters, current: idx };
  readingItem = item;
  readingText = body;
  activeWord = -1; genBaseWord = 0; timingCursor = 0; wordTimes = {};
  const chapterTitle = item.chapters[idx].title || `Chapter ${idx + 1}`;
  const headerTitle = `${item.title} — ${chapterTitle}`;
  document.getElementById('reading-title').textContent = headerTitle;
  invoke('media_set_title', { title: headerTitle }).catch(() => {});
  if (npOpen) refreshNowPlayingMeta();
  renderReading(readingText);
  showPanel('reading');
  setBottomNavVisible(false); // detail view: hide the tab bar, player bar to edge

  // Resume point: only the chapter the book was actually left on carries a
  // saved offset; any other pick (forward or backward) starts at its top.
  const prog = changedChapter ? 0 : (item.progress || 0);
  resumeWord = (prog > 0 && prog < readingText.length) ? wordIndexAtChar(prog) : 0;
  autoFollow = true;
  if (resumeWord > 0) setActiveWord(resumeWord);

  ttsStarted = false;
  setTtsLoading(false);
  genId++;
  articleEstMs = estDurationMsForText(readingText, ttsSpeed);
  const frac = readingText.length ? Math.min(1, prog / readingText.length) : 0;
  timelineBaseMs = articleEstMs > 0 ? frac * articleEstMs : 0;
  showPlayerBar();
  updatePlayerBar({ position_ms: 0, buffered_ms: 0, gen_done: false, paused: true, finished: false });

  await updateTtsPanel();
  loadCacheStatus();

  if (changedChapter) {
    invoke('book_set_position', { id: item.id, chapter: idx, offset: 0 }).catch(() => {});
    if (forward && ttsModelId) {
      const voiceVal = ttsVoice || '0';
      const sid = voiceVal.startsWith('custom:') ? 0 : (parseInt(voiceVal) || 0);
      invoke('book_trim_audio', { id: item.id, modelId: ttsModelId, current: idx, sid, speed: ttsSpeed }).catch(() => {});
    }
  }

  // Resume-aware, exactly like openReading's autoplay: the queue re-enters
  // the chapter the book was left on, which must continue from the saved
  // offset. Chapter auto-advance is unaffected — a chapter change zeroes
  // `prog` above, so resumeWord is 0 and this starts from the top as before.
  if (autoplay) startPlaybackFromResume();
}

document.getElementById('reading-back').addEventListener('click', () => {
  saveProgress(true);
  invoke('tts_stop');
  // The session is dead now; without this, ttsStarted stays stale-true and
  // anything keying off it later (mini-player keep-alive, live-session
  // detection on the next open) reasons from a session that no longer exists.
  resetTtsUI();
  hidePlayerBar();
  if (bookState) {
    openBook(bookState.id);
  } else {
    navigateTo('library');
  }
});

// Tap a word to jump playback there. Ignore taps that finish a text selection
// (long-press to select/copy still works).
document.getElementById('reading-text').addEventListener('click', (e) => {
  const span = e.target.closest('.rw');
  if (!span) return;
  const sel = window.getSelection();
  if (sel && !sel.isCollapsed) return;
  const i = parseInt(span.dataset.w, 10);
  if (!Number.isNaN(i)) jumpToWord(i);
});

// ── Voice sheet (player bar) ──

const voiceSheet = document.getElementById('voice-sheet');

function updateVoiceBtnLabel() {
  const label = document.getElementById('tts-voice-btn-label');
  if (!label) return;
  label.textContent = ttsVoice.startsWith('custom:') ? ttsVoice.slice(7) : voiceLabel(parseInt(ttsVoice, 10) || 0);
  document.getElementById('np-voice-label').textContent = label.textContent;
}

function speedForVoice(voice) {
  const s = ttsVoiceSpeeds[voice];
  return typeof s === 'number' ? s : 1.0;
}

function setTtsVoice(val) {
  if (String(val) !== ttsVoice) voiceDirty = true;
  ttsVoice = String(val);
  updateVoiceBtnLabel();
  // Each voice carries its own speed; restore it (default 1x) and reflect it in
  // the speed control.
  setTtsSpeed(speedForVoice(ttsVoice));
  persistVoicePrefs();
  // Cache coverage is per voice — refresh what's shown as buffered.
  if (readingItem) loadCacheStatus();
}

// Persist the voice prefs touched here (last voice + per-voice speeds) without
// disturbing the device/haptic/threads fields owned by saveConfig().
async function persistVoicePrefs() {
  try {
    const cfg = await invoke('get_config');
    cfg.tts_voice = ttsVoice;
    cfg.tts_voice_speeds = ttsVoiceSpeeds;
    await invoke('save_config', { cfg });
  } catch (_) { /* non-fatal */ }
}

function voiceSheetItemHtml(value, label) {
  const active = value === ttsVoice;
  return `<button class="voice-pick w-full flex items-center justify-between gap-3 px-3 py-2.5 rounded-xl text-left cursor-pointer ${active ? 'bg-primary/15' : 'hover:bg-surface-container-highest'}" data-voice="${escapeHtml(value)}">
      <span class="text-sm ${active ? 'text-primary font-semibold' : 'text-on-surface'}">${escapeHtml(label)}</span>
      ${active ? '<span class="material-symbols-outlined text-lg text-primary">check</span>' : ''}
    </button>`;
}

// Build the player voice list: starred voices from the catalogue (any model)
// if there are any, else all speakers of the active model; then any custom
// voices. Speaker count is fetched here so the sheet works even if the
// Voices page was never opened.
async function buildVoiceSheet() {
  const list = document.getElementById('voice-sheet-list');
  const hint = document.getElementById('voice-sheet-hint');
  if (!ttsModelsCache.length) {
    try {
      const models = await invoke('list_models');
      ttsModelsCache = models.filter(m => m.engine.startsWith('tts_'));
    } catch (_) { /* offline: sheet still renders from the active model */ }
  }

  let html = '';
  if (ttsFavourites.length) {
    hint.textContent = 'Favourites';
    html = ttsFavourites.map(key => {
      const [model, sidStr] = splitKey(key);
      if (model === ttsModelId) {
        return voiceSheetItemHtml(sidStr, catalogLabel(model, parseInt(sidStr, 10)));
      }
      const modelName = (modelById(model) || {}).name || model;
      const label = `${catalogLabel(model, parseInt(sidStr, 10))} \u{b7} ${modelName}`;
      return voiceSheetItemHtml(key, label);
    }).join('');
  } else {
    // No favourites yet: offer every voice that's ready to speak.
    const ready = VOICES.filter(v => isDownloaded(modelById(v.model)));
    hint.textContent = ready.length ? 'Voices' : '';
    html = ready.map(v => {
      const val = v.model === ttsModelId ? String(v.sid) : voiceKey(v.model, v.sid);
      return voiceSheetItemHtml(val, v.label);
    }).join('');
  }

  const custom = await invoke('tts_list_custom_voices');
  if (custom.length) {
    html += `<p class="text-[11px] text-on-surface-variant px-3 pt-3 pb-1 uppercase tracking-wider">Custom</p>`;
    html += custom.map(name => voiceSheetItemHtml('custom:' + name, name)).join('');
  }
  list.innerHTML = html || `<p class="text-sm text-on-surface-variant px-3 py-6 text-center">No voices available</p>`;
}

async function openVoiceSheet() {
  await buildVoiceSheet();
  voiceSheet.classList.remove('hidden');
}
function closeVoiceSheet() { voiceSheet.classList.add('hidden'); }

// Switch the active TTS model from the player, WITHOUT tearing down playback
// (unlike useVoice, which is for the Voices page). Fetches the backing voice
// if needed — the CALLER must confirm the download first (confirmVoiceDownload)
// so this never pulls MBs unprompted. The next startSpeak lazy-loads the new
// model because ttsLoadedModelId is cleared. Returns false if the fetch failed.
async function switchActiveModel(model) {
  if (ttsModelId === model) return true;
  if (!isDownloaded(modelById(model))) {
    try {
      await invoke('download_model', { id: model });
      const models = await invoke('list_models');
      ttsModelsCache = models.filter(m => m.engine.startsWith('tts_'));
    } catch (err) {
      showToast('Voice download failed: ' + err);
      return false;
    }
  }
  ttsActiveModel = model;
  try {
    const cfg = await invoke('get_config');
    cfg.tts_model = model;
    await invoke('save_config', { cfg });
  } catch (_) { /* non-fatal */ }
  ttsLoadedModelId = null; // force reload of the new model on next speak
  ttsModelId = model;
  voiceDirty = true;
  return true;
}

document.getElementById('tts-voice-btn').addEventListener('click', openVoiceSheet);
document.getElementById('voice-sheet-overlay').addEventListener('click', closeVoiceSheet);
document.getElementById('voice-sheet-list').addEventListener('click', async (e) => {
  const btn = e.target.closest('.voice-pick');
  if (!btn) return;
  const val = btn.dataset.voice;
  const cross = !val.startsWith('custom:') && val.includes(':');
  const targetModel = cross ? splitKey(val)[0] : null;
  closeVoiceSheet();

  // Prompt before any large download, BEFORE touching playback, so declining
  // leaves the current voice playing untouched.
  if (cross && !await confirmVoiceDownload(targetModel)) return;

  // Snapshot where we are, then FULLY STOP the current voice before switching.
  // Pausing is not enough: a paused generation keeps its worker AND player
  // alive (is_active stays true), so the old worker keeps generating while
  // tts_load swaps the engine under it, and the old audio resurfaces over the
  // new voice a few seconds in. Stopping tears it all down; we restart from
  // the snapshot below, with the buffering spinner covering the reload.
  const resume = isPlaying() ? currentResumePoint() : null;
  if (resume) {
    setTtsLoading(true);
    try { await invoke('tts_stop'); } catch (_) { /* nothing playing */ }
  }

  if (cross) {
    // A voice on another model: switch model + speaker together. Any download
    // was user-confirmed above. On failure ttsModelId/ttsVoice stay put, so the
    // restart below simply resumes the original voice.
    const [model, sidStr] = splitKey(val);
    if (await switchActiveModel(model)) setTtsVoice(sidStr);
  } else {
    setTtsVoice(val);
  }

  // Restart from the snapshot under the now-active voice.
  if (resume) await startSpeak(resume.text, resume.word, resume.ms);
  else setTtsLoading(false);
});

// ── Add-text modal ──

const libAddModal = document.getElementById('lib-add-modal');
const addChooserModal = document.getElementById('add-chooser-modal');
const addUrlModal = document.getElementById('add-url-modal');
function openModal(el) { el.classList.remove('hidden'); el.classList.add('flex'); }
function closeModal(el) { el.classList.add('hidden'); el.classList.remove('flex'); }

// The bottom-nav + button opens a source chooser (Text, URL, File, eBook).
function openAddModal() { openModal(addChooserModal); }

function openTextModal() {
  document.getElementById('lib-add-title').value = '';
  document.getElementById('lib-add-body').value = '';
  openModal(libAddModal);
}
function closeAddModal() { closeModal(libAddModal); }

function openUrlModal() {
  document.getElementById('add-url-input').value = '';
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
  else if (src === 'file' || src === 'ebook') { closeModal(addChooserModal); pickImportFile(src); }
});

document.getElementById('lib-add-cancel').addEventListener('click', closeAddModal);
document.getElementById('lib-add-save').addEventListener('click', async () => {
  const title = document.getElementById('lib-add-title').value;
  const body = document.getElementById('lib-add-body').value;
  if (!body.trim()) { showToast('Paste some text first'); return; }
  try {
    await invoke('library_add', { title: title || null, body });
    closeAddModal();
    loadLibrary();
  } catch (err) { showToast('Save failed: ' + err); }
});

document.getElementById('add-url-cancel').addEventListener('click', closeUrlModal);
document.getElementById('add-url-save').addEventListener('click', () => {
  let url = document.getElementById('add-url-input').value.trim();
  if (!url) { showToast('Paste a URL first'); return; }
  if (!/^https?:\/\//i.test(url)) url = 'https://' + url;
  closeUrlModal();
  // importSharedText extracts the URL, fetches + Readability-parses in Rust,
  // adds to the library, switches to Listen and opens the reader.
  importSharedText(url);
});

// ── File / eBook import ──

const importFileInput = document.getElementById('import-file-input');

// Open the OS file/document picker for File (.txt/.md/.pdf) or eBook (.epub).
// Both extensions AND MIME types are listed in `accept`: on some devices a
// bare extension whose MIME can't be resolved gets silently dropped by
// RustWebChromeClient's getValidTypes, narrowing the picker to nothing.
function pickImportFile(kind) {
  importFileInput.accept = kind === 'ebook'
    ? '.epub,application/epub+zip'
    : '.txt,.md,.markdown,.pdf,text/plain,text/markdown,application/pdf,application/octet-stream';
  importFileInput.dataset.kind = kind;
  importFileInput.click();
}

importFileInput.addEventListener('change', () => {
  const file = importFileInput.files && importFileInput.files[0];
  const kind = importFileInput.dataset.kind;
  importFileInput.value = ''; // let picking the same file again still fire 'change'
  if (file) importFile(file, kind);
});

// Import a picked File/eBook. SAF's reported MIME type is unreliable, so
// parsing is routed by the filename's own extension, not file.type; `kind`
// Re-encode raw cover bytes ({data, mime} from parseEpub) to a bounded JPEG
// and return bare base64, or null when the image can't decode. The canvas
// pass normalizes any source format and caps the long edge at 600px so a
// stored cover is a thumbnail (tens of KB), never the publisher's original.
async function coverToJpegB64({ data, mime }) {
  try {
    const bmp = await createImageBitmap(new Blob([data], { type: mime }));
    const scale = Math.min(1, 600 / Math.max(bmp.width, bmp.height));
    const canvas = document.createElement('canvas');
    canvas.width = Math.max(1, Math.round(bmp.width * scale));
    canvas.height = Math.max(1, Math.round(bmp.height * scale));
    canvas.getContext('2d').drawImage(bmp, 0, 0, canvas.width, canvas.height);
    bmp.close();
    const dataUrl = canvas.toDataURL('image/jpeg', 0.8);
    return dataUrl.split(',')[1] || null;
  } catch (_) {
    return null;
  }
}

// (which button was pressed) is only a fallback for an extensionless name.
// A short result becomes one article like importFeedEntry's shape; a long
// one (or any EPUB, even a single chapter) becomes a book.
async function importFile(file, kind) {
  const name = file.name || 'Untitled';
  const extMatch = name.match(/\.([^.]+)$/);
  const ext = extMatch ? extMatch[1].toLowerCase() : (kind === 'ebook' ? 'epub' : 'txt');
  const titleFromName = name.replace(/\.[^.]+$/, '') || 'Untitled';
  showToast('Importing…');
  try {
    if (ext === 'epub') {
      const buf = await file.arrayBuffer();
      let book;
      try {
        book = await parseEpub(buf);
      } catch (err) {
        showToast('Could not read EPUB: ' + err);
        return;
      }
      const item = await invoke('book_add', { title: book.title || titleFromName, chapters: book.chapters });
      // The EPUB's own cover, re-encoded through a canvas (bounded to 600px
      // JPEG) and stored server-side. Awaited BEFORE the library render:
      // rendering first would fetch (and cache) a null cover, and the
      // placeholder wouldn't heal until the next full library reload. The
      // canvas pass is tens of milliseconds — cheap against an import.
      // Best effort: a book whose cover fails to decode keeps its gradient.
      if (book.cover) {
        try {
          const b64 = await coverToJpegB64(book.cover);
          if (b64) await invoke('library_set_cover', { id: item.id, dataB64: b64 });
        } catch (_) { /* keep the gradient */ }
      }
      await loadLibrary();
      openBook(item.id);
      showToast('Book added');
      return;
    }

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

    if (countWords(text) <= 2500) {
      const item = await invoke('library_add', { title: titleFromName, body: text });
      await loadLibrary();
      openReading(item.id);
      showToast('Text added');
    } else {
      const chapters = splitIntoParts(text).map((body, i) => ({ title: `Part ${i + 1}`, body }));
      const item = await invoke('book_add', { title: titleFromName, chapters });
      await loadLibrary();
      openBook(item.id);
      showToast('Book added');
    }
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
    // Stable identity for seen-tracking; capped so a pathological guid can't
    // bloat the stored list.
    const key = (guid || link || `${title}|${date}`).slice(0, 500);
    return { key, title, link, date, dateMs, contentHtml };
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
    article = extractArticle(html, entry.link);
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
      btn.innerHTML = '<span class="material-symbols-outlined text-xl tts-spin">progress_activity</span>';
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
    icon.classList.add('tts-spin');
    text.textContent = 'Refreshing…';
    Promise.resolve(onRefresh()).catch(() => {}).finally(() => {
      icon.classList.remove('tts-spin');
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

// ── Meeting mode (desktop only) ──
//
// A third mode alongside Speak/Listen: record a meeting (mic + system-audio
// loopback when available), watch utterances arrive live, jot notes, then
// stop to get a transcript. With a summarizer installed, the meeting view can
// then run a local LLM over the transcript (map-reduce, notes as anchors) to
// produce a Summary / Decisions / Action items block.
// Every invoke() below is gated on isDesktop even though a couple of the
// backend commands happen to be safe cross-platform (see packages_status) —
// this UI never renders on Android, so it should never call the backend either.

let meetingRecording = false; // true for the life of an active recording session
let meetingUtterances = []; // live transcript for the in-progress session
let meetingAutoFollow = true; // live transcript auto-scroll; off once the user scrolls up
let meetingNotesSaveTimer = null; // debounce handle for meeting_note_set
let meetingClockTimer = null;
let meetingClockBaseMs = 0; // elapsed ms as of meetingClockBaseAt
let meetingClockBaseAt = 0; // performance.now() when the base was captured
let meetingStartEpoch = 0; // Date.now() at meeting start; wall-clock labels = this + t_ms

let meetingItems = []; // last meetings_list() result
let meetingViewId = null; // id shown by #meeting-view
let meetingViewMeta = null; // its MeetingMeta, for the folder button

function formatMmSs(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

function meetingClockNow() {
  return meetingClockBaseMs + (performance.now() - meetingClockBaseAt);
}

function startMeetingClock(elapsedMs) {
  stopMeetingClock();
  meetingClockBaseMs = elapsedMs;
  meetingClockBaseAt = performance.now();
  const clock = document.getElementById('meeting-live-clock');
  clock.textContent = formatMmSs(meetingClockNow());
  meetingClockTimer = setInterval(() => { clock.textContent = formatMmSs(meetingClockNow()); }, 500);
}

function stopMeetingClock() {
  if (meetingClockTimer) { clearInterval(meetingClockTimer); meetingClockTimer = null; }
}

function setMeetingLoopbackNotice(notice) {
  const el = document.getElementById('meeting-live-notice');
  document.getElementById('meeting-live-notice-text').textContent = notice || '';
  el.classList.toggle('hidden', !notice);
}

// Mirrors the backend's display merge (store::merge_for_display): consecutive
// same-speaker utterances within the gap join into one block. The raw
// meetingUtterances array stays per-segment so rehydrate matches the backend.
const MEETING_MERGE_GAP_MS = 30000;
function mergeMeetingUtterances(utts) {
  const out = [];
  let lastStart = 0;
  for (const u of utts) {
    const last = out[out.length - 1];
    if (last && last.speaker === u.speaker && u.t_ms - lastStart <= MEETING_MERGE_GAP_MS) {
      last.text += ' ' + u.text;
    } else {
      out.push({ t_ms: u.t_ms, speaker: u.speaker, text: u.text });
    }
    lastStart = u.t_ms;
  }
  return out;
}

function meetingWallClock(tMs) {
  const d = new Date(meetingStartEpoch + tMs);
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function meetingUtteranceHtml(u) {
  const speakerClass = u.speaker === 'You' ? 'text-primary' : 'text-on-surface-variant';
  return `
  <div class="meeting-utterance">
    <div class="flex items-baseline gap-2">
      <span class="text-xs font-semibold ${speakerClass}">${escapeHtml(u.speaker)}</span>
      <span class="text-[11px] text-on-surface-variant/60 tabular-nums">${meetingWallClock(u.t_ms)}</span>
    </div>
    <p class="text-[15px] leading-relaxed text-on-surface mt-0.5">${escapeHtml(u.text)}</p>
  </div>`;
}

function renderMeetingTranscript(scrollToEnd = true) {
  const el = document.getElementById('meeting-transcript');
  const prev = el.scrollTop;
  el.innerHTML = mergeMeetingUtterances(meetingUtterances).map(meetingUtteranceHtml).join('');
  el.scrollTop = scrollToEnd ? el.scrollHeight : prev;
}

function appendMeetingUtterance(u) {
  meetingUtterances.push(u);
  renderMeetingTranscript(meetingAutoFollow);
  if (!meetingAutoFollow) {
    document.getElementById('meeting-jump-latest').classList.remove('hidden');
  }
}

// Auto-follow tracks scroll position directly (simpler than the reader's
// wheel/touchmove flag — a live transcript is an append-only log, so "near
// the bottom" is the whole story): scrolling away disables it, scrolling back
// to the bottom resumes it, mirroring how the reading view resumes following
// once the highlighted word is back in view.
(function () {
  const el = document.getElementById('meeting-transcript');
  el.addEventListener('scroll', () => {
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    meetingAutoFollow = atBottom;
    if (atBottom) document.getElementById('meeting-jump-latest').classList.add('hidden');
  });
})();

document.getElementById('meeting-jump-latest').addEventListener('click', () => {
  meetingAutoFollow = true;
  const el = document.getElementById('meeting-transcript');
  el.scrollTop = el.scrollHeight;
  document.getElementById('meeting-jump-latest').classList.add('hidden');
});

listen('meeting-utterance', (event) => {
  if (meetingRecording) {
    appendMeetingUtterance(event.payload);
    hideSilenceBanner(); // speech resumed
  }
});

// Post-stop lifecycle (the UI has already left the live screen by the time
// these fire): diarizing/processed drive the busy card on the meetings list.
// idle covers a session ending from somewhere other than this tab's own
// Stop/Cancel, which must still clear the mode-switch guard.
listen('meeting-state', (event) => {
  const state = event.payload?.state;
  if (state === 'diarizing') {
    const label = document.querySelector(
      `.meeting-item[data-id="${event.payload?.id}"] .processing-label`);
    if (label) label.textContent = 'Analyzing speakers…';
  } else if (state === 'processed') {
    if (event.payload?.error) showToast('Meeting processing: ' + event.payload.error);
    loadMeetings(); // unstick the card (or surface it, if the list never saw it)
  } else if (state === 'idle' && meetingRecording) {
    endMeetingSession();
  }
});

// End-of-meeting nudge: the backend flags a long silence; we offer to stop
// (never a hard auto-stop, so a merely-paused meeting is never lost).
listen('meeting-silence', (event) => {
  if (!meetingRecording) return;
  const secs = event.payload?.silent_secs || 0;
  const mins = Math.max(1, Math.round(secs / 60));
  document.getElementById('meeting-silence-text').textContent =
    `No one's spoken for ${mins} min. Stop and save the meeting?`;
  const banner = document.getElementById('meeting-silence-banner');
  banner.classList.remove('hidden');
  banner.classList.add('flex');
});

function hideSilenceBanner() {
  const banner = document.getElementById('meeting-silence-banner');
  banner.classList.add('hidden');
  banner.classList.remove('flex');
}

document.getElementById('meeting-silence-stop').addEventListener('click', () => {
  hideSilenceBanner();
  document.getElementById('meeting-stop-btn').click(); // reuse the full stop flow
});
document.getElementById('meeting-silence-dismiss').addEventListener('click', hideSilenceBanner);

function endMeetingSession() {
  meetingRecording = false;
  stopMeetingClock();
  meetingUtterances = [];
  meetingAutoFollow = true;
  hideSilenceBanner();
}

async function startMeeting() {
  if (!isDesktop) return;
  let res;
  try {
    res = await invoke('meeting_start');
  } catch (err) {
    showToast('Could not start meeting: ' + err);
    return;
  }
  meetingRecording = true;
  meetingUtterances = [];
  meetingAutoFollow = true;
  document.getElementById('meeting-transcript').innerHTML = '';
  document.getElementById('meeting-jump-latest').classList.add('hidden');
  document.getElementById('meeting-notes').value = '';
  hideSilenceBanner();
  const stopBtn = document.getElementById('meeting-stop-btn');
  stopBtn.disabled = false;
  stopBtn.textContent = 'Stop';
  setMeetingLoopbackNotice(res.notice);
  meetingStartEpoch = Date.now();
  startMeetingClock(0);
  showPanel('meeting-live');
  setBottomNavVisible(false);
}

// Restores an in-progress meeting after a reload (or first paint) so the live
// view isn't just abandoned mid-recording. Called at boot; see DOMContentLoaded.
async function checkMeetingRehydrate() {
  if (!isDesktop) return;
  let status;
  try {
    status = await invoke('meeting_status');
  } catch (err) {
    console.error('meeting_status failed:', err);
    return;
  }
  if (status.state !== 'recording') return;
  meetingRecording = true;
  meetingUtterances = Array.isArray(status.utterances) ? status.utterances.slice() : [];
  meetingAutoFollow = true;
  meetingStartEpoch = Date.now() - (status.elapsed_ms || 0);
  renderMeetingTranscript();
  document.getElementById('meeting-jump-latest').classList.add('hidden');
  document.getElementById('meeting-notes').value = status.notes || '';
  const stopBtn = document.getElementById('meeting-stop-btn');
  stopBtn.disabled = false;
  stopBtn.textContent = 'Stop';
  setMeetingLoopbackNotice(status.notice);
  startMeetingClock(status.elapsed_ms || 0);
  setMode('meeting');
}

document.getElementById('meeting-notes').addEventListener('input', (e) => {
  clearTimeout(meetingNotesSaveTimer);
  const text = e.target.value;
  meetingNotesSaveTimer = setTimeout(() => {
    invoke('meeting_note_set', { text }).catch(() => {});
  }, 800);
});

document.getElementById('meeting-stop-btn').addEventListener('click', async () => {
  const btn = document.getElementById('meeting-stop-btn');
  // Ask for a name before stopping. Cancel backs out (keeps recording); an
  // empty name keeps the auto-generated "Meeting <date>" title.
  const title = await showPrompt('Name this meeting', {
    okLabel: 'Save & stop',
    placeholder: 'e.g. Weekly sync',
  });
  if (title === null) return;
  btn.disabled = true;
  btn.textContent = 'Stopping…';
  clearTimeout(meetingNotesSaveTimer);
  try {
    const notes = document.getElementById('meeting-notes').value;
    // Returns as soon as capture ends; speaker analysis and the transcript
    // write continue in the backend. Land on the list so a new meeting can
    // start immediately — the card shows Processing until it's ready.
    await invoke('meeting_stop', { notes, title: title || null });
    endMeetingSession();
    showToast('Meeting saved — analyzing speakers');
    navigateTo('meetings');
  } catch (err) {
    showToast('Stop failed: ' + err);
    btn.disabled = false;
    btn.textContent = 'Stop';
  }
});

document.getElementById('meeting-cancel-btn').addEventListener('click', async () => {
  if (!await showConfirm('Discard this meeting? The recording will not be saved.', { okLabel: 'Discard' })) return;
  clearTimeout(meetingNotesSaveTimer);
  try {
    await invoke('meeting_cancel');
  } catch (err) {
    showToast('Cancel failed: ' + err);
  }
  endMeetingSession();
  navigateTo('meetings');
});

// ── Meetings list ──

function renderMeetingCard(m, i) {
  const meta = [formatTimestamp(m.started), fmtMins(m.duration_ms),
    `${m.utterance_count} ${m.utterance_count === 1 ? 'utterance' : 'utterances'}`].join(' · ');
  // A just-stopped meeting stays busy while the backend finishes speaker
  // analysis; its card is highlighted and inert until meeting-state processed.
  const indicator = m.processing
    ? '<span class="inline-flex items-center gap-1 text-xs text-primary"><span class="material-symbols-outlined text-sm animate-spin">progress_activity</span><span class="processing-label">Processing…</span></span>'
    : m.summary_path
      ? '<span class="inline-flex items-center gap-1 text-xs text-primary"><span class="material-symbols-outlined text-sm">auto_awesome</span>Summarized</span>'
      : '<span class="inline-flex items-center gap-1 text-xs text-on-surface-variant"><span class="material-symbols-outlined text-sm">description</span>Transcript only</span>';
  const nameBadge = !m.processing && m.unnamed_speakers > 0
    ? `<span class="inline-flex items-center gap-1 text-xs text-primary"><span class="material-symbols-outlined text-sm">person_add</span>${m.unnamed_speakers} to name</span>`
    : '';
  const ring = m.processing ? ' ring-1 ring-primary/40' : '';
  return `
  <div class="meeting-item stagger-in bg-surface-container rounded-xl px-4 py-3.5 cursor-pointer hover:bg-surface-container-high transition-colors${ring}" data-id="${escapeHtml(m.id)}" style="--i:${i}">
    <div class="text-[15px] font-semibold leading-snug text-on-surface truncate">${escapeHtml(m.title)}</div>
    <div class="text-xs text-on-surface-variant tabular-nums mt-1">${escapeHtml(meta)}</div>
    <div class="mt-1.5 flex items-center gap-3">${indicator}${nameBadge}</div>
  </div>`;
}

async function loadMeetings() {
  if (!isDesktop) return;
  try {
    meetingItems = await invoke('meetings_list');
  } catch (err) {
    console.error('Failed to load meetings:', err);
    return;
  }
  const list = document.getElementById('meetings-list');
  if (!meetingItems.length) {
    list.innerHTML = `
      <div class="flex flex-col items-center justify-center pt-16 text-on-surface-variant">
        <span class="material-symbols-outlined text-4xl mb-3 opacity-30">groups</span>
        <p class="text-sm">No meetings yet</p>
        <p class="text-xs mt-2 max-w-xs text-center opacity-70">Press the record button to capture and transcribe a meeting</p>
      </div>`;
    return;
  }
  list.innerHTML = meetingItems.map(renderMeetingCard).join('');
  list.querySelectorAll('.meeting-item').forEach(el => {
    // While the post-stop pass runs there is no final transcript to open and
    // deleting would race the finalize thread — hold every action until then.
    const busy = () => {
      const m = meetingItems.find(x => x.id === el.dataset.id);
      if (m && m.processing) { showToast('Still processing — ready shortly'); return true; }
      return false;
    };
    el.addEventListener('click', () => {
      if (el._longPressed) { el._longPressed = false; return; }
      if (busy()) return;
      openMeetingView(el.dataset.id);
    });
    attachLongPress(el, () => { if (!busy()) openMeetingActionSheet(el.dataset.id); });
  });
}

// ── Meeting row long-press action sheet ──

const meetingActionSheet = document.getElementById('meeting-action-sheet');
let meetingActionTarget = null; // meeting id

function openMeetingActionSheet(id) {
  const m = meetingItems.find(x => x.id === id);
  if (!m) return;
  meetingActionTarget = id;
  document.getElementById('meeting-action-title').textContent = m.title;
  const summarizerReady = !!(pkgStatus && pkgStatus.meeting && pkgStatus.meeting.state === 'installed');
  const actions = [
    { action: 'open', icon: 'visibility', label: 'Open' },
    ...(summarizerReady ? [{ action: 'summarize', icon: 'auto_awesome', label: 'Summarize' }] : []),
    { action: 'open-folder', icon: 'folder_open', label: 'Open folder' },
    { action: 'delete', icon: 'delete', label: 'Delete', danger: true },
  ];
  document.getElementById('meeting-action-list').innerHTML = actions.map(a =>
    `<button class="more-item w-full flex items-center gap-3 px-4 py-3 rounded-xl text-left cursor-pointer hover:bg-surface-container-highest ${a.danger ? 'text-error' : 'text-on-surface'}" data-action="${a.action}">
      <span class="material-symbols-outlined text-lg ${a.danger ? 'text-error/80' : 'text-on-surface-variant'}">${a.icon}</span>
      <span class="text-sm">${a.label}</span>
    </button>`).join('');
  meetingActionSheet.classList.remove('hidden');
}
function closeMeetingActionSheet() { meetingActionSheet.classList.add('hidden'); }
document.getElementById('meeting-action-overlay').addEventListener('click', closeMeetingActionSheet);
document.getElementById('meeting-action-list').addEventListener('click', async (e) => {
  const btn = e.target.closest('.more-item');
  if (!btn || !meetingActionTarget) return;
  const id = meetingActionTarget;
  const action = btn.dataset.action;
  closeMeetingActionSheet();
  if (action === 'open') openMeetingView(id);
  else if (action === 'summarize') { await openMeetingView(id); summarizeMeeting(id); }
  else if (action === 'open-folder') openMeetingFolder(id);
  else if (action === 'delete') deleteMeeting(id);
});

async function deleteMeeting(id) {
  const m = meetingItems.find(x => x.id === id);
  if (!await showConfirm(`Delete "${m ? m.title : 'this meeting'}"? This also deletes its transcript and summary files.`)) return;
  try {
    await invoke('meeting_delete', { id, deleteFiles: true });
    showToast('Meeting deleted');
    if (meetingViewId === id) navigateTo('meetings'); else loadMeetings();
  } catch (err) {
    showToast('Delete failed: ' + err);
  }
}

// Path used by the "Open folder" affordance, from whichever cache has it —
// the list (meetings_list) or the detail view (meeting_get) — so it works
// from either entry point without an extra round trip.
function meetingPathFor(id) {
  const fromList = meetingItems.find(x => x.id === id);
  if (fromList) return fromList.transcript_path || fromList.summary_path || '';
  if (meetingViewMeta && meetingViewMeta.id === id) return meetingViewMeta.transcript_path || meetingViewMeta.summary_path || '';
  return '';
}

// tauri-plugin-opener (installed) exposes revealItemInDir under window.__TAURI__
// when the bundled global picks it up; feature-detect rather than assume, same
// caution as the Settings folder dialog below.
async function openMeetingFolder(id) {
  const path = meetingPathFor(id);
  const reveal = window.__TAURI__.opener?.revealItemInDir;
  if (!path || !reveal) { showToast('Open folder is not available'); return; }
  try {
    await reveal(path);
  } catch (err) {
    showToast('Could not open folder: ' + err);
  }
}

let meetingSummarizingId = null; // meeting id whose summary is being generated

// Human-readable line for a meeting-summary-progress event. The backend runs
// loading -> map (per chunk) -> combine -> done; short transcripts skip map.
function meetingSummaryProgressText(stage, done, total) {
  if (stage === 'map') return `Reading transcript… (${done}/${total})`;
  if (stage === 'combine') return 'Writing summary…';
  return 'Loading summarizer…';
}

// The LLM runs on a blocking backend thread; the invoke resolves only when the
// summary file is written. That can take tens of seconds, so drive a live
// status from meeting-summary-progress and lock the trigger against re-entry.
async function summarizeMeeting(id) {
  if (meetingSummarizingId) return; // one job at a time
  meetingSummarizingId = id;
  const inView = meetingViewId === id;
  const summaryBody = document.getElementById('meeting-summary-body');
  const summarizeBtn = document.getElementById('meeting-summarize-btn');
  if (inView) {
    summaryBody.textContent = 'Loading summarizer…';
    summarizeBtn.classList.add('hidden');
  }
  try {
    await invoke('meeting_summarize', { id });
    showToast('Summary ready');
    if (meetingViewId === id) openMeetingView(id); // re-render with the summary
    loadMeetings(); // refresh the list's Summarized badge
  } catch (err) {
    showToast('Summarize: ' + err);
    // Restore the prior view — an existing summary (re-run) or the button.
    if (meetingViewId === id) openMeetingView(id);
  } finally {
    meetingSummarizingId = null;
  }
}

listen('meeting-summary-progress', (event) => {
  const p = event.payload;
  if (!p || p.id !== meetingViewId || p.id !== meetingSummarizingId) return;
  if (p.stage === 'done') return;
  document.getElementById('meeting-summary-body').textContent =
    meetingSummaryProgressText(p.stage, p.done, p.total);
});

// ── Meeting detail view ──

// Minimal, safe Markdown -> HTML for the summary/transcript blocks. Handles the
// shape we write ourselves: '#'/'##' headings, '-'/'*' bullets, **bold**, and
// blank-line-separated paragraphs. Text is HTML-escaped first, so neither LLM
// output nor transcript text can inject markup. The leading '# Title' H1 is
// dropped — the view already shows the title in its header.
function renderMarkdownBasic(md) {
  const inline = (s) => escapeHtml(s).replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  const blocks = [];
  let list = null;
  const flushList = () => {
    if (list) { blocks.push(`<ul class="list-disc pl-5 space-y-1 my-2">${list.join('')}</ul>`); list = null; }
  };
  for (const raw of String(md).split('\n')) {
    const line = raw.trim();
    if (!line) { flushList(); continue; }
    let m;
    if (line.match(/^#\s+/)) { flushList(); continue; } // drop redundant H1 title
    if ((m = line.match(/^##+\s+(.*)/))) {
      flushList();
      blocks.push(`<h4 class="text-xs font-semibold text-on-surface-variant uppercase tracking-wider mt-4 mb-1.5 first:mt-0">${inline(m[1])}</h4>`);
    } else if ((m = line.match(/^[-*]\s+(.*)/))) {
      (list || (list = [])).push(`<li>${inline(m[1])}</li>`);
    } else {
      flushList();
      blocks.push(`<p class="mb-2">${inline(line)}</p>`);
    }
  }
  flushList();
  return blocks.join('') || '<p class="text-on-surface-variant">Empty.</p>';
}

let meetingTranscriptEntries = []; // structured lines shown in #meeting-view
let meetingTranscriptFilter = null; // when set, transcript shows only this speaker's lines

async function openMeetingView(id) {
  if (!isDesktop) return;
  let meta;
  try {
    meta = await invoke('meeting_get', { id });
  } catch (err) {
    showToast('Open failed: ' + err);
    return;
  }
  if (!meta) { showToast('Meeting not found'); return; }
  meetingViewId = id;
  meetingViewMeta = meta;
  document.getElementById('meeting-view-title').textContent = meta.title;
  document.getElementById('meeting-view-meta').textContent =
    [formatTimestamp(meta.started), fmtMins(meta.duration_ms), `${meta.utterance_count} utterances`].join(' · ');
  showPanel('meeting-view');
  setBottomNavVisible(false);
  exitSummaryEdit();
  document.getElementById('meeting-summary-edit-btn').classList.remove('hidden');

  const summaryBody = document.getElementById('meeting-summary-body');
  const summarizeBtn = document.getElementById('meeting-summarize-btn');
  if (meta.summary_path) {
    // Already summarized: render it and offer a re-run (the LLM is
    // nondeterministic, and notes may have changed since).
    summarizeBtn.textContent = 'Re-summarize';
    summarizeBtn.classList.remove('hidden');
    try {
      summaryBody.innerHTML = renderMarkdownBasic(await invoke('meeting_read_file', { id, which: 'summary' }));
    } catch (err) {
      summaryBody.textContent = 'Could not load summary: ' + err;
    }
  } else {
    summarizeBtn.textContent = 'Summarize';
    summarizeBtn.classList.remove('hidden');
    summaryBody.textContent = 'Not summarized yet.';
  }

  meetingTranscriptFilter = null;
  // Transcript first: the speaker strip's talk-time bars are computed from the
  // loaded entries.
  await loadMeetingTranscript(id);
  renderMeetingSpeakers(id);
}

document.getElementById('meeting-view-back').addEventListener('click', () => navigateTo('meetings'));
document.getElementById('meeting-view-folder').addEventListener('click', () => {
  if (meetingViewId) openMeetingFolder(meetingViewId);
});
document.getElementById('meeting-summarize-btn').addEventListener('click', () => {
  if (meetingViewId) summarizeMeeting(meetingViewId);
});

// ── Editable / dictatable summary ──
let summaryDictating = false;

function stripSummaryHeading(md) {
  return md.replace(/^#[^\n]*\n\n?/, ''); // drop the leading "# Title" line
}

function setSummaryMic(recording) {
  document.getElementById('meeting-summary-mic-label').textContent = recording ? 'Stop' : 'Dictate';
  const icon = document.getElementById('meeting-summary-mic-icon');
  icon.textContent = recording ? 'stop' : 'mic';
  icon.classList.toggle('animate-pulse', recording);
}

async function enterSummaryEdit() {
  const id = meetingViewId;
  if (!id) return;
  let text = '';
  if (meetingViewMeta && meetingViewMeta.summary_path) {
    try { text = stripSummaryHeading(await invoke('meeting_read_file', { id, which: 'summary' })); } catch (_) {}
  }
  document.getElementById('meeting-summary-edit').value = text;
  document.getElementById('meeting-summary-body').classList.add('hidden');
  document.getElementById('meeting-summarize-btn').classList.add('hidden');
  document.getElementById('meeting-summary-edit-btn').classList.add('hidden');
  document.getElementById('meeting-summary-editor').classList.remove('hidden');
  document.getElementById('meeting-summary-edit').focus();
}

// Leave edit mode. Stops any dictation in progress. Button visibility is
// re-established by the caller (openMeetingView or Cancel).
function exitSummaryEdit() {
  if (summaryDictating) {
    invoke('ui_stop_and_transcribe_raw').catch(() => {});
    summaryDictating = false;
    setSummaryMic(false);
  }
  document.getElementById('meeting-summary-editor').classList.add('hidden');
  document.getElementById('meeting-summary-body').classList.remove('hidden');
}

document.getElementById('meeting-summary-edit-btn').addEventListener('click', enterSummaryEdit);

document.getElementById('meeting-summary-cancel').addEventListener('click', () => {
  exitSummaryEdit();
  document.getElementById('meeting-summary-edit-btn').classList.remove('hidden');
  document.getElementById('meeting-summarize-btn').classList.remove('hidden');
});

document.getElementById('meeting-summary-mic').addEventListener('click', async () => {
  const ta = document.getElementById('meeting-summary-edit');
  if (summaryDictating) {
    summaryDictating = false;
    setSummaryMic(false);
    try {
      const text = ((await invoke('ui_stop_and_transcribe_raw')) || '').trim();
      if (text) {
        const sep = ta.value && !/\s$/.test(ta.value) ? ' ' : '';
        ta.value = ta.value + sep + text;
      }
    } catch (err) { showToast('Dictation failed: ' + err); }
    ta.focus();
  } else {
    try {
      await invoke('ui_start_recording');
      summaryDictating = true;
      setSummaryMic(true);
    } catch (err) { showToast('Could not start dictation: ' + err); }
  }
});

document.getElementById('meeting-summary-save').addEventListener('click', async () => {
  const id = meetingViewId;
  if (!id) return;
  if (summaryDictating) {
    try { await invoke('ui_stop_and_transcribe_raw'); } catch (_) {}
    summaryDictating = false;
    setSummaryMic(false);
  }
  const body = document.getElementById('meeting-summary-edit').value;
  try {
    await invoke('meeting_set_summary', { id, body });
    showToast('Summary saved');
    openMeetingView(id); // re-render + reset edit mode/buttons
  } catch (err) { showToast('Save failed: ' + err); }
});
// Speaker colours for the transcript: "You" uses the theme primary; everyone
// else gets a stable colour by first appearance, so the eye can track who's who.
const SPEAKER_COLORS = ['#7c9cff', '#f0883e', '#5bd1a0', '#e06c9f', '#c9a227', '#9b8cff', '#4fb6c4', '#d98880'];
function meetingSpeakerColors() {
  const map = {};
  let n = 0;
  for (const e of meetingTranscriptEntries) {
    if (e.speaker === 'You' || map[e.speaker]) continue;
    map[e.speaker] = SPEAKER_COLORS[n % SPEAKER_COLORS.length];
    n++;
  }
  return map;
}

// Load a finished meeting's structured transcript and render it (always visible;
// the per-speaker filter narrows it in place — that's the "who is this?" read).
async function loadMeetingTranscript(id) {
  const body = document.getElementById('meeting-transcript-view-body');
  try {
    meetingTranscriptEntries = await invoke('meeting_transcript', { id });
  } catch (err) {
    meetingTranscriptEntries = [];
    body.textContent = 'Could not load transcript: ' + err;
    return;
  }
  renderStructuredTranscript();
}

function renderStructuredTranscript() {
  const body = document.getElementById('meeting-transcript-view-body');
  const colors = meetingSpeakerColors();
  const filter = meetingTranscriptFilter;
  const rows = meetingTranscriptEntries.filter((e) => !filter || e.speaker === filter);
  if (!meetingTranscriptEntries.length) {
    body.innerHTML = '<p class="text-on-surface-variant/70">No transcript.</p>';
  } else if (!rows.length) {
    body.innerHTML = '<p class="text-on-surface-variant/70">Nothing from this speaker.</p>';
  } else {
    body.innerHTML = rows.map((e) => {
      const color = e.speaker === 'You' ? null : colors[e.speaker];
      const style = color ? ` style="color:${color}"` : '';
      const cls = e.speaker === 'You' ? 'text-primary' : '';
      // Remote names are tappable: naming and line fixes happen where the
      // words are ("You" is the mic channel and can't be renamed).
      const nameEl = e.speaker === 'You'
        ? `<span class="text-xs font-semibold ${cls}">${escapeHtml(e.speaker)}</span>`
        : `<button class="ml-name text-xs font-semibold cursor-pointer hover:underline underline-offset-2"${style} data-idx="${e.idx}" title="Name or fix this speaker">${escapeHtml(e.speaker)}</button>`;
      return `<div class="meeting-line">
        <div class="flex items-baseline gap-2">
          ${nameEl}
          <span class="text-[11px] text-on-surface-variant/60 tabular-nums">${escapeHtml(e.clock)}</span>
        </div>
        <p class="mt-0.5">${escapeHtml(e.text)}</p>
      </div>`;
    }).join('');
  }
  const clear = document.getElementById('meeting-transcript-filter-clear');
  if (filter) {
    document.getElementById('meeting-transcript-filter-name').textContent = filter;
    clear.classList.remove('hidden');
  } else {
    clear.classList.add('hidden');
  }
}

// Toggle a speaker filter (click the same speaker again to clear it).
function setTranscriptFilter(speaker) {
  meetingTranscriptFilter = meetingTranscriptFilter === speaker ? null : speaker;
  renderStructuredTranscript();
  document.getElementById('meeting-transcript-view-body')
    .scrollIntoView({ behavior: 'smooth', block: 'nearest' });
}

// Tapping a name on a transcript row opens the speaker sheet with that row's
// line context, so "this line isn't them" is fixable in place.
document.getElementById('meeting-transcript-view-body').addEventListener('click', (e) => {
  const btn = e.target.closest('.ml-name');
  if (!btn || !meetingViewId) return;
  const entry = meetingTranscriptEntries[Number(btn.dataset.idx)];
  if (!entry) return;
  openSpeakerSheet(meetingViewId, entry.speaker, { lines: entry.lines || [], text: entry.text });
});

document.getElementById('meeting-transcript-filter-clear').addEventListener('click', () => {
  meetingTranscriptFilter = null;
  renderStructuredTranscript();
});

// Show the meeting's speakers as chips; clicking one renames it, which enrolls
// that voiceprint so future meetings identify the person automatically.
let meetingSpeakersCache = []; // last meeting_speakers result for the open view

// Words + lines per speaker, from the loaded transcript entries. Powers the
// talk-time bars on the strip and the sheet header.
function speakerStats() {
  const stats = {};
  for (const e of meetingTranscriptEntries) {
    const s = (stats[e.speaker] ||= { lines: 0, words: 0 });
    s.lines += 1;
    s.words += e.text ? e.text.split(/\s+/).length : 0;
  }
  return stats;
}

function speakerInitial(name, unnamed) {
  return unnamed ? '?' : (name.trim()[0] || '?').toUpperCase();
}

// The speaker strip: one chip per remote speaker (avatar, name, talk-time bar).
// Everything else — rename, merge, line fixes, voice review — lives in the
// speaker sheet a chip opens. Unnamed speakers are highlighted as the call to
// action.
async function renderMeetingSpeakers(id) {
  const section = document.getElementById('meeting-speakers-section');
  const wrap = document.getElementById('meeting-speakers');
  let speakers = [];
  try { speakers = await invoke('meeting_speakers', { id }); } catch (_) {}
  meetingSpeakersCache = Array.isArray(speakers) ? speakers : [];
  if (!meetingSpeakersCache.length) {
    section.classList.add('hidden');
    wrap.innerHTML = '';
    return;
  }
  section.classList.remove('hidden');
  wrap.className = 'flex flex-wrap gap-2';
  wrap.innerHTML = '';
  const colors = meetingSpeakerColors();
  const stats = speakerStats();
  const maxWords = Math.max(1, ...meetingSpeakersCache.map((s) => (stats[s.name] || {}).words || 0));
  for (const sp of meetingSpeakersCache) {
    const st = stats[sp.name] || { words: 0, lines: 0 };
    const color = colors[sp.name] || SPEAKER_COLORS[0];
    const share = Math.max(4, Math.round((100 * st.words) / maxWords));
    const chip = document.createElement('button');
    chip.className =
      'inline-flex items-center gap-2 rounded-full pl-1.5 pr-3.5 min-h-11 cursor-pointer transition-colors ' +
      (sp.unnamed
        ? 'bg-primary/15 hover:bg-primary/25 ring-1 ring-primary/50'
        : 'bg-surface-container-highest hover:bg-primary/10');
    chip.title = sp.unnamed ? 'Unnamed - tap to name' : `Options for ${sp.name}`;
    chip.innerHTML =
      `<span class="w-7 h-7 rounded-full flex items-center justify-center text-[11px] font-bold shrink-0" style="background:${color}; color:#10131c">${escapeHtml(speakerInitial(sp.name, sp.unnamed))}</span>` +
      `<span class="flex flex-col items-start leading-tight">` +
      `<span class="text-sm ${sp.unnamed ? 'text-primary font-semibold' : 'text-on-surface'}">${escapeHtml(sp.name)}</span>` +
      `<span class="block w-12 h-[3px] rounded bg-outline-variant/30 mt-1"><span class="block h-full rounded" style="width:${share}%; background:${color}"></span></span>` +
      `</span>`;
    chip.addEventListener('click', () => openSpeakerSheet(id, sp.name));
    wrap.appendChild(chip);
  }
  const unnamed = meetingSpeakersCache.filter((s) => s.unnamed).length;
  document.getElementById('meeting-speakers-count').textContent = unnamed ? ` · ${unnamed} to name` : '';
  document.getElementById('meeting-speakers-hint').textContent = unnamed
    ? 'Tap the highlighted speaker to name them - known people are a single tap.'
    : 'Tap a speaker - or any name in the transcript - to rename, merge, or review.';
}

// The next free "Speaker N" for a moved-out voice; the user names it after.
function nextSpeakerName(speakers) {
  let max = 0;
  for (const s of speakers) {
    const m = /^Speaker (\d+)$/.exec(s.name);
    if (m) max = Math.max(max, Number(m[1]));
  }
  return `Speaker ${max + 1}`;
}

// ── Speaker sheet: the one surface for naming, merging, and fixing lines ──

const speakerSheet = document.getElementById('speaker-sheet');
let speakerSheetCtx = null; // { id, name, lineCtx, gallery, sigs, mergeOpen, reviewOpen }

function closeSpeakerSheet() {
  speakerSheet.classList.add('hidden');
  speakerSheetCtx = null;
}
document.getElementById('speaker-sheet-overlay').addEventListener('click', closeSpeakerSheet);

// Open for a speaker (from the strip) or for one transcript row (lineCtx =
// { lines, text }), which adds a "move this line" section on top.
async function openSpeakerSheet(id, name, lineCtx = null) {
  speakerSheetCtx = { id, name, lineCtx, gallery: [], sigs: [] };
  renderSpeakerSheet();
  speakerSheet.classList.remove('hidden');
  let gallery = [];
  try { gallery = await invoke('meeting_gallery_speakers'); } catch (_) {}
  let sigs = [];
  try {
    const all = await invoke('meeting_signatures', { id });
    const mine = ((all || []).find((s) => s.speaker === name) || {}).signatures || [];
    // Noise gating: one-line signatures are embedding noise on back-channels,
    // not extra people (measured: 45% of remote lines, 4% of speech). Review
    // only when at least two SUBSTANTIAL voices remain.
    const solid = mine.filter((g) => g.count >= 2);
    if (solid.length >= 2) sigs = solid;
  } catch (_) {}
  if (!speakerSheetCtx || speakerSheetCtx.id !== id || speakerSheetCtx.name !== name) return;
  speakerSheetCtx.gallery = Array.isArray(gallery) ? gallery : [];
  speakerSheetCtx.sigs = sigs;
  renderSpeakerSheet();
}

function renderSpeakerSheet() {
  if (!speakerSheetCtx) return;
  const { name, lineCtx, gallery, sigs } = speakerSheetCtx;
  const body = document.getElementById('speaker-sheet-body');
  const colors = meetingSpeakerColors();
  const color = colors[name] || SPEAKER_COLORS[0];
  const info = meetingSpeakersCache.find((s) => s.name === name) || {};
  const st = speakerStats()[name] || { lines: 0, words: 0 };
  const others = meetingSpeakersCache.filter((s) => s.name !== name).map((s) => s.name);
  const galleryChips = gallery.filter((g) => g !== name);
  const moveTargets = [...others, ...galleryChips.filter((g) => !others.includes(g))];

  const chip = (label, act, cls) =>
    `<button class="ss-act inline-flex items-center min-h-9 px-3.5 rounded-full text-sm cursor-pointer transition-colors ${cls || 'bg-surface-container-highest text-on-surface hover:bg-primary/15'}" ${act}>${escapeHtml(label)}</button>`;
  const row = (label, sub, act) =>
    `<button class="ss-act w-full flex items-center justify-between gap-3 min-h-11 text-left cursor-pointer border-t border-outline-variant/15 hover:bg-surface-container-highest/60 px-1 rounded-lg" ${act}>` +
    `<span class="text-sm text-on-surface">${escapeHtml(label)}</span><span class="text-xs text-on-surface-variant shrink-0 truncate max-w-[45%]">${escapeHtml(sub)}</span></button>`;

  let html =
    `<div class="flex items-center gap-3">` +
    `<span class="w-9 h-9 rounded-full flex items-center justify-center text-sm font-bold shrink-0" style="background:${color}; color:#10131c">${escapeHtml(speakerInitial(name, info.unnamed))}</span>` +
    `<div class="min-w-0"><p class="text-sm font-bold text-on-surface truncate">${escapeHtml(name)}</p>` +
    `<p class="text-xs text-on-surface-variant truncate">${st.lines} line${st.lines === 1 ? '' : 's'}${info.sample ? ` · “${escapeHtml(info.sample)}”` : ''}</p></div></div>`;

  if (lineCtx && Array.isArray(lineCtx.lines) && lineCtx.lines.length) {
    html +=
      `<div class="mt-4"><p class="text-xs font-semibold text-on-surface-variant uppercase tracking-wider mb-2">Not ${escapeHtml(name)}? Move this line to</p>` +
      `<div class="flex flex-wrap gap-2">` +
      moveTargets.map((n) => chip(n, `data-act="move-line" data-to="${escapeHtml(n)}"`)).join('') +
      chip('New speaker', 'data-act="move-line-new"') +
      `</div>` +
      `<p class="text-xs text-on-surface-variant/70 mt-1.5 truncate">“${escapeHtml(lineCtx.text || '')}”</p></div>`;
  }

  html +=
    `<div class="mt-4"><p class="text-xs font-semibold text-on-surface-variant uppercase tracking-wider mb-2">${info.unnamed ? 'Who is this?' : 'Rename'}</p>` +
    `<div class="flex flex-wrap gap-2 items-center">` +
    galleryChips.map((n) => chip(n, `data-act="rename" data-to="${escapeHtml(n)}"`, 'bg-primary text-on-primary font-semibold hover:opacity-90')).join('') +
    `<input id="ss-name-input" type="text" placeholder="Type a name…" spellcheck="false" class="min-h-9 px-3 rounded-full bg-surface-container-highest text-sm text-on-surface border border-outline-variant/40 focus:outline-none focus:border-primary/60 w-36" />` +
    `<button class="ss-act min-h-9 px-3.5 rounded-full text-sm font-semibold bg-surface-container-highest text-primary cursor-pointer hover:bg-primary/15" data-act="rename-typed">Save</button>` +
    `</div></div>`;

  html += `<div class="mt-4">`;
  html += row('Show only their lines', 'filters the transcript', 'data-act="filter"');
  if (others.length) {
    html += row('Merge into another speaker', speakerSheetCtx.mergeOpen ? '' : others.slice(0, 3).join(' · ') + (others.length > 3 ? ' · …' : ''), 'data-act="merge-open"');
  }
  if (sigs.length) {
    html += row('Review voices', `${sigs.length} worth checking`, 'data-act="review-open"');
  }
  html += `</div>`;

  if (speakerSheetCtx.mergeOpen && others.length) {
    html +=
      `<div class="mt-1 mb-2"><p class="text-xs text-on-surface-variant mb-2">All of ${escapeHtml(name)}’s lines become:</p>` +
      `<div class="flex flex-wrap gap-2">` + others.map((n) => chip(n, `data-act="merge" data-to="${escapeHtml(n)}"`)).join('') + `</div></div>`;
  }
  if (speakerSheetCtx.reviewOpen && sigs.length) {
    html +=
      `<div class="mt-1 mb-2 space-y-2"><p class="text-xs text-on-surface-variant">Distinct voices inside ${escapeHtml(name)} - move out any that isn’t them:</p>` +
      sigs
        .map(
          (g, i) =>
            `<div class="flex items-center gap-2">` +
            `<span class="text-xs text-on-surface-variant flex-1 min-w-0 truncate">${g.count} lines · “${escapeHtml(g.sample)}”</span>` +
            chip('Someone else', `data-act="sig-out" data-sig="${i}"`) +
            `</div>`
        )
        .join('') +
      `</div>`;
  }
  body.innerHTML = html;
}

// One refresh path after any speaker mutation: transcript, strip, then close.
async function refreshAfterSpeakerChange(id, followRename = null) {
  if (followRename && meetingTranscriptFilter === followRename.from) {
    meetingTranscriptFilter = followRename.to;
  }
  await loadMeetingTranscript(id);
  await renderMeetingSpeakers(id);
  closeSpeakerSheet();
}

async function speakerSheetRename(to) {
  const { id, name } = speakerSheetCtx;
  const target = (to || '').trim();
  if (!target || target === name) return;
  try {
    await invoke('meeting_rename_speaker', { id, from: name, to: target });
    showToast(`Named ${target}`);
    await refreshAfterSpeakerChange(id, { from: name, to: target });
  } catch (err) {
    showToast('Rename failed: ' + err);
  }
}

document.getElementById('speaker-sheet-body').addEventListener('click', async (e) => {
  const btn = e.target.closest('.ss-act');
  if (!btn || !speakerSheetCtx) return;
  const { id, name, lineCtx, sigs } = speakerSheetCtx;
  const act = btn.dataset.act;
  if (act === 'rename') {
    speakerSheetRename(btn.dataset.to);
  } else if (act === 'rename-typed') {
    speakerSheetRename(document.getElementById('ss-name-input').value);
  } else if (act === 'filter') {
    closeSpeakerSheet();
    setTranscriptFilter(name);
  } else if (act === 'merge-open') {
    speakerSheetCtx.mergeOpen = !speakerSheetCtx.mergeOpen;
    renderSpeakerSheet();
  } else if (act === 'review-open') {
    speakerSheetCtx.reviewOpen = !speakerSheetCtx.reviewOpen;
    renderSpeakerSheet();
  } else if (act === 'merge') {
    const to = btn.dataset.to;
    const ok = await showConfirm(`Merge ${name} into ${to}? All their lines become ${to}.`, { okLabel: 'Merge', danger: false });
    if (!ok || !speakerSheetCtx) return;
    try {
      await invoke('meeting_rename_speaker', { id, from: name, to });
      showToast(`Merged into ${to}`);
      await refreshAfterSpeakerChange(id, { from: name, to });
    } catch (err) {
      showToast('Merge failed: ' + err);
    }
  } else if (act === 'move-line' || act === 'move-line-new') {
    const to = act === 'move-line' ? btn.dataset.to : nextSpeakerName(meetingSpeakersCache);
    try {
      await invoke('meeting_reassign_lines', { id, lines: lineCtx.lines, to });
      showToast(`Moved to ${to}`);
      await refreshAfterSpeakerChange(id);
    } catch (err) {
      showToast('Move failed: ' + err);
    }
  } else if (act === 'sig-out') {
    const sig = sigs[Number(btn.dataset.sig)];
    if (!sig) return;
    const to = nextSpeakerName(meetingSpeakersCache);
    try {
      await invoke('meeting_reassign_lines', { id, lines: sig.lines, to });
      showToast(`Moved ${sig.count} lines to ${to} - tap it to name them`);
      if (meetingTranscriptFilter === name) meetingTranscriptFilter = null;
      await refreshAfterSpeakerChange(id);
    } catch (err) {
      showToast('Move failed: ' + err);
    }
  }
});
document.getElementById('speaker-sheet-body').addEventListener('keydown', (e) => {
  if (e.target.id === 'ss-name-input' && e.key === 'Enter') {
    e.preventDefault();
    speakerSheetRename(e.target.value);
  }
});

// ── Meeting settings (Settings > Meeting) ──

let meetingModels = []; // last meeting_models() result
let meetingSelectedSummarizer = ''; // radio selection; not necessarily installed yet

function pkgMeetingStatusLine(d, model) {
  if (d.state === 'downloading') return `Downloading… ${Math.round((d.progress || 0) * 100)}%`;
  // update_available covers a component the manifest gained since install (e.g.
  // the speaker-diarization model), even when the summarizer itself is present.
  if (d.state === 'update_available') return 'Update available — new speaker model';
  if (model && model.installed) return 'Installed';
  return model ? `Not downloaded - ${model.size}` : 'Select a summarizer';
}

function renderPkgMeeting() {
  const btn = document.getElementById('pkg-meeting-btn');
  if (!btn || !pkgStatus) return;
  const d = pkgStatus.meeting || { state: 'unavailable' };
  const model = meetingModels.find(m => m.id === meetingSelectedSummarizer);
  document.getElementById('pkg-meeting-status').textContent = pkgMeetingStatusLine(d, model);
  const progress = document.getElementById('pkg-meeting-progress');
  const downloading = d.state === 'downloading';
  progress.classList.toggle('hidden', !downloading);
  if (downloading) {
    document.getElementById('pkg-meeting-fill').style.width = `${Math.round((d.progress || 0) * 100)}%`;
    btn.disabled = true;
    btn.textContent = `${Math.round((d.progress || 0) * 100)}%`;
  } else if (d.state === 'update_available') {
    // A shared component (e.g. the diarization model) is missing even though the
    // summarizer is present — offer the update so the install can pull it.
    btn.disabled = !model;
    btn.textContent = 'Update';
  } else if (model && model.installed) {
    btn.disabled = true;
    btn.textContent = 'Installed';
  } else {
    btn.disabled = !model;
    btn.textContent = 'Download';
  }
}

function renderMeetingModelList() {
  const list = document.getElementById('meeting-model-list');
  if (!list) return;
  list.innerHTML = meetingModels.map(m => {
    const selected = m.id === meetingSelectedSummarizer;
    const rowClass = selected ? 'ring-1 ring-primary bg-primary/5' : 'hover:bg-surface-container-highest';
    const radio = selected
      ? '<span class="material-symbols-outlined text-primary text-[20px]">radio_button_checked</span>'
      : '<span class="material-symbols-outlined text-on-surface-variant/50 text-[20px]">radio_button_unchecked</span>';
    const recChip = m.recommended
      ? '<span class="text-[10px] font-semibold text-primary bg-primary/10 rounded-full px-2 py-0.5">Recommended</span>'
      : '';
    const installedIcon = m.installed
      ? '<span class="material-symbols-outlined text-on-surface-variant text-[18px]" title="Downloaded">check_circle</span>'
      : '';
    return `
    <div class="meeting-model-row flex items-center gap-3 rounded-lg px-3 py-2.5 cursor-pointer transition-colors ${rowClass}" data-id="${escapeHtml(m.id)}">
      ${radio}
      <div class="min-w-0 flex-1">
        <div class="flex items-center gap-2">
          <span class="text-sm font-medium text-on-surface truncate">${escapeHtml(m.name)}</span>
          ${recChip}
        </div>
        <p class="text-xs text-on-surface-variant mt-0.5">${escapeHtml(m.size)}</p>
      </div>
      ${installedIcon}
    </div>`;
  }).join('');
  list.querySelectorAll('.meeting-model-row').forEach(el => {
    el.addEventListener('click', () => selectSummarizer(el.dataset.id));
  });
  renderPkgMeeting();
}

// Picking an already-downloaded summarizer just switches the active choice
// (save_config); picking one that isn't downloaded waits for the Download
// button below — package_install_meeting is what actually sets it server-side.
async function selectSummarizer(id) {
  meetingSelectedSummarizer = id;
  renderMeetingModelList();
  const model = meetingModels.find(m => m.id === id);
  if (!model || !model.installed) return;
  try {
    const cfg = await invoke('get_config');
    if (cfg.meeting_summarizer !== id) {
      cfg.meeting_summarizer = id;
      await invoke('save_config', { cfg });
    }
  } catch (err) {
    showToast('Save failed: ' + err);
  }
}

async function loadMeetingModels() {
  if (!isDesktop) return;
  try {
    meetingModels = await invoke('meeting_models');
  } catch (err) {
    console.error('Failed to load meeting models:', err);
    meetingModels = [];
  }
  if (!meetingModels.some(m => m.id === meetingSelectedSummarizer)) {
    let active = '';
    try { active = (await invoke('get_config')).meeting_summarizer || ''; } catch (_) {}
    const recommended = meetingModels.find(m => m.recommended);
    meetingSelectedSummarizer = meetingModels.some(m => m.id === active) ? active
      : (recommended ? recommended.id : (meetingModels[0] ? meetingModels[0].id : ''));
  }
  renderMeetingModelList();
}

// Cross-meeting known speakers (Settings > Meeting > Known speakers): the
// enrolled voiceprint gallery, with rename + forget.
async function loadMeetingGallery() {
  if (!isDesktop) return;
  const list = document.getElementById('meeting-gallery-list');
  const empty = document.getElementById('meeting-gallery-empty');
  if (!list) return;
  let names = [];
  try { names = await invoke('meeting_gallery_speakers'); } catch (_) {}
  list.innerHTML = '';
  empty.classList.toggle('hidden', names.length > 0);
  for (const name of names) {
    const row = document.createElement('div');
    row.className = 'flex items-center gap-2 py-1.5';
    const label = document.createElement('span');
    label.className = 'flex-1 min-w-0 truncate text-sm text-on-surface';
    label.textContent = name;
    const rename = document.createElement('button');
    rename.className = 'shrink-0 w-9 h-9 flex items-center justify-center text-on-surface-variant hover:text-primary rounded-lg hover:bg-surface-container-highest transition-colors cursor-pointer';
    rename.title = `Rename ${name}`;
    rename.innerHTML = '<span class="material-symbols-outlined text-[20px]">edit</span>';
    rename.addEventListener('click', async () => {
      const to = await showPrompt('Rename speaker', { value: name, okLabel: 'Rename' });
      if (!to || to === name) return;
      try { await invoke('meeting_gallery_rename', { from: name, to }); loadMeetingGallery(); }
      catch (err) { showToast('Rename failed: ' + err); }
    });
    const forget = document.createElement('button');
    forget.className = 'shrink-0 w-9 h-9 flex items-center justify-center text-on-surface-variant hover:text-error rounded-lg hover:bg-surface-container-highest transition-colors cursor-pointer';
    forget.title = `Forget ${name}`;
    forget.innerHTML = '<span class="material-symbols-outlined text-[20px]">delete</span>';
    forget.addEventListener('click', async () => {
      const ok = await showConfirm(`Forget ${name}? Verba will stop recognizing them in future meetings.`, { okLabel: 'Forget' });
      if (!ok) return;
      try { await invoke('meeting_gallery_forget', { name }); loadMeetingGallery(); }
      catch (err) { showToast('Forget failed: ' + err); }
    });
    row.appendChild(label);
    row.appendChild(rename);
    // Merge is only meaningful with someone to merge into. It reuses the
    // gallery rename, which unions voiceprints when the target already exists.
    if (names.length > 1) {
      const merge = document.createElement('button');
      merge.className = 'shrink-0 w-9 h-9 flex items-center justify-center text-on-surface-variant hover:text-primary rounded-lg hover:bg-surface-container-highest transition-colors cursor-pointer';
      merge.title = `Merge ${name} into another speaker`;
      merge.innerHTML = '<span class="material-symbols-outlined text-[20px]">call_merge</span>';
      merge.addEventListener('click', () => startMergeSpeaker(name, names, row));
      row.appendChild(merge);
    }
    // Expand to the speaker's individual voiceprints (source + sample); a stray
    // one, flagged as an outlier, can be split out into another speaker.
    const panel = document.createElement('div');
    panel.className = 'hidden ml-3 pl-3 border-l-2 border-outline-variant/30 space-y-1.5 pb-1';
    const prints = document.createElement('button');
    prints.className = 'shrink-0 w-9 h-9 flex items-center justify-center text-on-surface-variant hover:text-primary rounded-lg hover:bg-surface-container-highest transition-colors cursor-pointer';
    prints.title = `Voiceprints for ${name}`;
    prints.innerHTML = '<span class="material-symbols-outlined text-[20px]">fingerprint</span>';
    let printsLoaded = false;
    prints.addEventListener('click', async () => {
      panel.classList.toggle('hidden');
      if (!panel.classList.contains('hidden') && !printsLoaded) {
        printsLoaded = true;
        await renderGalleryPrints(panel, name);
      }
    });
    row.appendChild(prints);
    row.appendChild(forget);
    list.appendChild(row);
    list.appendChild(panel);
  }
}

// A known speaker's individual voiceprints with provenance. A stray one (flagged
// as an outlier) can be split out to a new or existing speaker.
async function renderGalleryPrints(panel, name) {
  panel.innerHTML = '<p class="text-xs text-on-surface-variant/60">Loading…</p>';
  let prints = [];
  try { prints = await invoke('meeting_gallery_prints', { name }); } catch (_) {}
  panel.innerHTML = '';
  if (!prints.length) {
    panel.innerHTML = '<p class="text-xs text-on-surface-variant/60">No stored voiceprints.</p>';
    return;
  }
  for (const p of prints) {
    const r = document.createElement('div');
    r.className = 'flex items-center gap-2';
    const info = document.createElement('span');
    info.className = 'flex-1 min-w-0 truncate text-xs ' + (p.outlier ? 'text-primary font-medium' : 'text-on-surface-variant');
    const sample = p.sample ? ` · “${p.sample}”` : '';
    info.textContent = `${p.outlier ? '⚠ ' : ''}${p.source}${sample}`;
    if (p.outlier) info.title = 'Least like the others — likely a different person';
    const split = document.createElement('button');
    split.className = 'shrink-0 min-h-8 px-2.5 text-xs font-semibold text-primary rounded-lg hover:bg-primary/10 transition cursor-pointer';
    split.textContent = 'Split out';
    split.addEventListener('click', async () => {
      const to = await showPrompt(`Split this voiceprint out of ${name} — who is it?`, { okLabel: 'Split', placeholder: 'New speaker name' });
      if (!to) return;
      try {
        await invoke('meeting_gallery_split', { name, indices: [p.index], to });
        showToast(`Split to ${to}`);
        loadMeetingGallery();
      } catch (err) { showToast('Split failed: ' + err); }
    });
    r.appendChild(info);
    r.appendChild(split);
    panel.appendChild(r);
  }
  if (prints.length < 2) {
    const note = document.createElement('p');
    note.className = 'text-xs text-on-surface-variant/60';
    note.textContent = 'Only one voiceprint — nothing to split.';
    panel.appendChild(note);
  }
}

// Merge one known speaker into another (same person enrolled twice). Swaps the
// row for a target picker; confirming unions their voiceprints under the target.
function startMergeSpeaker(from, names, row) {
  const others = names.filter((n) => n !== from);
  if (!others.length) return;
  row.innerHTML = '';
  const lbl = document.createElement('span');
  lbl.className = 'text-sm text-on-surface-variant shrink-0';
  lbl.textContent = `Merge ${from} into`;
  const sel = document.createElement('select');
  sel.className = 'flex-1 min-w-0 bg-surface-container-highest border border-outline-variant/30 rounded-lg px-2 py-1.5 text-sm text-on-surface cursor-pointer';
  for (const n of others) {
    const o = document.createElement('option');
    o.value = n; o.textContent = n;
    sel.appendChild(o);
  }
  const go = document.createElement('button');
  go.className = 'shrink-0 min-h-9 px-3 text-xs font-semibold bg-primary text-on-primary rounded-lg hover:brightness-110 transition cursor-pointer';
  go.textContent = 'Merge';
  go.addEventListener('click', async () => {
    const to = sel.value;
    try {
      await invoke('meeting_gallery_rename', { from, to });
      showToast(`Merged into ${to}`);
    } catch (err) {
      showToast('Merge failed: ' + err);
    }
    loadMeetingGallery();
  });
  const cancel = document.createElement('button');
  cancel.className = 'shrink-0 w-9 h-9 flex items-center justify-center text-on-surface-variant hover:text-on-surface rounded-lg hover:bg-surface-container-highest transition cursor-pointer';
  cancel.innerHTML = '<span class="material-symbols-outlined text-[20px]">close</span>';
  cancel.addEventListener('click', () => loadMeetingGallery());
  row.appendChild(lbl);
  row.appendChild(sel);
  row.appendChild(go);
  row.appendChild(cancel);
}

document.getElementById('pkg-meeting-btn').addEventListener('click', async () => {
  if (!meetingSelectedSummarizer) return;
  const btn = document.getElementById('pkg-meeting-btn');
  btn.disabled = true;
  try {
    await invoke('package_install_meeting', { summarizerId: meetingSelectedSummarizer });
  } catch (err) {
    showToast('Install failed: ' + err);
  }
  await loadPackagesStatus();
  await loadMeetingModels();
});

// Folder pickers via tauri-plugin-dialog (desktop-only, registered in
// lib.rs under cfg(desktop)). Feature-detect the global anyway so the harness
// and any build without the plugin fall back to typing into the text input.
function wireMeetingDirBrowse(inputId, btnId) {
  const openDialog = window.__TAURI__.dialog?.open;
  if (!openDialog) return;
  const btn = document.getElementById(btnId);
  btn.classList.remove('hidden');
  btn.addEventListener('click', async () => {
    const input = document.getElementById(inputId);
    const dir = await openDialog({ directory: true, defaultPath: input.value || undefined });
    if (typeof dir === 'string') {
      input.value = dir;
      await saveConfig();
    }
  });
}
wireMeetingDirBrowse('cfg-meeting-transcript-dir', 'cfg-meeting-transcript-browse');
wireMeetingDirBrowse('cfg-meeting-summary-dir', 'cfg-meeting-summary-browse');

// ── Debug tab ──

const logOutput = document.getElementById('log-output');

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

// Backend-caught crashes (TTS/transcription) are emitted here so they surface
// in the debug log instead of taking the app down silently.
listen('dictation-error', (event) => {
  const msg = typeof event.payload === 'string' ? event.payload : JSON.stringify(event.payload);
  const el = document.createElement('div');
  el.textContent = '[error] ' + msg;
  el.className = 'text-red-400';
  logOutput.appendChild(el);
  logOutput.scrollTop = logOutput.scrollHeight;
  showToast('Error: ' + msg);
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
// read the DOM directly rather than `activeTab` — openReading/openBook/
// openFeedEntries call showPanel() directly and never update `activeTab`
// (only navigateTo does), so it does not track these views.
const BACK_OVERLAYS = [
  { el: document.getElementById('now-playing'), close: () => closeNowPlaying() },
  { el: document.getElementById('confirm-dialog'), close: () => document.getElementById('confirm-cancel').click() },
  { el: queueSheet, close: closeQueueSheet },
  { el: libActionSheet, close: closeLibActionSheet },
  { el: voiceSheet, close: closeVoiceSheet },
  { el: speedSheet, close: closeSpeedSheet },
  { el: moreSheet, close: closeMoreSheet },
  { el: addChooserModal, close: () => closeModal(addChooserModal) },
  { el: libAddModal, close: closeAddModal },
  { el: addUrlModal, close: closeUrlModal },
  { el: feedAddModal, close: closeFeedAddModal },
  { el: feedEditModal, close: closeFeedEditModal },
  { el: document.getElementById('snippet-picker'), close: closeSnippetPicker },
  { el: document.getElementById('snippet-wizard'), close: () => document.getElementById('wiz-cancel').click() },
  { el: document.getElementById('speaker-sheet'), close: () => closeSpeakerSheet() },
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
  if (!document.getElementById('book-chapters').classList.contains('hidden')) {
    navigateTo('library');
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
  // Show desktop-only UI elements when not on Android
  if (isDesktop) {
    // Settings > Speak > Dictation rows toggle via the `hidden` attribute so
    // the card's divide-y only separates rows that are actually shown. The mic
    // picker moved here from the old Audio page; haptics need a vibration
    // motor, so that row stays Android-only.
    const showRow = (id, show) => { const el = document.getElementById(id); if (el) el.hidden = !show; };
    showRow('hotkey-row', true);
    showRow('audio-device-row', true);
    showRow('haptic-row', false);
    // Meeting is a desktop-only third mode: reveal its pill button and its
    // Settings group (both ship hidden so Android stays a two-mode app).
    document.getElementById('mode-meeting-btn')?.classList.remove('hidden');
    document.getElementById('meeting-settings-group')?.classList.remove('hidden');
    // The thumb ships sized for two modes (50%); with three it must be a third
    // so translateX(idx*100%) lands under each button (see setMode). The p-1
    // track padding is 0.25rem, split three ways -> 0.1667rem.
    const mt = document.getElementById('mode-thumb');
    if (mt) mt.style.width = 'calc(33.333% - 0.1667rem)';
  }

  await loadHistory();
  await loadAudioDevices();
  await loadConfig();
  await loadVocab();
  await loadSnippets();

  setMode('speak');

  // Restore a meeting that was still recording when the webview reloaded; this
  // switches to Meeting mode and reopens the live view when one is found.
  checkMeetingRehydrate();

  if (!engineReady && await invoke('is_engine_ready')) {
    engineReady = true;
  }

  // A URL/text shared to the app before the webview was ready waits in Rust;
  // pull it now that everything (incl. the library) is loaded.
  try {
    const shared = await invoke('take_shared_text');
    if (shared) importSharedText(shared);
  } catch (_) {}

  // Check RSS feeds for new articles. Fire-and-forget: never blocks startup,
  // silent when offline.
  pollFeeds({}).catch(() => {});

  // Proactive model-update check: highlights a newly published or missing model
  // (like the meeting diarization model) so users don't have to hunt in Settings.
  checkPackageUpdatesOnStartup();
});

// A share arriving while the app is already open: Rust emits this once it has
// stashed the text; go back through take_shared_text so cold and warm shares
// use one consumption path (delivered exactly once).
listen('shared-text', () => {
  invoke('take_shared_text').then(t => { if (t) importSharedText(t); }).catch(() => {});
});
