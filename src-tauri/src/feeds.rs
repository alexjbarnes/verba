//! RSS/Atom feed subscriptions for the Listen reader. Persisted locally as
//! JSON, mirroring library.rs. The backend only stores subscriptions and does
//! the HTTP fetch (CORS-free, conditional GET); parsing the XML, deciding what
//! is new, and importing articles all happen in the frontend, which owns the
//! Readability pipeline.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

static FEEDS: OnceLock<Feeds> = OnceLock::new();

/// Hard safety cap on stored entry keys per feed. `seen` must hold EVERY key
/// currently listed in the feed — a capped FIFO churns on feeds that list
/// more entries than the cap (each poll evicts live keys, which then look
/// new again and resurrect old posts). The 10 MiB fetch limit makes feeds
/// beyond this size absurd.
const SEEN_CURRENT_CAP: usize = 2000;

/// How many recently-departed keys (no longer in the feed) to retain, so a
/// briefly-resurfacing entry isn't mistaken for new.
const SEEN_GRACE_CAP: usize = 200;

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feed {
    pub id: String,
    pub url: String,
    /// Channel title captured when the feed was added.
    pub title: String,
    pub added: String,
    /// RFC3339 of the last successful check; "" = never succeeded.
    #[serde(default)]
    pub last_checked: String,
    /// HTTP validators for conditional GET, "" when the server sent none.
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub last_modified: String,
    /// Auto-import entries that appear after the last check.
    #[serde(default = "default_true")]
    pub auto_add: bool,
    /// Entry keys already processed: every key currently listed in the feed
    /// (feed order) plus up to SEEN_GRACE_CAP recently-departed ones.
    #[serde(default)]
    pub seen: Vec<String>,
}

/// Result of fetching a feed URL. `not_modified` means the validators matched
/// (HTTP 304) and `body` is empty.
#[derive(Debug, Clone, Serialize)]
pub struct FetchFeedResult {
    pub not_modified: bool,
    pub body: String,
    pub etag: String,
    pub last_modified: String,
}

pub struct Feeds {
    feeds: Mutex<Vec<Feed>>,
}

/// Compare feed URLs ignoring whitespace and a trailing slash, so the same
/// feed pasted twice with cosmetic differences is rejected as a duplicate.
fn norm_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

