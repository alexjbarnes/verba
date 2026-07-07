//! Experimental speaker labelling for the loopback (system-audio) channel.
//!
//! Per VAD segment we extract a fixed-dim speaker embedding (sherpa-onnx
//! 3D-Speaker ERes2Net) and assign it to a running cluster by cosine
//! similarity — a new "Speaker N" when nothing is close enough. Only the
//! embeddings and running centroids live in memory; they are dropped when
//! the meeting ends. No cross-meeting voiceprints are stored: a transcript
//! doesn't need them and persisted voiceprints are a privacy surface with no
//! payoff here.
//!
//! The mic channel is never diarized — it's always "You".

use std::path::Path;
use std::sync::Mutex;

use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};

/// Cosine-similarity threshold for "same speaker". Above it, a segment joins
/// the nearest cluster; below, it opens a new one. 0.65 is the middle of the
/// range sherpa's own diarization uses for these embeddings.
const SIMILARITY_THRESHOLD: f32 = 0.65;
/// Cap on distinct speakers. Beyond this, a segment folds into its nearest
/// cluster rather than inventing an (N+1)th label — meetings rarely have
/// more, and unbounded growth just produces noise.
const MAX_SPEAKERS: usize = 8;

/// Owns the embedding extractor plus the running clusterer. `label()` is the
/// only entry point the session calls, per loopback segment.
pub struct SpeakerLabeler {
    extractor: SpeakerEmbeddingExtractor,
    clusterer: Mutex<OnlineClusterer>,
}

// The sherpa extractor wraps a raw ORT-session pointer that is safe to use
// from a single thread but not shared. A SpeakerLabeler is MOVED into exactly
// one consumer thread (the loopback consumer) and never shared, so asserting
// Send is sound — the same single-owner pattern recorder.rs's SendPtr uses.
unsafe impl Send for SpeakerLabeler {}

impl SpeakerLabeler {
    /// Load the embedding model. `None` when the model isn't downloaded or
    /// the extractor fails to build — the caller then labels everything
    /// "Speaker 1".
    pub fn new(model_path: &Path) -> Option<Self> {
        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(model_path.to_string_lossy().into_owned()),
            num_threads: 1,
            debug: false,
            provider: Some("cpu".into()),
        };
        let extractor = SpeakerEmbeddingExtractor::create(&config)?;
        Some(Self {
            extractor,
            clusterer: Mutex::new(OnlineClusterer::new()),
        })
    }

    /// Label a 16kHz mono loopback segment. Any failure (too short, embed
    /// error) falls back to "Speaker 1" so a diarization hiccup never drops
    /// the utterance.
    pub fn label(&self, samples: &[f32]) -> String {
        match self.embed(samples) {
            Some(emb) => {
                let id = self.clusterer.lock().unwrap().assign(&emb);
                format!("Speaker {}", id + 1)
            }
            None => "Speaker 1".into(),
        }
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

/// Online agglomerative clustering over unit-normalized embeddings. Keeps a
/// centroid (mean direction) and count per cluster; assignment is nearest
/// centroid above threshold, else a new cluster.
pub struct OnlineClusterer {
    centroids: Vec<Centroid>,
}

struct Centroid {
    /// Sum of the normalized embeddings assigned so far (its direction is the
    /// mean; magnitude carries the count for incremental update).
    sum: Vec<f32>,
    count: u32,
}

impl OnlineClusterer {
    pub fn new() -> Self {
        Self { centroids: Vec::new() }
    }

    /// Assign an embedding to a cluster, returning its 0-based index.
    pub fn assign(&mut self, embedding: &[f32]) -> usize {
        let emb = normalize(embedding);

        // Best existing cluster by cosine similarity (centroids compared by
        // their mean direction).
        let mut best = None;
        let mut best_sim = SIMILARITY_THRESHOLD;
        for (i, c) in self.centroids.iter().enumerate() {
            let sim = cosine_to_mean(&emb, c);
            if sim >= best_sim {
                best_sim = sim;
                best = Some(i);
            }
        }

        let idx = match best {
            Some(i) => i,
            None if self.centroids.len() < MAX_SPEAKERS => {
                self.centroids.push(Centroid { sum: vec![0.0; emb.len()], count: 0 });
                self.centroids.len() - 1
            }
            // At the cap: fold into the nearest cluster regardless of the
            // threshold (never invent a 9th speaker).
            None => self.nearest(&emb),
        };

        let c = &mut self.centroids[idx];
        for (s, e) in c.sum.iter_mut().zip(emb.iter()) {
            *s += *e;
        }
        c.count += 1;
        idx
    }

    /// Index of the closest centroid by mean direction (used only at the cap,
    /// where at least one centroid exists).
    fn nearest(&self, emb: &[f32]) -> usize {
        self.centroids
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

    pub fn speaker_count(&self) -> usize {
        self.centroids.len()
    }
}

fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

/// Cosine similarity between a unit vector and a centroid's MEAN direction
/// (sum normalized on the fly, so centroids don't have to be re-normalized on
/// every update).
fn cosine_to_mean(unit: &[f32], c: &Centroid) -> f32 {
    let mean_norm = c.sum.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mean_norm <= f32::EPSILON {
        return 0.0;
    }
    unit.iter().zip(c.sum.iter()).map(|(a, b)| a * (b / mean_norm)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two clearly-separated directions in embedding space must stay in
    // separate clusters; repeats of each must merge.
    #[test]
    fn separates_and_merges() {
        let mut c = OnlineClusterer::new();
        let a1 = vec![1.0, 0.0, 0.0, 0.0];
        let a2 = vec![0.98, 0.02, 0.0, 0.0];
        let b1 = vec![0.0, 0.0, 1.0, 0.0];
        assert_eq!(c.assign(&a1), 0);
        assert_eq!(c.assign(&b1), 1); // orthogonal -> new cluster
        assert_eq!(c.assign(&a2), 0); // near a1 -> merges
        assert_eq!(c.speaker_count(), 2);
    }

    #[test]
    fn near_duplicates_single_cluster() {
        let mut c = OnlineClusterer::new();
        for i in 0..5 {
            let v = vec![1.0, 0.01 * i as f32, 0.0, 0.0];
            assert_eq!(c.assign(&v), 0);
        }
        assert_eq!(c.speaker_count(), 1);
    }

    #[test]
    fn caps_at_max_speakers() {
        let mut c = OnlineClusterer::new();
        // Nine mutually near-orthogonal vectors; the 9th must fold in, not
        // create a 9th cluster.
        for i in 0..9 {
            let mut v = vec![0.0; 9];
            v[i] = 1.0;
            c.assign(&v);
        }
        assert_eq!(c.speaker_count(), MAX_SPEAKERS);
    }

    #[test]
    fn magnitude_invariant() {
        // Scaling an embedding must not change its cluster (cosine only).
        let mut c = OnlineClusterer::new();
        assert_eq!(c.assign(&[3.0, 0.0, 0.0]), 0);
        assert_eq!(c.assign(&[0.5, 0.0, 0.0]), 0);
        assert_eq!(c.speaker_count(), 1);
    }
}
