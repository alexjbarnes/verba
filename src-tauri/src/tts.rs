use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tauri::Emitter;

use crate::player;

static TTS_ENGINE: OnceLock<Mutex<Option<TtsEngine>>> = OnceLock::new();
static ACTIVE_PLAYER: OnceLock<Mutex<Option<Arc<player::AudioPlayer>>>> = OnceLock::new();

// Engine properties cached at load time so the hot paths (sample_rate, seek,
// is_loaded) never lock the engine mutex. Locking it from a command thread
// while the generation thread holds it across native inference is what poisons
// the mutex on a caught panic and cascades into an uncaught crash.
static SAMPLE_RATE: AtomicI32 = AtomicI32::new(0);
static NUM_SPEAKERS: AtomicI32 = AtomicI32::new(0);
static ENGINE_LOADED: AtomicBool = AtomicBool::new(false);

// Serializes speak() setup so two concurrent tts_speak calls can never each
// build their own player. Without it, overlapping calls produce two audio
// streams playing the same text offset by a sentence (sounds like jumping
// forward then back). Held only during setup, never across generation.
static SPEAK_LOCK: Mutex<()> = Mutex::new(());

// Which voice is audible right now: "model-id[+custom][#sid]". Attached to
// mispronunciation reports so a word flagged on one voice isn't "fixed" on
// another. Base is set where the model id is known (the lib.rs commands);
// the sid half updates at each speak.
static CURRENT_VOICE: Mutex<(String, i32)> = Mutex::new((String::new(), -1));

pub fn set_voice_base(base: String) {
    let mut v = CURRENT_VOICE.lock().unwrap_or_else(|e| e.into_inner());
    if v.0 != base {
        *v = (base, -1);
    }
}

fn note_speak_sid(sid: i32) {
    CURRENT_VOICE.lock().unwrap_or_else(|e| e.into_inner()).1 = sid;
}

pub fn current_voice() -> String {
    let v = CURRENT_VOICE.lock().unwrap_or_else(|e| e.into_inner());
    if v.0.is_empty() {
        String::new()
    } else if v.1 < 0 {
        v.0.clone()
    } else {
        format!("{}#{}", v.0, v.1)
    }
}

fn global() -> &'static Mutex<Option<TtsEngine>> {
    TTS_ENGINE.get_or_init(|| Mutex::new(None))
}

fn active_player() -> &'static Mutex<Option<Arc<player::AudioPlayer>>> {
    ACTIVE_PLAYER.get_or_init(|| Mutex::new(None))
}

/// Lock the engine, recovering from poisoning. A caught panic during inference
/// poisons the mutex; without this, every later `.lock().unwrap()` would panic
/// (uncaught, on a command thread) and crash the whole app.
fn lock_engine() -> std::sync::MutexGuard<'static, Option<TtsEngine>> {
    global().lock().unwrap_or_else(|e| e.into_inner())
}

fn lock_player() -> std::sync::MutexGuard<'static, Option<Arc<player::AudioPlayer>>> {
    active_player().lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Debug)]
pub enum TtsModelConfig {
    /// Piper VITS run directly via `ort` + `piper-plus-g2p` (no espeak / GPL).
    /// `config` is the `.onnx.json` sidecar; speakers selected by sid.
    PiperOrt {
        model: String,
        config: String,
    },
}

impl TtsModelConfig {
    pub fn override_voices(&mut self, _path: String) {
        match self {
            // Piper selects speakers by sid, not a voices file — nothing to override.
            TtsModelConfig::PiperOrt { .. } => {}
        }
    }
}

/// Loaded backend behind the global engine slot. The Piper path drives
/// `PiperEngine` (ort + piper-plus-g2p, GPL-free). `speak()` and the
/// `AudioPlayer` are shared regardless of source.
enum TtsEngine {
    Piper(crate::piper::PiperEngine),
}

impl TtsEngine {
    /// Generate one already-batched chunk. Returns the whole chunk's PCM, or
    /// `None` when an empty Piper chunk should be skipped.
    fn generate_chunk(
        &mut self,
        text: &str,
        speed: f32,
        sid: i32,
    ) -> Result<Option<(Vec<f32>, Vec<crate::piper::SegmentSpan>)>, String> {
        match self {
            TtsEngine::Piper(engine) => {
                let (samples, spans) = engine.synth_chunk(text, sid, speed)?;
                if samples.is_empty() { Ok(None) } else { Ok(Some((samples, spans))) }
            }
        }
    }
}