impl Feeds {
    pub fn init_global() -> &'static Self {
        FEEDS.get_or_init(Self::new)
    }

    pub fn global() -> &'static Self {
        FEEDS.get().expect("Feeds not initialized")
    }

    pub fn new() -> Self {
        Self {
            feeds: Mutex::new(Self::load_from_disk().unwrap_or_default()),
        }
    }

    pub fn list(&self) -> Vec<Feed> {
        // Reload from disk in case of external edits.
        if let Some(feeds) = Self::load_from_disk() {
            *self.feeds.lock().unwrap() = feeds;
        }
        self.feeds.lock().unwrap().clone()
    }

    /// Add a subscription. `seen` carries every entry key currently in the
    /// feed so nothing existing is treated as new (no backfill on add).
    pub fn add(&self, url: String, title: String, seen: Vec<String>) -> Result<Feed, String> {
        let url = url.trim().to_string();
        if url.is_empty() {
            return Err("Empty URL".into());
        }
        let mut feeds = self.feeds.lock().unwrap();
        if feeds.iter().any(|f| norm_url(&f.url) == norm_url(&url)) {
            return Err("Feed already added".into());
        }
        let now = chrono::Utc::now();
        let title = if title.trim().is_empty() {
            url.clone()
        } else {
            title.trim().to_string()
        };
        // Keep the whole list (feed/document order, newest first); truncating
        // the tail drops only the oldest keys past the safety cap.
        let mut seen = seen;
        seen.truncate(SEEN_CURRENT_CAP);
        let feed = Feed {
            id: format!("{:x}", now.timestamp_micros()),
            url,
            title,
            added: now.to_rfc3339(),
            last_checked: now.to_rfc3339(),
            etag: String::new(),
            last_modified: String::new(),
            auto_add: true,
            seen,
        };
        feeds.push(feed.clone());
        if let Err(e) = Self::save_to_disk(&feeds) {
            log::error!("Failed to save feeds: {e}");
        }
        Ok(feed)
    }

    /// Edit a subscription's title/url (from the pencil button next to a feed
    /// row). Rejects an empty title, an empty/non-http(s) url, or a url that
    /// collides with a DIFFERENT feed already on the list. When the
    /// normalized url actually changes, resets `etag`/`last_modified`/`seen`:
    /// the edited url is treated as a different feed, so the next poll
    /// re-evaluates every entry as fresh. That reset can't flood the library —
    /// the same per-poll auto-import cap that bounds a first-time `add`
    /// applies to every subsequent poll too.
    pub fn update(&self, id: &str, title: String, url: String) -> Result<(), String> {
        let title = title.trim().to_string();
        let url = url.trim().to_string();
        if title.is_empty() {
            return Err("Empty title".into());
        }
        if url.is_empty() {
            return Err("Empty URL".into());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("Not an http(s) URL".into());
        }
        let mut feeds = self.feeds.lock().unwrap();
        if feeds
            .iter()
            .any(|f| f.id != id && norm_url(&f.url) == norm_url(&url))
        {
            return Err("Another feed already uses that URL".into());
        }
        if let Some(feed) = feeds.iter_mut().find(|f| f.id == id) {
            let url_changed = norm_url(&feed.url) != norm_url(&url);
            feed.title = title;
            feed.url = url;
            if url_changed {
                feed.etag = String::new();
                feed.last_modified = String::new();
                feed.seen = Vec::new();
            }
            if let Err(e) = Self::save_to_disk(&feeds) {
                log::error!("Failed to save feeds: {e}");
            }
        }
        Ok(())
    }

    pub fn delete(&self, id: &str) {
        let mut feeds = self.feeds.lock().unwrap();
        feeds.retain(|f| f.id != id);
        if let Err(e) = Self::save_to_disk(&feeds) {
            log::error!("Failed to save feeds: {e}");
        }
    }

    pub fn set_auto_add(&self, id: &str, auto_add: bool) {
        let mut feeds = self.feeds.lock().unwrap();
        if let Some(feed) = feeds.iter_mut().find(|f| f.id == id) {
            feed.auto_add = auto_add;
            if let Err(e) = Self::save_to_disk(&feeds) {
                log::error!("Failed to save feeds: {e}");
            }
        }
    }

    /// Rebuild the seen list from the feed's CURRENT key set (passed on every
    /// poll): all current keys, then up to SEEN_GRACE_CAP formerly-seen keys
    /// that have dropped off the feed. Never evicts a live key — a capped
    /// FIFO did, which made >cap feeds churn (evicted keys looked new again
    /// and resurrected old posts on every poll).
    pub fn mark_seen(&self, id: &str, current: Vec<String>) {
        let mut feeds = self.feeds.lock().unwrap();
        if let Some(feed) = feeds.iter_mut().find(|f| f.id == id) {
            let mut next = current;
            next.truncate(SEEN_CURRENT_CAP);
            let departed: Vec<String> = feed
                .seen
                .iter()
                .filter(|k| !next.contains(k))
                .take(SEEN_GRACE_CAP)
                .cloned()
                .collect();
            next.extend(departed);
            feed.seen = next;
            if let Err(e) = Self::save_to_disk(&feeds) {
                log::error!("Failed to save feeds: {e}");
            }
        }
    }

    /// Record a successful check: bumps last_checked and stores the HTTP
    /// validators. Called only after new entries were imported and marked
    /// seen, so a crash mid-poll re-fetches with the old validators.
    pub fn checked(&self, id: &str, etag: String, last_modified: String) {
        let mut feeds = self.feeds.lock().unwrap();
        if let Some(feed) = feeds.iter_mut().find(|f| f.id == id) {
            feed.last_checked = chrono::Utc::now().to_rfc3339();
            feed.etag = etag;
            feed.last_modified = last_modified;
            if let Err(e) = Self::save_to_disk(&feeds) {
                log::error!("Failed to save feeds: {e}");
            }
        }
    }

    fn feeds_path() -> Option<PathBuf> {
        #[cfg(target_os = "android")]
        {
            std::env::var_os("VERBA_DATA_DIR").map(|d| PathBuf::from(d).join("feeds.json"))
        }
        #[cfg(not(target_os = "android"))]
        {
            dirs::config_dir().map(|d| d.join("verba").join("feeds.json"))
        }
    }

    fn load_from_disk() -> Option<Vec<Feed>> {
        let path = Self::feeds_path()?;
        let data = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_to_disk(feeds: &[Feed]) -> Result<(), String> {
        let path = Self::feeds_path().ok_or("no data dir")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        let data = serde_json::to_string(feeds).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, data).map_err(|e| format!("write: {e}"))?;
        Ok(())
    }
}

