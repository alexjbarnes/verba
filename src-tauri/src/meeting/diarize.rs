//! Offline batch speaker diarization, run once at meeting stop.
//!
//! sherpa's `OfflineSpeakerDiarization` (pyannote segmentation + ERes2Net
//! embedding + agglomerative clustering) over the meeting's reconstructed
//! loopback waveform, followed by a two-pass fix for its auto speaker-count:
//!   - MERGE: pool each raw cluster's audio, re-embed (a long, stable
//!     voiceprint), fuse clusters whose centroids are close, re-pooling on each
//!     merge. Kills the wrong-speaker errors the raw auto-threshold produces.
//!   - CONSOLIDATE: absorb small fragment clusters into the nearest cluster
//!     that holds a real share of the speech, so the final count reflects real
//!     speakers.
//!
//! Proven on the AMI corpus in the diarization POC: ~13% DER and the exact
//! speaker count recovered automatically, versus ~37% for the old online
//! clusterer. See MODEL_PACKAGES.md and the meeting-mode graph node.

use std::collections::BTreeSet;
use std::path::Path;

use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
};

use super::gallery::normalize;

/// Over-cluster on purpose in pass 1; the merge/consolidate passes recover the
/// true count. (A single "right" threshold does not generalize across meetings.)
const INIT_THRESHOLD: f32 = 0.5;
/// Fuse two clusters whose pooled voiceprints exceed this cosine similarity.
const MERGE_THRESHOLD: f32 = 0.5;
/// Cap pooled audio per cluster so re-embedding stays cheap.
const POOL_CAP_SECS: f64 = 30.0;
const MERGE_CAP_SAMPLES: usize = 60 * 16_000;
/// A cluster is a real "anchor" speaker if it holds at least this share of the
/// meeting's speech (with an absolute floor). Fragments below it are absorbed.
/// A real speaker talks at least this many seconds in total. Groups below it are
/// segmentation fragments and fold into the nearest real speaker. This is an
/// ABSOLUTE floor, not a fraction of the meeting: 5% of a 42-minute meeting is
/// over two minutes, which scaled away genuinely brief speakers (a presenter who
/// only introduced a talk), while too small a floor keeps sub-second noise as
/// phantom speakers. 6s clears real chatter and drops fragments.
const MIN_SPEAKER_SECS: f32 = 6.0;
/// Second-chance fold. A cluster shorter than SECOND_CHANCE_MAX is "small". If it
/// resembles a big speaker at or above SECOND_CHANCE_SIM it is a mis-split of that
/// speaker (the merge pass just missed it) and folds in; if it resembles no big
/// speaker it is a genuinely brief, distinct speaker and is kept. Clusters at or
/// above SECOND_CHANCE_MAX are definite speakers and never fold. On two real
/// meetings the leftover mis-splits measured 0.41 and 0.50 cosine to their parent
/// (merge's own bar is 0.50), while distinct voices run well below 0.35, so 0.40
/// folds both without touching real speakers.
const SECOND_CHANCE_MAX: f32 = 20.0;
const SECOND_CHANCE_SIM: f32 = 0.40;
/// Coalesce anchor-unlike fragments with each other before the noise floor
/// applies. A sparse speaker's short conversational turns often shatter into
/// several sub-floor clusters whose individual embeddings are too noisy to
/// pass the main merge (on AMI ES2004a the same speaker's fragments measure
/// 0.34-0.47 pairwise while sitting at 0.02-0.11 to the real anchor), so a
/// relaxed bar reunites them and the pooled re-embed firms up the combined
/// voiceprint. Set below the observed same-speaker fragment range and above
/// the observed cross-speaker range.
const FRAGMENT_COALESCE_SIM: f32 = 0.35;

/// One diarized span: `[start, end)` seconds, tagged with a final speaker id.
#[derive(Debug, Clone)]
pub struct Span {
    pub start: f32,
    pub end: f32,
    pub speaker: usize,
}

/// Diarization result: time spans plus one voiceprint per final speaker id
/// (for gallery matching and enrollment).
pub struct Diarization {
    pub spans: Vec<Span>,
    pub voiceprints: Vec<Vec<f32>>,
    pub speaker_count: usize,
}

struct Group {
    labels: Vec<i32>,
    audio: Vec<f32>,
    centroid: Vec<f32>,
    dur: f32,
}

