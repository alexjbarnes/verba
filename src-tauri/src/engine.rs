//! Platform-agnostic transcription engine.
//!
//! Owns a recorder and transcriber, handles VAD streaming, background
//! segment transcription, chunk joining, post-processing, and history.
//! Platform-specific code (JNI, Tauri commands) wraps this.
//!
//! The engine is a process-wide singleton. Both the Tauri app and the
//! Android IME accessibility service share the same instance, so the
//! ONNX model is only loaded once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread::JoinHandle;

use crate::history::{ChunkTiming, History};
use crate::postprocess;
use crate::recorder::AudioRecorder;
use crate::transcribe::Transcriber;

static ENGINE: OnceLock<Mutex<Option<Engine>>> = OnceLock::new();
static INIT_CLAIMED: AtomicBool = AtomicBool::new(false);

fn engine_cell() -> &'static Mutex<Option<Engine>> {
    ENGINE.get_or_init(|| Mutex::new(None))
}

/// Atomically claim the right to build the engine. Returns true if this
/// caller won and should proceed with init. Returns false if another
/// thread already started building -- the caller should wait.
pub fn try_claim_init() -> bool {
    !INIT_CLAIMED.swap(true, Ordering::SeqCst)
}

/// Block until the engine singleton is ready (another thread is building it).
pub fn wait_until_ready() {
    while !is_initialized() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Initialize the global engine singleton. Safe to call from multiple
/// entry points (Tauri setup, JNI nativeInit) -- whichever runs first
/// creates the engine; subsequent calls are no-ops.
pub fn init_global(engine: Engine) {
    let cell = engine_cell();
    let mut guard = cell.lock().unwrap();
    if guard.is_some() {
        log::info!("Engine: already initialized, skipping");
        return;
    }
    *guard = Some(engine);
    log::info!("Engine: global singleton initialized");
}

/// Returns true if the engine singleton has been initialized.
pub fn is_initialized() -> bool {
    let cell = engine_cell();
    cell.lock().unwrap().is_some()
}

/// Run a closure with a shared reference to the engine.
pub fn with<R>(f: impl FnOnce(&Engine) -> R) -> Option<R> {
    let cell = engine_cell();
    let guard = cell.lock().unwrap();
    guard.as_ref().map(f)
}

/// Run a closure with a mutable reference to the engine.
pub fn with_mut<R>(f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    let cell = engine_cell();
    let mut guard = cell.lock().unwrap();
    guard.as_mut().map(f)
}

/// Destroy the engine, freeing all resources.
/// Also resets INIT_CLAIMED so nativeInit can re-create the engine
/// if the accessibility service outlives the app process.
pub fn destroy() {
    let cell = engine_cell();
    *cell.lock().unwrap() = None;
    INIT_CLAIMED.store(false, Ordering::SeqCst);
    log::info!("Engine: global singleton destroyed");
}

pub struct ChunkResult {
    pub text: String,
    pub audio_ms: u64,
    pub transcribe_ms: u64,
}

pub struct SegmentConsumerResult {
    pub chunks: Vec<ChunkResult>,
}

/// Final output from a transcription cycle.
pub struct TranscriptionResult {
    pub text: String,
    pub model_id: String,
    pub audio_duration_ms: u64,
    pub transcribe_ms: u64,
}

/// Intermediate state between stopping the recorder and finalizing transcription.
/// Designed to be created while holding a lock, then finalized after releasing it.
pub struct PendingTranscription {
    samples: Vec<f32>,
    transcriber: Arc<Transcriber>,
    consumer_handle: Option<JoinHandle<SegmentConsumerResult>>,
    model_id: String,
    audio_duration_ms: u64,
}

impl PendingTranscription {
    /// Run the heavy work: wait for background segments, transcribe tail,
    /// join chunks, post-process, and record in history.
    pub fn finalize(mut self) -> Option<TranscriptionResult> {
        let transcribe_start = std::time::Instant::now();

        // Wait for background segment transcription to finish
        let mut all_chunks: Vec<ChunkResult> = Vec::new();
        if let Some(handle) = self.consumer_handle {
            match handle.join() {
                Ok(result) => {
                    log::info!("Engine: got {} pre-transcribed chunks", result.chunks.len());
                    all_chunks = result.chunks;
                }
                Err(_) => log::warn!("Engine: segment consumer thread panicked"),
            }
        }

        // Transcribe remaining tail (audio after the last VAD silence boundary)
        let tail_audio_ms = (self.samples.len() as f64 / 16.0) as u64;
        if !self.samples.is_empty() && tail_audio_ms > 100 {
            // Pad with 200ms of silence so the model sees a clean trailing
            // boundary, matching the silence-bounded segments it was trained on.
            const TAIL_PAD_SAMPLES: usize = 16_000 / 5; // 200ms at 16kHz
            self.samples.extend(std::iter::repeat(0.0f32).take(TAIL_PAD_SAMPLES));

            log::info!("Engine: transcribing tail ({:.1}s + 200ms pad)", tail_audio_ms as f64 / 1000.0);
            let t = std::time::Instant::now();
            match self.transcriber.transcribe(self.samples, 16_000) {
                Ok(text) if !text.is_empty() => {
                    let transcribe_ms = t.elapsed().as_millis() as u64;
                    log::info!("Engine: tail: \"{text}\" ({transcribe_ms}ms)");
                    all_chunks.push(ChunkResult { text, audio_ms: tail_audio_ms, transcribe_ms });
                }
                Ok(_) => {}
                Err(e) => log::error!("Engine: tail transcription failed: {e}"),
            }
        }

        let total_transcribe_ms = transcribe_start.elapsed().as_millis() as u64;

        if all_chunks.is_empty() {
            log::warn!("Engine: no text produced");
            return None;
        }

        let chunk_timings: Vec<ChunkTiming> = all_chunks.iter()
            .map(|c| ChunkTiming { audio_ms: c.audio_ms, transcribe_ms: c.transcribe_ms })
            .collect();
        let all_texts: Vec<&str> = all_chunks.iter().map(|c| c.text.as_str()).collect();
        let raw_text = postprocess::join_chunks(&all_texts);

        let result = postprocess::postprocess(&raw_text);
        let full_text = result.text.clone();
        let postprocess_ms = result.total_ms;

        log::info!("Engine: final ({} chunks, {}ms audio, {}ms transcribe, {}ms postprocess): \"{}\"",
            all_chunks.len(), self.audio_duration_ms, total_transcribe_ms, postprocess_ms,
            if full_text.len() > 60 { &full_text[..60] } else { &full_text });

        History::global().add(
            full_text.clone(),
            self.model_id.clone(),
            total_transcribe_ms,
            self.audio_duration_ms,
            postprocess_ms,
            result.stages,
            chunk_timings,
        );

        Some(TranscriptionResult {
            text: full_text,
            model_id: self.model_id,
            audio_duration_ms: self.audio_duration_ms,
            transcribe_ms: total_transcribe_ms,
        })
    }

    /// Same as [`finalize_without_history`] but also skips post-processing.
    /// Returns raw joined transcription text. Used for snippet triggers
    /// where post-processing (grammar correction, capitalization) is unwanted.
    pub fn finalize_raw(mut self) -> Option<String> {
        let mut all_chunks: Vec<ChunkResult> = Vec::new();
        if let Some(handle) = self.consumer_handle {
            if let Ok(result) = handle.join() {
                all_chunks = result.chunks;
            }
        }

        let tail_audio_ms = (self.samples.len() as f64 / 16.0) as u64;
        if !self.samples.is_empty() && tail_audio_ms > 100 {
            const TAIL_PAD_SAMPLES: usize = 16_000 / 5;
            self.samples.extend(std::iter::repeat(0.0f32).take(TAIL_PAD_SAMPLES));
            match self.transcriber.transcribe(self.samples, 16_000) {
                Ok(text) if !text.is_empty() => {
                    all_chunks.push(ChunkResult { text, audio_ms: tail_audio_ms, transcribe_ms: 0 });
                }
                _ => {}
            }
        }

        if all_chunks.is_empty() { return None; }

        let all_texts: Vec<&str> = all_chunks.iter().map(|c| c.text.as_str()).collect();
        Some(postprocess::join_chunks(&all_texts))
    }

    /// Same as [`finalize`] but skips history persistence.
    /// Used for UI-driven recording (snippet wizard) where we just
    /// need the text back without creating a history entry.
    pub fn finalize_without_history(mut self) -> Option<String> {
        let mut all_chunks: Vec<ChunkResult> = Vec::new();
        if let Some(handle) = self.consumer_handle {
            if let Ok(result) = handle.join() {
                all_chunks = result.chunks;
            }
        }

        let tail_audio_ms = (self.samples.len() as f64 / 16.0) as u64;
        if !self.samples.is_empty() && tail_audio_ms > 100 {
            const TAIL_PAD_SAMPLES: usize = 16_000 / 5;
            self.samples.extend(std::iter::repeat(0.0f32).take(TAIL_PAD_SAMPLES));
            match self.transcriber.transcribe(self.samples, 16_000) {
                Ok(text) if !text.is_empty() => {
                    all_chunks.push(ChunkResult { text, audio_ms: tail_audio_ms, transcribe_ms: 0 });
                }
                _ => {}
            }
        }

        if all_chunks.is_empty() { return None; }

        let all_texts: Vec<&str> = all_chunks.iter().map(|c| c.text.as_str()).collect();
        let raw_text = postprocess::join_chunks(&all_texts);
        let result = postprocess::postprocess(&raw_text);
        Some(result.text)
    }
}

pub struct Engine {
    recorder: AudioRecorder,
    transcriber: Arc<Transcriber>,
    model_id: String,
    segment_consumer: Mutex<Option<JoinHandle<SegmentConsumerResult>>>,
    recording_start: Mutex<Option<std::time::Instant>>,
}

impl Engine {
    pub fn new(
        recorder: AudioRecorder,
        transcriber: Transcriber,
        model_id: String,
    ) -> Self {
        Self {
            recorder,
            transcriber: Arc::new(transcriber),
            model_id,
            segment_consumer: Mutex::new(None),
            recording_start: Mutex::new(None),
        }
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn recorder(&self) -> &AudioRecorder {
        &self.recorder
    }

    /// Clone of the shared transcriber handle. Meeting mode's two segment
    /// consumers transcribe through this same worker (its internal mutex
    /// interleaves segments), so Parakeet is loaded exactly once.
    pub fn transcriber_arc(&self) -> Arc<Transcriber> {
        self.transcriber.clone()
    }

    /// Replace the transcriber with a new model.
    pub fn reload_model(&mut self, transcriber: Transcriber, model_id: String) {
        self.transcriber = Arc::new(transcriber);
        self.model_id = model_id;
    }

    /// Warm up the post-processing pipeline.
    /// The ONNX model is already loaded by the time Engine::new() returns.
    pub fn preload(&self) {
        postprocess::warm_up();
    }

    /// Start recording with background VAD segment transcription.
    /// If the recorder thread has died, attempts to respawn it once before failing.
    pub fn start_streaming(&mut self) -> Result<(), String> {
        if !self.recorder.is_alive() {
            log::warn!("Engine: recorder thread dead, attempting respawn");
            self.recorder.respawn()?;
            log::info!("Engine: recorder thread respawned");
        }

        let seg_rx = self.recorder.start_streaming()?;
        log::info!("Engine: recording started (streaming segments)");
        *self.recording_start.lock().unwrap() = Some(std::time::Instant::now());

        let transcriber = self.transcriber.clone();
        let handle = std::thread::Builder::new()
            .name("segment-transcriber".into())
            .spawn(move || consume_segments(seg_rx, transcriber))
            .ok();
        *self.segment_consumer.lock().unwrap() = handle;

        Ok(())
    }

    /// Stop recording and extract state needed for transcription.
    /// Returns a PendingTranscription that can be finalized without holding
    /// any lock on the Engine.
    pub fn stop_recording(&self) -> Result<PendingTranscription, String> {
        let samples = self.recorder.stop()?;

        let audio_duration_ms = self.recording_start
            .lock().unwrap()
            .take()
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);

        let transcriber = self.transcriber.clone();
        let handle = self.segment_consumer.lock().unwrap().take();
        let model_id = self.model_id.clone();

        Ok(PendingTranscription {
            samples,
            transcriber,
            consumer_handle: handle,
            model_id,
            audio_duration_ms,
        })
    }
}

/// Transcribe VAD speech segments as they arrive from the recorder.
fn consume_segments(
    seg_rx: mpsc::Receiver<Vec<f32>>,
    transcriber: Arc<Transcriber>,
) -> SegmentConsumerResult {
    let mut chunks = Vec::new();

    while let Ok(segment) = seg_rx.recv() {
        let audio_ms = (segment.len() as f64 / 16.0) as u64;
        if audio_ms < 300 {
            continue;
        }

        log::info!("Engine: transcribing segment ({:.1}s)", audio_ms as f64 / 1000.0);
        let t = std::time::Instant::now();
        match transcriber.transcribe(segment, 16_000) {
            Ok(text) if !text.is_empty() => {
                let transcribe_ms = t.elapsed().as_millis() as u64;
                log::info!("Engine: segment: \"{text}\" ({transcribe_ms}ms)");
                chunks.push(ChunkResult { text, audio_ms, transcribe_ms });
            }
            Ok(_) => {}
            Err(e) => log::warn!("Engine: segment transcription error: {e}"),
        }
    }
    log::info!("Engine: segment consumer done, {} chunks", chunks.len());
    SegmentConsumerResult { chunks }
}