pub fn is_loaded() -> bool {
    ENGINE_LOADED.load(Ordering::SeqCst)
}

pub fn load(config: TtsModelConfig, num_threads: i32) -> Result<(), String> {
    // Piper-ort needs no espeak data: phonemes come from the bundled CMU dict.
    let engine = match config {
        TtsModelConfig::PiperOrt { model, config } => {
            crate::piper::PiperEngine::load(&model, &config, num_threads)?
        }
    };
    let sr = engine.sample_rate();
    let speakers = engine.num_speakers();
    log::info!("TTS loaded (piper-ort): sample_rate={sr}, speakers={speakers}");
    SAMPLE_RATE.store(sr, Ordering::SeqCst);
    NUM_SPEAKERS.store(speakers, Ordering::SeqCst);
    *lock_engine() = Some(TtsEngine::Piper(engine));
    ENGINE_LOADED.store(true, Ordering::SeqCst);
    Ok(())
}

fn panic_to_string(context: &str, panic: Box<dyn std::any::Any + Send>) -> String {
    let detail = if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".into()
    };
    format!("{context} crashed: {detail}")
}

pub fn unload() {
    stop();
    ENGINE_LOADED.store(false, Ordering::SeqCst);
    *lock_engine() = None;
}

pub fn is_speaking() -> bool {
    lock_player().as_ref()
        .map_or(false, |p| p.is_active())
}

pub fn stop() {
    if let Some(player) = lock_player().take() {
        player.stop();
    }
    // Explicit stop (dismiss/back/unload) ends the listening session: tear down
    // the media session so its notification + audio focus are released. A
    // mid-listen re-render stops the old player inline (not via stop()), so this
    // does not fire on speed/voice/seek re-renders.
    #[cfg(target_os = "android")]
    crate::android_stop_media_session();
}

pub fn pause() {
    if let Some(player) = lock_player().as_ref() {
        player.pause();
    }
}

pub fn resume() {
    if let Some(player) = lock_player().as_ref() {
        player.resume();
    }
}

pub fn seek_ms(position_ms: u64) {
    let sr = sample_rate() as u64;
    if sr == 0 { return; }
    if let Some(player) = lock_player().as_ref() {
        // Do NOT clamp to the buffered amount. Seeking past the generation
        // frontier is allowed: the player rebuffers there and resumes once
        // generation catches up (or finishes if it's past the end). Clamping to
        // the frontier is what made forward seeks jump back. The audio callback
        // only reads below `buffered`, so an out-of-range cursor just starves.
        let buffered = player.position().buffered;
        let target = (position_ms * sr / 1000) as usize;
        let buffered_ms = buffered as u64 * 1000 / sr;
        if target > buffered {
            log::info!("TTS seek -> {position_ms}ms (ahead of buffered {buffered_ms}ms — will rebuffer)");
        } else {
            log::info!("TTS seek -> {position_ms}ms (buffered {buffered_ms}ms)");
        }
        player.seek(target);
    }
}

pub fn num_speakers() -> i32 {
    NUM_SPEAKERS.load(Ordering::SeqCst)
}

pub fn sample_rate() -> i32 {
    SAMPLE_RATE.load(Ordering::SeqCst)
}

/// Build the player position callback: emits `tts-position` on every poll and
/// `tts-finished` (once) when generation is done and playback has caught up to
/// it. Shared by `speak()` and `speak_from_cache()` — completion is purely a
/// function of the player's own position/buffered/gen_done state, identical
/// regardless of whether the audio came from the engine or the cache.
fn build_position_callback(
    gen: u64,
    sr: u64,
    app: Option<tauri::AppHandle>,
) -> Option<Box<dyn Fn(player::PlaybackPosition) + Send + 'static>> {
    app.map(|app| {
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        Box::new(move |pos: player::PlaybackPosition| {
            let position_ms = (pos.cursor as u64 * 1000) / sr;
            let buffered_ms = (pos.buffered as u64 * 1000) / sr;
            let estimated_ms = (pos.estimated as u64 * 1000) / sr;
            let duration_ms = if pos.gen_done { buffered_ms } else { estimated_ms.max(buffered_ms) };
            let done = pos.gen_done && pos.cursor >= pos.buffered;
            let _ = app.emit("tts-position", serde_json::json!({
                "gen": gen,
                "position_ms": position_ms,
                "buffered_ms": buffered_ms,
                "duration_ms": duration_ms,
                "gen_done": pos.gen_done,
                "paused": pos.paused,
                "finished": done,
                "rebuffering": pos.rebuffering,
            }));
            #[cfg(target_os = "android")]
            crate::android_update_media_session(position_ms as i64, duration_ms as i64, pos.paused);
            if done && !finished.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let _ = app.emit("tts-finished", serde_json::json!({ "gen": gen }));
                #[cfg(target_os = "android")]
                crate::android_stop_media_session();
            }
        }) as Box<dyn Fn(player::PlaybackPosition) + Send + 'static>
    })
}