/// Fetch a feed URL's XML. Same shape as share::fetch_html (browser UA,
/// timeout, size cap, lossy UTF-8) plus conditional-GET support: the stored
/// validators ride If-None-Match / If-Modified-Since, and a 304 short-circuits
/// with `not_modified` so an unchanged feed costs no re-parse.
pub async fn fetch_feed(
    url: &str,
    etag: &str,
    last_modified: &str,
) -> Result<FetchFeedResult, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("not an http(s) URL".into());
    }
    const MAX_BYTES: u64 = 10 * 1024 * 1024;
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let mut req = client.get(url);
    if !etag.is_empty() {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if !last_modified.is_empty() {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, last_modified);
    }
    let resp = req.send().await.map_err(|e| format!("fetch: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchFeedResult {
            not_modified: true,
            body: String::new(),
            etag: etag.to_string(),
            last_modified: last_modified.to_string(),
        });
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let header = |name: reqwest::header::HeaderName| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let new_etag = header(reqwest::header::ETAG);
    let new_last_modified = header(reqwest::header::LAST_MODIFIED);
    if let Some(len) = resp.content_length() {
        if len > MAX_BYTES {
            return Err(format!("feed too large ({len} bytes)"));
        }
    }
    let bytes = resp.bytes().await.map_err(|e| format!("read body: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("feed too large".into());
    }
    Ok(FetchFeedResult {
        not_modified: false,
        body: String::from_utf8_lossy(&bytes).into_owned(),
        etag: new_etag,
        last_modified: new_last_modified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Feeds {
        Feeds { feeds: Mutex::new(vec![]) }
    }

    #[test]
    fn add_rejects_duplicate_url() {
        let feeds = empty();
        feeds.add("https://a.com/feed/".into(), "A".into(), vec![]).unwrap();
        let err = feeds.add("  https://a.com/feed  ".into(), "A2".into(), vec![]);
        assert!(err.is_err());
        assert_eq!(feeds.feeds.lock().unwrap().len(), 1);
    }

    #[test]
    fn add_falls_back_to_url_title_and_preseeds_seen() {
        let feeds = empty();
        let f = feeds
            .add("https://a.com/feed".into(), "  ".into(), vec!["k1".into(), "k2".into()])
            .unwrap();
        assert_eq!(f.title, "https://a.com/feed");
        assert_eq!(f.seen, vec!["k1".to_string(), "k2".to_string()]);
        assert!(f.auto_add);
    }

    #[test]
    fn mark_seen_never_evicts_live_keys() {
        // Regression: a 218-item feed with a 200-key FIFO churned — evicted
        // live keys looked new again and resurrected old posts every poll.
        let feeds = empty();
        let keys: Vec<String> = (0..218).map(|i| format!("k{i}")).collect();
        let f = feeds
            .add("https://a.com/feed".into(), "A".into(), keys.clone())
            .unwrap();
        assert_eq!(f.seen.len(), 218);
        feeds.mark_seen(&f.id, keys.clone());
        let seen = &feeds.list_unlocked()[0].seen;
        assert!(keys.iter().all(|k| seen.contains(k)));
        assert_eq!(seen.len(), 218);
    }

    #[test]
    fn mark_seen_retains_departed_keys_in_grace() {
        let feeds = empty();
        let f = feeds
            .add("https://a.com/feed".into(), "A".into(), vec!["a".into(), "b".into(), "c".into()])
            .unwrap();
        // "a" drops off the feed, "d" is new.
        feeds.mark_seen(&f.id, vec!["d".into(), "b".into(), "c".into()]);
        let seen = &feeds.list_unlocked()[0].seen;
        assert_eq!(seen, &["d", "b", "c", "a"]);
        // Grace buffer is bounded.
        let many: Vec<String> = (0..SEEN_GRACE_CAP + 50).map(|i| format!("old{i}")).collect();
        feeds.mark_seen(&f.id, many);
        feeds.mark_seen(&f.id, vec!["only".into()]);
        let seen = &feeds.list_unlocked()[0].seen;
        assert!(seen.len() <= 1 + SEEN_GRACE_CAP);
        assert_eq!(seen[0], "only");
    }

    #[test]
    fn add_truncates_seen_keeping_newest_first() {
        let feeds = empty();
        let keys: Vec<String> = (0..SEEN_CURRENT_CAP + 100).map(|i| format!("k{i}")).collect();
        let f = feeds.add("https://a.com/feed".into(), "A".into(), keys).unwrap();
        assert_eq!(f.seen.len(), SEEN_CURRENT_CAP);
        // Document order is newest-first; the tail (oldest) is what's dropped.
        assert_eq!(f.seen[0], "k0");
        assert!(!f.seen.contains(&format!("k{}", SEEN_CURRENT_CAP + 50)));
    }

    #[test]
    fn auto_add_defaults_true_when_absent_in_json() {
        let json = r#"[{"id":"1","url":"u","title":"t","added":"now"}]"#;
        let feeds: Vec<Feed> = serde_json::from_str(json).unwrap();
        assert!(feeds[0].auto_add);
        assert!(feeds[0].seen.is_empty());
        assert_eq!(feeds[0].etag, "");
    }

    #[test]
    fn set_auto_add_toggles() {
        let feeds = empty();
        let f = feeds.add("https://a.com/feed".into(), "A".into(), vec![]).unwrap();
        feeds.set_auto_add(&f.id, false);
        assert!(!feeds.list_unlocked()[0].auto_add);
    }

    #[test]
    fn update_rejects_duplicate_url_of_another_feed() {
        let feeds = empty();
        feeds.add("https://a.com/feed".into(), "A".into(), vec![]).unwrap();
        let b = feeds.add("https://b.com/feed".into(), "B".into(), vec![]).unwrap();
        let err = feeds.update(&b.id, "B2".into(), "https://a.com/feed".into());
        assert!(err.is_err());
        // Untouched: still its original url.
        assert_eq!(feeds.list_unlocked()[1].url, "https://b.com/feed");
    }

    #[test]
    fn update_title_only_preserves_etag_and_seen() {
        let feeds = empty();
        let f = feeds
            .add("https://a.com/feed".into(), "A".into(), vec!["k1".into()])
            .unwrap();
        feeds.checked(&f.id, "etag123".into(), "lastmod".into());
        feeds
            .update(&f.id, "New Title".into(), "https://a.com/feed".into())
            .unwrap();
        let got = &feeds.list_unlocked()[0];
        assert_eq!(got.title, "New Title");
        assert_eq!(got.url, "https://a.com/feed");
        assert_eq!(got.etag, "etag123");
        assert_eq!(got.last_modified, "lastmod");
        assert_eq!(got.seen, vec!["k1".to_string()]);
    }

    #[test]
    fn update_changed_url_clears_etag_and_seen() {
        let feeds = empty();
        let f = feeds
            .add("https://a.com/feed".into(), "A".into(), vec!["k1".into()])
            .unwrap();
        feeds.checked(&f.id, "etag123".into(), "lastmod".into());
        feeds
            .update(&f.id, "A".into(), "https://a.com/new-feed".into())
            .unwrap();
        let got = &feeds.list_unlocked()[0];
        assert_eq!(got.url, "https://a.com/new-feed");
        assert_eq!(got.etag, "");
        assert_eq!(got.last_modified, "");
        assert!(got.seen.is_empty());
    }

    impl Feeds {
        /// Test helper: read current state without the disk reload in list().
        fn list_unlocked(&self) -> Vec<Feed> {
            self.feeds.lock().unwrap().clone()
        }
    }
}
