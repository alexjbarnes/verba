//! Fallback fetch through a hidden Android WebView.
//!
//! Some sites (Cloudflare "Just a moment…" challenges) 403 every non-browser
//! HTTP client — the TLS fingerprint gives reqwest away, so no header tweak
//! helps. A real browser engine passes the JavaScript challenge, gets the
//! clearance cookie, and sees the content. The frontend falls back here when
//! `fetch_feed` / `fetch_article` come back 403/503.
//!
//! Flow: `fetch()` registers a oneshot under a request id and asks Kotlin
//! (`VerbaApp.webViewFetch`, via the cached-class JNI helper in lib.rs) to
//! load the URL in an offscreen WebView. Kotlin polls the page until the
//! challenge clears, re-fetches the URL from inside the page (same-origin,
//! carries the clearance cookie) to get the raw body, then calls back through
//! `nativeWebFetchDone`, which resolves the oneshot via `complete()`.

#[cfg(target_os = "android")]
use std::collections::BTreeMap;
#[cfg(target_os = "android")]
use std::sync::atomic::{AtomicI64, Ordering};
#[cfg(target_os = "android")]
use std::sync::Mutex;

#[cfg(target_os = "android")]
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

#[cfg(target_os = "android")]
static PENDING: Mutex<BTreeMap<i64, tokio::sync::oneshot::Sender<Result<String, String>>>> =
    Mutex::new(BTreeMap::new());

/// Resolve a pending fetch (called from the JNI callback in lib.rs).
#[cfg(target_os = "android")]
pub fn complete(id: i64, result: Result<String, String>) {
    if let Some(tx) = PENDING.lock().unwrap().remove(&id) {
        let _ = tx.send(result);
    }
}

pub async fn fetch(url: &str) -> Result<String, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("not an http(s) URL".into());
    }
    #[cfg(not(target_os = "android"))]
    {
        Err("browser fetch is only available on Android".into())
    }
    #[cfg(target_os = "android")]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        PENDING.lock().unwrap().insert(id, tx);
        crate::android_webview_fetch(url, id);
        // The Kotlin side gives up at ~40s; this is the belt-and-braces cap so
        // a lost callback can never wedge the command.
        match tokio::time::timeout(std::time::Duration::from_secs(45), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("browser fetch aborted".into()),
            Err(_) => {
                PENDING.lock().unwrap().remove(&id);
                Err("browser fetch timed out".into())
            }
        }
    }
}
