//! Inbound Android share-target: receive text/URL shared to the app from another
//! app, and fetch a URL's HTML so the frontend can extract a readable article.
//!
//! Flow: Android delivers an ACTION_SEND intent to MainActivity, which calls
//! `VerbaApp.nativeSharedText(text)` (JNI export lives in lib.rs, mirroring the
//! TTS bridges). That stashes the text here and emits a `shared-text` event.
//! The frontend consumes it via the `take_shared_text` command — on startup
//! (cold share, the event fired before any listener existed) and on the event
//! (warm share, app already open). Both go through `take()` so it is delivered
//! exactly once.

use std::sync::Mutex;
use tauri::Emitter;

static PENDING: Mutex<Option<String>> = Mutex::new(None);
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// Stash the app handle so a share arriving while the app is open can notify the
/// frontend immediately. Set once during setup (same point as the logger's).
pub fn set_app_handle(handle: tauri::AppHandle) {
    *APP_HANDLE.lock().unwrap() = Some(handle);
}

/// Record shared text (called from the JNI bridge) and, if the app is already
/// running, nudge the frontend. The text itself rides the `take` command, not
/// the event payload, so cold and warm starts share one consumption path.
pub fn push_shared_text(text: String) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    log::info!("share: received {} chars of shared text", text.len());
    *PENDING.lock().unwrap() = Some(text);
    if let Some(h) = APP_HANDLE.lock().unwrap().as_ref() {
        let _ = h.emit("shared-text", ());
    }
}

/// Take the pending shared text, clearing it (delivered at most once).
pub fn take() -> Option<String> {
    PENDING.lock().unwrap().take()
}

/// Fetch a URL's raw HTML for readability extraction. Runs in Rust (not the
/// webview) so it isn't blocked by CORS, and sends a browser User-Agent because
/// many sites reject non-browser clients. Bounded by a timeout and a response
/// size cap so a hostile or huge page can't hang or exhaust memory.
pub async fn fetch_html(url: &str) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("not an http(s) URL".into());
    }
    const MAX_BYTES: u64 = 10 * 1024 * 1024;
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Mobile Safari/537.36")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    let resp = client.get(url).send().await.map_err(|e| format!("fetch: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_BYTES {
            return Err(format!("page too large ({len} bytes)"));
        }
    }
    let bytes = resp.bytes().await.map_err(|e| format!("read body: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("page too large".into());
    }
    // Lossy decode: article HTML is overwhelmingly UTF-8; a few mojibake bytes
    // are harmless once readability strips to text.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
