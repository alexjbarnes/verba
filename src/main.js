const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let engineReady = false;

// ── Confirm dialog (window.confirm doesn't work in WKWebView) ──

function showConfirm(message) {
  return new Promise((resolve) => {
    const dialog = document.getElementById('confirm-dialog');
    document.getElementById('confirm-msg').textContent = message;
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

// ── Sidebar & navigation ──

const sidebar = document.getElementById('sidebar');
const sidebarOverlay = document.getElementById('sidebar-overlay');
const pageTitle = document.getElementById('page-title');

const tabLabels = {
  history: 'History', models: 'Models', audio: 'Audio',
  snippets: 'Snippets', reader: 'Reader', general: 'Settings', debug: 'Debug',
};

function openSidebar() {
  sidebar.classList.remove('-translate-x-full');
  sidebarOverlay.classList.remove('hidden');
}

function closeSidebar() {
  sidebar.classList.add('-translate-x-full');
  sidebarOverlay.classList.add('hidden');
}

document.getElementById('sidebar-toggle').addEventListener('click', openSidebar);
sidebarOverlay.addEventListener('click', closeSidebar);

document.querySelectorAll('.nav-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    document.querySelectorAll('.nav-btn').forEach(b => {
      b.classList.remove('text-primary', 'bg-primary/10');
      b.classList.add('text-on-surface-variant');
    });
    document.querySelectorAll('.tab-panel').forEach(p => p.classList.add('hidden'));

    btn.classList.remove('text-on-surface-variant');
    btn.classList.add('text-primary', 'bg-primary/10');
    document.getElementById(btn.dataset.tab).classList.remove('hidden');
    pageTitle.textContent = tabLabels[btn.dataset.tab] || '';

    closeSidebar();

    // Refresh data when switching to relevant tabs
    if (btn.dataset.tab === 'history') loadHistory();
    if (btn.dataset.tab === 'models') loadModels();
    if (btn.dataset.tab === 'general') loadVocab();
    if (btn.dataset.tab === 'snippets') loadSnippets();
    if (btn.dataset.tab === 'reader') updateTtsPanel();
  });
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

  document.getElementById('tts-models').innerHTML = models
    .filter(m => m.engine.startsWith('tts_'))
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

let ttsLoadedModelId = null;
let ttsLoadedEngine = null;

async function updateTtsPanel() {
  const models = await invoke('list_models');
  const ttsModels = models.filter(m => m.engine.startsWith('tts_'));
  const select = document.getElementById('tts-model-select');
  const actionBtn = document.getElementById('tts-model-action');
  const playBtn = document.getElementById('tts-play');
  const info = await invoke('tts_info');

  select.innerHTML = ttsModels.map(m => {
    const suffix = m.status === 'not_downloaded' ? ` (${m.size})` : '';
    const loaded = info.loaded && ttsLoadedModelId === m.id;
    const tag = loaded ? ' [loaded]' : '';
    return `<option value="${m.id}" data-status="${m.status}" data-engine="${m.engine}">${m.name}${suffix}${tag}</option>`;
  }).join('');

  if (ttsModels.length === 0) {
    select.innerHTML = '<option value="">No TTS models available</option>';
    actionBtn.disabled = true;
    playBtn.disabled = true;
    return;
  }

  if (info.loaded && ttsLoadedModelId) {
    select.value = ttsLoadedModelId;
  }

  updateTtsActionBtn();
  updateTtsVoices(info);
}

function updateTtsActionBtn() {
  const select = document.getElementById('tts-model-select');
  const actionBtn = document.getElementById('tts-model-action');
  const playBtn = document.getElementById('tts-play');
  const opt = select.selectedOptions[0];
  if (!opt || !opt.value) {
    actionBtn.disabled = true;
    return;
  }
  const status = opt.dataset.status;
  const isCurrentLoaded = ttsLoadedModelId === opt.value;

  if (isCurrentLoaded) {
    actionBtn.textContent = 'Loaded';
    actionBtn.disabled = true;
    playBtn.disabled = false;
  } else if (status === 'downloaded' || status === 'active') {
    actionBtn.textContent = 'Load';
    actionBtn.disabled = false;
    playBtn.disabled = true;
  } else if (status === 'downloading') {
    actionBtn.textContent = 'Downloading...';
    actionBtn.disabled = true;
    playBtn.disabled = true;
  } else {
    actionBtn.textContent = 'Download';
    actionBtn.disabled = false;
    playBtn.disabled = true;
  }
}

