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
    fn generate_chunk(&mut self, text: &str, speed: f32, sid: i32) -> Result<Option<Vec<f32>>, String> {
        match self {
            TtsEngine::Piper(engine) => {
                let samples = engine.synth_chunk(text, sid, speed)?;
                if samples.is_empty() { Ok(None) } else { Ok(Some(samples)) }
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
    log::info!("TTS seek -> {position_ms}ms");
    if let Some(player) = lock_player().as_ref() {
        player.seek((position_ms * sr / 1000) as usize);
    }
}

pub fn num_speakers() -> i32 {
    NUM_SPEAKERS.load(Ordering::SeqCst)
}

pub fn sample_rate() -> i32 {
    SAMPLE_RATE.load(Ordering::SeqCst)
}

pub fn speak(text: &str, speed: f32, sid: i32, app: Option<tauri::AppHandle>) -> Result<(), String> {
    // Serialize the whole setup: two concurrent calls must not each spin up a
    // player. The second waits here, then stops the first's player below.
    let _speak_guard = SPEAK_LOCK.lock().unwrap_or_else(|e| e.into_inner());

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
    let wpm = 155.0 / speed;
    let estimated_secs = (word_count as f64) / (wpm as f64) * 60.0;
    let estimated_samples = (estimated_secs * sample_rate as f64) as usize;

    let sr = (sample_rate as u64).max(1);
    let app_pos = app.clone();
    let finished_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ff = finished_flag.clone();
    let on_position: Option<Box<dyn Fn(player::PlaybackPosition) + Send + 'static>> =
        app_pos.map(|app| {
            Box::new(move |pos: player::PlaybackPosition| {
                let position_ms = (pos.cursor as u64 * 1000) / sr;
                let buffered_ms = (pos.buffered as u64 * 1000) / sr;
                let estimated_ms = (pos.estimated as u64 * 1000) / sr;
                let duration_ms = if pos.gen_done { buffered_ms } else { estimated_ms.max(buffered_ms) };
                let done = pos.gen_done && pos.cursor >= pos.buffered;
                let _ = app.emit("tts-position", serde_json::json!({
                    "position_ms": position_ms,
                    "buffered_ms": buffered_ms,
                    "duration_ms": duration_ms,
                    "paused": pos.paused,
                    "finished": done,
                    "rebuffering": pos.rebuffering,
                }));
                #[cfg(target_os = "android")]
                crate::android_update_media_session(
                    position_ms as i64, duration_ms as i64, pos.paused,
                );
                if done && !ff.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let _ = app.emit("tts-finished", ());
                    #[cfg(target_os = "android")]
                    crate::android_stop_media_session();
                }
            }) as Box<dyn Fn(player::PlaybackPosition) + Send + 'static>
        });

    #[cfg(target_os = "android")]
    crate::android_start_media_session();

    let player = Arc::new(player::AudioPlayer::new(sample_rate, estimated_samples, on_position)?);
    *lock_player() = Some(player.clone());

    let player_cleanup = player.clone();
    let app_cleanup = app.clone();
    std::thread::Builder::new()
        .name("tts-generate".into())
        .spawn(move || {
            let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
                        Ok(Ok(Some(samples))) => {
                            player.push(samples.clone());
                            let elapsed_ms = t.elapsed().as_millis();
                            let audio_ms = (samples.len() as u64 * 1000) / sample_rate.max(1) as u64;
                            let rtf = if audio_ms > 0 { elapsed_ms as f32 / audio_ms as f32 } else { 0.0 };
                            log::info!(
                                "TTS [{}/{}]: {:.1}s audio in {}ms, RTF {:.2}x",
                                i + 1, total, audio_ms as f32 / 1000.0, elapsed_ms, rtf
                            );
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
    let mut sentences = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        current.push(ch);
        let is_end = match ch {
            '.' | '!' | '?' => chars.peek().map_or(true, |c| c.is_whitespace()),
            '\n' => true,
            _ => false,
        };

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
