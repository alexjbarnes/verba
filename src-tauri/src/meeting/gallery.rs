//! Cross-meeting speaker voiceprint gallery.
//!
//! A small JSON store of `{ name -> [embeddings] }` at `data_dir/verba/
//! speakers.json`. Naming a speaker in one meeting enrolls their voiceprint
//! here; later meetings load it into a sherpa `SpeakerEmbeddingManager` and
//! identify the same person live (see speakers.rs). This is the only place a
//! biometric derivative is persisted — the raw audio never is.
//!
//! Mirrors store.rs: an in-memory `Mutex` over the entries, tmp+rename writes,
//! reload-from-disk so external edits are picked up.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sherpa_onnx::SpeakerEmbeddingManager;

static GALLERY: OnceLock<Gallery> = OnceLock::new();

/// Keep at most this many voiceprints per person. Several (from different
/// sessions/conditions) improve recall; unbounded growth just bloats the file
/// and slows matching. Oldest drops first.
const MAX_PER_NAME: usize = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerEntry {
    pub name: String,
    /// Unit-normalized speaker embeddings, all the extractor's dimension.
    pub embeddings: Vec<Vec<f32>>,
}

pub struct Gallery {
    items: Mutex<Vec<SpeakerEntry>>,
}

impl Gallery {
    pub fn global() -> &'static Self {
        GALLERY.get_or_init(|| Self {
            items: Mutex::new(Self::load_from_disk().unwrap_or_default()),
        })
    }

    fn path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("verba").join("speakers.json"))
    }

    fn load_from_disk() -> Option<Vec<SpeakerEntry>> {
        let raw = std::fs::read_to_string(Self::path()?).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn save(items: &[SpeakerEntry]) -> Result<(), String> {
        let path = Self::path().ok_or("no data dir")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("speakers dir: {e}"))?;
        }
        let raw = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, raw).map_err(|e| format!("speakers write: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("speakers rename: {e}"))
    }

    /// Enrolled names, newest activity irrelevant — just the set.
    pub fn names(&self) -> Vec<String> {
        self.items.lock().unwrap().iter().map(|e| e.name.clone()).collect()
    }

    /// Build a sherpa manager seeded with every enrolled voiceprint of the
    /// given dimension (embeddings from an older, differently-sized model are
    /// skipped rather than corrupting the index). `None` if the manager can't
    /// be created.
    pub fn build_manager(&self, dim: i32) -> Option<SpeakerEmbeddingManager> {
        let mgr = SpeakerEmbeddingManager::create(dim)?;
        for e in self.items.lock().unwrap().iter() {
            let ok: Vec<Vec<f32>> =
                e.embeddings.iter().filter(|v| v.len() == dim as usize).cloned().collect();
            if !ok.is_empty() {
                mgr.add_list(&e.name, &ok);
            }
        }
        Some(mgr)
    }

    /// Enroll a voiceprint under `name`, capping stored prints per person.
    pub fn add(&self, name: &str, embedding: Vec<f32>) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("speaker name is empty".into());
        }
        let mut items = self.items.lock().unwrap();
        match items.iter_mut().find(|e| e.name == name) {
            Some(e) => {
                e.embeddings.push(embedding);
                while e.embeddings.len() > MAX_PER_NAME {
                    e.embeddings.remove(0);
                }
            }
            None => items.push(SpeakerEntry { name: name.to_string(), embeddings: vec![embedding] }),
        }
        Self::save(&items)
    }

    /// Rename an enrolled speaker, merging voiceprints if `to` already exists.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let to = to.trim();
        if to.is_empty() {
            return Err("new name is empty".into());
        }
        let mut items = self.items.lock().unwrap();
        let Some(pos) = items.iter().position(|e| e.name == from) else {
            return Ok(());
        };
        let moved = items.remove(pos);
        match items.iter_mut().find(|e| e.name == to) {
            Some(e) => {
                e.embeddings.extend(moved.embeddings);
                while e.embeddings.len() > MAX_PER_NAME {
                    e.embeddings.remove(0);
                }
            }
            None => items.push(SpeakerEntry { name: to.to_string(), embeddings: moved.embeddings }),
        }
        Self::save(&items)
    }

    pub fn remove(&self, name: &str) -> Result<(), String> {
        let mut items = self.items.lock().unwrap();
        items.retain(|e| e.name != name);
        Self::save(&items)
    }
}

/// Per-meeting speaker voiceprints, saved at stop keyed by meeting id, so a
/// later "name Speaker N" can enroll that voiceprint into the gallery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingVoiceprint {
    pub label: String,
    pub embedding: Vec<f32>,
}

fn voiceprints_path(id: &str) -> Option<PathBuf> {
    let safe: String =
        id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    dirs::data_dir().map(|d| d.join("verba").join("voiceprints").join(format!("{safe}.json")))
}

pub fn save_meeting_voiceprints(id: &str, vps: &[MeetingVoiceprint]) -> Result<(), String> {
    let path = voiceprints_path(id).ok_or("no data dir")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("voiceprints dir: {e}"))?;
    }
    let raw = serde_json::to_string(vps).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, raw).map_err(|e| format!("voiceprints write: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("voiceprints rename: {e}"))
}

pub fn load_meeting_voiceprints(id: &str) -> Vec<MeetingVoiceprint> {
    voiceprints_path(id)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn delete_meeting_voiceprints(id: &str) {
    if let Some(p) = voiceprints_path(id) {
        let _ = std::fs::remove_file(p);
    }
}

/// Unit-normalize an embedding for storage/comparison (cosine space).
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unit_length() {
        let n = normalize(&[3.0, 4.0]);
        let len = (n[0] * n[0] + n[1] * n[1]).sqrt();
        assert!((len - 1.0).abs() < 1e-6);
    }

    #[test]
    fn entry_serde_round_trip() {
        let e = SpeakerEntry { name: "Alex".into(), embeddings: vec![vec![0.1, 0.2], vec![0.3, 0.4]] };
        let raw = serde_json::to_string(&e).unwrap();
        let back: SpeakerEntry = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.name, "Alex");
        assert_eq!(back.embeddings.len(), 2);
    }
}
