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
const modeDefaultTab = { speak: 'history', listen: 'library' };
let currentMode = 'speak';
let activeTab = null;

// Detail views hide the bottom nav (and drop the player bar to the screen edge).
const DETAIL_PANELS = new Set(['reading', 'feed-entries', 'book-chapters']);

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
    { tab: 'models', label: 'Models', icon: 'layers' },
    { tab: 'general', label: 'Settings', icon: 'settings' },
    { overflow: true, label: 'More', icon: 'more_horiz' },
  ],
};
// Pages reachable only through the More sheet, per mode.
const MORE_ITEMS = {
  listen: [
    { tab: 'reports', label: 'Reports', icon: 'flag' },
    { tab: 'debug', label: 'Debug', icon: 'bug_report' },
  ],
  speak: [
    ...(isDesktop ? [{ tab: 'audio', label: 'Audio', icon: 'mic' }] : []),
    { tab: 'debug', label: 'Debug', icon: 'bug_report' },
  ],
};

const bottomNav = document.getElementById('bottom-nav');
const moreSheet = document.getElementById('nav-more-sheet');

// Per-tab data refreshers, run when a tab is shown.
const TAB_LOADERS = {
  history: () => loadHistory(),
  models: () => loadModels(),
  general: () => loadVocab(),
  snippets: () => loadSnippets(),
  library: () => loadLibrary(),
  feeds: () => loadFeeds(),
  voices: () => loadVoices(),
  reports: () => loadReports(),
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
    return `<button class="nav-slot flex-1 flex flex-col items-center justify-center gap-0.5 py-1.5 ${color} active:opacity-70 transition-colors" ${attr}>
      <span class="material-symbols-outlined text-[22px]" style="font-variation-settings:'FILL' ${active ? 1 : 0}">${it.icon}</span>
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
  if (slot.dataset.overflow) { openMoreSheet(); return; }
  if (slot.dataset.tab) navigateTo(slot.dataset.tab);
});

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

function setMode(mode) {
  currentMode = mode;
  if (modeThumb) modeThumb.style.transform = mode === 'listen' ? 'translateX(100%)' : 'translateX(0)';
  document.querySelectorAll('.mode-btn').forEach(b => {
    const active = b.dataset.mode === mode;
    b.classList.toggle('text-primary', active);
    b.classList.toggle('text-on-surface-variant', !active);
    const icon = b.querySelector('.material-symbols-outlined');
    if (icon) icon.style.fontVariationSettings = `'FILL' ${active ? 1 : 0}`;
  });
  renderBottomNav();
  navigateTo(modeDefaultTab[mode]);
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
  for (const entry of [...entries].reverse()) {
    const card = document.createElement('div');
    card.className = 'bg-surface-container-low rounded-xl border border-outline-variant/20 p-4';

    const hasStages = entry.pipeline_stages && entry.pipeline_stages.length > 1;
    const hasChunks = entry.chunk_timings && entry.chunk_timings.length > 0;
    const hasDetails = hasStages || hasChunks;

    const stats = [
      formatTimestamp(entry.timestamp),
      formatAudioDuration(entry.audio_duration_ms),
      formatDuration(entry.duration_ms) + ' to transcribe',
      entry.postprocess_ms ? entry.postprocess_ms + 'ms postprocess' : null,
      formatSpeed(entry),
      escapeHtml(entry.model_id),
    ].filter(Boolean);

    const toggleBtn = hasDetails
      ? '<button class="pipeline-toggle text-[10px] font-semibold text-primary/70 hover:text-primary transition-colors cursor-pointer">Details</button>'
      : '';

    card.innerHTML = `
      <p class="text-sm text-on-surface leading-relaxed mb-2 select-text">${escapeHtml(entry.text)}</p>
      <div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-on-surface-variant">
        ${stats.map(s => '<span>' + s + '</span>').join('')}
        ${toggleBtn}
        <button class="copy-entry-btn text-[10px] font-semibold text-primary/70 hover:text-primary transition-colors cursor-pointer">Copy</button>
      </div>
      ${renderPipelineStages(entry.pipeline_stages, entry.chunk_timings)}`;

    if (hasDetails) {
      card.querySelector('.pipeline-toggle').addEventListener('click', (e) => {
        const stagesEl = card.querySelector('.pipeline-stages');
        stagesEl.classList.toggle('hidden');
        e.target.textContent = stagesEl.classList.contains('hidden') ? 'Details' : 'Hide';
      });
    }

    card.querySelector('.copy-entry-btn').addEventListener('click', (e) => {
      const text = formatEntryForCopy(entry);
      invoke('copy_to_clipboard', { text }).then(() => {
        e.target.textContent = 'Copied!';
        setTimeout(() => { e.target.textContent = 'Copy'; }, 1500);
      });
    });

    historyList.appendChild(card);
  }
}

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
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
  for (const entry of [...entries].reverse()) {
    const row = document.createElement('div');
    row.className = 'flex items-center justify-between gap-3 bg-surface-container-low rounded-xl border border-outline-variant/20 px-4 py-3';
    const when = entry.reported_at_ms ? new Date(entry.reported_at_ms).toLocaleString() : '';
    row.innerHTML = `
      <div class="min-w-0 flex-1">
        <span class="text-sm font-medium text-on-surface truncate select-text block">${escapeHtml(entry.word)}</span>
        ${entry.voice ? `<span class="text-[10px] text-on-surface-variant/70 truncate block mt-0.5">${escapeHtml(entry.voice)}</span>` : ''}
      </div>
      <span class="text-[11px] text-on-surface-variant tabular-nums shrink-0">${escapeHtml(when)}</span>`;
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

// ── Models tab ──

function renderModelRow(model) {
  const isActive = model.status === 'active';
  const isDownloaded = model.status === 'downloaded';
  const isDownloading = model.status === 'downloading';

  const deleteBtn = `<button class="del-btn text-on-surface-variant hover:text-error transition-colors cursor-pointer p-1.5 rounded-lg hover:bg-error/10" data-id="${model.id}" title="Delete"><span class="material-symbols-outlined text-base">delete</span></button>`;

  let actionHtml;
  if (isActive) {
    actionHtml = `<span class="text-xs text-primary font-semibold px-3 py-1.5 bg-primary/10 rounded-lg">Active</span>${deleteBtn}`;
  } else if (isDownloaded) {
    actionHtml = `<button class="use-btn text-xs font-semibold px-3 py-1.5 bg-primary text-on-primary rounded-lg hover:brightness-110 transition-all cursor-pointer" data-id="${model.id}">Use</button>${deleteBtn}`;
  } else if (isDownloading) {
    const pct = Math.round(model.progress * 100);
    actionHtml = `
      <div class="w-28" id="progress-${model.id}">
        <div class="progress-bar"><div class="progress-bar-fill" style="width:${pct}%"></div></div>
        <span class="text-[10px] text-on-surface-variant mt-1 block text-right">${pct}%</span>
      </div>`;
  } else {
    actionHtml = `<button class="dl-btn text-xs font-semibold px-3 py-1.5 border border-outline-variant/30 text-on-surface rounded-lg hover:bg-surface-container-highest transition-colors cursor-pointer" data-id="${model.id}">Download</button>`;
  }

  return `
    <div class="flex items-center justify-between px-4 py-3" data-model-id="${model.id}">
      <div class="flex items-center gap-3 min-w-0 flex-1 mr-4">
        <span class="material-symbols-outlined text-lg ${isActive ? 'text-primary' : 'text-on-surface-variant'}">layers</span>
        <div class="min-w-0">
          <div class="text-sm font-medium ${isActive ? 'text-primary' : 'text-on-surface'}">${model.name}</div>
          <div class="text-xs text-on-surface-variant mt-0.5">${model.desc}</div>
        </div>
      </div>
      <div class="flex items-center gap-3 shrink-0">
        <span class="text-xs font-mono text-on-surface-variant">${model.size}</span>
        ${actionHtml}
      </div>
    </div>`;
}

async function loadModels() {
  const models = await invoke('list_models');

  document.getElementById('whisper-models').innerHTML = models
    .filter(m => m.engine === 'whisper')
    .map(renderModelRow)
    .join('');

  document.getElementById('parakeet-models').innerHTML = models
    .filter(m => m.engine === 'parakeet')
    .map(renderModelRow)
    .join('');

  document.getElementById('zipformer-models').innerHTML = models
    .filter(m => m.engine === 'zipformer')
    .map(renderModelRow)
    .join('');

  document.getElementById('conformer-models').innerHTML = models
    .filter(m => m.engine === 'conformer_ctc')
    .map(renderModelRow)
    .join('');

  // Attach download button handlers
  document.querySelectorAll('.dl-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      const id = btn.dataset.id;
      btn.disabled = true;
      btn.textContent = 'Starting...';
      btn.classList.add('opacity-50');

      try {
        await invoke('download_model', { id });
      } catch (err) {
        console.error('Download failed:', err);
        showToast(`Download failed: ${err}`);
      }

      // Refresh model list after download completes or fails
      await loadModels();
    });
  });

  // Attach use button handlers
  document.querySelectorAll('.use-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      if (!engineReady) {
        showToast('Engine still loading...');
        return;
      }
      const id = btn.dataset.id;
      try {
        await invoke('switch_model', { id });
        await loadModels();
      } catch (err) {
        console.error('Switch failed:', err);
        showToast('Failed to switch model: ' + err);
      }
    });
  });

  // Attach delete button handlers
  document.querySelectorAll('.del-btn').forEach(btn => {
    btn.addEventListener('click', async () => {
      const id = btn.dataset.id;
      if (!await showConfirm(`Delete model ${id}?`)) return;
      try {
        await invoke('delete_model', { id });
      } catch (err) {
        console.error('Delete failed:', err);
        showToast(`Failed to delete model: ${err}`);
      }
      await loadModels();
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

  const container = document.querySelector(`[data-model-id="${id}"]`);
  if (!container) return;

  // Replace button with progress bar if not already showing
  let progressEl = document.getElementById(`progress-${id}`);
  if (!progressEl) {
    const actionArea = container.querySelector('.dl-btn, .use-btn');
    if (actionArea) {
      const wrapper = document.createElement('div');
      wrapper.className = 'w-28';
      wrapper.id = `progress-${id}`;
      wrapper.innerHTML = `
        <div class="progress-bar"><div class="progress-bar-fill" style="width:${pct}%"></div></div>
        <span class="text-[10px] text-on-surface-variant mt-1 block text-right">${pct}%</span>`;
      actionArea.replaceWith(wrapper);
      progressEl = wrapper;
    }
  } else {
    const fill = progressEl.querySelector('.progress-bar-fill');
    const label = progressEl.querySelector('span');
    if (fill) fill.style.width = `${pct}%`;
    if (label) label.textContent = `${pct}%`;
  }
});

listen('download-complete', async () => {
  await loadModels();
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
  loadModels();
});

listen('model-error', (event) => {
  if (!event.payload?.native_toast) {
    showToast('Model load failed: ' + (event.payload?.error || 'unknown error'));
  }
  loadModels();
});

// ── Audio tab ──

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

// ── General tab ──

async function loadConfig() {
  const cfg = await invoke('get_config');
  document.getElementById('cfg-haptic').checked = cfg.haptic_feedback;
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
}

async function saveConfig() {
  const cfg = await invoke('get_config');
  cfg.device_index = parseInt(document.getElementById('audio-device').value, 10);
  cfg.haptic_feedback = document.getElementById('cfg-haptic').checked;
  cfg.threads = parseInt(document.getElementById('cfg-threads').value, 10);
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

  for (const snippet of items) {
    const row = document.createElement('div');
    row.className = 'flex items-start justify-between gap-3 px-4 py-3';
    row.innerHTML = `
      <div class="min-w-0 flex-1 snippet-edit-target cursor-pointer" data-id="${escapeHtml(snippet.id)}">
        <div class="flex flex-wrap gap-1 mb-1">
          ${snippet.triggers.map(t =>
            `<span class="text-xs font-mono bg-primary/10 text-primary px-2 py-0.5 rounded">${escapeHtml(t)}</span>`
          ).join('')}
        </div>
        <p class="text-sm text-on-surface leading-relaxed">${escapeHtml(snippet.body)}</p>
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

function voiceRowHtml(v) {
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
    ? `<span class="block text-[11px] text-on-surface-variant/70 truncate">${hintBits.join(' \u{b7} ')}</span>`
    : '';
  const right = inUse
    ? '<span class="text-[11px] font-semibold text-primary shrink-0">In use</span>'
    : key === pendingUseKey
      ? `<span class="text-[11px] font-semibold text-on-surface-variant shrink-0" data-voice-progress="${escapeHtml(v.model)}">0%</span>`
      : `<button class="voice-use shrink-0 text-xs font-semibold text-on-surface-variant hover:text-primary transition-colors px-2 py-1 rounded-lg hover:bg-primary/10 cursor-pointer" data-model="${escapeHtml(v.model)}" data-sid="${v.sid}">Use</button>`;
  return `
    <div class="voice-row flex items-center justify-between gap-2 px-4 py-2.5" data-key="${escapeHtml(key)}">
      <button class="voice-sample flex items-center gap-3 min-w-0 flex-1 text-left cursor-pointer" data-model="${escapeHtml(v.model)}" data-sid="${v.sid}">
        <span class="material-symbols-outlined text-xl ${playing ? 'text-primary' : 'text-on-surface-variant'}">${playing ? 'graphic_eq' : 'play_circle'}</span>
        <span class="min-w-0">
          <span class="block text-sm text-on-surface truncate">${escapeHtml(v.label)}</span>
          ${hint}
        </span>
      </button>
      ${right}
      <button class="voice-fav shrink-0 p-1 cursor-pointer ${fav ? 'text-primary' : 'text-on-surface-variant/40'}" data-key="${escapeHtml(key)}">
        <span class="material-symbols-outlined text-lg" style="font-variation-settings:'FILL' ${fav ? 1 : 0}">star</span>
      </button>
    </div>`;
}

function rebuildFavSection() {
  const favSection = document.getElementById('voices-fav-section');
  const favList = document.getElementById('voices-fav-list');
  const rows = ttsFavourites
    .map(key => {
      const v = voiceByKey(key);
      // Stale keys (unknown voice or its backing file left the registry)
      // stay in config but don't render.
      return v && modelById(v.model) ? voiceRowHtml(v) : '';
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
        <div class="bg-surface-container-low rounded-xl overflow-hidden divide-y divide-white/5">${rows.map(voiceRowHtml).join('')}</div>
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
const PLAYER_PAD = '7rem';

function fmtTime(ms) {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

// Estimated spoken duration of `text` at a given speed. Two parts: speech time
// scales with speed (length_scale ~ 1/speed), but the silence spliced at
// punctuation (see piper.rs) is added in PCM at fixed lengths and does NOT scale
// with speed. Keeping them separate is what makes the estimate hold up at slow
// speeds (the old word-count/speed estimate doubled the pauses too). Approximate
// — used for the library time-left and the ready-state bar before generation.
// Calibrated against a real article (950 words, 0.75x measured 5:40): Piper
// reads faster than a naive ~165 wpm, and the spliced pauses overlap/collapse so
// they count for less than their raw splice lengths. These are rough — the real
// duration is measured and saved after the first full play (itemDurationMs).
const SPEAK_WPM = 255;
function estDurationMsForText(text, speed) {
  if (!text || !text.trim()) return 0;
  const words = text.trim().split(/\s+/).filter(Boolean).length;
  const spokenMs = words / SPEAK_WPM * 60000 / (speed || 1);
  const sentences = (text.match(/[.!?…]+/g) || []).length;
  const clauses = (text.match(/[,;:–—]/g) || []).length;
  const paragraphs = (text.match(/\n+/g) || []).length;
  const pauseMs = sentences * 300 + clauses * 150 + paragraphs * 500;
  return Math.round(spokenMs + pauseMs);
}

// Word-count-only variant of estDurationMsForText, for a book chapter row:
// only ChapterMeta.words is available there (the chapter body isn't loaded
// just to render the list), so there's no punctuation to estimate pauses from.
function estMsForWords(words, speed) {
  if (!words) return 0;
  return Math.round(words / SPEAK_WPM * 60000 / (speed || 1));
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

// Find where a segment's words line up in readingWords, searching outward from
// the expected cursor. Returns the matched start index (or the cursor unchanged
// if no confident match), so per-segment word-count mismatches self-correct
// each segment instead of accumulating.
function anchorTiming(words, cursor) {
  const R = 12;
  for (let d = 0; d <= R; d++) {
    for (const cand of (d === 0 ? [cursor] : [cursor + d, cursor - d])) {
      if (cand < genBaseWord || cand >= readingWords.length || !readingWords[cand]) continue;
      // Match the first word; for a 1-word segment also require a non-ambiguous
      // hit by checking the next word, to avoid latching onto a common word.
      if (readingWords[cand].text === words[0] &&
          (words.length < 2 || !readingWords[cand + 1] || readingWords[cand + 1].text === words[1])) {
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

progressTrack.addEventListener('mousedown', (e) => { ttsSeeking = true; seekFromPointer(e); });
progressTrack.addEventListener('touchstart', (e) => { ttsSeeking = true; seekFromPointer(e); }, { passive: true });
document.addEventListener('mousemove', (e) => { if (ttsSeeking) seekFromPointer(e); });
document.addEventListener('touchmove', (e) => { if (ttsSeeking) seekFromPointer(e); }, { passive: true });
document.addEventListener('mouseup', () => { if (ttsSeeking) { ttsSeeking = false; seekToFull(pendingSeekMs); } });
document.addEventListener('touchend', () => { if (ttsSeeking) { ttsSeeking = false; seekToFull(pendingSeekMs); } });

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
// counts): chapters before the current one count fully, the current one
// counts by its character-offset progress within itself.
function bookProgress(it) {
  const totalWords = it.chapters.reduce((sum, c) => sum + c.words, 0);
  const cur = Math.min(it.current_chapter, it.chapters.length - 1);
  const wordsBefore = it.chapters.slice(0, cur).reduce((sum, c) => sum + c.words, 0);
  const curChapter = it.chapters[cur];
  const progressFrac = curChapter && curChapter.chars ? Math.min(1, it.progress / curChapter.chars) : 0;
  const wordsRead = wordsBefore + progressFrac * (curChapter ? curChapter.words : 0);
  return { totalWords, wordsRead, pct: totalWords ? Math.round(wordsRead / totalWords * 100) : 0 };
}

async function loadLibrary() {
  const items = await invoke('library_list');
  // Feed titles for the source badge on imported articles (cheap local JSON).
  const feedsById = {};
  try {
    (await invoke('feeds_list')).forEach(f => { feedsById[f.id] = f; });
  } catch (_) {}
  const list = document.getElementById('lib-list');
  const empty = document.getElementById('lib-empty');
  empty.classList.toggle('hidden', items.length > 0);
  list.innerHTML = items.slice().reverse().map(it => {
    if (it.chapters && it.chapters.length) {
      const { totalWords, wordsRead, pct } = bookProgress(it);
      const chaptersLabel = `${it.chapters.length} chapter${it.chapters.length === 1 ? '' : 's'}`;
      const meta = pct >= 100 ? `${chaptersLabel} · Finished`
        : pct > 0 ? `${chaptersLabel} · ${pct}% · ${fmtMins(estMsForWords(totalWords - wordsRead, ttsSpeed))} left`
        : `${chaptersLabel} · ${fmtMins(estMsForWords(totalWords, ttsSpeed))}`;
      return `
      <div class="lib-item flex items-center justify-between gap-3 bg-surface-container-low rounded-xl px-4 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-id="${escapeHtml(it.id)}">
        <div class="min-w-0 flex-1">
          <div class="text-sm font-medium text-on-surface truncate"><span class="material-symbols-outlined text-sm align-middle mr-1 text-on-surface-variant/70">menu_book</span>${escapeHtml(it.title)}</div>
          <div class="text-[11px] ${pct >= 100 ? 'text-primary' : 'text-on-surface-variant'} tabular-nums mt-1">${meta}</div>
        </div>
        <button class="lib-del shrink-0 text-on-surface-variant/50 hover:text-error transition-colors p-1 cursor-pointer" data-id="${escapeHtml(it.id)}">
          <span class="material-symbols-outlined text-lg">delete</span>
        </button>
      </div>`;
    }
    const len = (it.body || '').length || 1;
    const prog = Math.max(0, Math.min(len, it.progress || 0));
    const pct = Math.round(prog / len * 100);
    // Real measured duration if we have it, else an estimate; at the current
    // playback speed so the list matches the reader.
    const totalMs = itemDurationMs(it, ttsSpeed);
    const leftMs = totalMs * (1 - prog / len);
    // Finished -> "Finished"; in-progress -> "42% · 6 min left"; fresh -> length.
    let meta = pct >= 100 ? 'Finished'
      : pct > 0 ? `${pct}% · ${fmtMins(leftMs)} left`
      : fmtMins(totalMs);
    // Publication date, when the source provided one.
    const pub = fmtPubDate(it.published);
    if (pub) meta += ` · ${pub}`;
    // Feed provenance: source badge (feed title, else hostname once the feed
    // is deleted) and a NEW chip until the article is first listened to.
    if (it.feed_id) {
      const feed = feedsById[it.feed_id];
      let src = feed ? feed.title : '';
      if (!src && it.url) { try { src = new URL(it.url).hostname; } catch (_) {} }
      if (src) meta += ` · ${escapeHtml(src)}`;
    }
    const newChip = (it.feed_id && !(it.progress > 0))
      ? '<span class="text-[10px] font-semibold text-primary bg-primary/10 rounded px-1.5 py-0.5 mr-1.5">NEW</span>'
      : '';
    return `
    <div class="lib-item flex items-center justify-between gap-3 bg-surface-container-low rounded-xl px-4 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-id="${escapeHtml(it.id)}">
      <div class="min-w-0 flex-1">
        <div class="text-sm font-medium text-on-surface truncate">${newChip}${escapeHtml(it.title)}</div>
        <div class="text-xs text-on-surface-variant truncate mt-0.5">${escapeHtml(it.body.slice(0, 80))}</div>
        <div class="text-[11px] ${pct >= 100 ? 'text-primary' : 'text-on-surface-variant'} tabular-nums mt-1">${meta}</div>
      </div>
      <button class="lib-del shrink-0 text-on-surface-variant/50 hover:text-error transition-colors p-1 cursor-pointer" data-id="${escapeHtml(it.id)}">
        <span class="material-symbols-outlined text-lg">delete</span>
      </button>
    </div>`;
  }).join('');
  const bookIds = new Set(items.filter(it => it.chapters && it.chapters.length).map(it => it.id));
  list.querySelectorAll('.lib-item').forEach(el => {
    el.addEventListener('click', (e) => {
      if (e.target.closest('.lib-del')) return;
      const id = el.dataset.id;
      if (bookIds.has(id)) openBook(id); else openReading(id);
    });
  });
  list.querySelectorAll('.lib-del').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const id = btn.dataset.id;
      // A book's audio is keyed off its chapter bodies, which library_delete
      // also removes (books/<id>.json) — forget the cache first or it leaks.
      if (bookIds.has(id) && ttsModelId) {
        const voiceVal = ttsVoice || '0';
        const sid = voiceVal.startsWith('custom:') ? 0 : (parseInt(voiceVal) || 0);
        await invoke('book_forget_audio', { id, modelId: ttsModelId, sid, speed: ttsSpeed }).catch(() => {});
      }
      await invoke('library_delete', { id });
      loadLibrary();
    });
  });
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
      item = await invoke('library_add', { title, body, url: url || null, published });
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

async function openReading(id) {
  bookState = null; // opening a plain article, not continuing a book chapter
  let item;
  try { item = await invoke('library_get', { id }); }
  catch (err) { showToast('Open failed: ' + err); return; }
  if (!item) { showToast('Text not found'); return; }
  readingItem = item;
  readingText = item.body;
  activeWord = -1; genBaseWord = 0; timingCursor = 0; wordTimes = {};
  document.getElementById('reading-title').textContent = item.title;
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
  listEl.innerHTML = item.chapters.map((ch, idx) => {
    const done = idx < item.current_chapter;
    const current = idx === item.current_chapter;
    const currentChip = current
      ? '<span class="text-[10px] font-semibold text-primary bg-primary/10 rounded px-1.5 py-0.5 mr-1.5">CURRENT</span>'
      : '';
    const doneIcon = done
      ? '<span class="material-symbols-outlined text-lg text-primary shrink-0">check_circle</span>'
      : '';
    return `
    <div class="lib-item book-chapter-row flex items-center justify-between gap-3 bg-surface-container-low rounded-xl px-4 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-idx="${idx}">
      <div class="min-w-0 flex-1">
        <div class="text-sm font-medium text-on-surface truncate">${currentChip}${idx + 1}. ${escapeHtml(ch.title || `Chapter ${idx + 1}`)}</div>
        <div class="text-[11px] text-on-surface-variant tabular-nums mt-1">${fmtMins(estMsForWords(ch.words, ttsSpeed))}</div>
      </div>
      ${doneIcon}
    </div>`;
  }).join('');
  listEl.querySelectorAll('.book-chapter-row').forEach(el => {
    el.addEventListener('click', () => openBookChapter(item, Number(el.dataset.idx)));
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

  const forward = idx > item.current_chapter;
  const changedChapter = idx !== item.current_chapter;
  bookState = { id: item.id, chapters: item.chapters, current: idx };
  readingItem = item;
  readingText = body;
  activeWord = -1; genBaseWord = 0; timingCursor = 0; wordTimes = {};
  const chapterTitle = item.chapters[idx].title || `Chapter ${idx + 1}`;
  document.getElementById('reading-title').textContent = `${item.title} — ${chapterTitle}`;
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

  if (autoplay) startSpeak(readingText, 0, 0);
}

document.getElementById('reading-back').addEventListener('click', () => {
  saveProgress(true);
  invoke('tts_stop');
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
  label.textContent = ttsVoice.startsWith('custom:') ? ttsVoice.slice(7) : (ttsVoice || '0');
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

// The bottom-nav + button and the Library page button open a source chooser
// (Text, URL, File, eBook).
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

document.getElementById('lib-add-btn').addEventListener('click', openAddModal);
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
  list.innerHTML = feeds.slice().reverse().map(f => {
    const checked = f.last_checked ? formatTimestamp(f.last_checked) : 'never';
    const meta = `Checked ${checked} · Auto-add ${f.auto_add ? 'on' : 'off'}`;
    return `
    <div class="feed-item flex items-center justify-between gap-3 bg-surface-container-low rounded-xl px-4 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-id="${escapeHtml(f.id)}">
      <div class="min-w-0 flex-1">
        <div class="text-sm font-medium text-on-surface truncate">${escapeHtml(f.title)}</div>
        <div class="text-xs text-on-surface-variant truncate mt-0.5">${escapeHtml(f.url)}</div>
        <div class="text-[11px] text-on-surface-variant tabular-nums mt-1">${meta}</div>
      </div>
      <button class="feed-del shrink-0 text-on-surface-variant/50 hover:text-error transition-colors p-1 cursor-pointer" data-id="${escapeHtml(f.id)}">
        <span class="material-symbols-outlined text-lg">delete</span>
      </button>
    </div>`;
  }).join('');
  list.querySelectorAll('.feed-item').forEach(el => {
    el.addEventListener('click', (e) => {
      if (e.target.closest('.feed-del')) return;
      openFeedEntries(el.dataset.id);
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
      ? '<span class="text-[11px] font-semibold text-primary shrink-0">Added</span>'
      : `<button class="feed-entry-add shrink-0 text-on-surface-variant hover:text-primary transition-colors p-1 cursor-pointer" data-i="${i}">
           <span class="material-symbols-outlined text-lg">add_circle</span>
         </button>`;
    return `
    <div class="feed-entry flex items-center justify-between gap-3 bg-surface-container-low rounded-xl px-4 py-3 ${itemId ? 'cursor-pointer hover:bg-surface-container-high transition-colors' : ''}" data-item-id="${escapeHtml(itemId)}">
      <div class="min-w-0 flex-1">
        <div class="text-sm font-medium text-on-surface truncate">${escapeHtml(e.title || e.link || 'Untitled')}</div>
        ${date ? `<div class="text-[11px] text-on-surface-variant tabular-nums mt-1">${escapeHtml(date)}</div>` : ''}
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
      btn.innerHTML = '<span class="material-symbols-outlined text-lg tts-spin">progress_activity</span>';
      try {
        await importFeedEntry(feed, entry);
        showToast('Article added');
        renderFeedEntries(feed);
      } catch (err) {
        showToast('Could not add: ' + err);
        btn.disabled = false;
        btn.innerHTML = '<span class="material-symbols-outlined text-lg">add_circle</span>';
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

// ── Init ──

document.addEventListener('DOMContentLoaded', async () => {
  // Show desktop-only UI elements when not on Android
  if (isDesktop) {
    document.getElementById('hotkey-row')?.classList.remove('hidden');
  }

  await loadHistory();
  await loadModels();
  await loadAudioDevices();
  await loadConfig();
  await loadVocab();
  await loadSnippets();

  setMode('speak');

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
});

// A share arriving while the app is already open: Rust emits this once it has
// stashed the text; go back through take_shared_text so cold and warm shares
// use one consumption path (delivered exactly once).
listen('shared-text', () => {
  invoke('take_shared_text').then(t => { if (t) importSharedText(t); }).catch(() => {});
});
