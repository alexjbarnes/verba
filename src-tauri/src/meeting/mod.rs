//! Meeting mode (desktop only): live dual-stream transcription with local LLM
//! summaries. See MODEL_PACKAGES.md and the meeting-mode plan.
//!
//! `MeetingSession` coordinates two `AudioRecorder`s (mic + system loopback),
//! funnels both VAD segment streams through the dictation engine's SHARED
//! transcriber (segments interleave through its worker; Parakeet loads once),
//! tags utterances with wall-clock offsets and a speaker label, and streams
//! them to the frontend. Audio only ever exists in the recorder→VAD→
//! transcriber channels — nothing is written to disk except text.
//!
//! The transcript autosaves every ~30s so a crash mid-meeting loses at most
//! half a minute of text. Dictation and Meeting exclude each other: lib.rs
//! checks `is_active()` on every dictation start path, and `meeting_start`
//! refuses while dictation records.

pub mod diarize;
pub mod gallery;
pub mod loopback;
pub mod speakers;
pub mod store;
pub mod summarize;
#[cfg(target_os = "macos")]
pub mod system_tap;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use tauri::Emitter;

use crate::recorder::{AudioRecorder, DeviceSpec};
use store::{MeetingMeta, Utterance};

static MEETING_ACTIVE: AtomicBool = AtomicBool::new(false);
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn session_slot() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

/// True while a meeting records — the dictation paths bail on this.
pub fn is_active() -> bool {
    MEETING_ACTIVE.load(Ordering::SeqCst)
}

/// Segments shorter than this never reach the transcriber (mirrors the
/// dictation engine's 300ms floor).
const MIN_SEGMENT_SAMPLES: usize = 4800; // 300ms at 16kHz

struct Session {
    meta: MeetingMeta,
    started_at: Instant,
    notes: Arc<Mutex<String>>,
    utterances: Arc<Mutex<Vec<Utterance>>>,
    mic: AudioRecorder,
    loopback: Option<AudioRecorder>,
    loopback_notice: Option<String>,
    /// Loopback speech buffered as (segment-start ms, 16kHz samples) so the
    /// offline batch pass at stop can reconstruct the timeline and diarize.
    /// Held only for the meeting's life, discarded with the Session.
    loopback_audio: Arc<Mutex<Vec<(u64, Vec<f32>)>>>,
    /// Wall-clock of the last speech segment from either stream, for
    /// end-of-meeting (silence) detection.
    last_activity: Arc<Mutex<Instant>>,
    consumers: Vec<std::thread::JoinHandle<()>>,
    autosaver: Option<std::thread::JoinHandle<()>>,
}