/// Run the full offline pipeline on a 16kHz mono waveform. `None` if the models
/// can't load or the diarizer fails; the caller then leaves the live labels.
pub fn diarize(seg_model: &Path, emb_model: &Path, samples: &[f32]) -> Option<Diarization> {
    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(seg_model.to_string_lossy().into_owned()),
            },
            num_threads: 2,
            ..Default::default()
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(emb_model.to_string_lossy().into_owned()),
            num_threads: 2,
            ..Default::default()
        },
        clustering: FastClusteringConfig { num_clusters: -1, threshold: INIT_THRESHOLD },
        ..Default::default()
    };
    let sd = OfflineSpeakerDiarization::create(&config)?;
    let raw: Vec<Span> = sd
        .process(samples)?
        .sort_by_start_time()
        .into_iter()
        .map(|s| Span { start: s.start, end: s.end, speaker: s.speaker as usize })
        .collect();
    if raw.is_empty() {
        return Some(Diarization { spans: Vec::new(), voiceprints: Vec::new(), speaker_count: 0 });
    }
    let raw_clusters = raw.iter().map(|s| s.speaker).collect::<BTreeSet<_>>().len();
    let total_speech: f32 = raw.iter().map(|s| s.end - s.start).sum();
    log::info!(
        "diarize: segmentation found {} raw speaker(s) over {:.1}s of speech ({} spans)",
        raw_clusters, total_speech, raw.len()
    );

    let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
        model: Some(emb_model.to_string_lossy().into_owned()),
        num_threads: 2,
        debug: false,
        provider: Some("cpu".into()),
    })?;

    // Pass 1 result -> one group per raw cluster label (that has enough audio).
    let labels: BTreeSet<i32> = raw.iter().map(|s| s.speaker as i32).collect();
    let mut groups: Vec<Group> = Vec::new();
    for lab in labels {
        let audio = pool_audio(samples, &raw, lab as usize, POOL_CAP_SECS);
        if audio.len() < 8_000 {
            continue; // < 0.5s — too little to embed reliably
        }
        let dur = label_duration(&raw, lab as usize);
        if let Some(e) = embed(&extractor, &audio) {
            groups.push(Group { labels: vec![lab], audio, centroid: normalize(&e), dur });
        }
    }

    log::info!("diarize: {} embeddable group(s) before merge", groups.len());
    merge_groups(&extractor, &mut groups);
    log::info!("diarize: {} group(s) after merge", groups.len());
    consolidate_groups(Some(&extractor), &mut groups);
    log::info!(
        "diarize: {} final speaker(s), durations(s) {:?}",
        groups.len(),
        groups.iter().map(|g| g.dur.round() as i32).collect::<Vec<_>>()
    );
    // Largest first so orphan (un-embeddable) labels fall into a real speaker.
    groups.sort_by(|a, b| b.dur.partial_cmp(&a.dur).unwrap_or(std::cmp::Ordering::Equal));

    let mut remap: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
    for (gi, g) in groups.iter().enumerate() {
        for &l in &g.labels {
            remap.insert(l, gi);
        }
    }
    let spans: Vec<Span> = raw
        .iter()
        .map(|s| Span {
            start: s.start,
            end: s.end,
            speaker: remap.get(&(s.speaker as i32)).copied().unwrap_or(0),
        })
        .collect();
    let voiceprints: Vec<Vec<f32>> = groups.iter().map(|g| g.centroid.clone()).collect();
    let speaker_count = groups.len();
    Some(Diarization { spans, voiceprints, speaker_count })
}

/// Agglomerative merge with re-pooling: fuse the closest pair above threshold,
/// concatenate their audio (capped), and re-embed for a stronger voiceprint.
fn merge_groups(extractor: &SpeakerEmbeddingExtractor, groups: &mut Vec<Group>) {
    loop {
        let mut best: Option<(usize, usize)> = None;
        let mut best_sim = MERGE_THRESHOLD;
        for i in 0..groups.len() {
            for j in (i + 1)..groups.len() {
                let sim = cosine(&groups[i].centroid, &groups[j].centroid);
                if sim > best_sim {
                    best_sim = sim;
                    best = Some((i, j));
                }
            }
        }
        let Some((i, j)) = best else { break };
        log::info!(
            "diarize: merge {:.0}s + {:.0}s at cosine {:.2}",
            groups[i].dur, groups[j].dur, best_sim
        );
        let mut removed = groups.remove(j);
        groups[i].labels.append(&mut removed.labels);
        groups[i].dur += removed.dur;
        groups[i].audio.extend(removed.audio);
        groups[i].audio.truncate(MERGE_CAP_SAMPLES);
        let audio = groups[i].audio.clone();
        if let Some(e) = embed(extractor, &audio) {
            groups[i].centroid = normalize(&e);
        }
    }
}

