//! Live speaker labelling for the loopback (system-audio) channel.
//!
//! Per VAD segment we extract a speaker embedding (sherpa 3D-Speaker ERes2Net)
//! and do two things:
//!   1. Group it into a provisional online cluster with a running voiceprint.
//!   2. Match that cluster's ACCUMULATED voiceprint against the persisted
//!      gallery (people named in past meetings). A hit labels the cluster with
//!      the enrolled name (e.g. "Alex"); otherwise it stays "Speaker N".
//!
//! Matching on the running voiceprint (not a single short segment) is what
//! makes short utterances resolve: a two-word "yeah" inherits its cluster's
//! identity once the cluster has heard enough. Online clustering is imperfect,
//! so the FINAL, accurate labels come from the offline batch pass at stop
//! (mod.rs). The mic channel is never diarized — it is always "You".

use std::path::Path;

use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig, SpeakerEmbeddingManager};

use super::gallery::{normalize, Gallery};

/// Cosine threshold for identifying a cluster as an enrolled gallery speaker.
/// 0.5 was the best live-ID default in the POC (high recall on >=3s of speech,
/// few false names).
const MATCH_THRESHOLD: f32 = 0.5;
/// Cosine threshold for grouping segments into the same provisional cluster.
const CLUSTER_THRESHOLD: f32 = 0.5;
/// Cap on distinct provisional speakers so a run of odd embeddings can't invent
/// an unbounded number of "Speaker N"s live.
const MAX_SPEAKERS: usize = 8;

/// Owns the embedding extractor, the enrolled-speaker gallery, and the live
/// provisional clusterer. `label()` is the only entry the session calls.
pub struct SpeakerLabeler {
    extractor: SpeakerEmbeddingExtractor,
    gallery: SpeakerEmbeddingManager,
    clusterer: ProvisionalClusterer,
}

// The sherpa handles wrap raw ORT pointers that are safe from a single thread
// but not shared. A SpeakerLabeler is MOVED into exactly one consumer thread
// (the loopback consumer) and never shared, so asserting Send is sound — the
// same single-owner pattern recorder.rs's SendPtr uses.
unsafe impl Send for SpeakerLabeler {}

impl SpeakerLabeler {
    /// Load the embedding model and seed the gallery from disk. `None` when the
    /// model isn't downloaded or a handle fails to build — the caller then
    /// labels everything "Speaker 1".
    pub fn new(model_path: &Path) -> Option<Self> {
        let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: Some(model_path.to_string_lossy().into_owned()),
            num_threads: 1,
            debug: false,
            provider: Some("cpu".into()),
        })?;
        let gallery = Gallery::global().build_manager(extractor.dim())?;
        Some(Self { extractor, gallery, clusterer: ProvisionalClusterer::new() })
    }

    /// Label a 16kHz mono loopback segment. Any failure (too short, embed
    /// error) falls back to "Speaker 1" so a hiccup never drops the utterance.
    pub fn label(&mut self, samples: &[f32]) -> String {
        let emb = match self.embed(samples) {
            Some(e) => normalize(&e),
            None => return "Speaker 1".into(),
        };
        let cid = self.clusterer.assign(&emb);
        // Try to name the cluster from its accumulated voiceprint (stable),
        // once, then keep the name.
        if self.clusterer.name(cid).is_none() {
            let centroid = self.clusterer.centroid(cid);
            if let Some(name) = self.gallery.search(&centroid, MATCH_THRESHOLD) {
                self.clusterer.set_name(cid, name);
            }
        }
        self.clusterer.name(cid).unwrap_or_else(|| format!("Speaker {}", cid + 1))
    }

    fn embed(&self, samples: &[f32]) -> Option<Vec<f32>> {
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(16_000, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream)
    }
}

/// A standalone embedder for the stop-time per-utterance voiceprint pass. By
/// then the live `SpeakerLabeler` has been consumed along with its thread, so
/// this builds a fresh extractor to embed each buffered segment.
pub struct Embedder {
    extractor: SpeakerEmbeddingExtractor,
}

// Same single-owner soundness as SpeakerLabeler: built and used on one thread.
unsafe impl Send for Embedder {}

impl Embedder {
    pub fn new(model_path: &Path) -> Option<Self> {
        let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: Some(model_path.to_string_lossy().into_owned()),
            num_threads: 1,
            debug: false,
            provider: Some("cpu".into()),
        })?;
        Some(Self { extractor })
    }

    /// Unit-normalized embedding of a 16kHz mono segment, or None if too short.
    pub fn embed(&self, samples: &[f32]) -> Option<Vec<f32>> {
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(16_000, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream).map(|e| normalize(&e))
    }
}

/// Online agglomerative clustering over unit embeddings, with a running
/// voiceprint (summed direction = mean) and an optional resolved name per
/// cluster. Model-free, so it unit-tests without ONNX.
pub struct ProvisionalClusterer {
    clusters: Vec<Cluster>,
    /// Cosine below which a segment opens a new cluster. Live labelling uses the
    /// between-speaker default; the within-speaker split pass raises it so one
    /// person's noisy short utterances don't fracture into phantom signatures.
    threshold: f32,
}

