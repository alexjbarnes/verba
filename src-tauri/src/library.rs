use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

static LIBRARY: OnceLock<Library> = OnceLock::new();

/// A legacy book chapter's metadata (bodies live in `books/<id>.json`). Books
/// were removed in Sep 2026; this only lets old ones round-trip untouched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterMeta {
    pub title: String,
    pub words: u32,
    pub chars: u32,
}

/// A saved text the user can play back in the Listen reader. Persisted locally
/// as JSON.
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
    /// Source article URL; "" for pasted/typed text.
    #[serde(default)]
    pub url: String,
    /// Feed this item was imported from; "" when not from a feed. Kept as a
    /// dangling reference if the feed is deleted (the UI falls back to the
    /// URL's hostname for the badge).
    #[serde(default)]
    pub feed_id: String,
    /// Feed entry key, for exact provenance.
    #[serde(default)]
    pub guid: String,
    /// Article publication date as given by the source (feed pubDate or the
    /// page's published-time metadata), "" when unknown. Display only.
    #[serde(default)]
    pub published: String,
    /// Legacy book state: non-empty `chapters` marks an old EPUB or long-file
    /// import (`body` is "" for those). Kept on disk as it was, but never
    /// listed (see `list`). Always empty for anything added now.
    #[serde(default)]
    pub chapters: Vec<ChapterMeta>,
    #[serde(default)]
    pub current_chapter: u32,
    #[serde(default)]
    pub completed: Vec<u32>,
    /// Whether the item has been opened at least once (`mark_seen`). Drives
    /// the Library list's NEW badge (feed-imported items only), independent
    /// of playback progress. Legacy JSON predating this field loads as false.
    #[serde(default)]
    pub seen: bool,
    /// Lead image URL scraped at import (og:image and friends); "" when the
    /// source had none. Remote — rendered directly by the webview, with the
    /// generated gradient cover as fallback.
    #[serde(default)]
    pub image_url: String,
    /// Free-form labels for grouping across sources, set at import or edited
    /// later.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Read but worth keeping: archived items drop out of the main Library
    /// list into its Archived filter, progress and tags intact. Legacy JSON
    /// predating this field loads as false.
    #[serde(default)]
    pub archived: bool,
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

    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &self,
        title: String,
        body: String,
        url: String,
        feed_id: String,
        guid: String,
        published: String,
        image_url: String,
        tags: Vec<String>,
    ) -> LibraryItem {
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
            url,
            feed_id,
            guid,
            published,
            chapters: Vec::new(),
            current_chapter: 0,
            completed: Vec::new(),
            seen: false,
            image_url,
            tags: clean_tags(tags),
            archived: false,
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
        // Legacy books stay on disk untouched but are never shown.
        self.items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.chapters.is_empty())
            .cloned()
            .collect()
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

    /// Replace an item's tags (trimmed, deduplicated, empties dropped).
    pub fn set_tags(&self, id: &str, tags: Vec<String>) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.tags = clean_tags(tags);
            if let Err(e) = Self::save_to_disk(&items) {
                log::error!("Failed to save library: {e}");
            }
        }
    }

    /// Move an item into (or back out of) the archive. Nothing else changes,
    /// so unarchiving restores it exactly as it was.
    pub fn set_archived(&self, id: &str, archived: bool) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.archived = archived;
            if let Err(e) = Self::save_to_disk(&items) {
                log::error!("Failed to save library: {e}");
            }
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

    /// Mark an item opened at least once. Idempotent — a no-op (no disk
    /// write) once already true, so re-opening a previously-seen item costs
    /// nothing.
    pub fn mark_seen(&self, id: &str) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            if item.seen {
                return;
            }
            item.seen = true;
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