/// Absorb fragment clusters into the nearest "anchor" (a cluster holding a real
/// share of the meeting's speech), by centroid. Between the mis-split fold and
/// the noise floor, anchor-unlike fragments get one chance to coalesce with
/// each other: a sparse speaker's scattered turns often arrive as several
/// sub-floor clusters that only add up to a real speaker once pooled. Folding
/// them into the anchor instead would attribute their words to the wrong
/// person (observed on AMI: a 16.7s participant vanished into the main
/// speaker at cosine 0.02-0.11).
fn consolidate_groups(extractor: Option<&SpeakerEmbeddingExtractor>, groups: &mut Vec<Group>) {
    if groups.is_empty() {
        return;
    }
    // Definite speakers, long enough to anchor on. Anything shorter is a
    // candidate to fold: a fragment (noise), a mis-split of an anchor, or a
    // shard of a sparse-but-real speaker.
    let anchors: Vec<usize> =
        (0..groups.len()).filter(|&i| groups[i].dur >= SECOND_CHANCE_MAX).collect();
    if anchors.is_empty() {
        return; // nothing dominant enough to fold into — keep as is
    }
    // Diagnostic: the full similarity structure, small meetings only (the
    // dump is quadratic and a 40-minute meeting would flood the log).
    if groups.len() <= 10 {
        for i in 0..groups.len() {
            for j in (i + 1)..groups.len() {
                log::info!(
                    "diarize: pairwise [{:.1}s vs {:.1}s] cosine {:.2}",
                    groups[i].dur, groups[j].dur,
                    cosine(&groups[i].centroid, &groups[j].centroid)
                );
            }
        }
    }

    let nearest_anchor = |groups: &[Group], i: usize| -> (usize, f32) {
        let mut best = anchors[0];
        let mut best_sim = f32::MIN;
        for &a in &anchors {
            let sim = cosine(&groups[i].centroid, &groups[a].centroid);
            if sim > best_sim {
                best_sim = sim;
                best = a;
            }
        }
        (best, best_sim)
    };

    // Pass A: a small cluster that resembles an anchor is a mis-split of that
    // anchor (the merge pass just missed it) — fold it in, whatever its size.
    for i in 0..groups.len() {
        if groups[i].labels.is_empty() || groups[i].dur >= SECOND_CHANCE_MAX {
            continue;
        }
        let (best, best_sim) = nearest_anchor(groups, i);
        if best_sim >= SECOND_CHANCE_SIM {
            log::info!(
                "diarize: {} {:.1}s, nearest-anchor cosine {:.2} -> fold (mis-split)",
                if groups[i].dur < MIN_SPEAKER_SECS { "fragment" } else { "small speaker" },
                groups[i].dur, best_sim
            );
            let labels = std::mem::take(&mut groups[i].labels);
            groups[best].labels.extend(labels);
            groups[i].dur = 0.0;
        }
    }

    // Pass B: coalesce the anchor-unlike leftovers with each other (tombstone
    // in place — `anchors` holds indices, so no removals until the end).
    loop {
        let mut best: Option<(usize, usize)> = None;
        let mut best_sim = FRAGMENT_COALESCE_SIM;
        for i in 0..groups.len() {
            if groups[i].labels.is_empty() || groups[i].dur >= SECOND_CHANCE_MAX {
                continue;
            }
            for j in (i + 1)..groups.len() {
                if groups[j].labels.is_empty() || groups[j].dur >= SECOND_CHANCE_MAX {
                    continue;
                }
                let sim = cosine(&groups[i].centroid, &groups[j].centroid);
                if sim > best_sim {
                    best_sim = sim;
                    best = Some((i, j));
                }
            }
        }
        let Some((i, j)) = best else { break };
        log::info!(
            "diarize: coalesce fragments {:.1}s + {:.1}s at cosine {:.2}",
            groups[i].dur, groups[j].dur, best_sim
        );
        let mut labels = std::mem::take(&mut groups[j].labels);
        let audio_j = std::mem::take(&mut groups[j].audio);
        let centroid_j = std::mem::take(&mut groups[j].centroid);
        let dur_j = groups[j].dur;
        groups[j].dur = 0.0;
        let dur_i = groups[i].dur;
        groups[i].labels.append(&mut labels);
        groups[i].dur += dur_j;
        groups[i].audio.extend(audio_j);
        groups[i].audio.truncate(MERGE_CAP_SAMPLES);
        let audio = groups[i].audio.clone();
        let pooled = extractor.and_then(|x| embed(x, &audio)).map(|e| normalize(&e));
        let new_centroid = match pooled {
            Some(c) => c,
            None => {
                // No extractor (tests) or embed failure: duration-weighted average.
                let avg: Vec<f32> = groups[i]
                    .centroid
                    .iter()
                    .zip(centroid_j.iter())
                    .map(|(a, b)| a * dur_i + b * dur_j)
                    .collect();
                normalize(&avg)
            }
        };
        groups[i].centroid = new_centroid;
    }

    // Pass C: final disposition. Coalescing changed centroids, so a pooled
    // group may now reveal itself as an anchor's mis-split after all; below
    // the floor it is noise and folds regardless.
    for i in 0..groups.len() {
        if groups[i].labels.is_empty() || groups[i].dur >= SECOND_CHANCE_MAX {
            continue;
        }
        let (best, best_sim) = nearest_anchor(groups, i);
        let fold = best_sim >= SECOND_CHANCE_SIM || groups[i].dur < MIN_SPEAKER_SECS;
        log::info!(
            "diarize: {} {:.1}s, nearest-anchor cosine {:.2} -> {}",
            if groups[i].dur < MIN_SPEAKER_SECS { "fragment" } else { "small speaker" },
            groups[i].dur,
            best_sim,
            if !fold {
                "keep (distinct)"
            } else if best_sim >= SECOND_CHANCE_SIM {
                "fold (mis-split)"
            } else {
                "fold (noise floor)"
            }
        );
        if !fold {
            continue;
        }
        let labels = std::mem::take(&mut groups[i].labels);
        groups[best].labels.extend(labels);
        groups[i].dur = 0.0;
    }
    groups.retain(|g| !g.labels.is_empty());
}