/// Load the engine in the background if it isn't already, swallowing errors
/// (best-effort warm-up). Used after a cache-only play starts, so the engine is
/// likely ready if the user later seeks somewhere the cache can't cover — a
/// real load attempt (`tts_load`) still runs and reports errors normally.
pub fn load_in_background(config: TtsModelConfig, num_threads: i32) {
    if is_loaded() {
        return;
    }
    std::thread::spawn(move || match load(config, num_threads) {
        Ok(()) => log::info!("TTS: background engine warm-up complete"),
        Err(e) => log::warn!("TTS: background engine warm-up failed: {e}"),
    });
}

/// Attempt to play `text` straight from the persistent segment cache, without
/// loading the ONNX engine at all. Returns `Ok(true)` if every segment the text
/// needs was already cached and playback has started; `Ok(false)` if any part
/// is missing (the caller falls back to loading the engine + `speak()`, which
/// generates normally and caches as it goes). `model`/`config` are the on-disk
/// paths (from `TtsModelConfig::PiperOrt`) — read directly, independent of
/// whatever is (or isn't) currently loaded into the global engine slot.
pub fn speak_from_cache(
    text: &str,
    speed: f32,
    sid: i32,
    gen: u64,
    app: Option<tauri::AppHandle>,
    model: &str,
    config: &str,
) -> Result<bool, String> {
    note_speak_sid(sid);
    let Some((sample_rate_u32, num_speakers)) = crate::piper::read_piper_meta(config) else {
        return Ok(false);
    };
    let sample_rate = sample_rate_u32 as i32;
    let model_fp = crate::piper::cache_fingerprint(model, config);

    // Cheap header-only check BEFORE touching the player or the SPEAK_LOCK: a
    // partial cache must have zero side effects so the caller's fallback to a
    // normal load+speak starts from a clean slate (current playback untouched).
    let cov = crate::piper::cache_coverage(model, config, sid, speed, text);
    if !cov.is_all_cached() {
        return Ok(false);
    }

    let _speak_guard = SPEAK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = lock_player().take() {
        log::info!("TTS: stopping previous player before new (cached) playback");
        old.stop();
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    let clean = text.replace('\u{0}', " ");
    let text_owned = clean;
    let chunks = split_sentences(&text_owned);
    let chunks = batch_sentences(chunks, 15, 45);
    let total = chunks.len();
    log::info!("TTS: {total} chunks, all cached ({}ms) — skipping engine load", cov.cached_ms);

    let sr = (sample_rate as u64).max(1);
    // Exact, not estimated: every segment's real duration is already known.
    let estimated_samples = ((cov.cached_ms as f64 / 1000.0) * sample_rate as f64) as usize;

    let on_position = build_position_callback(gen, sr, app.clone());

    #[cfg(target_os = "android")]
    if app.is_some() {
        crate::android_start_media_session();
    }

    let player = Arc::new(player::AudioPlayer::new(sample_rate, estimated_samples, on_position)?);
    *lock_player() = Some(player.clone());

    let player_cleanup = player.clone();
    let app_cleanup = app.clone();
    std::thread::Builder::new()
        .name("tts-generate-cached".into())
        .spawn(move || {
            let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut cumulative_ms: u64 = 0;
                for (i, chunk) in chunks.iter().enumerate() {
                    if !player.is_active() { break; }
                    if let Some(ref app) = app {
                        let _ = app.emit("tts-progress", serde_json::json!({ "current": i + 1, "total": total }));
                    }
                    match crate::piper::synth_chunk_cache_only(&model_fp, sample_rate, num_speakers, sid, speed, chunk) {
                        Some((samples, spans)) => {
                            player.push(samples);
                            for span in &spans {
                                if let Some(ref app) = app {
                                    let _ = app.emit("tts-timing", serde_json::json!({
                                        "gen": gen,
                                        "text": span.text,
                                        "start_ms": cumulative_ms,
                                        "duration_ms": span.speech_ms,
                                        "word_ms": span.word_ms,
                                    }));
                                }
                                cumulative_ms += span.speech_ms + span.pause_ms;
                            }
                        }
                        // A confirmed-cached segment vanished (e.g. a concurrent
                        // eviction) — extremely rare given the coverage check just
                        // ran. Stop here rather than silently switching to the
                        // engine mid-stream; the player still finishes cleanly with
                        // whatever was pushed.
                        None => {
                            log::warn!("TTS cache-only playback: chunk {}/{total} missing after coverage check — stopping early", i + 1);
                            break;
                        }
                    }
                }
            }));
            if let Err(panic) = gen_result {
                let msg = panic_to_string("TTS cached-generation thread", panic);
                log::error!("{msg}");
                if let Some(ref a) = app_cleanup {
                    let _ = a.emit("dictation-error", &msg);
                }
            }
            player_cleanup.finish();
        })
        .map_err(|e| format!("spawn cached generator: {e}"))?;

    Ok(true)
}