/// Start a meeting. Fails when one is already running or the dictation
/// engine (transcriber + VAD) isn't ready yet.
pub fn start(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let mut slot = session_slot().lock().unwrap();
    if slot.is_some() {
        return Err("a meeting is already recording".into());
    }

    let transcriber = crate::engine::with(|e| e.transcriber_arc())
        .ok_or("dictation engine not ready — download the dictation package first")?;
    let vad_path = crate::models::ModelManager::global()
        .ensure_vad_model()
        .map_err(|e| format!("voice detection unavailable: {e}"))?;

    // Optional speaker labeling for the loopback channel (experimental).
    // Off -> loopback utterances are all "Speaker 1"; model missing -> same.
    let cfg = crate::config::AppConfig::load();
    let labeler = if cfg.meeting_diarize {
        crate::models::ModelManager::global()
            .speaker_model_path()
            .and_then(|p| speakers::SpeakerLabeler::new(&p))
    } else {
        None
    };

    // Mic: the meeting-specific device if chosen, else the configured/default
    // input. System audio: the chosen output (loopback) when the platform can.
    let mic_spec = if cfg.meeting_mic_device.is_empty() {
        DeviceSpec::ConfigInput
    } else {
        DeviceSpec::InputByName(cfg.meeting_mic_device.clone())
    };
    let mic = AudioRecorder::new_with_device(Some(&vad_path), mic_spec)?;
    let preferred_output = Some(cfg.meeting_output_device.as_str()).filter(|s| !s.is_empty());
    let (loopback_rec, loopback_notice) = match loopback::resolve(preferred_output) {
        loopback::Loopback::Available(spec) => {
            match AudioRecorder::new_with_device(Some(&vad_path), spec) {
                Ok(r) => (Some(r), None),
                Err(e) => (None, Some(format!("System audio unavailable: {e}"))),
            }
        }
        loopback::Loopback::Unsupported(reason) => (None, Some(reason)),
    };

    let now = chrono::Local::now();
    let meta = MeetingMeta {
        id: format!("{:x}", chrono::Utc::now().timestamp_micros()),
        title: format!("Meeting {}", now.format("%-d %b %H:%M")),
        started: chrono::Utc::now().to_rfc3339(),
        duration_ms: 0,
        utterance_count: 0,
        transcript_path: String::new(),
        summary_path: String::new(),
        summarizer_id: String::new(),
        unnamed_speakers: 0,
    };
    let filename = store::meeting_filename(&now.format("%Y-%m-%d %H-%M").to_string(), "Meeting");

    let started_at = Instant::now();
    let notes = Arc::new(Mutex::new(String::new()));
    let utterances = Arc::new(Mutex::new(Vec::<Utterance>::new()));

    // Start the streams BEFORE spawning consumers so a failed mic start
    // doesn't leave orphan threads.
    let mic_rx = mic.start_streaming()?;
    let loop_rx = match &loopback_rec {
        Some(r) => match r.start_streaming() {
            Ok(rx) => Some(rx),
            Err(e) => {
                let _ = mic.stop();
                return Err(format!("loopback start failed: {e}"));
            }
        },
        None => None,
    };

    MEETING_ACTIVE.store(true, Ordering::SeqCst);

    let loopback_audio = Arc::new(Mutex::new(Vec::<(u64, Vec<f32>)>::new()));
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let mut consumers = Vec::new();
    consumers.push(spawn_consumer(
        mic_rx,
        "mic",
        transcriber.clone(),
        started_at,
        utterances.clone(),
        app.clone(),
        None,
        None,
        last_activity.clone(),
    ));
    if let Some(rx) = loop_rx {
        consumers.push(spawn_consumer(
            rx,
            "system",
            transcriber.clone(),
            started_at,
            utterances.clone(),
            app.clone(),
            labeler,
            Some(loopback_audio.clone()),
            last_activity.clone(),
        ));
    }

    // Crash-safety autosave: rewrite the transcript markdown every ~30s while
    // the meeting runs. Watches MEETING_ACTIVE at 1s granularity so stop()
    // doesn't wait half a minute for the thread.
    let autosaver = {
        let meta = meta.clone();
        let filename = filename.clone();
        let notes = notes.clone();
        let utterances = utterances.clone();
        let app = app.clone();
        let last_activity = last_activity.clone();
        let silence_timeout = crate::config::AppConfig::load().meeting_silence_timeout_secs;
        std::thread::Builder::new()
            .name("meeting-autosave".into())
            .spawn(move || {
                let mut ticks = 0u32;
                let mut silence_notified = false;
                while MEETING_ACTIVE.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    ticks += 1;
                    // End-of-meeting: offer to stop after a long quiet stretch.
                    if silence_timeout > 0 {
                        let silent = last_activity.lock().unwrap().elapsed().as_secs() as u32;
                        if silent >= silence_timeout && !silence_notified {
                            silence_notified = true;
                            let _ = app
                                .emit("meeting-silence", serde_json::json!({ "silent_secs": silent }));
                        } else if silent < silence_timeout {
                            silence_notified = false; // activity resumed; re-arm
                        }
                    }
                    if ticks % 30 != 0 {
                        continue;
                    }
                    let cfg = crate::config::AppConfig::load();
                    let notes = notes.lock().unwrap().clone();
                    let utts = utterances.lock().unwrap().clone();
                    if utts.is_empty() && notes.trim().is_empty() {
                        continue;
                    }
                    let mut meta = meta.clone();
                    meta.utterance_count = utts.len() as u32;
                    let md = store::transcript_markdown(&meta, &notes, &utts);
                    match store::write_markdown(&cfg.meeting_transcript_dir, &filename, &md) {
                        Ok(path) => {
                            meta.transcript_path = path.to_string_lossy().into_owned();
                            let _ = store::MeetingStore::global().upsert(meta);
                        }
                        Err(e) => log::warn!("meeting autosave failed: {e}"),
                    }
                }
            })
            .ok()
    };

    let notice = loopback_notice.clone();
    *slot = Some(Session {
        meta,
        started_at,
        notes,
        utterances,
        mic,
        loopback: loopback_rec,
        loopback_notice,
        loopback_audio,
        last_activity,
        consumers,
        autosaver,
    });
    drop(slot);

    let payload = serde_json::json!({
        "state": "recording",
        "loopback_ok": notice.is_none(),
        "notice": notice,
    });
    let _ = app.emit("meeting-state", payload.clone());
    Ok(payload)
}

