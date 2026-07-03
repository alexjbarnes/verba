//! On-device log of words the user flags as mispronounced, for the author to
//! fix later (eventually reported to a server; for now saved locally with
//! export + clear, mirroring history.rs).
//!
//! Reports arrive via the Android ACTION_PROCESS_TEXT flow: the user selects a
//! word in the reader (or any app), taps "Report mispronunciation" in the text
//! selection toolbar, and ProcessTextActivity forwards the text through the JNI
//! bridge to `global().add(...)`. `global()` self-initializes so it works even
//! if the report activity launches before the main Tauri setup has run.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

static STORE: OnceLock<Mispronunciations> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MispronunciationEntry {
    pub word: String,
    /// Unix epoch milliseconds; the frontend formats it for display.
    pub reported_at_ms: u64,
}

pub struct Mispronunciations {
    entries: Mutex<Vec<MispronunciationEntry>>,
}

impl Mispronunciations {
    /// Self-initializing global (loads from disk on first use) so the
    /// PROCESS_TEXT bridge works without an explicit setup call.
    pub fn global() -> &'static Self {
        STORE.get_or_init(Self::new)
    }

    fn new() -> Self {
        Self {
            entries: Mutex::new(Self::load_from_disk().unwrap_or_default()),
        }
    }

    pub fn add(&self, word: String) {
        let word = word.trim().to_string();
        if word.is_empty() {
            return;
        }
        log::info!("mispronunciation reported: {word:?}");
        let mut entries = self.entries.lock().unwrap();
        entries.push(MispronunciationEntry { word, reported_at_ms: now_ms() });
        if let Err(e) = Self::save_to_disk(&entries) {
            log::error!("save mispronunciations: {e}");
        }
    }

    pub fn list(&self) -> Vec<MispronunciationEntry> {
        // Reload from disk to pick up entries written by the report activity.
        if let Some(v) = Self::load_from_disk() {
            *self.entries.lock().unwrap() = v;
        }
        self.entries.lock().unwrap().clone()
    }

    pub fn export(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.list()).map_err(|e| format!("serialize: {e}"))
    }

    pub fn clear(&self) {
        let mut entries = self.entries.lock().unwrap();
        entries.clear();
        if let Err(e) = Self::save_to_disk(&entries) {
            log::error!("save mispronunciations: {e}");
        }
    }

    fn path() -> Option<PathBuf> {
        #[cfg(target_os = "android")]
        {
            std::env::var_os("VERBA_DATA_DIR")
                .map(|d| PathBuf::from(d).join("mispronunciations.json"))
        }
        #[cfg(not(target_os = "android"))]
        {
            dirs::config_dir().map(|d| d.join("verba").join("mispronunciations.json"))
        }
    }

    fn load_from_disk() -> Option<Vec<MispronunciationEntry>> {
        let data = std::fs::read_to_string(Self::path()?).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_to_disk(entries: &[MispronunciationEntry]) -> Result<(), String> {
        let path = Self::path().ok_or("no data dir")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        let data = serde_json::to_string(entries).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, data).map_err(|e| format!("write: {e}"))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
