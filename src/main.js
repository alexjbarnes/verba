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
const isDesktop = !navigator.userAgent.includes('Android');
const modeDefaultTab = { speak: 'history', listen: 'library' };
let currentMode = 'speak';

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

    closeSidebar();

    // Sidebar nav leaves any open reading. Keep the player bar only while audio
    // is actively playing (as a mini-player); otherwise hide it.
    if (!(ttsStarted && !ttsState.finished && !ttsState.paused)) hidePlayerBar();

    // Refresh data when switching to relevant tabs
    if (btn.dataset.tab === 'history') loadHistory();
    if (btn.dataset.tab === 'models') loadModels();
    if (btn.dataset.tab === 'general') loadVocab();
    if (btn.dataset.tab === 'snippets') loadSnippets();
    if (btn.dataset.tab === 'library') loadLibrary();
    if (btn.dataset.tab === 'voices') loadVoices();
  });
});

// ── Speak / Listen mode ──

function applyModeVisibility(mode) {
  document.querySelectorAll('.nav-btn').forEach(btn => {
    const m = btn.dataset.mode;
    const visible = (m === mode || m === 'global')
      && (btn.dataset.desktopOnly !== 'true' || isDesktop);
    btn.classList.toggle('hidden', !visible);
  });
}

function setMode(mode) {
  currentMode = mode;
  document.querySelectorAll('.mode-btn').forEach(b => {
    const active = b.dataset.mode === mode;
    b.classList.toggle('bg-surface-container-low', active);
    b.classList.toggle('shadow-sm', active);
    b.classList.toggle('text-on-surface', active);
    b.classList.toggle('text-on-surface-variant', !active);
  });
  applyModeVisibility(mode);
  const target = document.querySelector(`.nav-btn[data-tab="${modeDefaultTab[mode]}"]`);
  if (target) target.click();
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

  // Reader's inline voice-download bar + Voices-page empty-state bar (neither
  // has a model-row container).
  if (id === ttsModelId) {
    for (const [fillId, pctId] of [['tts-dl-fill', 'tts-dl-pct'], ['voices-dl-fill', 'voices-dl-pct']]) {
      const f = document.getElementById(fillId);
      const p = document.getElementById(pctId);
      if (f) f.style.width = `${pct}%`;
      if (p) p.textContent = `${pct}%`;
    }
  }

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
  ttsFavourites = Array.isArray(cfg.tts_favourite_sids) ? cfg.tts_favourite_sids.slice() : [];
  ttsVoiceSpeeds = (cfg.tts_voice_speeds && typeof cfg.tts_voice_speeds === 'object') ? cfg.tts_voice_speeds : {};
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

let ttsModelId = null;       // single Piper TTS model id, resolved from the registry
let ttsLoadedModelId = null;
let ttsLoadedEngine = null;

// Friendly label for a speaker id. Models with a named preset (e.g. Kokoro) use
// it; LibriTTS speakers are unnamed, so fall back to "Voice N".
function voiceLabel(sid) {
  const presets = TTS_VOICE_PRESETS[ttsModelId] || [];
  const p = presets.find(v => v.sid === sid);
  return p ? p.label : `Voice ${sid}`;
}

async function updateTtsPanel() {
  const models = await invoke('list_models');
  const tts = models.find(m => m.engine.startsWith('tts_'));
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

// ── Voices page: sample + favourite ──

// Speaker count for the model, cached when the Voices page or voice sheet builds.
let voicesNumSpeakers = 0;
// sid currently auditioning (drives the row's playing icon); -1 = none.
let samplingSid = -1;
let sampleClearTimer = null;

async function loadVoices() {
  const models = await invoke('list_models');
  const tts = models.find(m => m.engine.startsWith('tts_'));
  const dl = document.getElementById('voices-download');
  const favSection = document.getElementById('voices-fav-section');
  const allSection = document.getElementById('voices-all-section');
  const dlBtn = document.getElementById('voices-download-btn');

  if (!tts) {
    dl.classList.remove('hidden');
    document.getElementById('voices-download-label').textContent = 'No voice model available';
    dlBtn.classList.add('hidden');
    favSection.classList.add('hidden');
    allSection.classList.add('hidden');
    return;
  }

  ttsModelId = tts.id;
  const downloaded = tts.status === 'downloaded' || tts.status === 'active';
  if (!downloaded) {
    dl.classList.remove('hidden');
    document.getElementById('voices-download-label').textContent = `${tts.name}, ${tts.size}`;
    dlBtn.classList.remove('hidden');
    dlBtn.disabled = tts.status === 'downloading';
    favSection.classList.add('hidden');
    allSection.classList.add('hidden');
    return;
  }
  dl.classList.add('hidden');

  const info = await invoke('tts_info');
  voicesNumSpeakers = info.loaded ? info.num_speakers : await invoke('tts_model_speakers', { id: tts.id });
  renderVoiceLists();
}

function voiceRowHtml(sid) {
  const fav = ttsFavourites.includes(sid);
  const playing = sid === samplingSid;
  return `
    <div class="voice-row flex items-center justify-between gap-2 px-4 py-2.5" data-sid="${sid}">
      <button class="voice-sample flex items-center gap-3 min-w-0 flex-1 text-left cursor-pointer" data-sid="${sid}">
        <span class="material-symbols-outlined text-xl ${playing ? 'text-primary' : 'text-on-surface-variant'}">${playing ? 'graphic_eq' : 'play_circle'}</span>
        <span class="text-sm text-on-surface truncate">${escapeHtml(voiceLabel(sid))}</span>
      </button>
      <button class="voice-fav shrink-0 p-1 cursor-pointer ${fav ? 'text-primary' : 'text-on-surface-variant/40'}" data-sid="${sid}">
        <span class="material-symbols-outlined text-lg" style="font-variation-settings:'FILL' ${fav ? 1 : 0}">star</span>
      </button>
    </div>`;
}

function rebuildFavSection() {
  const favSection = document.getElementById('voices-fav-section');
  const favList = document.getElementById('voices-fav-list');
  const favs = ttsFavourites.filter(sid => sid < voicesNumSpeakers);
  favSection.classList.toggle('hidden', favs.length === 0);
  favList.innerHTML = favs.map(voiceRowHtml).join('');
}

function renderVoiceLists() {
  rebuildFavSection();
  const allSection = document.getElementById('voices-all-section');
  const allList = document.getElementById('voices-all-list');
  allSection.classList.toggle('hidden', voicesNumSpeakers <= 0);
  let html = '';
  for (let sid = 0; sid < voicesNumSpeakers; sid++) html += voiceRowHtml(sid);
  allList.innerHTML = html;
}

async function sampleVoice(sid) {
  if (!ttsModelId) { showToast('No voice available'); return; }
  try {
    if (!ttsLoadedModelId) {
      await invoke('tts_load', { id: ttsModelId });
      ttsLoadedModelId = ttsModelId;
    }
    await invoke('tts_sample', { sid, speed: ttsSpeed });
    setSamplingSid(sid);
  } catch (err) { showToast('Sample failed: ' + err); }
}

// The sample plays with no app handle, so no finished event comes back. Show the
// playing icon on the row and clear it after a short delay (or when another row
// is sampled).
function setSamplingSid(sid) {
  const prev = samplingSid;
  samplingSid = sid;
  if (prev !== sid) updateSampleIcon(prev);
  updateSampleIcon(sid);
  if (sampleClearTimer) clearTimeout(sampleClearTimer);
  sampleClearTimer = setTimeout(() => { const s = samplingSid; samplingSid = -1; updateSampleIcon(s); }, 4000);
}

function updateSampleIcon(sid) {
  if (sid < 0) return;
  const playing = sid === samplingSid;
  document.querySelectorAll(`.voice-sample[data-sid="${sid}"] .material-symbols-outlined`).forEach(icon => {
    icon.textContent = playing ? 'graphic_eq' : 'play_circle';
    icon.classList.toggle('text-primary', playing);
    icon.classList.toggle('text-on-surface-variant', !playing);
  });
}

async function toggleFavourite(sid) {
  const i = ttsFavourites.indexOf(sid);
  const adding = i < 0;
  if (adding) ttsFavourites.push(sid);
  else ttsFavourites.splice(i, 1);
  ttsFavourites.sort((a, b) => a - b);
  await persistFavourites();
  // Update the star in place across both lists, then rebuild only the small
  // favourites list (avoids re-rendering the full ~900-row list each tap).
  document.querySelectorAll(`.voice-fav[data-sid="${sid}"]`).forEach(btn => {
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
    cfg.tts_favourite_sids = ttsFavourites;
    await invoke('save_config', { cfg });
  } catch (err) { showToast('Save failed: ' + err); }
}

function onVoiceListClick(e) {
  const sampleBtn = e.target.closest('.voice-sample');
  if (sampleBtn) { sampleVoice(parseInt(sampleBtn.dataset.sid, 10)); return; }
  const favBtn = e.target.closest('.voice-fav');
  if (favBtn) { toggleFavourite(parseInt(favBtn.dataset.sid, 10)); }
}
document.getElementById('voices-fav-list').addEventListener('click', onVoiceListClick);
document.getElementById('voices-all-list').addEventListener('click', onVoiceListClick);

document.getElementById('voices-download-btn').addEventListener('click', async () => {
  if (!ttsModelId) return;
  const btn = document.getElementById('voices-download-btn');
  const progress = document.getElementById('voices-dl-progress');
  btn.disabled = true;
  btn.textContent = 'Downloading...';
  progress.classList.remove('hidden');
  try {
    await invoke('download_model', { id: ttsModelId });
    // download-complete refreshes the page; reset the button for next time.
    btn.textContent = 'Download';
  } catch (err) {
    showToast('Download failed: ' + err);
    progress.classList.add('hidden');
    btn.disabled = false;
    btn.textContent = 'Download';
  }
});

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
const speedBtn = document.getElementById('tts-speed-btn');
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
  document.querySelectorAll('.tab-panel').forEach(p => { p.style.paddingBottom = PLAYER_PAD; });
}

function hidePlayerBar() {
  playerBar.classList.add('translate-y-full');
  document.querySelectorAll('.tab-panel').forEach(p => { p.style.paddingBottom = ''; });
}

function resetTtsUI() {
  // The player bar is the only transport now; returning to a not-started state
  // means the next play press starts (resume or from 0) rather than pausing.
  ttsStarted = false;
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
  if (st.finished) {
    icon.textContent = 'replay';
  } else if (st.rebuffering && !st.paused) {
    // Seeked ahead of (or generation fell behind) the buffer — show it's loading,
    // not frozen. Resumes automatically once generation catches up.
    icon.textContent = 'hourglass_top';
  } else {
    icon.textContent = st.paused ? 'play_arrow' : 'pause';
  }
}

listen('tts-position', (event) => {
  if (event.payload.gen !== genId) return;
  liveBufferedMs = event.payload.buffered_ms || 0;
  refineDuration();
  if (!ttsSeeking) updatePlayerBar(event.payload);
  showPlayerBar();
  highlightAt(timelineBaseMs + event.payload.position_ms);
  if (!event.payload.paused) saveProgress(false);
  maybeSaveMeasuredDuration(event.payload);
});

// Once a full-article play finishes generating, the buffered amount IS the true
// duration at this speed. Save it so the library + ready bar show the real
// length next time instead of the estimate. Only a play from the top (genBaseWord
// 0) measures the whole article; resumes generate just the remainder.
function maybeSaveMeasuredDuration(p) {
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
  const { start_ms, duration_ms, text } = event.payload;
  const words = (text || '').trim().split(/\s+/).filter(Boolean);
  const n = words.length;
  if (n === 0) return;
  let totalLen = 0;
  const lens = words.map(w => { const l = w.length || 1; totalLen += l; return l; });
  let acc = start_ms;
  for (let k = 0; k < n; k++) {
    const dur = duration_ms * (lens[k] / totalLen);
    // Absolute timeline position = fragment base + offset within the fragment.
    wordTimes[timingCursor + k] = { s: timelineBaseMs + acc, e: timelineBaseMs + acc + dur };
    acc += dur;
  }
  timingCursor += n;
});

listen('tts-finished', (event) => {
  if (event.payload && event.payload.gen !== genId) return;
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
  autoFollow = true;
  dynMilestoneIdx = 0;

  const voiceVal = ttsVoice || '0';
  const isCustom = voiceVal.startsWith('custom:');

  // Lazy-load the voice at generation time, no manual load step. First load can
  // take a couple seconds, so toast while it happens. The custom branch loads
  // the engine itself, so only plain-load when not custom.
  if (!isCustom && !ttsLoadedModelId) {
    showToast('Loading voice…');
    try {
      await invoke('tts_load', { id: ttsModelId });
      ttsLoadedModelId = ttsModelId;
    } catch (err) { showToast('Voice load failed: ' + err); resetTtsUI(); return; }
  }
  if (isCustom) {
    showToast('Loading voice…');
    try {
      await invoke('tts_load', { id: ttsModelId, customVoice: voiceVal.slice(7) });
      ttsLoadedModelId = ttsModelId;
    } catch (err) { showToast('Failed to load custom voice: ' + err); resetTtsUI(); return; }
  }

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

  showPlayerBar();
  // Recompute the full-article duration at the current speed (measured if known,
  // else estimated); updatePlayerBar holds this as the total until generation
  // completes, so the bar stays stable (and consistent with the library).
  articleEstMs = itemDurationMs(readingItem, ttsSpeed) || estDurationMsForText(readingText, ttsSpeed);
  updatePlayerBar({ position_ms: 0, buffered_ms: 0, gen_done: false, paused: false, finished: false });

  try {
    await invoke('tts_speak', { text, speed: ttsSpeed, sid, gen: myGen });
  } catch (err) {
    showToast('TTS error: ' + err);
    resetTtsUI();
    hidePlayerBar();
  }
}

// On a speed or voice change mid-listen, continue from the current word at the
// new setting rather than restarting from the top.
async function resumeFromCurrentWord() {
  if (ttsState.finished || ttsState.paused) return;
  if (playerBar.classList.contains('translate-y-full')) return; // not playing
  if (!readingText) return;
  // Derive the current word from the PLAYBACK position, not the highlight, so a
  // re-render works even when highlighting hasn't caught up. Fall back to the
  // fragment start before any timing has arrived.
  const curMs = timelineBaseMs + ttsState.position_ms;
  let w = wordAtTime(curMs);
  if (w == null) w = genBaseWord;
  if (!readingWords[w]) return;
  const remaining = readingText.slice(readingWords[w].start);
  if (!remaining.trim()) return;
  await startSpeak(remaining, w, wordTimes[w] ? wordTimes[w].s : curMs);
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
  invoke('library_set_progress', { id: readingItem.id, progress: offset }).catch(() => {});
}

async function loadLibrary() {
  const items = await invoke('library_list');
  const list = document.getElementById('lib-list');
  const empty = document.getElementById('lib-empty');
  empty.classList.toggle('hidden', items.length > 0);
  list.innerHTML = items.slice().reverse().map(it => {
    const len = (it.body || '').length || 1;
    const prog = Math.max(0, Math.min(len, it.progress || 0));
    const pct = Math.round(prog / len * 100);
    // Real measured duration if we have it, else an estimate; at the current
    // playback speed so the list matches the reader.
    const totalMs = itemDurationMs(it, ttsSpeed);
    const leftMs = totalMs * (1 - prog / len);
    // Finished -> "Finished"; in-progress -> "42% · 6 min left"; fresh -> length.
    const meta = pct >= 100 ? 'Finished'
      : pct > 0 ? `${pct}% · ${fmtMins(leftMs)} left`
      : fmtMins(totalMs);
    return `
    <div class="lib-item flex items-center justify-between gap-3 bg-surface-container-low rounded-xl px-4 py-3 cursor-pointer hover:bg-surface-container-high transition-colors" data-id="${escapeHtml(it.id)}">
      <div class="min-w-0 flex-1">
        <div class="text-sm font-medium text-on-surface truncate">${escapeHtml(it.title)}</div>
        <div class="text-xs text-on-surface-variant truncate mt-0.5">${escapeHtml(it.body.slice(0, 80))}</div>
        <div class="text-[11px] ${pct >= 100 ? 'text-primary' : 'text-on-surface-variant'} tabular-nums mt-1">${meta}</div>
      </div>
      <button class="lib-del shrink-0 text-on-surface-variant/50 hover:text-error transition-colors p-1 cursor-pointer" data-id="${escapeHtml(it.id)}">
        <span class="material-symbols-outlined text-lg">delete</span>
      </button>
    </div>`;
  }).join('');
  list.querySelectorAll('.lib-item').forEach(el => {
    el.addEventListener('click', (e) => {
      if (e.target.closest('.lib-del')) return;
      openReading(el.dataset.id);
    });
  });
  list.querySelectorAll('.lib-del').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      await invoke('library_delete', { id: btn.dataset.id });
      loadLibrary();
    });
  });
}