async function updateTtsVoices(info) {
  const voiceRow = document.getElementById('tts-voice-row');
  const voiceSelect = document.getElementById('tts-voice-select');
  if (!info.loaded || info.num_speakers <= 1) {
    voiceRow.classList.add('hidden');
    return;
  }

  voiceRow.classList.remove('hidden');
  const prevVal = voiceSelect.value;
  const count = info.num_speakers;
  const presets = TTS_VOICE_PRESETS[ttsLoadedModelId] || [];

  // Cap the numbered list: some models (e.g. Piper LibriTTS) expose ~900
  // speakers, which would build a huge sluggish dropdown.
  const MAX_VOICES = 60;
  const shown = Math.min(count, MAX_VOICES);
  let html = presets
    .filter(v => v.sid < count)
    .map(v => `<option value="${v.sid}">${v.label}</option>`)
    .join('');
  for (let i = presets.length; i < shown; i++) {
    html += `<option value="${i}">Speaker ${i}</option>`;
  }
  voiceSelect.innerHTML = html;

  const custom = await invoke('tts_list_custom_voices');
  if (custom.length > 0) {
    voiceSelect.innerHTML += '<option disabled>-- Custom --</option>';
    for (const name of custom) {
      voiceSelect.innerHTML += `<option value="custom:${name}">${name}</option>`;
    }
  }

  if (prevVal) voiceSelect.value = prevVal;
}

document.getElementById('tts-model-select').addEventListener('change', updateTtsActionBtn);

document.getElementById('tts-model-action').addEventListener('click', async () => {
  const select = document.getElementById('tts-model-select');
  const actionBtn = document.getElementById('tts-model-action');
  const id = select.value;
  if (!id) return;

  const opt = select.selectedOptions[0];
  const status = opt.dataset.status;

  if (status === 'downloaded' || status === 'active') {
    actionBtn.textContent = 'Loading...';
    actionBtn.disabled = true;
    try {
      await invoke('tts_load', { id });
      ttsLoadedModelId = id;
      ttsLoadedEngine = opt.dataset.engine;
      await updateTtsPanel();
    } catch (err) {
      showToast('TTS load failed: ' + err);
      actionBtn.textContent = 'Load';
      actionBtn.disabled = false;
    }
  } else {
    actionBtn.textContent = 'Downloading...';
    actionBtn.disabled = true;
    document.getElementById('tts-dl-progress').classList.remove('hidden');
    try {
      await invoke('download_model', { id });
      document.getElementById('tts-dl-progress').classList.add('hidden');
      await updateTtsPanel();
    } catch (err) {
      showToast('Download failed: ' + err);
      document.getElementById('tts-dl-progress').classList.add('hidden');
      actionBtn.textContent = 'Download';
      actionBtn.disabled = false;
    }
  }
});

// ── TTS Player Bar ──

let ttsState = { position_ms: 0, buffered_ms: 0, duration_ms: 0, paused: false, finished: false };
let ttsSeeking = false;
let ttsSpeed = 1.0;
let ttsLastParams = null;

const playerBar = document.getElementById('tts-player');
const positionFill = document.getElementById('tts-position-fill');
const bufferFill = document.getElementById('tts-buffer-fill');
const seekThumb = document.getElementById('tts-seek-thumb');
const timeCurrent = document.getElementById('tts-time-current');
const timeTotal = document.getElementById('tts-time-total');
const playPauseBtn = document.getElementById('tts-play-pause');
const progressTrack = document.getElementById('tts-progress-track');
const speedBtn = document.getElementById('tts-speed-btn');
const PLAYER_PAD = '7rem';