/// One VAD segment stream -> transcriber -> tagged utterances + events.
#[allow(clippy::too_many_arguments)]
fn spawn_consumer(
    rx: std::sync::mpsc::Receiver<Vec<f32>>,
    source: &'static str,
    transcriber: Arc<crate::transcribe::Transcriber>,
    started_at: Instant,
    utterances: Arc<Mutex<Vec<Utterance>>>,
    app: tauri::AppHandle,
    mut labeler: Option<speakers::SpeakerLabeler>,
    audio_buf: Option<Arc<Mutex<Vec<(u64, Vec<f32>)>>>>,
    last_activity: Arc<Mutex<Instant>>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("meeting-{source}"))
        .spawn(move || {
            for segment in rx.iter() {
                if segment.len() < MIN_SEGMENT_SAMPLES {
                    continue;
                }
                *last_activity.lock().unwrap() = Instant::now();
                // Tag with when the segment STARTED: elapsed minus its length.
                let seg_ms = (segment.len() as u64) * 1000 / 16_000;
                let t_ms = started_at.elapsed().as_millis() as u64;
                let t_ms = t_ms.saturating_sub(seg_ms);
                // Cluster the speaker from the SAME samples before the
                // transcriber consumes them. Mic is always "You".
                let speaker = if source == "mic" {
                    "You".to_string()
                } else if let Some(l) = labeler.as_mut() {
                    l.label(&segment)
                } else {
                    "Speaker 1".to_string()
                };
                // Keep the loopback audio (with its start time) for the batch
                // diarization pass at stop. Cloned before transcribe consumes it.
                if let Some(buf) = &audio_buf {
                    buf.lock().unwrap().push((t_ms, segment.clone()));
                }
                match transcriber.transcribe(segment, 16_000) {
                    Ok(text) => {
                        // Meetings keep verbatim wording but strip disfluencies
                        // ("um"/"uh"/false starts) — the dictation pipeline never
                        // runs here, so do the one stage that matters for reading.
                        let text = crate::postprocess::remove_fillers(text.trim());
                        if text.is_empty() {
                            continue;
                        }
                        let utterance = Utterance {
                            source: source.into(),
                            speaker,
                            text,
                            t_ms,
                            embedding: None, // attached at stop from the buffered audio
                        };
                        utterances.lock().unwrap().push(utterance.clone());
                        let _ = app.emit("meeting-utterance", &utterance);
                    }
                    Err(e) => log::warn!("meeting {source} transcribe failed: {e}"),
                }
            }
        })
        .expect("spawn meeting consumer")
}