fn pool_audio(samples: &[f32], raw: &[Span], label: usize, budget_secs: f64) -> Vec<f32> {
    let mut pooled = Vec::new();
    let mut secs = 0.0;
    for s in raw.iter().filter(|s| s.speaker == label) {
        let a = (s.start as f64 * 16_000.0) as usize;
        let b = ((s.end as f64 * 16_000.0) as usize).min(samples.len());
        if a < b {
            pooled.extend_from_slice(&samples[a..b]);
            secs += (s.end - s.start) as f64;
        }
        if secs >= budget_secs {
            break;
        }
    }
    pooled
}

fn label_duration(raw: &[Span], label: usize) -> f32 {
    raw.iter().filter(|s| s.speaker == label).map(|s| s.end - s.start).sum()
}

fn embed(extractor: &SpeakerEmbeddingExtractor, samples: &[f32]) -> Option<Vec<f32>> {
    let stream = extractor.create_stream()?;
    stream.accept_waveform(16_000, samples);
    stream.input_finished();
    if !extractor.is_ready(&stream) {
        return None;
    }
    extractor.compute(&stream)
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A sub-floor fragment folds into the nearest anchor by voiceprint.
    #[test]
    fn consolidate_absorbs_fragments() {
        let mut groups = vec![
            Group { labels: vec![0], audio: vec![], centroid: vec![1.0, 0.0], dur: 60.0 },
            Group { labels: vec![1], audio: vec![], centroid: vec![0.0, 1.0], dur: 60.0 },
            Group { labels: vec![2], audio: vec![], centroid: vec![0.98, 0.2], dur: 1.0 }, // 1s fragment
        ];
        consolidate_groups(None, &mut groups);
        assert_eq!(groups.len(), 2); // fragment absorbed
        assert!(groups[0].labels.contains(&2)); // joined the [1,0] anchor
    }

    // A short fragment is absorbed even when its voiceprint resembles no anchor:
    // sub-second clusters are segmentation noise, not real speakers.
    #[test]
    fn consolidate_absorbs_short_distinct_fragment() {
        let mut groups = vec![
            Group { labels: vec![0], audio: vec![], centroid: vec![1.0, 0.0, 0.0], dur: 600.0 },
            Group { labels: vec![1], audio: vec![], centroid: vec![0.0, 1.0, 0.0], dur: 600.0 },
            Group { labels: vec![2], audio: vec![], centroid: vec![0.0, 0.0, 1.0], dur: 2.0 },
        ];
        consolidate_groups(None, &mut groups);
        assert_eq!(groups.len(), 2); // 2s < floor -> absorbed despite being distinct
    }

    // A speaker above the floor is kept even if brief and distinct: a presenter
    // who only introduced the talk, not folded into the main speaker.
    #[test]
    fn consolidate_keeps_real_brief_speaker() {
        let mut groups = vec![
            Group { labels: vec![0], audio: vec![], centroid: vec![1.0, 0.0, 0.0], dur: 600.0 },
            Group { labels: vec![1], audio: vec![], centroid: vec![0.0, 0.0, 1.0], dur: 12.0 },
        ];
        consolidate_groups(None, &mut groups);
        assert_eq!(groups.len(), 2); // small but resembles no anchor -> kept
    }

    // A small cluster that resembles a big speaker is a mis-split and folds in.
    #[test]
    fn consolidate_folds_similar_mis_split() {
        let mut groups = vec![
            Group { labels: vec![0], audio: vec![], centroid: vec![1.0, 0.0, 0.0], dur: 600.0 },
            Group { labels: vec![1], audio: vec![], centroid: vec![0.95, 0.31, 0.0], dur: 12.0 },
        ];
        consolidate_groups(None, &mut groups);
        assert_eq!(groups.len(), 1); // 12s at cosine 0.95 -> folded into the anchor
    }

    #[test]
    fn cosine_of_orthogonal_is_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    // Real-audio integration check (manual). Runs the actual diarizer on a WAV of
    // a real multi-speaker meeting and reports the speaker breakdown. Ignored by
    // default (needs the models + an audio file). Run with:
    //
    //   SHERPA_ONNX_LIB_DIR=.desktop-deps/sherpa-onnx/lib \
    //   LD_LIBRARY_PATH=.desktop-deps/sherpa-onnx/lib \
    //   VERBA_DIARIZE_WAV=/path/meeting.wav \
    //   VERBA_SEG_MODEL=/path/seg.onnx VERBA_EMB_MODEL=/path/emb.onnx \
    //   cargo test --lib -- --ignored --nocapture diarize_real
    //
    // The models default to the installed meeting package when the env vars are
    // unset, so it also runs on a machine where Verba is set up.
    #[test]
    #[ignore]
    fn diarize_real_meeting() {
        let _ = env_logger::builder().filter_level(log::LevelFilter::Info).is_test(false).try_init();
        let wav = std::env::var("VERBA_DIARIZE_WAV").expect("set VERBA_DIARIZE_WAV");
        let samples = read_wav_16k_mono(&wav);
        eprintln!("[diarize_real] {:.1}s of audio", samples.len() as f32 / 16_000.0);
        let seg = model_path("VERBA_SEG_MODEL", |m| m.segmentation_model_path());
        let emb = model_path("VERBA_EMB_MODEL", |m| m.speaker_model_path());
        let d = diarize(&seg, &emb, &samples).expect("diarize returned None");
        let mut dur = std::collections::BTreeMap::<usize, f32>::new();
        for s in &d.spans {
            *dur.entry(s.speaker).or_default() += s.end - s.start;
        }
        eprintln!("[diarize_real] {} final speaker(s):", d.speaker_count);
        for (spk, secs) in &dur {
            eprintln!("    speaker {spk}: {secs:.1}s");
        }
        // VERBA_DUMP_SPANS=<path>: one "start end speaker" line per span, for
        // scoring a real meeting's labels against its transcript.
        if let Ok(path) = std::env::var("VERBA_DUMP_SPANS") {
            let mut out = String::new();
            for s in &d.spans {
                out.push_str(&format!("{:.3} {:.3} {}\n", s.start, s.end, s.speaker));
            }
            std::fs::write(&path, out).expect("write spans");
            eprintln!("[diarize_real] wrote {} spans to {path}", d.spans.len());
        }
        assert!(d.speaker_count > 1, "expected more than one speaker, got {}", d.speaker_count);
    }

    // Production-faithful variant of diarize_real_meeting: instead of handing
    // the diarizer the continuous WAV, push it through the recorder's VAD in
    // 256-sample chunks, timestamp each emitted segment the way the meeting
    // consumer does (receipt time minus segment length), and rebuild the
    // waveform the way refine_speakers does (silence + segments at their
    // offsets). Measures what the VAD/reconstruction path costs versus the
    // original audio. Extra env var: VERBA_VAD_MODEL (a Silero VAD ONNX).
    #[test]
    #[ignore]
    fn diarize_real_meeting_vad_path() {
        let _ = env_logger::builder().filter_level(log::LevelFilter::Info).is_test(false).try_init();
        let wav = std::env::var("VERBA_DIARIZE_WAV").expect("set VERBA_DIARIZE_WAV");
        let samples = read_wav_16k_mono(&wav);
        let vad_model = std::env::var("VERBA_VAD_MODEL")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| crate::models::ModelManager::global().vad_model_path());
        let mut vad = crate::vad::Vad::new(&vad_model).expect("load VAD");

        let mut buffered: Vec<(u64, Vec<f32>)> = Vec::new();
        for (ci, chunk) in samples.chunks(256).enumerate() {
            if let Some(seg) = vad.accept(chunk) {
                let elapsed_ms = ((ci + 1) * 256 / 16) as u64;
                let seg_ms = (seg.len() as u64) * 1000 / 16_000;
                buffered.push((elapsed_ms.saturating_sub(seg_ms), seg));
            }
        }
        let duration_ms = (samples.len() as u64) * 1000 / 16_000;
        if let Some(seg) = vad.flush() {
            let seg_ms = (seg.len() as u64) * 1000 / 16_000;
            buffered.push((duration_ms.saturating_sub(seg_ms), seg));
        }
        let speech: usize = buffered.iter().map(|(_, s)| s.len()).sum();
        eprintln!(
            "[vad_path] {} VAD segment(s), {:.1}s speech of {:.1}s audio",
            buffered.len(),
            speech as f32 / 16_000.0,
            samples.len() as f32 / 16_000.0
        );

        // refine_speakers' reconstruction, verbatim.
        let total = (duration_ms as usize) * 16 + 16_000;
        let mut wave = vec![0.0f32; total];
        for (t_ms, seg) in &buffered {
            let start = (*t_ms as usize) * 16;
            if start >= wave.len() {
                continue;
            }
            let end = (start + seg.len()).min(wave.len());
            wave[start..end].copy_from_slice(&seg[..end - start]);
        }

        let seg = model_path("VERBA_SEG_MODEL", |m| m.segmentation_model_path());
        let emb = model_path("VERBA_EMB_MODEL", |m| m.speaker_model_path());

        // Optional forensics dump: per-VAD-segment embeddings as JSON, for
        // cross-machine capture comparisons (VERBA_DUMP_EMB=/path/out.json).
        if let Ok(dump) = std::env::var("VERBA_DUMP_EMB") {
            let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
                model: Some(emb.to_string_lossy().into_owned()),
                num_threads: 2,
                debug: false,
                provider: Some("cpu".into()),
            })
            .expect("extractor");
            let mut rows = Vec::new();
            for (t_ms, seg_samples) in &buffered {
                let Some(e) = embed(&extractor, seg_samples) else { continue };
                let v = normalize(&e);
                let vs: Vec<String> = v.iter().map(|x| format!("{x:.6}")).collect();
                rows.push(format!(
                    "{{\"t_ms\":{},\"secs\":{:.2},\"emb\":[{}]}}",
                    t_ms,
                    seg_samples.len() as f32 / 16_000.0,
                    vs.join(",")
                ));
            }
            std::fs::write(&dump, format!("[{}]", rows.join(","))).expect("write dump");
            eprintln!("[vad_path] dumped {} segment embeddings to {dump}", rows.len());
        }

        let d = diarize(&seg, &emb, &wave).expect("diarize returned None");
        let mut dur = std::collections::BTreeMap::<usize, f32>::new();
        for s in &d.spans {
            *dur.entry(s.speaker).or_default() += s.end - s.start;
        }
        eprintln!("[vad_path] {} final speaker(s):", d.speaker_count);
        for (spk, secs) in &dur {
            eprintln!("    speaker {spk}: {secs:.1}s");
        }
        for s in &d.spans {
            eprintln!("    span {:7.1}-{:7.1}s -> speaker {}", s.start, s.end, s.speaker);
        }
        assert!(d.speaker_count > 1, "expected more than one speaker, got {}", d.speaker_count);
    }

    // Re-identification consistency (the "reuse the signatures on a re-run"
    // check). Enroll each diarized speaker's voiceprint, then re-embed every span
    // of the same meeting and confirm it matches its own speaker. Isolates the
    // gallery/embedding re-ID mechanism from the live online clusterer and from
    // the persisted global gallery. Same env + run line as diarize_real_meeting.
    #[test]
    #[ignore]
    fn diarize_reid_consistency() {
        use sherpa_onnx::SpeakerEmbeddingManager;
        fn norm(v: &[f32]) -> Vec<f32> {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if n <= f32::EPSILON { v.to_vec() } else { v.iter().map(|x| x / n).collect() }
        }
        let _ = env_logger::builder().filter_level(log::LevelFilter::Info).is_test(false).try_init();
        let wav = std::env::var("VERBA_DIARIZE_WAV").expect("set VERBA_DIARIZE_WAV");
        let samples = read_wav_16k_mono(&wav);
        let seg = model_path("VERBA_SEG_MODEL", |m| m.segmentation_model_path());
        let emb = model_path("VERBA_EMB_MODEL", |m| m.speaker_model_path());
        let d = diarize(&seg, &emb, &samples).expect("diarize returned None");
        assert!(!d.voiceprints.is_empty(), "no voiceprints");
        let dim = d.voiceprints[0].len() as i32;

        // Enroll the diarized speakers in a fresh manager (no global gallery).
        let mgr = SpeakerEmbeddingManager::create(dim).expect("manager");
        for (i, vp) in d.voiceprints.iter().enumerate() {
            mgr.add_list(&format!("S{i}"), &[vp.clone()]);
        }

        // Re-embed each span (>=0.5s) and search; agreement weighted by duration.
        let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: Some(emb.to_string_lossy().into_owned()),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".into()),
        })
        .expect("extractor");
        let mut per: std::collections::BTreeMap<usize, (f32, f32)> = std::collections::BTreeMap::new();
        let (mut total, mut correct) = (0.0f32, 0.0f32);
        for span in &d.spans {
            let a = (span.start as f64 * 16_000.0) as usize;
            let b = ((span.end as f64 * 16_000.0) as usize).min(samples.len());
            if b <= a || b - a < 8_000 {
                continue;
            }
            let Some(e) = embed(&extractor, &samples[a..b]) else { continue };
            let got = mgr.search(&norm(&e), 0.5);
            let dur = span.end - span.start;
            let hit = got.as_deref() == Some(format!("S{}", span.speaker).as_str());
            total += dur;
            if hit {
                correct += dur;
            }
            let ent = per.entry(span.speaker).or_default();
            ent.0 += dur;
            if hit {
                ent.1 += dur;
            }
        }
        let agree = if total > 0.0 { correct / total } else { 0.0 };
        eprintln!(
            "[reid] overall agreement {:.1}% ({:.0}s of {:.0}s scored)",
            agree * 100.0, correct, total
        );
        for (spk, (tot, cor)) in &per {
            let pct = if *tot > 0.0 { cor / tot * 100.0 } else { 0.0 };
            eprintln!("    speaker {spk}: {pct:.0}% re-identified ({tot:.0}s)");
        }
        assert!(agree > 0.5, "re-id agreement too low: {:.1}%", agree * 100.0);
    }

    // Cross-meeting re-identification: diarize two meetings, then match each
    // speaker in meeting 2 against meeting 1's speakers by voiceprint. Shared
    // people should line up (cosine >= 0.5); new people should not. This is the
    // true test of the gallery's cross-meeting recognition. Reports a table for
    // manual validation (no ground-truth labels available). Set both WAVs:
    //   VERBA_DIARIZE_WAV=m1.wav VERBA_DIARIZE_WAV2=m2.wav ... cargo test ...
    #[test]
    #[ignore]
    fn diarize_cross_meeting_reid() {
        let _ = env_logger::builder().filter_level(log::LevelFilter::Info).is_test(false).try_init();
        let seg = model_path("VERBA_SEG_MODEL", |m| m.segmentation_model_path());
        let emb = model_path("VERBA_EMB_MODEL", |m| m.speaker_model_path());
        let s1 = read_wav_16k_mono(&std::env::var("VERBA_DIARIZE_WAV").expect("VERBA_DIARIZE_WAV"));
        let s2 = read_wav_16k_mono(&std::env::var("VERBA_DIARIZE_WAV2").expect("VERBA_DIARIZE_WAV2"));
        let d1 = diarize(&seg, &emb, &s1).expect("diarize m1");
        let d2 = diarize(&seg, &emb, &s2).expect("diarize m2");
        let mut dur2: std::collections::BTreeMap<usize, f32> = std::collections::BTreeMap::new();
        for span in &d2.spans {
            *dur2.entry(span.speaker).or_default() += span.end - span.start;
        }
        eprintln!(
            "[cross] meeting 1: {} speakers, meeting 2: {} speakers",
            d1.speaker_count, d2.speaker_count
        );
        eprintln!("[cross] each meeting-2 speaker -> nearest meeting-1 speaker:");
        let mut matched = 0;
        for (j, vp2) in d2.voiceprints.iter().enumerate() {
            let (mut best_i, mut best) = (0usize, f32::MIN);
            for (i, vp1) in d1.voiceprints.iter().enumerate() {
                let c = cosine(vp2, vp1);
                if c > best {
                    best = c;
                    best_i = i;
                }
            }
            let dur = dur2.get(&j).copied().unwrap_or(0.0);
            let verdict = if best >= 0.5 {
                matched += 1;
                format!("MATCHES meeting-1 speaker {best_i}")
            } else {
                "new person".to_string()
            };
            eprintln!("    M2 speaker {j} ({dur:.0}s): nearest M1 speaker {best_i}, cosine {best:.2} -> {verdict}");
        }
        eprintln!("[cross] {matched} of {} meeting-2 speakers matched someone from meeting 1", d2.voiceprints.len());
        assert!(d2.speaker_count > 1, "meeting 2 diarization collapsed");
    }

    fn model_path(
        env: &str,
        f: impl Fn(&crate::models::ModelManager) -> Option<std::path::PathBuf>,
    ) -> std::path::PathBuf {
        if let Ok(p) = std::env::var(env) {
            return std::path::PathBuf::from(p);
        }
        f(crate::models::ModelManager::global())
            .unwrap_or_else(|| panic!("set {env} or install the meeting package"))
    }

    // Minimal WAV reader: 16-bit PCM or 32-bit float, mono or stereo (downmixed),
    // any sample rate (linearly resampled to 16kHz).
    fn read_wav_16k_mono(path: &str) -> Vec<f32> {
        let bytes = std::fs::read(path).expect("read wav");
        assert!(bytes.len() > 44 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE", "not a WAV");
        let (mut channels, mut rate, mut bits, mut fmt) = (1u16, 16_000u32, 16u16, 1u16);
        let mut data: &[u8] = &[];
        let mut i = 12;
        while i + 8 <= bytes.len() {
            let id = &bytes[i..i + 4];
            let sz = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
            let body = &bytes[i + 8..(i + 8 + sz).min(bytes.len())];
            if id == b"fmt " && body.len() >= 16 {
                fmt = u16::from_le_bytes([body[0], body[1]]);
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                bits = u16::from_le_bytes([body[14], body[15]]);
            } else if id == b"data" {
                data = body;
            }
            i += 8 + sz + (sz & 1);
        }
        let mut inter: Vec<f32> = Vec::new();
        match (fmt, bits) {
            (1, 16) => {
                for c in data.chunks_exact(2) {
                    inter.push(i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0);
                }
            }
            (3, 32) => {
                for c in data.chunks_exact(4) {
                    inter.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
            _ => panic!("unsupported WAV: format {fmt} bits {bits}"),
        }
        let ch = channels.max(1) as usize;
        let mono: Vec<f32> = if ch == 1 {
            inter
        } else {
            inter.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()
        };
        if rate == 16_000 {
            return mono;
        }
        let ratio = rate as f64 / 16_000.0;
        let n = (mono.len() as f64 / ratio) as usize;
        (0..n)
            .map(|i| {
                let pos = i as f64 * ratio;
                let a = pos as usize;
                let frac = (pos - a as f64) as f32;
                let s0 = mono[a];
                let s1 = if a + 1 < mono.len() { mono[a + 1] } else { s0 };
                s0 + (s1 - s0) * frac
            })
            .collect()
    }
}
