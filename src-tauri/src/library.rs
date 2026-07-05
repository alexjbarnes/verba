use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

static LIBRARY: OnceLock<Library> = OnceLock::new();

/// One chapter's metadata as stored on `LibraryItem`; the body itself lives in
/// `books/<id>.json` (see `Library::add_book`), never inline here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterMeta {
    pub title: String,
    /// Drives duration estimates.
    pub words: u32,
    /// Drives the library list's read percentage only. A Unicode scalar count
    /// (Rust `chars`), not a JS UTF-16 length, so it can drift slightly from
    /// the frontend's own char offsets on text with surrogate-pair characters.
    /// Harmless: resume always goes through the reader's `wordIndexAtChar` over
    /// the real chapter text, never through this count.
    pub chars: u32,
}

/// One chapter as submitted from the frontend. Tauri invoke args only — never
/// persisted as-is (see `ChapterMeta`).
#[derive(Deserialize)]
pub struct ChapterIn {
    pub title: String,
    pub body: String,
}

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
    /// For a book (`chapters` non-empty), `body` is "" and this is instead an
    /// offset into `chapters[current_chapter]`'s text (see `set_book_position`).
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
    /// Chapter list for a book; empty for a plain article (whose full text is
    /// in `body`, which stays "" for a book). Legacy JSON predating this field
    /// loads as an empty Vec, i.e. a plain article.
    #[serde(default)]
    pub chapters: Vec<ChapterMeta>,
    /// Index into `chapters` of the chapter currently open; meaningless while
    /// `chapters` is empty.
    #[serde(default)]
    pub current_chapter: u32,
    /// Indices of chapters that have actually been listened to the end
    /// (marked on tts-finished). Kept explicit rather than inferred from
    /// `current_chapter`: jumping into chapter 8 must NOT mark 1-7 done.
    #[serde(default)]
    pub completed: Vec<u32>,
    /// Whether the item has been opened at least once (`mark_seen`). Drives
    /// the Library list's NEW badge (feed-imported items only), independent
    /// of playback progress. Legacy JSON predating this field loads as false.
    #[serde(default)]
    pub seen: bool,
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

    pub fn add(
        &self,
        title: String,
        body: String,
        url: String,
        feed_id: String,
        guid: String,
        published: String,
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
        };
        let mut items = self.items.lock().unwrap();
        items.push(item.clone());
        if let Err(e) = Self::save_to_disk(&items) {
            log::error!("Failed to save library: {e}");
        }
        item
    }

    /// Create a book: one library entry whose chapters are stored in
    /// `books/<id>.json` (never inline — `library.json` is fully rewritten on
    /// every progress save, so an inline multi-chapter body would multiply
    /// that write). The book file is written FIRST, so a crash between the two
    /// writes leaves only an orphan file, never a library entry pointing at
    /// nothing.
    pub fn add_book(
        &self,
        title: String,
        chapters: Vec<ChapterIn>,
        url: String,
    ) -> Result<LibraryItem, String> {
        let title = if title.trim().is_empty() {
            chapters
                .first()
                .map(|c| {
                    if c.title.trim().is_empty() {
                        Self::derive_title(&c.body)
                    } else {
                        c.title.trim().to_string()
                    }
                })
                .unwrap_or_else(|| "Untitled".to_string())
        } else {
            title.trim().to_string()
        };
        let meta: Vec<ChapterMeta> = chapters
            .iter()
            .map(|c| ChapterMeta {
                title: c.title.trim().to_string(),
                words: c.body.split_whitespace().count() as u32,
                chars: c.body.chars().count() as u32,
            })
            .collect();

        let now = chrono::Utc::now();
        let id = format!("{:x}", now.timestamp_micros());
        let bodies: Vec<String> = chapters.into_iter().map(|c| c.body).collect();
        let path = Self::book_path(&id).ok_or("no data dir")?;
        Self::write_book_file(&path, &bodies)?;

        let item = LibraryItem {
            id,
            title,
            body: String::new(),
            created: now.to_rfc3339(),
            progress: 0,
            duration_ms: 0,
            duration_speed: 0.0,
            url,
            feed_id: String::new(),
            guid: String::new(),
            published: String::new(),
            chapters: meta,
            current_chapter: 0,
            completed: Vec::new(),
            seen: false,
        };
        let mut items = self.items.lock().unwrap();
        items.push(item.clone());
        if let Err(e) = Self::save_to_disk(&items) {
            log::error!("Failed to save library: {e}");
        }
        Ok(item)
    }

    /// Text of one chapter, read straight from the book's on-disk file (never
    /// held in memory as part of the item).
    pub fn chapter(&self, id: &str, idx: u32) -> Result<String, String> {
        let path = Self::book_path(id).ok_or("no data dir")?;
        let bodies = Self::read_book_file(&path).ok_or("book file missing or unreadable")?;
        bodies
            .into_iter()
            .nth(idx as usize)
            .ok_or_else(|| "chapter index out of range".to_string())
    }

    /// Record which chapter (and character offset within it) the reader is
    /// on — the book equivalent of `set_progress`, in one save.
    pub fn set_book_position(&self, id: &str, chapter: u32, offset: u64) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.current_chapter = chapter;
            item.progress = offset;
            if let Err(e) = Self::save_to_disk(&items) {
                log::error!("Failed to save library: {e}");
            }
        }
    }

    /// Record that a chapter was listened to the end. Idempotent; re-listening
    /// a completed chapter never duplicates the entry.
    pub fn mark_chapter_completed(&self, id: &str, chapter: u32) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            if !item.completed.contains(&chapter) {
                item.completed.push(chapter);
                if let Err(e) = Self::save_to_disk(&items) {
                    log::error!("Failed to save library: {e}");
                }
            }
        }
    }

    /// Mark a whole book read or unread in one shot (long-press action sheet).
    /// Read: every chapter completed, parked on the last one at its full
    /// length (100%). Unread: back to a fresh, unstarted book. No-op for a
    /// plain article (empty `chapters`) — there's nothing to mark.
    pub fn set_book_read(&self, id: &str, read: bool) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            if item.chapters.is_empty() {
                return;
            }
            if read {
                let last = item.chapters.len() as u32 - 1;
                item.completed = (0..item.chapters.len() as u32).collect();
                item.current_chapter = last;
                item.progress = item.chapters[last as usize].chars as u64;
            } else {
                item.completed.clear();
                item.current_chapter = 0;
                item.progress = 0;
            }
            if let Err(e) = Self::save_to_disk(&items) {
                log::error!("Failed to save library: {e}");
            }
        }
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
        let had_chapters = items.iter().any(|i| i.id == id && !i.chapters.is_empty());
        items.retain(|i| i.id != id);
        if let Err(e) = Self::save_to_disk(&items) {
            log::error!("Failed to save library: {e}");
        }
        if had_chapters {
            // Best effort: an orphaned book file just wastes a little disk.
            if let Some(path) = Self::book_path(id) {
                let _ = std::fs::remove_file(path);
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

    fn books_dir() -> Option<PathBuf> {
        #[cfg(target_os = "android")]
        {
            std::env::var_os("VERBA_DATA_DIR").map(|d| PathBuf::from(d).join("books"))
        }
        #[cfg(not(target_os = "android"))]
        {
            dirs::config_dir().map(|d| d.join("verba").join("books"))
        }
    }

    fn book_path(id: &str) -> Option<PathBuf> {
        Self::books_dir().map(|d| d.join(format!("{id}.json")))
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

    /// Write a book's chapter bodies, tmp + rename so a kill mid-write can't
    /// leave `chapter()` reading a torn file (mirrors tts_cache.rs's writer).
    fn write_book_file(path: &Path, chapters: &[String]) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        let data = serde_json::to_string(chapters).map_err(|e| format!("serialize: {e}"))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &data).map_err(|e| format!("write: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
        Ok(())
    }

    fn read_book_file(path: &Path) -> Option<Vec<String>> {
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }
}

/// Chapters more than one behind `current` — forgotten on a forward chapter
/// transition (trim keeps current + previous; backward navigation never
/// trims). Empty for `current` 0 or 1 (nothing two-behind yet).
pub fn chapters_to_forget(current: u32) -> Range<u32> {
    0..current.saturating_sub(1)
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
        let a = lib.add("My Title".into(), "some body".into(), String::new(), String::new(), String::new(), String::new());
        assert_eq!(a.title, "My Title");
        let b = lib.add("  ".into(), "First line\nsecond".into(), String::new(), String::new(), String::new(), String::new());
        assert_eq!(b.title, "First line");
        assert_ne!(a.id, b.id);
        assert_eq!(lib.items.lock().unwrap().len(), 2);
    }

    #[test]
    fn delete_removes_by_id() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "one".into(), String::new(), String::new(), String::new(), String::new());
        lib.add("t".into(), "two".into(), String::new(), String::new(), String::new(), String::new());
        lib.delete(&a.id);
        let items = lib.items.lock().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].body, "two");
    }

    #[test]
    fn set_progress_updates_item() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "body".into(), String::new(), String::new(), String::new(), String::new());
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
    fn book_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("books").join("abc123.json");
        let chapters = vec!["Chapter one.".to_string(), "Chapter two.".to_string()];
        Library::write_book_file(&path, &chapters).unwrap();
        assert_eq!(Library::read_book_file(&path).unwrap(), chapters);
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

        // A book item's chapter fields round-trip through serialize/deserialize.
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
        };
        let round: LibraryItem = serde_json::from_str(&serde_json::to_string(&book).unwrap()).unwrap();
        assert_eq!(round.chapters.len(), 1);
        assert_eq!(round.chapters[0].words, 10);
        assert_eq!(round.completed, vec![0]);
        assert_eq!(round.current_chapter, 1);
        assert!(!round.seen);
    }

    #[test]
    fn add_book_computes_meta() {
        let lib = Library { items: Mutex::new(vec![]) };
        let chapters = vec![
            ChapterIn { title: String::new(), body: "one two three".into() },
            ChapterIn { title: "Second".into(), body: String::new() },
        ];
        let item = lib.add_book(String::new(), chapters, String::new()).unwrap();
        assert_eq!(item.title, "one two three"); // derived: no title, no first-chapter title
        assert_eq!(item.body, "");
        assert_eq!(item.current_chapter, 0);
        assert_eq!(item.chapters.len(), 2);
        assert_eq!(item.chapters[0].words, 3);
        assert_eq!(item.chapters[0].chars, 13);
        assert_eq!(item.chapters[1].words, 0);
        assert_eq!(item.chapters[1].chars, 0);
    }

    #[test]
    fn chapters_to_forget_range() {
        assert_eq!(chapters_to_forget(0), 0..0);
        assert_eq!(chapters_to_forget(1), 0..0);
        assert_eq!(chapters_to_forget(2), 0..1);
        assert_eq!(chapters_to_forget(5), 0..4);
    }

    #[test]
    fn mark_chapter_completed_is_idempotent_and_unordered() {
        let lib = Library { items: Mutex::new(vec![]) };
        let item = lib
            .add_book(
                "B".into(),
                vec![
                    ChapterIn { title: "1".into(), body: "one one".into() },
                    ChapterIn { title: "2".into(), body: "two two".into() },
                ],
                String::new(),
            )
            .unwrap();
        // Finish chapter 1 first, then chapter 0 (a back-jump), then 1 again.
        lib.mark_chapter_completed(&item.id, 1);
        lib.mark_chapter_completed(&item.id, 0);
        lib.mark_chapter_completed(&item.id, 1);
        let got = lib.items.lock().unwrap()[0].completed.clone();
        assert_eq!(got, vec![1, 0]);
    }

    #[test]
    fn mark_seen_sets_true_once_and_is_idempotent() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "body".into(), String::new(), String::new(), String::new(), String::new());
        assert!(!lib.items.lock().unwrap()[0].seen);
        lib.mark_seen(&a.id);
        assert!(lib.items.lock().unwrap()[0].seen);
        lib.mark_seen(&a.id); // second call: stays true, no error
        assert!(lib.items.lock().unwrap()[0].seen);
    }

    #[test]
    fn set_book_read_round_trips_both_directions() {
        let lib = Library { items: Mutex::new(vec![]) };
        let item = lib
            .add_book(
                "B".into(),
                vec![
                    ChapterIn { title: "1".into(), body: "one one".into() },
                    ChapterIn { title: "2".into(), body: "two two two".into() },
                    ChapterIn { title: "3".into(), body: "three".into() },
                ],
                String::new(),
            )
            .unwrap();
        let last_chars = item.chapters[2].chars as u64;

        lib.set_book_read(&item.id, true);
        {
            let got = lib.items.lock().unwrap();
            assert_eq!(got[0].completed, vec![0, 1, 2]);
            assert_eq!(got[0].current_chapter, 2);
            assert_eq!(got[0].progress, last_chars);
        }

        lib.set_book_read(&item.id, false);
        let got = lib.items.lock().unwrap();
        assert!(got[0].completed.is_empty());
        assert_eq!(got[0].current_chapter, 0);
        assert_eq!(got[0].progress, 0);
    }

    #[test]
    fn set_book_read_is_noop_for_plain_article() {
        let lib = Library { items: Mutex::new(vec![]) };
        let a = lib.add("t".into(), "body".into(), String::new(), String::new(), String::new(), String::new());
        lib.set_book_read(&a.id, true);
        let got = lib.items.lock().unwrap();
        assert!(got[0].completed.is_empty());
        assert_eq!(got[0].current_chapter, 0);
        assert_eq!(got[0].progress, 0);
    }
}