/// Stop the meeting: flush tails, write the final transcript, index it.
/// Returns the finished meta (summarization is a separate step).
pub fn stop(
    app: tauri::AppHandle,
    final_notes: String,
    title: Option<String>,
) -> Result<MeetingMeta, String> {
    let session = session_slot()
        .lock()
        .unwrap()
        .take()
        .ok_or("no meeting is recording")?;
    MEETING_ACTIVE.store(false, Ordering::SeqCst);

    if !final_notes.is_empty() {
        *session.notes.lock().unwrap() = final_notes;
    }

    // Stopping the recorders closes their segment channels; consumers drain
    // what's queued and exit. Tails come back as raw samples.
    let transcriber = crate::engine::with(|e| e.transcriber_arc());
    let mut tails: Vec<(&'static str, Vec<f32>)> = Vec::new();
    match session.mic.stop() {
        Ok(t) => tails.push(("mic", t)),
        Err(e) => log::warn!("mic stop: {e}"),
    }
    if let Some(rec) = &session.loopback {
        match rec.stop() {
            Ok(t) => tails.push(("system", t)),
            Err(e) => log::warn!("loopback stop: {e}"),
        }
    }
    for h in session.consumers {
        let _ = h.join();
    }
    if let Some(h) = session.autosaver {
        let _ = h.join();
    }

    let duration_ms = session.started_at.elapsed().as_millis() as u64;
    if let Some(t) = transcriber {
        for (source, samples) in tails {
            if samples.len() < MIN_SEGMENT_SAMPLES {
                continue;
            }
            let seg_ms = (samples.len() as u64) * 1000 / 16_000;
            let t_ms = duration_ms.saturating_sub(seg_ms);
            if source == "system" {
                session.loopback_audio.lock().unwrap().push((t_ms, samples.clone()));
            }
            if let Ok(text) = t.transcribe(samples, 16_000) {
                let text = crate::postprocess::remove_fillers(text.trim());
                if !text.is_empty() {
                    session.utterances.lock().unwrap().push(Utterance {
                        source: source.into(),
                        speaker: (if source == "mic" { "You" } else { "Speaker 1" }).into(),
                        text,
                        t_ms,
                        embedding: None, // attached below from the buffered audio
                    });
                }
            }
        }
    }

    // Offline batch diarization: reconstruct the loopback timeline and relabel
    // the system utterances accurately. Best-effort — leaves live labels on any
    // gap (models missing, no system audio, diarizer failure).
    refine_speakers(&app, &session.meta.id, &session.loopback_audio, duration_ms, &session.utterances);

    // Per-utterance voiceprints: embed each system utterance from its buffered
    // audio so speakers stay traceable through later edits without the audio.
    attach_utterance_voiceprints(&session.utterances, &session.loopback_audio);

    // Final transcript + index entry.
    let cfg = crate::config::AppConfig::load();
    let notes = session.notes.lock().unwrap().clone();
    let mut utts = session.utterances.lock().unwrap().clone();
    utts.sort_by_key(|u| u.t_ms);
    let mut meta = session.meta.clone();
    meta.duration_ms = duration_ms;
    meta.utterance_count = utts.len() as u32;
    if let Some(t) = title {
        let t = t.trim();
        if !t.is_empty() {
            meta.title = t.to_string();
        }
    }
    meta.unnamed_speakers = {
        let mut s = std::collections::BTreeSet::new();
        for u in &utts {
            if u.source == "system" && u.speaker.starts_with("Speaker ") {
                s.insert(u.speaker.clone());
            }
        }
        s.len() as u32
    };
    let started_local = chrono::DateTime::parse_from_rfc3339(&meta.started)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H-%M").to_string())
        .unwrap_or_else(|_| "meeting".into());
    let filename = store::meeting_filename(&started_local, "Meeting");
    let md = store::transcript_markdown(&meta, &notes, &utts);
    let path = store::write_markdown(&cfg.meeting_transcript_dir, &filename, &md)?;
    meta.transcript_path = path.to_string_lossy().into_owned();
    // Structured sidecar (source of truth for speaker ops; carries voiceprints).
    let _ = store::save_transcript(&meta.id, &utts);
    store::MeetingStore::global().upsert(meta.clone())?;

    let _ = app.emit("meeting-state", serde_json::json!({ "state": "idle" }));
    Ok(meta)
}

/// Reconstruct the loopback timeline from buffered speech, run offline
/// diarization, and relabel the `system` utterances with accurate, gallery-
/// matched speaker names. Best-effort: returns quietly if the models aren't
/// present or the diarizer fails, leaving the live labels in place.
fn refine_speakers(
    app: &tauri::AppHandle,
    id: &str,
    buffer: &Arc<Mutex<Vec<(u64, Vec<f32>)>>>,
    duration_ms: u64,
    utterances: &Arc<Mutex<Vec<Utterance>>>,
) {
    let mm = crate::models::ModelManager::global();
    let (Some(seg_model), Some(emb_model)) = (mm.segmentation_model_path(), mm.speaker_model_path())
    else {
        return;
    };

    // Reconstruct a continuous 16kHz waveform: silence everywhere, each buffered
    // speech segment placed at its start offset (pyannote needs the timeline).
    let total = (duration_ms as usize) * 16 + 16_000;
    let mut wave = vec![0.0f32; total];
    {
        let buf = buffer.lock().unwrap();
        if buf.is_empty() {
            return;
        }
        for (t_ms, samples) in buf.iter() {
            let start = (*t_ms as usize) * 16;
            if start >= wave.len() {
                continue;
            }
            let end = (start + samples.len()).min(wave.len());
            wave[start..end].copy_from_slice(&samples[..end - start]);
        }
    }

    let _ = app.emit("meeting-state", serde_json::json!({ "state": "diarizing", "id": id }));
    let Some(diar) = diarize::diarize(&seg_model, &emb_model, &wave) else {
        return;
    };
    if diar.spans.is_empty() {
        return;
    }

    // Name each final speaker: gallery match on its voiceprint, else "Speaker N".
    let dim = diar.voiceprints.first().map(|v| v.len() as i32).unwrap_or(0);
    let manager = gallery::Gallery::global().build_manager(dim);
    let labels: Vec<(String, bool)> = diar
        .voiceprints
        .iter()
        .enumerate()
        .map(|(i, vp)| match manager.as_ref().and_then(|m| m.search(vp, 0.5)) {
            Some(name) => (name, true),
            None => (format!("Speaker {}", i + 1), false),
        })
        .collect();

    // Strengthen the gallery for anyone we recognized with this fresh voiceprint.
    for (i, (name, matched)) in labels.iter().enumerate() {
        if *matched {
            let _ = gallery::Gallery::global().add(name, diar.voiceprints[i].clone());
        }
    }

    // Persist every speaker's voiceprint so naming a "Speaker N" later can
    // enroll it into the gallery for future meetings.
    let vps: Vec<gallery::MeetingVoiceprint> = labels
        .iter()
        .zip(diar.voiceprints.iter())
        .map(|((name, _), vp)| gallery::MeetingVoiceprint { label: name.clone(), embedding: vp.clone() })
        .collect();
    let _ = gallery::save_meeting_voiceprints(id, &vps);

    // Relabel system utterances by their segment's majority diarized speaker.
    let mut label_by_tms: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    {
        let buf = buffer.lock().unwrap();
        for (t_ms, samples) in buf.iter() {
            let start = *t_ms as f32 / 1000.0;
            let end = start + samples.len() as f32 / 16_000.0;
            if let Some(sid) = majority_speaker(&diar.spans, start, end) {
                if let Some((name, _)) = labels.get(sid) {
                    label_by_tms.insert(*t_ms, name.clone());
                }
            }
        }
    }

    let mut utts = utterances.lock().unwrap();
    for u in utts.iter_mut() {
        if u.source == "system" {
            if let Some(name) = label_by_tms.get(&u.t_ms) {
                u.speaker = name.clone();
            }
        }
    }
}

/// Embed each system utterance from its buffered audio (matched by segment
/// start time) and store the voiceprint on the utterance, so speakers stay
/// traceable through later split/merge/re-cluster without keeping the audio.
/// Best-effort: no-op if the speaker model is absent or the buffer is empty.
/// Mic ("You") utterances are never embedded — that channel isn't diarized.
fn attach_utterance_voiceprints(
    utterances: &Arc<Mutex<Vec<Utterance>>>,
    buffer: &Arc<Mutex<Vec<(u64, Vec<f32>)>>>,
) {
    let Some(model) = crate::models::ModelManager::global().speaker_model_path() else {
        return;
    };
    let Some(embedder) = speakers::Embedder::new(&model) else {
        return;
    };
    let by_tms: std::collections::HashMap<u64, Vec<f32>> = {
        let buf = buffer.lock().unwrap();
        if buf.is_empty() {
            return;
        }
        buf.iter().map(|(t, s)| (*t, s.clone())).collect()
    };
    let mut utts = utterances.lock().unwrap();
    for u in utts.iter_mut() {
        if u.source == "system" && u.embedding.is_none() {
            if let Some(samples) = by_tms.get(&u.t_ms) {
                u.embedding = embedder.embed(samples);
            }
        }
    }
}

/// The diarized speaker with the most overlap over `[start, end)` seconds.
fn majority_speaker(spans: &[diarize::Span], start: f32, end: f32) -> Option<usize> {
    let mut acc: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    for s in spans {
        let overlap = (s.end.min(end) - s.start.max(start)).max(0.0);
        if overlap > 0.0 {
            *acc.entry(s.speaker).or_insert(0.0) += overlap;
        }
    }
    acc.into_iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(k, _)| k)
}

