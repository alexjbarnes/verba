use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

static LIBRARY: OnceLock<Library> = OnceLock::new();

/// A saved text the user can play back in the Listen reader. Persisted locally
/// as JSON, mirroring history.rs / snippets.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: String,
    pub title: String,
    pub body: String,
    pub created: String,
    /// Reading progress as a character offset into `body`; 0 until the reader
    /// records progress. Used for resume-where-you-left-off and the library
    /// percentage. A char offset (not ms) so it's stable across speed/voice.
    #[serde(default, alias = "position_ms")]
    pub progress: u64,
    /// Real measured playback duration (ms) of the whole article, captured once
    /// generation completes. 0 = never measured (fall back to an estimate).
    #[serde(default)]
    pub duration_ms: u64,
    /// Speed the duration was measured at, so it can be rescaled for other
    /// speeds. 0 = unmeasured.
    #[serde(default)]
    pub duration_speed: f32,
}

pub struct Library {
    items: Mutex<Vec<LibraryItem>>,
}

impl Library {
    pub fn init_global() -> &'static Self {
        LIBRARY.get_or_init(Self::new)
    }

    pub fn global() -> &'static Self {
        LIBRARY.get().expect("Library not initialized")
    }

    pub fn new() -> Self {
        Self {
            items: Mutex::new(Self::load_from_disk().unwrap_or_default()),
        }
    }

    /// Derive a title from the first non-empty line when the caller leaves it
    /// blank, so paste-only input still reads sensibly in the list.
    fn derive_title(body: &str) -> String {
        let line = body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let trimmed: String = line.chars().take(60).collect();
        if trimmed.is_empty() {
            "Untitled".to_string()
        } else {
            trimmed
        }
    }

    pub fn add(&self, title: String, body: String) -> LibraryItem {
        let title = if title.trim().is_empty() {
            Self::derive_title(&body)
        } else {
            title.trim().to_string()
        };
        let now = chrono::Utc::now();
        let item = LibraryItem {
            id: format!("{:x}", now.timestamp_micros()),
            title,
            body,
            created: now.to_rfc3339(),
            progress: 0,
            duration_ms: 0,
            duration_speed: 0.0,
        };
        let mut items = self.items.lock().unwrap();
        items.push(item.clone());
        if let Err(e) = Self::save_to_disk(&items) {
            log::error!("Failed to save library: {e}");
        }
        item
    }

    pub fn list(&self) -> Vec<LibraryItem> {
        // Reload from disk in case of external edits.
        if let Some(items) = Self::load_from_disk() {
            *self.items.lock().unwrap() = items;
        }
        self.items.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<LibraryItem> {
        self.list().into_iter().find(|i| i.id == id)
    }

    pub fn delete(&self, id: &str) {
        let mut items = self.items.lock().unwrap();
        items.retain(|i| i.id != id);
        if let Err(e) = Self::save_to_disk(&items) {
            log::error!("Failed to save library: {e}");
        }
    }

    pub fn set_progress(&self, id: &str, progress: u64) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.progress = progress;
            if let Err(e) = Self::save_to_disk(&items) {
                log::error!("Failed to save library: {e}");
            }
        }
    }

    pub fn set_duration(&self, id: &str, duration_ms: u64, speed: f32) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.duration_ms = duration_ms;
            item.duration_speed = speed;
            if let Err(e) = Self::save_to_disk(&items) {
                log::error!("Failed to save library: {e}");
            }
        }
    }

    fn library_path() -> Option<PathBuf> {
        #[cfg(target_os = "android")]
        {
            std::env::var_os("VERBA_DATA_DIR").map(|d| PathBuf::from(d).join("library.json"))
        }
        #[cfg(not(target_os = "android"))]
        {
            dirs::config_dir().map(|d| d.join("verba").join("library.json"))
        }
    }

    fn load_from_disk() -> Option<Vec<LibraryItem>> {
        let path = Self::library_path()?;
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_to_disk(items: &[LibraryItem]) -> Result<(), String> {
        let path = Self::library_path().ok_or("no data dir")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        let data = serde_json::to_string(items).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, data).map_err(|e| format!("write: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_title_uses_first_non_empty_line() {
        assert_eq!(Library::derive_title("\n  Hello world\nmore"), "Hello world");
        assert_eq!(Library::derive_title("   \n\t"), "Untitled");
    }

    #[test]
    fn derive_title_caps_length() {
        let long = "a".repeat(200);
        assert_eq!(Library::derive_title(&long).chars().count(), 60);
    }

    #[test]
    fn add_uses_explicit_title_then_derives() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("My Title".into(), "some body".into());
        assert_eq!(a.title, "My Title");
        let b = lib.add("  ".into(), "First line\nsecond".into());
        assert_eq!(b.title, "First line");
        assert_ne!(a.id, b.id);
        assert_eq!(lib.items.lock().unwrap().len(), 2);
    }

    #[test]
    fn delete_removes_by_id() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "one".into());
        lib.add("t".into(), "two".into());
        lib.delete(&a.id);
        let items = lib.items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].body, "two");
    }

    #[test]
    fn set_progress_updates_item() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "body".into());
        lib.set_progress(&a.id, 42);
        assert_eq!(lib.items.lock().unwrap()[0].progress, 42);
    }

    #[test]
    fn deserializes_legacy_position_ms_alias() {
        // Old library.json used `position_ms`; the alias keeps it loading.
        let json = r#"[{"id":"1","title":"t","body":"b","created":"now","position_ms":7}]"#;
        let items: Vec<LibraryItem> = serde_json::from_str(json).unwrap();
        assert_eq!(items[0].progress, 7);
    }
}