pub fn speak(text: &str, speed: f32, sid: i32, gen: u64, app: Option<tauri::AppHandle>) -> Result<(), String> {
    // Serialize the whole setup: two concurrent calls must not each spin up a
    // player. The second waits here, then stops the first's player below.
    let _speak_guard = SPEAK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    note_speak_sid(sid);

    // Atomically take and stop any existing player before creating a new one.
    if let Some(old) = lock_player().take() {
        log::info!("TTS: stopping previous player before new playback");
        old.stop();
        // Give the old audio thread time to release the output device before
        // opening a new stream (Android oboe dislikes two live output streams).
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    if !is_loaded() {
        return Err("TTS not loaded".into());
    }
    let sample_rate = sample_rate();
    if sample_rate == 0 {
        return Err("TTS not loaded".into());
    }

    // sherpa-onnx calls CString::new(text).unwrap() internally, which panics on
    // an interior NUL byte. Strip them so odd clipboard content can't crash gen.
    let clean = text.replace('\u{0}', " ");
    let text = clean.as_str();

    let raw_sentences = split_sentences(text);
    let sentences = batch_sentences(raw_sentences, 15, 45);
    let total = sentences.len();
    log::info!("TTS: {total} chunks to generate");

    let word_count = text.split_whitespace().count();
    // Spoken duration scales inversely with speed (length_scale = base/speed),
    // so faster speed -> shorter. The buffer is a hard cap (capacity =
    // estimated_samples*2), so getting this right prevents the tail being
    // dropped on long texts at slow speeds.
    let estimated_secs = (word_count as f64) / 155.0 * 60.0 / (speed.max(0.1) as f64);
    let estimated_samples = (estimated_secs * sample_rate as f64) as usize;

    let sr = (sample_rate as u64).max(1);
    let on_position = build_position_callback(gen, sr, app.clone());

    // Samples (no app handle) play through the same player but must stay quiet:
    // no media-session notification, no UI events.
    #[cfg(target_os = "android")]
    if app.is_some() {
        crate::android_start_media_session();
    }

    let player = Arc::new(player::AudioPlayer::new(sample_rate, estimated_samples, on_position)?);
    *lock_player() = Some(player.clone());

    let player_cleanup = player.clone();
    let app_cleanup = app.clone();
    std::thread::Builder::new()
        .name("tts-generate".into())
        .spawn(move || {
            let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut cumulative_ms: u64 = 0;
                for (i, sentence) in sentences.iter().enumerate() {
                    if !player.is_active() { break; }

                    if let Some(ref app) = app {
                        let _ = app.emit("tts-progress", serde_json::json!({
                            "current": i + 1,
                            "total": total,
                        }));
                    }

                    let t = std::time::Instant::now();
                    let infer_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mut guard = lock_engine();
                        match guard.as_mut() {
                            Some(engine) => engine.generate_chunk(sentence, speed, sid),
                            None => Ok(None),
                        }
                    }));

                    match infer_result {
                        Ok(Ok(Some((samples, spans)))) => {
                            player.push(samples.clone());
                            let elapsed_ms = t.elapsed().as_millis();
                            let audio_ms = (samples.len() as u64 * 1000) / sample_rate.max(1) as u64;
                            let rtf = if audio_ms > 0 { elapsed_ms as f32 / audio_ms as f32 } else { 0.0 };
                            log::info!(
                                "TTS [{}/{}]: {:.1}s audio in {}ms, RTF {:.2}x",
                                i + 1, total, audio_ms as f32 / 1000.0, elapsed_ms, rtf
                            );
                            // Emit per-segment timing (speech-only duration) so the
                            // reading view highlight tracks speech and treats the
                            // inserted pauses as gaps, not part of a word.
                            for span in &spans {
                                if let Some(ref app) = app {
                                    let _ = app.emit("tts-timing", serde_json::json!({
                                        "gen": gen,
                                        "text": span.text,
                                        "start_ms": cumulative_ms,
                                        "duration_ms": span.speech_ms,
                                        "word_ms": span.word_ms,
                                    }));
                                }
                                cumulative_ms += span.speech_ms + span.pause_ms;
                            }
                        }
                        // No audio this chunk: an empty Piper chunk -> skip.
                        Ok(Ok(None)) => continue,
                        Ok(Err(msg)) => {
                            log::error!("TTS inference: {msg}");
                            if let Some(ref app) = app {
                                let _ = app.emit("dictation-error", &msg);
                            }
                            break;
                        }
                        Err(panic) => {
                            let msg = panic_to_string("TTS inference", panic);
                            log::error!("{msg}");
                            if let Some(ref app) = app {
                                let _ = app.emit("dictation-error", &msg);
                            }
                            break;
                        }
                    }
                }
            }));

            if let Err(panic) = gen_result {
                let msg = panic_to_string("TTS generation thread", panic);
                log::error!("{msg}");
                if let Some(ref a) = app_cleanup {
                    let _ = a.emit("dictation-error", &msg);
                }
            }
            player_cleanup.finish();
        })
        .map_err(|e| format!("spawn generator: {e}"))?;

    Ok(())
}