/// Abandon the meeting: stop capture, remove any autosaved transcript and
/// index entry. Nothing is kept.
pub fn cancel(app: tauri::AppHandle) -> Result<(), String> {
    let session = session_slot()
        .lock()
        .unwrap()
        .take()
        .ok_or("no meeting is recording")?;
    MEETING_ACTIVE.store(false, Ordering::SeqCst);

    let _ = session.mic.stop();
    if let Some(rec) = &session.loopback {
        let _ = rec.stop();
    }
    for h in session.consumers {
        let _ = h.join();
    }
    if let Some(h) = session.autosaver {
        let _ = h.join();
    }
    let _ = store::MeetingStore::global().delete(&session.meta.id, true);

    let _ = app.emit("meeting-state", serde_json::json!({ "state": "idle" }));
    Ok(())
}

/// Replace the live notes buffer (debounced from the frontend textarea).
pub fn note_set(text: String) -> Result<(), String> {
    let slot = session_slot().lock().unwrap();
    let session = slot.as_ref().ok_or("no meeting is recording")?;
    *session.notes.lock().unwrap() = text;
    Ok(())
}

/// Current session snapshot, for UI resync after a reload.
pub fn status() -> serde_json::Value {
    let slot = session_slot().lock().unwrap();
    match slot.as_ref() {
        Some(s) => serde_json::json!({
            "state": "recording",
            "id": s.meta.id,
            "title": s.meta.title,
            "elapsed_ms": s.started_at.elapsed().as_millis() as u64,
            "utterances": s.utterances.lock().unwrap().clone(),
            "notes": s.notes.lock().unwrap().clone(),
            "loopback_ok": s.loopback.is_some(),
            "notice": s.loopback_notice,
        }),
        None => serde_json::json!({ "state": "idle" }),
    }
}

