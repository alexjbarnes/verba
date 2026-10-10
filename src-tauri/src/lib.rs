mod config;
mod debug_log;
mod feeds;
mod library;
mod share;
mod webfetch;
#[cfg(target_os = "android")]
mod android_bridge;

#[tauri::command]
fn get_config() -> config::AppConfig {
    config::AppConfig::load()
}

#[tauri::command]
fn save_config(cfg: config::AppConfig) -> Result<(), String> {
    cfg.save()
}

#[cfg(desktop)]
#[tauri::command]
fn copy_to_clipboard(text: String) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut cb| cb.set_text(text))
        .map_err(|e| e.to_string())
}

#[cfg(mobile)]
#[tauri::command]
fn copy_to_clipboard(text: String) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        crate::android_bridge::copy_to_clipboard(&text);
        Ok(())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = text;
        Err("clipboard not available on this platform".into())
    }
}

/// Consume text shared to the app (URL or prose), if any. Returns null when
/// nothing is pending. Called by the frontend on startup and on `shared-text`.
#[tauri::command]
fn take_shared_text() -> Option<String> {
    share::take()
}

/// Fetch a URL's HTML so the frontend can run readability on it. Kept in Rust to
/// bypass the webview's CORS restriction on cross-origin reads.
#[tauri::command]
async fn fetch_article(url: String) -> Result<String, String> {
    share::fetch_html(&url).await
}

/// Fallback fetch through a hidden WebView for sites whose HTTP endpoints sit
/// behind a browser-verification challenge (Cloudflare 403s every non-browser
/// TLS fingerprint). Android only; slow (the challenge takes seconds).
#[tauri::command]
async fn webview_fetch(url: String) -> Result<String, String> {
    webfetch::fetch(&url).await
}

// ── Library (saved texts for the reader) ──

#[tauri::command]
fn library_list() -> Vec<library::LibraryItem> {
    library::Library::global().list()
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn library_add(
    title: Option<String>,
    body: String,
    url: Option<String>,
    feed_id: Option<String>,
    guid: Option<String>,
    published: Option<String>,
    image_url: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<library::LibraryItem, String> {
    let body = body.trim().to_string();
    if body.is_empty() {
        return Err("Empty text".into());
    }
    Ok(library::Library::global().add(
        title.unwrap_or_default(),
        body,
        url.unwrap_or_default(),
        feed_id.unwrap_or_default(),
        guid.unwrap_or_default(),
        published.unwrap_or_default(),
        image_url.unwrap_or_default(),
        tags.unwrap_or_default(),
    ))
}

/// Replace a library item's tags.
#[tauri::command]
fn library_set_tags(id: String, tags: Vec<String>) {
    library::Library::global().set_tags(&id, tags);
}

#[tauri::command]
fn library_get(id: String) -> Option<library::LibraryItem> {
    library::Library::global().get(&id)
}

#[tauri::command]
fn library_delete(id: String) {
    library::Library::global().delete(&id);
}

/// Archive or unarchive a library item.
#[tauri::command]
fn library_set_archived(id: String, archived: bool) {
    library::Library::global().set_archived(&id, archived);
}

#[tauri::command]
fn library_set_progress(id: String, progress: u64) {
    library::Library::global().set_progress(&id, progress);
}

/// Mark an item opened at least once, clearing the Library list's NEW badge.
#[tauri::command]
fn library_mark_seen(id: String) {
    library::Library::global().mark_seen(&id);
}

/// Share an article's source URL via Android's system share sheet, or copy it
/// to the clipboard on other platforms. Errors when the item has no URL
/// (pasted text).
#[tauri::command]
fn library_share_url(id: String) -> Result<(), String> {
    let item = library::Library::global().get(&id).ok_or("Item not found")?;
    if item.url.is_empty() {
        return Err("This item has no link to share".into());
    }
    #[cfg(target_os = "android")]
    crate::android_bridge::share_text(&item.url);
    #[cfg(not(target_os = "android"))]
    copy_to_clipboard(item.url.clone())?;
    Ok(())
}

#[tauri::command]
fn feeds_list() -> Vec<feeds::Feed> {
    feeds::Feeds::global().list()
}

#[tauri::command]
fn feed_add(url: String, title: String, seen: Vec<String>) -> Result<feeds::Feed, String> {
    feeds::Feeds::global().add(url, title, seen)
}

#[tauri::command]
fn feed_delete(id: String) {
    feeds::Feeds::global().delete(&id);
}

#[tauri::command]
fn feed_set_auto_add(id: String, auto_add: bool) {
    feeds::Feeds::global().set_auto_add(&id, auto_add);
}

#[tauri::command]
fn feed_mark_seen(id: String, keys: Vec<String>) {
    feeds::Feeds::global().mark_seen(&id, keys);
}

#[tauri::command]
fn feed_checked(id: String, etag: String, last_modified: String) {
    feeds::Feeds::global().checked(&id, etag, last_modified);
}

#[tauri::command]
fn feed_update(id: String, title: String, url: String) -> Result<(), String> {
    feeds::Feeds::global().update(&id, title, url)
}

#[tauri::command]
async fn fetch_feed(
    url: String,
    etag: String,
    last_modified: String,
) -> Result<feeds::FetchFeedResult, String> {
    feeds::fetch_feed(&url, &etag, &last_modified).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = log::set_logger(&debug_log::DebugLogger);
    log::set_max_level(log::LevelFilter::Info);

    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());

    builder
        .setup(|app| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                if let Ok(data_dir) = app.path().app_data_dir() {
                    std::env::set_var("VERBA_DATA_DIR", &data_dir);
                    let _ = std::fs::create_dir_all(&data_dir);
                }
            }

            library::Library::init_global();
            feeds::Feeds::init_global();

            share::set_app_handle(app.handle().clone());
            debug_log::set_app_handle(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            copy_to_clipboard,
            take_shared_text,
            fetch_article,
            webview_fetch,
            library_list,
            library_add,
            library_set_tags,
            library_get,
            library_delete,
            library_set_archived,
            library_set_progress,
            library_mark_seen,
            library_share_url,
            feeds_list,
            feed_add,
            feed_delete,
            feed_set_auto_add,
            feed_mark_seen,
            feed_checked,
            feed_update,
            fetch_feed,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