pub(crate) fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        // A newline is a paragraph/line break. Preserve it as a trailing '\n' on
        // the sentence it follows so split_for_pauses can apply the longer
        // paragraph pause downstream. Trimming it away (the old behaviour) made
        // paragraphs sound like ordinary sentence breaks, and a line with no
        // terminal punctuation (e.g. a heading) run straight into the next.
        if ch == '\n' {
            let trimmed = current.trim().to_string();
            current.clear();
            if !trimmed.is_empty() {
                // Line ended without terminal punctuation.
                sentences.push(format!("{trimmed}\n"));
            } else if let Some(last) = sentences.last_mut() {
                // Newline after a punctuation-ended sentence, or a blank line:
                // mark the preceding sentence as ending a paragraph (once).
                if !last.ends_with('\n') {
                    last.push('\n');
                }
            }
            continue;
        }

        current.push(ch);
        // Dots inside single-letter abbreviations ("i.e.", "e.g.", "U.S.")
        // don't end a sentence (same guard as piper::split_for_pauses).
        let abbrev_dot = ch == '.' && {
            let mut it = current.chars().rev();
            it.next();
            match it.next() {
                Some(prev) if prev.is_alphabetic() => {
                    it.next().map_or(true, |b| !b.is_alphanumeric())
                }
                _ => false,
            }
        };
        let is_end = !abbrev_dot
            && matches!(ch, '.' | '!' | '?')
            && chars.peek().map_or(true, |c| c.is_whitespace());
        if is_end {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
            current.clear();
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }

    if sentences.is_empty() && !text.trim().is_empty() {
        sentences.push(text.trim().to_string());
    }

    sentences
}

pub(crate) fn batch_sentences(sentences: Vec<String>, min_words: usize, max_words: usize) -> Vec<String> {
    let mut batches = Vec::new();
    let mut current = String::new();
    let mut current_words = 0;

    for sentence in sentences {
        let words = sentence.split_whitespace().count();
        if current_words > 0 && current_words + words > max_words {
            batches.push(std::mem::take(&mut current));
            current_words = 0;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&sentence);
        current_words += words;
        if current_words >= min_words {
            batches.push(std::mem::take(&mut current));
            current_words = 0;
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

pub fn custom_voices_dir() -> std::path::PathBuf {
    crate::models::ModelManager::global().base_dir.join("custom_voices")
}

pub fn list_custom_voices() -> Vec<String> {
    let dir = custom_voices_dir();
    if !dir.exists() { return vec![]; }
    let mut voices = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "bin") {
                if let Some(name) = path.file_stem() {
                    voices.push(name.to_string_lossy().into_owned());
                }
            }
        }
    }
    voices.sort();
    voices
}