/// Parse a stored transcript markdown back into notes + speaker/text lines,
/// so summarization (at stop time or a later re-run) has a single source.
/// The format is ours (store::transcript_markdown): a `## Notes` block then
/// `## Transcript` with `**[MM:SS] Speaker:** text` lines.
fn parse_transcript(md: &str) -> (String, Vec<summarize::TranscriptLine>) {
    let mut notes = String::new();
    let mut lines = Vec::new();
    let mut section = "";
    for raw in md.lines() {
        if let Some(h) = raw.strip_prefix("## ") {
            section = if h.trim() == "Notes" {
                "notes"
            } else if h.trim() == "Transcript" {
                "transcript"
            } else {
                ""
            };
            continue;
        }
        match section {
            "notes" => {
                notes.push_str(raw);
                notes.push('\n');
            }
            "transcript" => {
                // **[MM:SS] Speaker:** text
                if let Some(rest) = raw.strip_prefix("**[") {
                    if let Some((_, after)) = rest.split_once("] ") {
                        if let Some((speaker, text)) = after.split_once(":** ") {
                            lines.push(summarize::TranscriptLine {
                                speaker: speaker.to_string(),
                                text: text.to_string(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (notes.trim().to_string(), lines)
}

/// Summarize a stored meeting with the chosen local LLM, writing the summary
/// markdown next to the transcript and recording its path. Emits
/// `meeting-summary-progress {id, stage, done, total}`. Runs on a blocking
/// thread (the LLM is CPU-heavy); the model is loaded per job and dropped
/// after so its multi-GB session doesn't linger.
pub fn summarize(app: tauri::AppHandle, id: String) -> Result<MeetingMeta, String> {
    let store = store::MeetingStore::global();
    let mut meta = store.get(&id).ok_or("meeting not found")?;
    if meta.transcript_path.is_empty() {
        return Err("no transcript to summarize".into());
    }

    let cfg = crate::config::AppConfig::load();
    if cfg.meeting_summarizer.is_empty() {
        return Err("no summarizer model installed — choose one in Settings".into());
    }
    let model_dir = crate::models::ModelManager::global()
        .summarizer_dir(&cfg.meeting_summarizer)
        .ok_or("summarizer model not downloaded")?;

    let md = std::fs::read_to_string(&meta.transcript_path)
        .map_err(|e| format!("read transcript: {e}"))?;
    let (notes, lines) = parse_transcript(&md);
    if lines.is_empty() && notes.is_empty() {
        return Err("transcript is empty".into());
    }

    let emit = |stage: &str, done: usize, total: usize| {
        let _ = app.emit(
            "meeting-summary-progress",
            serde_json::json!({ "id": id, "stage": stage, "done": done, "total": total }),
        );
    };

    emit("loading", 0, 1);
    let runner = summarize::LlmRunner::load(&model_dir)?;
    emit("loading", 1, 1);

    let summary = summarize::summarize_meeting(&runner, &notes, &lines, |stage, done, total| {
        emit(stage, done, total);
    })?;
    drop(runner); // free the session before writing files

    // Summary markdown beside the transcript, in the configured summary dir.
    let body = format!("# {}\n\n{summary}\n", meta.title);
    let filename = {
        let stem = std::path::Path::new(&meta.transcript_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Meeting");
        format!("{stem} Summary.md")
    };
    let path = store::write_markdown(&cfg.meeting_summary_dir, &filename, &body)?;
    meta.summary_path = path.to_string_lossy().into_owned();
    meta.summarizer_id = cfg.meeting_summarizer.clone();
    store.upsert(meta.clone())?;

    emit("done", 1, 1);
    Ok(meta)
}

/// Rename a speaker in a finished meeting: enroll their stored voiceprint under
/// the new name (so future meetings recognize them live) and rewrite the
/// transcript. Renaming into a name that already appears merges the two.
pub fn rename_speaker(id: String, from: String, to: String) -> Result<MeetingMeta, String> {
    let to = to.trim().to_string();
    if to.is_empty() {
        return Err("name is empty".into());
    }
    let store = store::MeetingStore::global();
    let meta = store.get(&id).ok_or("meeting not found")?;
    if meta.transcript_path.is_empty() {
        return Err("no transcript".into());
    }

    // Enroll the voiceprint we saved for `from` under `to`, then relabel it in
    // the sidecar so a later merge/rename still resolves.
    let mut vps = gallery::load_meeting_voiceprints(&id);
    if let Some(v) = vps.iter().find(|v| v.label == from) {
        gallery::Gallery::global().add(&to, v.embedding.clone())?;
    }
    for v in vps.iter_mut() {
        if v.label == from {
            v.label = to.clone();
        }
    }
    let _ = gallery::save_meeting_voiceprints(&id, &vps);

    // Rewrite the transcript with the renamed speaker.
    let md = std::fs::read_to_string(&meta.transcript_path)
        .map_err(|e| format!("read transcript: {e}"))?;
    let new_md = store::rename_speaker_in_markdown(&md, &from, &to);
    let path = std::path::PathBuf::from(&meta.transcript_path);
    let dir = path.parent().and_then(|p| p.to_str()).ok_or("bad transcript path")?;
    let filename = path.file_name().and_then(|f| f.to_str()).ok_or("bad transcript path")?;
    store::write_markdown(dir, filename, &new_md)?;

    // Refresh the unnamed-speaker count so the meetings-list badge updates.
    let (_, lines) = parse_transcript(&new_md);
    let mut remaining = std::collections::BTreeSet::new();
    for l in &lines {
        if l.speaker != "You" && l.speaker.starts_with("Speaker ") {
            remaining.insert(l.speaker.clone());
        }
    }
    let mut meta = meta;
    meta.unnamed_speakers = remaining.len() as u32;
    store.upsert(meta.clone())?;
    Ok(meta)
}

/// A speaker in a finished meeting, with a recognizable sample utterance so the
/// user can tell who "Speaker 2" is when naming them.
#[derive(serde::Serialize)]
pub struct SpeakerInfo {
    pub name: String,
    pub sample: String,
    /// Whether the label is still an unnamed "Speaker N".
    pub unnamed: bool,
}

/// Distinct non-"You" speakers in a finished meeting's transcript, each with
/// their longest utterance as a sample, for the naming UI.
pub fn speakers(id: &str) -> Result<Vec<SpeakerInfo>, String> {
    let meta = store::MeetingStore::global().get(id).ok_or("meeting not found")?;
    if meta.transcript_path.is_empty() {
        return Err("no transcript".into());
    }
    let md = std::fs::read_to_string(&meta.transcript_path)
        .map_err(|e| format!("read transcript: {e}"))?;
    let (_, lines) = parse_transcript(&md);
    // Longest utterance per speaker is the most recognizable sample.
    let mut best: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for l in lines {
        if l.speaker == "You" {
            continue;
        }
        let entry = best.entry(l.speaker).or_default();
        if l.text.len() > entry.len() {
            *entry = l.text;
        }
    }
    Ok(best
        .into_iter()
        .map(|(name, text)| {
            let s = text.trim();
            let sample = if s.chars().count() > 140 {
                format!("{}…", s.chars().take(140).collect::<String>())
            } else {
                s.to_string()
            };
            let unnamed = name.starts_with("Speaker ");
            SpeakerInfo { name, sample, unnamed }
        })
        .collect())
}

/// One line of a finished meeting's transcript, for the structured, filterable
/// per-speaker view. `idx` is the line's position among transcript lines.
#[derive(serde::Serialize)]
pub struct TranscriptEntry {
    pub idx: usize,
    pub clock: String,
    pub speaker: String,
    pub text: String,
}

/// Structured transcript lines for a finished meeting, parsed from the stored
/// markdown (`**[MM:SS] Speaker:** text`). Powers the per-speaker view where
/// clicking a speaker filters the transcript to just their lines.
pub fn transcript(id: &str) -> Result<Vec<TranscriptEntry>, String> {
    // Prefer the structured sidecar (carries per-utterance data and stable idx);
    // meetings recorded before it fall back to parsing the markdown.
    if let Some(utts) = store::load_transcript(id) {
        return Ok(utts
            .iter()
            .enumerate()
            .map(|(i, u)| TranscriptEntry {
                idx: i,
                clock: store::fmt_clock(u.t_ms),
                speaker: u.speaker.clone(),
                text: u.text.clone(),
            })
            .collect());
    }
    let meta = store::MeetingStore::global().get(id).ok_or("meeting not found")?;
    if meta.transcript_path.is_empty() {
        return Err("no transcript".into());
    }
    let md = std::fs::read_to_string(&meta.transcript_path)
        .map_err(|e| format!("read transcript: {e}"))?;
    let mut out = Vec::new();
    let mut in_transcript = false;
    for raw in md.lines() {
        if let Some(h) = raw.strip_prefix("## ") {
            in_transcript = h.trim() == "Transcript";
            continue;
        }
        if !in_transcript {
            continue;
        }
        if let Some(rest) = raw.strip_prefix("**[") {
            if let Some((clock, after)) = rest.split_once("] ") {
                if let Some((speaker, text)) = after.split_once(":** ") {
                    out.push(TranscriptEntry {
                        idx: out.len(),
                        clock: clock.to_string(),
                        speaker: speaker.to_string(),
                        text: text.to_string(),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Replace a meeting's summary markdown — used by the editable summary panel,
/// where the user can dictate or type. Wraps the body under the title heading,
/// matching what `summarize()` writes. Works even if no summary existed yet.
pub fn set_summary(id: &str, body: &str) -> Result<MeetingMeta, String> {
    let store = store::MeetingStore::global();
    let mut meta = store.get(id).ok_or("meeting not found")?;
    let cfg = crate::config::AppConfig::load();
    let filename = if !meta.transcript_path.is_empty() {
        let stem = std::path::Path::new(&meta.transcript_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Meeting");
        format!("{stem} Summary.md")
    } else {
        store::meeting_filename(&meta.title, "Summary")
    };
    let content = format!("# {}\n\n{}\n", meta.title, body.trim());
    let path = store::write_markdown(&cfg.meeting_summary_dir, &filename, &content)?;
    meta.summary_path = path.to_string_lossy().into_owned();
    store.upsert(meta.clone())?;
    Ok(meta)
}