/// Normalise a tag list: trimmed, empties dropped, deduplicated
/// case-insensitively (keeping the first spelling) so "AI" and "ai" can't both
/// end up on one item.
fn clean_tags(tags: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim();
        if !t.is_empty() && !out.iter().any(|e| e.eq_ignore_ascii_case(t)) {
            out.push(t.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_tags_trims_dedupes_and_drops_empties() {
        let out = clean_tags(vec![
            "  AI ".into(), "".into(), "ai".into(), "   ".into(), "Rust".into(), "AI".into(),
        ]);
        // First spelling wins; case-variants and blanks are dropped.
        assert_eq!(out, vec!["AI".to_string(), "Rust".to_string()]);
        assert!(clean_tags(vec![]).is_empty());
    }

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
        let a = lib.add("My Title".into(), "some body".into(), String::new(), String::new(), String::new(), String::new(), String::new(), Vec::new());
        assert_eq!(a.title, "My Title");
        let b = lib.add("  ".into(), "First line\nsecond".into(), String::new(), String::new(), String::new(), String::new(), String::new(), Vec::new());
        assert_eq!(b.title, "First line");
        assert_ne!(a.id, b.id);
        assert_eq!(lib.items.lock().unwrap().len(), 2);
    }

    #[test]
    fn delete_removes_by_id() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "one".into(), String::new(), String::new(), String::new(), String::new(), String::new(), Vec::new());
        lib.add("t".into(), "two".into(), String::new(), String::new(), String::new(), String::new(), String::new(), Vec::new());
        lib.delete(&a.id);
        let items = lib.items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].body, "two");
    }

    #[test]
    fn set_progress_updates_item() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "body".into(), String::new(), String::new(), String::new(), String::new(), String::new(), Vec::new());
        lib.set_progress(&a.id, 42);
        assert_eq!(lib.items.lock().unwrap()[0].progress, 42);
    }

    #[test]
    fn deserializes_legacy_position_ms_alias() {
        // Old library.json used `position_ms`; the alias keeps it loading.
        let json = r#"[{"id":"1","title":"t","body":"b","created":"now","position_ms":7}]"#;
        let items: Vec<LibraryItem> = serde_json::from_str(json).unwrap();
        assert_eq!(items[0].progress, 7);
        // Provenance fields are additive; older JSON without them still loads.
        assert_eq!(items[0].url, "");
        assert_eq!(items[0].feed_id, "");
        assert_eq!(items[0].guid, "");
    }

    #[test]
    fn legacy_library_json_still_loads() {
        // Pre-chapters library.json has neither field.
        let json = r#"[{"id":"1","title":"t","body":"b","created":"now"}]"#;
        let items: Vec<LibraryItem> = serde_json::from_str(json).unwrap();
        assert!(items[0].chapters.is_empty());
        assert_eq!(items[0].current_chapter, 0);
        assert!(items[0].completed.is_empty());
        assert!(!items[0].seen);
        assert!(!items[0].archived);

        // A legacy book's fields survive a save, so hidden books stay intact.
        let book = LibraryItem {
            id: "2".into(),
            title: "Book".into(),
            body: String::new(),
            created: "now".into(),
            progress: 5,
            duration_ms: 0,
            duration_speed: 0.0,
            url: String::new(),
            feed_id: String::new(),
            guid: String::new(),
            published: String::new(),
            chapters: vec![ChapterMeta { title: "Ch1".into(), words: 10, chars: 50 }],
            current_chapter: 1,
            completed: vec![0],
            seen: false,
            image_url: String::new(),
            tags: vec!["ai".into()],
            archived: true,
        };
        let round: LibraryItem = serde_json::from_str(&serde_json::to_string(&book).unwrap()).unwrap();
        assert_eq!(round.tags, vec!["ai".to_string()]);
        assert_eq!(round.chapters.len(), 1);
        assert_eq!(round.chapters[0].words, 10);
        assert_eq!(round.completed, vec![0]);
        assert_eq!(round.current_chapter, 1);
        assert!(!round.seen);
        assert!(round.archived);
    }

    #[test]
    fn set_archived_round_trips_and_keeps_progress() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "body".into(), String::new(), String::new(), String::new(), String::new(), String::new(), vec!["ai".into()]);
        assert!(!a.archived);
        lib.set_progress(&a.id, 3);
        lib.set_archived(&a.id, true);
        {
            let got = lib.items.lock().unwrap();
            assert!(got[0].archived);
            assert_eq!(got[0].progress, 3);
            assert_eq!(got[0].tags, vec!["ai".to_string()]);
        }
        lib.set_archived(&a.id, false);
        assert!(!lib.items.lock().unwrap()[0].archived);
    }

    #[test]
    fn mark_seen_sets_true_once_and_is_idempotent() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "body".into(), String::new(), String::new(), String::new(), String::new(), String::new(), Vec::new());
        assert!(!lib.items.lock().unwrap()[0].seen);
        lib.mark_seen(&a.id);
        assert!(lib.items.lock().unwrap()[0].seen);
        lib.mark_seen(&a.id); // second call: stays true, no error
        assert!(lib.items.lock().unwrap()[0].seen);
    }
}