function fmtTime(ms) {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

function showPlayerBar() {
  playerBar.classList.remove('translate-y-full');
  document.querySelectorAll('.tab-panel').forEach(p => { p.style.paddingBottom = PLAYER_PAD; });
}

function hidePlayerBar() {
  playerBar.classList.add('translate-y-full');
  document.querySelectorAll('.tab-panel').forEach(p => { p.style.paddingBottom = ''; });
}

function resetTtsUI() {
  document.getElementById('tts-play').disabled = false;
  document.getElementById('tts-play-label').textContent = 'Play';
}

function updatePlayerBar(st) {
  ttsState = st;
  const dur = st.duration_ms || st.buffered_ms || 1;
  const posPct = Math.min(100, (st.position_ms / dur) * 100);
  const bufPct = dur > 0 ? Math.min(100, (st.buffered_ms / dur) * 100) : 0;
  positionFill.style.width = `${posPct}%`;
  bufferFill.style.width = `${bufPct}%`;
  seekThumb.style.left = `${posPct}%`;
  seekThumb.style.opacity = dur > 0 ? '1' : '0';
  timeCurrent.textContent = fmtTime(st.position_ms);
  timeTotal.textContent = fmtTime(dur);
  const icon = playPauseBtn.querySelector('.material-symbols-outlined');
  if (st.finished) {
    icon.textContent = 'replay';
  } else {
    icon.textContent = st.paused ? 'play_arrow' : 'pause';
  }
  const label = document.getElementById('tts-play-label');
  if (label && !st.finished) {
    label.textContent = st.rebuffering ? 'Buffering...' : 'Playing...';
  }
}

listen('tts-position', (event) => {
  if (!ttsSeeking) updatePlayerBar(event.payload);
  showPlayerBar();
});

listen('tts-finished', () => {
  resetTtsUI();
});

playPauseBtn.addEventListener('click', () => {
  if (ttsState.finished) {
    invoke('tts_seek', { positionMs: 0 });
    invoke('tts_resume');
    ttsState.finished = false;
  } else if (ttsState.paused) {
    invoke('tts_resume');
  } else {
    invoke('tts_pause');
  }
});

document.getElementById('tts-skip-back').addEventListener('click', () => {
  invoke('tts_seek', { positionMs: Math.max(0, ttsState.position_ms - 10000) });
});

document.getElementById('tts-skip-fwd').addEventListener('click', () => {
  invoke('tts_seek', { positionMs: Math.min(ttsState.buffered_ms, ttsState.position_ms + 10000) });
});

document.getElementById('tts-dismiss').addEventListener('click', () => {
  invoke('tts_stop');
  hidePlayerBar();
  resetTtsUI();
});

function seekFromPointer(e) {
  const rect = progressTrack.getBoundingClientRect();
  const x = (e.touches ? e.touches[0].clientX : e.clientX) - rect.left;
  const pct = Math.max(0, Math.min(1, x / rect.width));
  const seekable = ttsState.buffered_ms;
  const pos = Math.floor(Math.min(pct * (ttsState.duration_ms || seekable), seekable));
  invoke('tts_seek', { positionMs: pos });
  const dur = ttsState.duration_ms || seekable || 1;
  const displayPct = (pos / dur) * 100;
  positionFill.style.width = `${displayPct}%`;
  seekThumb.style.left = `${displayPct}%`;
  timeCurrent.textContent = fmtTime(pos);
}

progressTrack.addEventListener('mousedown', (e) => { ttsSeeking = true; seekFromPointer(e); });
progressTrack.addEventListener('touchstart', (e) => { ttsSeeking = true; seekFromPointer(e); }, { passive: true });
document.addEventListener('mousemove', (e) => { if (ttsSeeking) seekFromPointer(e); });
document.addEventListener('touchmove', (e) => { if (ttsSeeking) seekFromPointer(e); }, { passive: true });
document.addEventListener('mouseup', () => { ttsSeeking = false; });
document.addEventListener('touchend', () => { ttsSeeking = false; });

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

speedPresets.forEach(chip => {
  chip.addEventListener('click', () => {
    setTtsSpeed(parseFloat(chip.dataset.speed));
    closeSpeedSheet();
  });
});

speedSlider.addEventListener('input', () => {
  const val = parseFloat(speedSlider.value);
  setTtsSpeed(val);
});

// ── TTS Play in Reader tab ──

document.getElementById('tts-play').addEventListener('click', async () => {
  const text = document.getElementById('tts-text').value.trim();
  if (!text) { showToast('Enter some text first'); return; }
  if (!ttsLoadedModelId) { showToast('Load a TTS model first'); return; }

  const playBtn = document.getElementById('tts-play');
  const label = document.getElementById('tts-play-label');

  // Disable before any await so a second tap during async setup (e.g. custom
  // voice load) can't start a second playback. Two players overlap audio and
  // make it jump forward then back.
  if (playBtn.disabled) return;
  playBtn.disabled = true;
  label.textContent = 'Generating...';

  const voiceVal = document.getElementById('tts-voice-select').value || '0';
  const isCustom = voiceVal.startsWith('custom:');
  if (isCustom) {
    try { await invoke('tts_load', { id: ttsLoadedModelId, customVoice: voiceVal.slice(7) }); }
    catch (err) { showToast('Failed to load custom voice: ' + err); resetTtsUI(); return; }
  }

  const speed = ttsSpeed;
  const sid = isCustom ? 0 : parseInt(voiceVal);
  ttsLastParams = { text, sid, customVoice: isCustom ? voiceVal.slice(7) : null };

  showPlayerBar();
  updatePlayerBar({ position_ms: 0, buffered_ms: 0, duration_ms: 0, paused: false, finished: false });

  try {
    await invoke('tts_speak', { text, speed, sid });
    label.textContent = 'Playing...';
  } catch (err) {
    showToast('TTS error: ' + err);
    resetTtsUI();
    hidePlayerBar();
  }
});

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
  const isDesktop = !navigator.userAgent.includes('Android');
  if (isDesktop) {
    document.getElementById('hotkey-row')?.classList.remove('hidden');
    document.getElementById('nav-audio')?.classList.remove('hidden');
  }

  await loadHistory();
  await loadModels();
  await loadAudioDevices();
  await loadConfig();
  await loadVocab();
  await loadSnippets();

  if (!engineReady && await invoke('is_engine_ready')) {
    engineReady = true;
  }

});