async function openReading(id) {
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
  ttsStarted = false;
  articleEstMs = itemDurationMs(item, ttsSpeed);
  const frac = readingText.length ? Math.min(1, prog / readingText.length) : 0;
  timelineBaseMs = articleEstMs > 0 ? frac * articleEstMs : 0;
  showPlayerBar();
  updatePlayerBar({ position_ms: 0, buffered_ms: 0, gen_done: false, paused: true, finished: false });

  await updateTtsPanel();
}

document.getElementById('reading-back').addEventListener('click', () => {
  saveProgress(true);
  invoke('tts_stop');
  hidePlayerBar();
  showPanel('library');
  loadLibrary();
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
  ttsVoice = String(val);
  updateVoiceBtnLabel();
  // Each voice carries its own speed; restore it (default 1x) and reflect it in
  // the speed control.
  setTtsSpeed(speedForVoice(ttsVoice));
  persistVoicePrefs();
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

// Build the player voice list: favourites if any, else all speakers; then any
// custom voices. Speaker count is fetched here so the sheet works even if the
// Voices page was never opened.
async function buildVoiceSheet() {
  const list = document.getElementById('voice-sheet-list');
  const hint = document.getElementById('voice-sheet-hint');
  let n = 0;
  try {
    const info = await invoke('tts_info');
    n = info.loaded ? info.num_speakers : (ttsModelId ? await invoke('tts_model_speakers', { id: ttsModelId }) : 0);
  } catch (_) {}
  voicesNumSpeakers = n;

  const favs = ttsFavourites.filter(sid => sid < n);
  const usingFavs = favs.length > 0;
  const sids = usingFavs ? favs : Array.from({ length: n }, (_, i) => i);
  hint.textContent = usingFavs ? 'Favourites' : (n > 0 ? 'All voices' : '');

  let html = sids.map(sid => voiceSheetItemHtml(String(sid), voiceLabel(sid))).join('');

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

document.getElementById('tts-voice-btn').addEventListener('click', openVoiceSheet);
document.getElementById('voice-sheet-overlay').addEventListener('click', closeVoiceSheet);
document.getElementById('voice-sheet-list').addEventListener('click', (e) => {
  const btn = e.target.closest('.voice-pick');
  if (!btn) return;
  setTtsVoice(btn.dataset.voice);
  closeVoiceSheet();
  resumeFromCurrentWord();
});

// ── Add-text modal ──

const libAddModal = document.getElementById('lib-add-modal');
function closeAddModal() {
  libAddModal.classList.add('hidden');
  libAddModal.classList.remove('flex');
}
document.getElementById('lib-add-btn').addEventListener('click', () => {
  document.getElementById('lib-add-title').value = '';
  document.getElementById('lib-add-body').value = '';
  libAddModal.classList.remove('hidden');
  libAddModal.classList.add('flex');
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

});
