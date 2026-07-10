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
const ANCHOR_FRACTION: f32 = 0.05;
const ANCHOR_MIN_SECS: f32 = 3.0;
/// A fragment is only absorbed into an anchor it actually resembles. Below this
/// cosine it is kept as its own speaker: a distinct voice that just spoke little
/// (a presenter who only introduced the talk), not segmentation noise.
const CONSOLIDATE_MIN_SIM: f32 = 0.5;

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
    consolidate_groups(&raw, &mut groups);
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
/// share of the meeting's speech), by centroid.
fn consolidate_groups(raw: &[Span], groups: &mut Vec<Group>) {
    if groups.is_empty() {
        return;
    }
    let total: f32 = raw.iter().map(|s| s.end - s.start).sum();
    let floor = (ANCHOR_FRACTION * total).max(ANCHOR_MIN_SECS);
    let anchors: Vec<usize> = (0..groups.len()).filter(|&i| groups[i].dur >= floor).collect();
    if anchors.is_empty() {
        return; // nothing dominant — keep as is
    }
    for i in 0..groups.len() {
        if groups[i].dur >= floor {
            continue;
        }
        let mut best = anchors[0];
        let mut best_sim = f32::MIN;
        for &a in &anchors {
            let sim = cosine(&groups[i].centroid, &groups[a].centroid);
            if sim > best_sim {
                best_sim = sim;
                best = a;
            }
        }
        // Only absorb a fragment that plausibly belongs to an anchor. One that
        // resembles no anchor is a real, quiet speaker (a brief presenter), so
        // keep it rather than folding a distinct voice into someone else.
        if best_sim < CONSOLIDATE_MIN_SIM {
            continue;
        }
        let labels = std::mem::take(&mut groups[i].labels);
        groups[best].labels.extend(labels);
        groups[i].dur = 0.0; // marks empty
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

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Consolidation folds a tiny fragment into the dominant anchor it resembles.
    #[test]
    fn consolidate_absorbs_fragments() {
        let raw = vec![
            Span { start: 0.0, end: 60.0, speaker: 0 },  // dominant
            Span { start: 60.0, end: 120.0, speaker: 1 }, // dominant
            Span { start: 120.0, end: 121.0, speaker: 2 }, // fragment (1s)
        ];
        let mut groups = vec![
            Group { labels: vec![0], audio: vec![], centroid: vec![1.0, 0.0], dur: 60.0 },
            Group { labels: vec![1], audio: vec![], centroid: vec![0.0, 1.0], dur: 60.0 },
            Group { labels: vec![2], audio: vec![], centroid: vec![0.98, 0.2], dur: 1.0 },
        ];
        consolidate_groups(&raw, &mut groups);
        assert_eq!(groups.len(), 2); // fragment absorbed
        // The fragment (near [1,0]) joined anchor 0.
        assert!(groups[0].labels.contains(&2));
    }

    // A brief fragment that resembles no anchor is a distinct quiet speaker (a
    // presenter who only introduced the talk), and must be kept, not absorbed.
    #[test]
    fn consolidate_keeps_dissimilar_fragment() {
        let raw = vec![
            Span { start: 0.0, end: 60.0, speaker: 0 },
            Span { start: 60.0, end: 120.0, speaker: 1 },
            Span { start: 120.0, end: 121.0, speaker: 2 }, // 1s fragment, distinct voice
        ];
        let mut groups = vec![
            Group { labels: vec![0], audio: vec![], centroid: vec![1.0, 0.0, 0.0], dur: 60.0 },
            Group { labels: vec![1], audio: vec![], centroid: vec![0.0, 1.0, 0.0], dur: 60.0 },
            Group { labels: vec![2], audio: vec![], centroid: vec![0.0, 0.0, 1.0], dur: 1.0 },
        ];
        consolidate_groups(&raw, &mut groups);
        assert_eq!(groups.len(), 3); // orthogonal to both anchors -> kept
    }

    #[test]
    fn cosine_of_orthogonal_is_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }
}