struct Cluster {
    /// Sum of the assigned unit embeddings; its direction is the mean.
    sum: Vec<f32>,
    count: u32,
    /// Enrolled name once the gallery has matched this cluster.
    name: Option<String>,
}

impl ProvisionalClusterer {
    pub fn new() -> Self {
        Self { clusters: Vec::new(), threshold: CLUSTER_THRESHOLD }
    }

    /// A clusterer with a custom split threshold (higher = more conservative).
    pub fn with_threshold(threshold: f32) -> Self {
        Self { clusters: Vec::new(), threshold }
    }

    /// Assign an embedding to the nearest cluster above threshold, else open a
    /// new one (capped). Returns the 0-based cluster index and folds the
    /// embedding into that cluster's running voiceprint.
    pub fn assign(&mut self, embedding: &[f32]) -> usize {
        let emb = normalize(embedding);
        let mut best = None;
        let mut best_sim = self.threshold;
        for (i, c) in self.clusters.iter().enumerate() {
            let sim = cosine_to_mean(&emb, c);
            if sim >= best_sim {
                best_sim = sim;
                best = Some(i);
            }
        }
        let idx = match best {
            Some(i) => i,
            None if self.clusters.len() < MAX_SPEAKERS => {
                self.clusters.push(Cluster { sum: vec![0.0; emb.len()], count: 0, name: None });
                self.clusters.len() - 1
            }
            None => self.nearest(&emb),
        };
        let c = &mut self.clusters[idx];
        for (s, e) in c.sum.iter_mut().zip(emb.iter()) {
            *s += *e;
        }
        c.count += 1;
        idx
    }

    /// The cluster's mean direction (unit vector), for gallery matching.
    pub fn centroid(&self, idx: usize) -> Vec<f32> {
        normalize(&self.clusters[idx].sum)
    }

    pub fn name(&self, idx: usize) -> Option<String> {
        self.clusters[idx].name.clone()
    }

    pub fn set_name(&mut self, idx: usize, name: String) {
        self.clusters[idx].name = Some(name);
    }

    pub fn speaker_count(&self) -> usize {
        self.clusters.len()
    }

    fn nearest(&self, emb: &[f32]) -> usize {
        self.clusters
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                cosine_to_mean(emb, a)
                    .partial_cmp(&cosine_to_mean(emb, b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

/// Cosine similarity between a unit vector and a cluster's MEAN direction (its
/// sum normalized on the fly, so clusters aren't renormalized every update).
fn cosine_to_mean(unit: &[f32], c: &Cluster) -> f32 {
    let mean_norm = c.sum.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mean_norm <= f32::EPSILON {
        return 0.0;
    }
    unit.iter().zip(c.sum.iter()).map(|(a, b)| a * (b / mean_norm)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_and_merges() {
        let mut c = ProvisionalClusterer::new();
        let a1 = vec![1.0, 0.0, 0.0, 0.0];
        let a2 = vec![0.98, 0.02, 0.0, 0.0];
        let b1 = vec![0.0, 0.0, 1.0, 0.0];
        assert_eq!(c.assign(&a1), 0);
        assert_eq!(c.assign(&b1), 1); // orthogonal -> new cluster
        assert_eq!(c.assign(&a2), 0); // near a1 -> merges
        assert_eq!(c.speaker_count(), 2);
    }

    #[test]
    fn caps_at_max_speakers() {
        let mut c = ProvisionalClusterer::new();
        for i in 0..9 {
            let mut v = vec![0.0; 9];
            v[i] = 1.0;
            c.assign(&v);
        }
        assert_eq!(c.speaker_count(), MAX_SPEAKERS);
    }

    #[test]
    fn magnitude_invariant() {
        let mut c = ProvisionalClusterer::new();
        assert_eq!(c.assign(&[3.0, 0.0, 0.0]), 0);
        assert_eq!(c.assign(&[0.5, 0.0, 0.0]), 0);
        assert_eq!(c.speaker_count(), 1);
    }

    #[test]
    fn name_sticks_once_set() {
        let mut c = ProvisionalClusterer::new();
        let idx = c.assign(&[1.0, 0.0, 0.0]);
        assert!(c.name(idx).is_none());
        c.set_name(idx, "Alex".into());
        c.assign(&[0.97, 0.03, 0.0]); // same cluster, more audio
        assert_eq!(c.name(idx).as_deref(), Some("Alex"));
    }

    #[test]
    fn centroid_tracks_mean_direction() {
        let mut c = ProvisionalClusterer::new();
        c.assign(&[1.0, 0.0]);
        c.assign(&[0.0, 1.0]); // orthogonal opens a 2nd cluster... check first
        let cen = c.centroid(0);
        let len = (cen[0] * cen[0] + cen[1] * cen[1]).sqrt();
        assert!((len - 1.0).abs() < 1e-6);
    }
}
