mod audio;
mod config;
mod debug_log;
mod delivery;
mod engine;
mod feeds;
mod gb_english;
mod history;
mod library;
mod media;
mod mispronunciations;
mod models;
#[cfg(desktop)]
mod paste;
mod piper;
mod player;
pub mod postprocess;
#[cfg(desktop)]
mod sound;
mod recorder;
mod snippets;
mod share;
mod transcribe;
mod tts;
mod tts_cache;
mod webfetch;
pub mod vad;
#[cfg(target_os = "android")]
mod android_ime;

use tauri::{Emitter, Manager};

#[cfg(desktop)]
struct AppState {
    recording: std::sync::atomic::AtomicBool,
}

#[tauri::command]
fn is_engine_ready() -> bool {
    engine::is_initialized()
}

#[tauri::command]
fn list_models() -> Vec<models::ModelInfo> {
    models::ModelManager::global().list()
}

#[tauri::command]
fn list_audio_devices() -> Vec<audio::AudioDevice> {
    audio::list_input_devices()
}

#[tauri::command]
fn get_config() -> config::AppConfig {
    config::AppConfig::load()
}

#[tauri::command]
fn save_config(cfg: config::AppConfig) -> Result<(), String> {
    cfg.save()
}

#[tauri::command]
fn list_history() -> Vec<history::HistoryEntry> {
    history::History::global().list()
}

#[tauri::command]
fn clear_history() {
    history::History::global().clear()
}

#[tauri::command]
fn export_history() -> Result<String, String> {
    history::History::global().export()
}

#[tauri::command]
fn mispronunciations_list() -> Vec<mispronunciations::MispronunciationEntry> {
    mispronunciations::Mispronunciations::global().list()
}

#[tauri::command]
fn export_mispronunciations() -> Result<String, String> {
    mispronunciations::Mispronunciations::global().export()
}

#[tauri::command]
fn clear_mispronunciations() {
    mispronunciations::Mispronunciations::global().clear()
}

/// In-app report path (e.g. a future button); the Android text-selection flow
/// uses the JNI bridge below instead.
#[tauri::command]
fn report_mispronunciation(word: String) {
    mispronunciations::Mispronunciations::global().add(word);
}

/// Android ACTION_PROCESS_TEXT bridge: ProcessTextActivity forwards the word the
/// user selected + tapped "Report mispronunciation" on. Mirrors the VerbaApp TTS
/// JNI exports.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeReportMispronunciation(
    mut env: jni::JNIEnv, _class: jni::objects::JClass, text: jni::objects::JString,
) {
    match env.get_string(&text) {
        Ok(s) => mispronunciations::Mispronunciations::global().add(s.into()),
        Err(e) => log::error!("report: failed to read text: {e}"),
    }
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
        android_copy_to_clipboard(&text);
        Ok(())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = text;
        Err("clipboard not available on this platform".into())
    }
}

#[cfg(target_os = "android")]
static GLOBAL_JVM: std::sync::OnceLock<jni::JavaVM> = std::sync::OnceLock::new();

/// GlobalRef to VerbaApp class, cached at JNI_OnLoad time.
///
/// Android's class loader is thread-local. Background Rust threads that attach to
/// the JVM via attach_current_thread get the bootstrap class loader, which only
/// knows SDK classes — it cannot find app classes like VerbaApp. By resolving and
/// caching the class during JNI_OnLoad (called on the Java main thread, which has
/// the application class loader), we can safely use it from any thread afterwards.
#[cfg(target_os = "android")]
static VERBA_APP_CLASS: std::sync::OnceLock<jni::objects::GlobalRef> = std::sync::OnceLock::new();

/// Called once by the Android runtime when System.loadLibrary("verba_rs_lib") runs.
/// We are on the Java main thread here, so find_class works for app classes.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jni::sys::jint {
    let Ok(vm) = (unsafe { jni::JavaVM::from_raw(vm) }) else {
        return jni::sys::JNI_VERSION_1_6;
    };
    if let Ok(mut env) = vm.get_env() {
        if let Ok(class) = env.find_class("com/alexb151/verba/VerbaApp") {
            if let Ok(global) = env.new_global_ref(class) {
                let _ = VERBA_APP_CLASS.set(global);
            }
        }
    }
    let _ = GLOBAL_JVM.set(vm);
    jni::sys::JNI_VERSION_1_6
}

#[cfg(target_os = "android")]
fn android_show_toast(msg: &str) {
    use jni::objects::JValue;
    let vm = match GLOBAL_JVM.get() {
        Some(v) => v,
        None => return,
    };
    let class_ref = match VERBA_APP_CLASS.get() {
        Some(c) => c,
        None => return,
    };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(_) => return,
    };
    let Ok(msg_str) = env.new_string(msg) else { return };
    let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
    let _ = env.call_static_method(
        class, "showToast", "(Ljava/lang/String;)V",
        &[JValue::Object(&*msg_str)],
    );
}

#[cfg(target_os = "android")]
fn android_copy_to_clipboard(text: &str) {
    use jni::objects::JValue;
    let vm = match GLOBAL_JVM.get() {
        Some(v) => v,
        None => return,
    };
    let class_ref = match VERBA_APP_CLASS.get() {
        Some(c) => c,
        None => return,
    };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(_) => return,
    };
    let Ok(text_str) = env.new_string(text) else { return };
    let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
    let _ = env.call_static_method(
        class, "copyToClipboard", "(Ljava/lang/String;)V",
        &[JValue::Object(&*text_str)],
    );
}

#[cfg(target_os = "android")]
fn android_call_static(method: &str, sig: &str, args: &[jni::objects::JValue]) {
    let vm = match GLOBAL_JVM.get() {
        Some(v) => v,
        None => return,
    };
    let class_ref = match VERBA_APP_CLASS.get() {
        Some(c) => c,
        None => return,
    };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(_) => return,
    };
    let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
    let _ = env.call_static_method(class, method, sig, args);
}

#[cfg(target_os = "android")]
pub fn android_update_media_session(position_ms: i64, duration_ms: i64, paused: bool) {
    use jni::objects::JValue;
    android_call_static(
        "updateMediaSession",
        "(JJZ)V",
        &[JValue::Long(position_ms), JValue::Long(duration_ms), JValue::Bool(paused as u8)],
    );
}

#[cfg(target_os = "android")]
pub fn android_start_media_session() {
    android_call_static("startMediaSession", "()V", &[]);
}

#[cfg(target_os = "android")]
pub fn android_stop_media_session() {
    android_call_static("stopMediaSession", "()V", &[]);
}

/// Ask Kotlin to load a URL in a hidden WebView (challenge-passing fallback
/// fetch). The result comes back asynchronously via nativeWebFetchDone.
#[cfg(target_os = "android")]
pub fn android_webview_fetch(url: &str, request_id: i64) {
    use jni::objects::JValue;
    let vm = match GLOBAL_JVM.get() {
        Some(v) => v,
        None => return webfetch::complete(request_id, Err("JVM not ready".into())),
    };
    let class_ref = match VERBA_APP_CLASS.get() {
        Some(c) => c,
        None => return webfetch::complete(request_id, Err("app class not ready".into())),
    };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(e) => return webfetch::complete(request_id, Err(format!("JNI attach: {e}"))),
    };
    let Ok(url_str) = env.new_string(url) else {
        return webfetch::complete(request_id, Err("JNI string alloc failed".into()));
    };
    let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
    if env
        .call_static_method(
            class,
            "webViewFetch",
            "(Ljava/lang/String;J)V",
            &[JValue::Object(&*url_str), JValue::Long(request_id)],
        )
        .is_err()
    {
        webfetch::complete(request_id, Err("webViewFetch call failed".into()));
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeTtsPause(
    _env: jni::JNIEnv, _class: jni::objects::JClass,
) {
    tts::pause();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeTtsResume(
    _env: jni::JNIEnv, _class: jni::objects::JClass,
) {
    tts::resume();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeTtsStop(
    _env: jni::JNIEnv, _class: jni::objects::JClass,
) {
    tts::stop();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeTtsSeek(
    _env: jni::JNIEnv, _class: jni::objects::JClass, position_ms: jni::sys::jlong,
) {
    if position_ms >= 0 {
        tts::seek_ms(position_ms as u64);
    }
}

/// Android share-target bridge: MainActivity forwards an ACTION_SEND intent's
/// text here. Mirrors the VerbaApp TTS JNI exports above.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeSharedText(
    mut env: jni::JNIEnv, _class: jni::objects::JClass, text: jni::objects::JString,
) {
    match env.get_string(&text) {
        Ok(s) => share::push_shared_text(s.into()),
        Err(e) => log::error!("share: failed to read shared text: {e}"),
    }
}

/// Hidden-WebView fetch completion: Kotlin hands back the page body (or an
/// error) for the request id issued by webfetch::fetch.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeWebFetchDone(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    request_id: jni::sys::jlong,
    content: jni::objects::JString,
    error: jni::objects::JString,
) {
    let content: String = env.get_string(&content).map(Into::into).unwrap_or_default();
    let error: String = env.get_string(&error).map(Into::into).unwrap_or_default();
    let result = if error.is_empty() {
        Ok(content)
    } else {
        Err(error)
    };
    webfetch::complete(request_id, result);
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

#[tauri::command]
async fn switch_model(app: tauri::AppHandle, id: String) -> Result<(), String> {
    // Validate the model is downloaded before accepting the switch.
    // set_active is deferred until after the model successfully loads so a
    // failed load never corrupts the persisted active model.
    if models::ModelManager::global().model_engine(&id).is_none() {
        return Err("model not downloaded".into());
    }
    log::info!("Switching to model: {id}");
    #[cfg(not(target_os = "android"))]
    let _ = app.emit("model-loading", serde_json::json!({ "id": &id }));
    #[cfg(target_os = "android")]
    android_show_toast("Loading model...");

    tokio::spawn(async move {
        let id2 = id.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let mgr = models::ModelManager::global();
            let result = engine::with_mut(|eng| {
                if eng.model_id() == id2 {
                    return Ok(());
                }
                log::info!("Reloading model to {id2}");
                let model_engine = mgr.model_engine(&id2)
                    .ok_or_else(|| format!("model {id2} not downloaded"))?;
                let t = transcribe::Transcriber::new(model_engine)?;
                // Only persist active model after a successful load.
                mgr.set_active(&id2)?;
                eng.reload_model(t, id2.clone());
                Ok(())
            });
            result.unwrap_or_else(|| Err("Engine still loading, please wait".into()))
        })
        .await;

        match result {
            Ok(Ok(())) => {
                #[cfg(target_os = "android")]
                android_show_toast("Model ready");
                let _ = app.emit("model-loaded", serde_json::json!({
                    "id": &id,
                    "native_toast": cfg!(target_os = "android"),
                }));
            }
            Ok(Err(e)) => {
                log::error!("Model reload failed: {e}");
                #[cfg(target_os = "android")]
                android_show_toast(&format!("Model load failed: {e}"));
                let _ = app.emit("model-error", serde_json::json!({
                    "id": &id,
                    "error": e,
                    "native_toast": cfg!(target_os = "android"),
                }));
            }
            Err(e) => {
                log::error!("Model reload task panicked: {e}");
                let _ = app.emit("model-error", serde_json::json!({ "id": &id, "error": e.to_string() }));
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn download_model(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let mgr = models::ModelManager::global();
    if mgr.is_downloaded(&id) {
        return Ok(());
    }
    mgr.download(&id, &app).await
}

#[tauri::command]
fn delete_model(id: String) -> Result<(), String> {
    models::ModelManager::global().delete(&id)
}

#[tauri::command]
fn get_vocab_entries() -> Vec<postprocess::vocab::VocabEntry> {
    postprocess::vocab::get_entries()
}

#[tauri::command]
fn add_vocab_entry(from: String, to: String) -> Result<(), String> {
    postprocess::vocab::add_entry(from, to)
}

#[tauri::command]
fn remove_vocab_entry(from: String) -> Result<(), String> {
    postprocess::vocab::remove_entry(&from)
}

#[tauri::command]
fn list_snippets() -> Vec<snippets::Snippet> {
    snippets::SnippetManager::global().list()
}

#[tauri::command]
fn save_snippet(trigger: String, body: String) -> Result<snippets::Snippet, String> {
    if trigger.trim().is_empty() {
        return Err("trigger cannot be empty".into());
    }
    if body.trim().is_empty() {
        return Err("body cannot be empty".into());
    }
    Ok(snippets::SnippetManager::global().add(trigger, body))
}

#[tauri::command]
fn delete_snippet(id: String) -> Result<(), String> {
    snippets::SnippetManager::global().delete(&id)
}

#[tauri::command]
fn update_snippet(id: String, triggers: Vec<String>, body: String) -> Result<snippets::Snippet, String> {
    let triggers: Vec<String> = triggers.into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if triggers.is_empty() {
        return Err("at least one trigger is required".into());
    }
    if body.trim().is_empty() {
        return Err("body cannot be empty".into());
    }
    snippets::SnippetManager::global().update(&id, triggers, body)
}

#[tauri::command]
fn add_snippet_trigger(id: String, trigger: String) -> Result<(), String> {
    if trigger.trim().is_empty() {
        return Err("trigger cannot be empty".into());
    }
    snippets::SnippetManager::global().add_trigger(&id, trigger)
}

#[tauri::command]
fn tts_is_loaded() -> bool {
    tts::is_loaded()
}

#[tauri::command]
fn tts_is_speaking() -> bool {
    tts::is_speaking()
}

#[tauri::command]
async fn tts_load(id: String, custom_voice: Option<String>) -> Result<(), String> {
    let mgr = models::ModelManager::global();
    let mut config = mgr.tts_model_config(&id)
        .ok_or("TTS model not downloaded")?;
    tts::set_voice_base(match &custom_voice {
        Some(name) => format!("{id}+{name}"),
        None => id.clone(),
    });
    if let Some(name) = custom_voice {
        let path = tts::custom_voices_dir().join(format!("{name}.bin"));
        if !path.exists() {
            return Err(format!("Custom voice not found: {name}"));
        }
        config.override_voices(path.to_string_lossy().into_owned());
    }
    // Generation must outrun playback to avoid starving the player, but using
    // all cores backfires on big.LITTLE phones: extra threads land on slow
    // little cores AND sustained all-core load triggers thermal throttling
    // (measured RTF degraded 1.06x -> 2.48x over ~70s on an 8-core device at 8
    // threads). Respect the configured thread count (default 4, which targets
    // the performance cores); log cores + per-chunk RTF so the sweet spot can
    // be found by experiment in Settings.
    let threads = config::AppConfig::load().threads as i32;
    let cores = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(0);
    log::info!("TTS loading with {threads} threads (cores={cores})");
    tokio::task::spawn_blocking(move || tts::load(config, threads))
        .await
        .map_err(|e| format!("{e}"))?
}

#[tauri::command]
fn tts_unload() {
    tts::unload();
}

#[tauri::command]
fn tts_info() -> serde_json::Value {
    serde_json::json!({
        "loaded": tts::is_loaded(),
        "speaking": tts::is_speaking(),
        "num_speakers": tts::num_speakers(),
        "sample_rate": tts::sample_rate(),
    })
}

#[tauri::command]
async fn tts_speak(app: tauri::AppHandle, text: String, speed: Option<f32>, sid: Option<i32>, gen: Option<u64>) -> Result<(), String> {
    let speed = speed.unwrap_or(1.0);
    let sid = sid.unwrap_or(0);
    tts::speak(&text, speed, sid, gen.unwrap_or(0), Some(app))
}

/// Play `text` straight from the persistent cache if every segment it needs is
/// already on disk, without loading the ONNX engine. Returns `false` (not an
/// error) when any part is missing, so the frontend's normal fall back path
/// (`tts_load` if needed, then `tts_speak`) takes over unchanged. On a
/// successful cache-only start, also warms the engine in the background so a
/// later seek into uncached text (or voice/speed change) doesn't start cold.
#[tauri::command]
async fn tts_speak_cached(
    app: tauri::AppHandle,
    id: String,
    custom_voice: Option<String>,
    text: String,
    speed: Option<f32>,
    sid: Option<i32>,
    gen: Option<u64>,
) -> Result<bool, String> {
    let speed = speed.unwrap_or(1.0);
    let sid = sid.unwrap_or(0);
    let gen = gen.unwrap_or(0);

    let mgr = models::ModelManager::global();
    let Some(mut model_cfg) = mgr.tts_model_config(&id) else {
        return Ok(false);
    };
    tts::set_voice_base(match &custom_voice {
        Some(name) => format!("{id}+{name}"),
        None => id.clone(),
    });
    if let Some(name) = &custom_voice {
        let path = tts::custom_voices_dir().join(format!("{name}.bin"));
        if path.exists() {
            model_cfg.override_voices(path.to_string_lossy().into_owned());
        }
    }
    let tts::TtsModelConfig::PiperOrt { model, config } = model_cfg.clone();

    let played = tokio::task::spawn_blocking(move || {
        tts::speak_from_cache(&text, speed, sid, gen, Some(app), &model, &config)
    })
    .await
    .map_err(|e| format!("{e}"))??;

    if played {
        let threads = config::AppConfig::load().threads as i32;
        tts::load_in_background(model_cfg, threads);
    }
    Ok(played)
}

/// Play a short fixed phrase in one voice so the user can audition it on the
/// Voices page. Passes no app handle, so `tts::speak` emits no UI events and
/// starts no media session — it just plays through the shared player (stopping
/// any current playback, same as a normal speak).
#[tauri::command]
async fn tts_sample(sid: i32, speed: Option<f32>) -> Result<(), String> {
    let speed = speed.unwrap_or(1.0);
    tts::speak(
        "Hello, this is how I sound when I read to you.",
        speed,
        sid,
        0,
        None,
    )
}

#[tauri::command]
fn tts_stop() {
    tts::stop();
}

#[tauri::command]
fn tts_pause() {
    tts::pause();
}

#[tauri::command]
fn tts_resume() {
    tts::resume();
}

#[tauri::command]
fn tts_seek(position_ms: u64) {
    tts::seek_ms(position_ms);
}

#[tauri::command]
fn tts_list_custom_voices() -> Vec<String> {
    tts::list_custom_voices()
}

/// Which parts of `text` are already cached for this voice+speed, so the reader
/// can show buffered ranges (and the real duration when fully cached) on open,
/// before any playback. Computed from cache headers without loading the engine.
#[tauri::command]
fn tts_cache_status(id: String, sid: i32, speed: f32, text: String) -> serde_json::Value {
    let mgr = models::ModelManager::global();
    let Some(crate::tts::TtsModelConfig::PiperOrt { model, config }) = mgr.tts_model_config(&id)
    else {
        return serde_json::json!({ "supported": false });
    };
    let cov = piper::cache_coverage(&model, &config, sid, speed, &text);
    serde_json::json!({
        "supported": true,
        "ranges": cov.ranges,            // [[start_ms, end_ms], ...] merged blocks
        "total_ms": cov.total_ms,
        "cached_ms": cov.cached_ms,
        "total_segments": cov.total_segments,
        "cached_segments": cov.cached_segments,
        "all_cached": cov.total_segments > 0 && cov.cached_segments == cov.total_segments,
    })
}

/// Size of the persistent generated-audio cache, in megabytes (for Settings).
#[tauri::command]
fn tts_cache_size_mb() -> f64 {
    tts_cache::size_bytes() as f64 / (1024.0 * 1024.0)
}

/// Delete all cached generated audio.
#[tauri::command]
fn tts_cache_clear() -> Result<(), String> {
    tts_cache::clear()
}

/// Speaker count for a TTS model read from its on-disk config, without loading
/// the engine. Lets the Reader show the voice picker before first generation.
#[tauri::command]
fn tts_model_speakers(id: String) -> i32 {
    let mgr = models::ModelManager::global();
    match mgr.tts_model_config(&id) {
        Some(tts::TtsModelConfig::PiperOrt { config, .. }) => {
            piper::num_speakers_from_config(std::path::Path::new(&config))
        }
        None => 0,
    }
}

// ── Library (saved texts for the Listen reader) ──

#[tauri::command]
fn library_list() -> Vec<library::LibraryItem> {
    library::Library::global().list()
}

#[tauri::command]
fn library_add(
    title: Option<String>,
    body: String,
    url: Option<String>,
    feed_id: Option<String>,
    guid: Option<String>,
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
    ))
}

#[tauri::command]
fn library_get(id: String) -> Option<library::LibraryItem> {
    library::Library::global().get(&id)
}

#[tauri::command]
fn library_delete(id: String) {
    library::Library::global().delete(&id);
}

#[tauri::command]
fn library_set_progress(id: String, progress: u64) {
    library::Library::global().set_progress(&id, progress);
}

#[tauri::command]
fn library_set_duration(id: String, duration_ms: u64, speed: f32) {
    library::Library::global().set_duration(&id, duration_ms, speed);
}

// ── RSS feeds (Listen reader subscriptions) ──

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
async fn fetch_feed(
    url: String,
    etag: String,
    last_modified: String,
) -> Result<feeds::FetchFeedResult, String> {
    feeds::fetch_feed(&url, &etag, &last_modified).await
}

/// Start recording from the UI (no hotkey, no delivery).
/// The frontend calls this, then later calls `ui_stop_and_transcribe`
/// to get the text back.
#[tauri::command]
fn ui_start_recording() -> Result<(), String> {
    media::pause_media();
    engine::with_mut(|eng| eng.start_streaming())
        .unwrap_or_else(|| Err("Engine not ready".into()))
}

/// Stop a UI-initiated recording and return the transcribed text.
/// Runs post-processing but does NOT save to history or deliver text.
#[tauri::command]
async fn ui_stop_and_transcribe() -> Result<String, String> {
    let pending = engine::with(|eng| eng.stop_recording())
        .unwrap_or_else(|| Err("Engine not ready".into()))?;

    let result = tokio::task::spawn_blocking(move || {
        pending.finalize_without_history()
            .ok_or_else(|| "No speech detected".into())
    })
    .await
    .map_err(|e| format!("{e}"))?;
    media::resume_media();
    result
}

/// Stop a UI-initiated recording and return raw transcription text.
/// Skips post-processing. Used for snippet triggers where grammar
/// correction and capitalization are unwanted.
#[tauri::command]
async fn ui_stop_and_transcribe_raw() -> Result<String, String> {
    let pending = engine::with(|eng| eng.stop_recording())
        .unwrap_or_else(|| Err("Engine not ready".into()))?;

    let result = tokio::task::spawn_blocking(move || {
        pending.finalize_raw()
            .ok_or_else(|| "No speech detected".into())
    })
    .await
    .map_err(|e| format!("{e}"))?;
    media::resume_media();
    result
}

/// Initialize the Engine in the background: VAD, recorder, transcriber,
/// then preload model + post-processing pipeline.
/// Shared by both Tauri app and Android IME -- whichever starts first
/// creates the engine; the other is a no-op.
fn init_engine(app: tauri::AppHandle) {
    std::thread::Builder::new()
        .name("engine-init".into())
        .spawn(move || {
            if engine::is_initialized() {
                log::info!("Engine init: already initialized, skipping");
                let _ = app.emit("engine-ready", ());
                return;
            }
            if !engine::try_claim_init() {
                log::info!("Engine init: another thread is building the engine, waiting");
                engine::wait_until_ready();
                let _ = app.emit("engine-ready", ());
                return;
            }

            let mgr = models::ModelManager::global();

            let vad = match mgr.ensure_vad_model() {
                Ok(p) => {
                    log::info!("Engine init: VAD model at {}", p.display());
                    Some(p)
                }
                Err(e) => {
                    log::warn!("Engine init: VAD setup failed: {e}");
                    None
                }
            };

            let recorder = match recorder::AudioRecorder::new(vad.as_deref()) {
                Ok(r) => r,
                Err(e) => {
                    log::error!("Engine init: failed to create recorder: {e}");
                    return;
                }
            };

            // Try loading the active/preferred model, falling back through the
            // preferred list if one fails. Clears a broken active model so the
            // next startup doesn't loop on it.
            let transcriber_and_id = loop {
                let (model_id, model_engine) = match mgr.first_downloaded_model() {
                    Some(pair) => pair,
                    None => {
                        log::warn!("Engine init: no model downloaded yet");
                        let _ = app.emit("dictation-error", "No transcription model downloaded");
                        return;
                    }
                };

                log::info!("Engine init: loading model {model_id}");
                match transcribe::Transcriber::new(model_engine) {
                    Ok(t) => break (t, model_id),
                    Err(e) => {
                        log::error!("Engine init: failed to load {model_id}: {e}");
                        mgr.clear_active();
                        // first_downloaded_model will now skip this model and
                        // return the next in the preferred list, or None if
                        // there are no more candidates.
                    }
                }
            };
            let (transcriber, model_id) = transcriber_and_id;

            let eng = engine::Engine::new(recorder, transcriber, model_id.clone());
            eng.preload();
            engine::init_global(eng);

            log::info!("Engine init: ready (model: {model_id})");
            let _ = app.emit("engine-ready", ());
        })
        .ok();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = log::set_logger(&debug_log::LOGGER);
    log::set_max_level(log::LevelFilter::Debug);

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init());

    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());
    }

    builder.setup(|app| {
            #[cfg(target_os = "android")]
            {
                if let Ok(data_dir) = app.path().app_data_dir() {
                    std::env::set_var("VERBA_DATA_DIR", &data_dir);
                    let _ = std::fs::create_dir_all(&data_dir);
                }
            }

            if let Err(e) = models::ModelManager::init_global() {
                log::error!("Failed to create model manager: {e}");
                return Ok(());
            }
            history::History::init_global();
            library::Library::init_global();
            feeds::Feeds::init_global();
            snippets::SnippetManager::init_global();
            // Load neural grammar models on a background thread so the UI
            // stays responsive during startup.
            std::thread::Builder::new()
                .name("grammar-init".into())
                .spawn(|| postprocess::grammar_neural::init_global())
                .ok();

            debug_log::set_app_handle(app.handle().clone());
            share::set_app_handle(app.handle().clone());

            #[cfg(desktop)]
            {
                app.manage(AppState {
                    recording: std::sync::atomic::AtomicBool::new(false),
                });
                use std::sync::atomic::Ordering;
                use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
                use tauri::tray::TrayIconBuilder;
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

                // Alt+D: press to start recording, release to stop and paste
                let shortcut = Shortcut::new(Some(Modifiers::ALT), Code::KeyD);
                let app_handle = app.handle().clone();
                let captured_target: std::sync::Mutex<Option<paste::PasteTarget>> = std::sync::Mutex::new(None);
                app.global_shortcut().on_shortcut(shortcut, move |_app, _shortcut, event| {
                    let state = app_handle.state::<AppState>();
                    match event.state {
                        ShortcutState::Pressed => {
                            if state.recording.load(Ordering::SeqCst) {
                                return;
                            }
                            *captured_target.lock().unwrap() = paste::capture_frontmost_app();
                            let started = engine::with_mut(|eng| eng.start_streaming());
                            match started {
                                Some(Ok(())) => {
                                    state.recording.store(true, Ordering::SeqCst);
                                    media::pause_media();
                                    sound::play_start();
                                    let _ = app_handle.emit("dictation-state", "recording");
                                    log::info!("Shortcut: recording started");
                                }
                                Some(Err(e)) => {
                                    log::error!("Shortcut: failed to start: {e}");
                                    let _ = app_handle.emit("dictation-error", e.as_str());
                                }
                                None => {
                                    log::warn!("Shortcut: engine not ready");
                                    let _ = app_handle.emit("dictation-error", "Engine not ready. Wait for model to load.");
                                }
                            }
                        }
                        ShortcutState::Released => {
                            if !state.recording.swap(false, Ordering::SeqCst) {
                                return;
                            }
                            sound::play_stop();
                            let _ = app_handle.emit("dictation-state", "processing");
                            log::info!("Shortcut: stopping recording");

                            let target = captured_target.lock().unwrap().take();
                            let deliver = delivery::DesktopDelivery { target };

                            let pending = match engine::with(|eng| eng.stop_recording()) {
                                Some(Ok(p)) => p,
                                Some(Err(e)) => {
                                    log::error!("Shortcut: stop failed: {e}");
                                    media::resume_media();
                                    let _ = app_handle.emit("dictation-state", "idle");
                                    return;
                                }
                                None => return,
                            };

                            let app_for_paste = app_handle.clone();
                            std::thread::Builder::new()
                                .name("transcribe".into())
                                .spawn(move || {
                                    match pending.finalize() {
                                        Some(result) => {
                                            log::info!("Shortcut: transcribed: \"{}\"",
                                                if result.text.len() > 60 { &result.text[..60] } else { &result.text });
                                            let _ = app_for_paste.emit("transcription-result",
                                                serde_json::json!({
                                                    "text": &result.text,
                                                    "model_id": &result.model_id,
                                                    "audio_duration_ms": result.audio_duration_ms,
                                                    "transcribe_ms": result.transcribe_ms,
                                                }));
                                            use delivery::TextDelivery;
                                            match deliver.deliver(&result.text) {
                                                Ok(delivery::DeliveryResult::Inserted) => {}
                                                Ok(delivery::DeliveryResult::ClipboardOnly) => {
                                                    sound::play_error();
                                                    let _ = app_for_paste.emit("paste-fallback",
                                                        "Text copied to clipboard — paste manually");
                                                }
                                                Err(e) => {
                                                    log::error!("Shortcut: delivery failed: {e}");
                                                    sound::play_error();
                                                }
                                            }
                                        }
                                        None => {
                                            log::warn!("Shortcut: no text produced");
                                        }
                                    }
                                    media::resume_media();
                                    let _ = app_for_paste.emit("dictation-state", "idle");
                                })
                                .ok();
                        }
                    }
                })?;

                // Alt+S: press to record a snippet trigger, release to look up
                // and paste the matching snippet body. If no snippet matches,
                // emits `snippet-no-match` so the frontend can show a picker.
                let snippet_shortcut = Shortcut::new(Some(Modifiers::ALT), Code::KeyS);
                let app_handle_s = app.handle().clone();
                let captured_target_s: std::sync::Mutex<Option<paste::PasteTarget>> = std::sync::Mutex::new(None);
                app.global_shortcut().on_shortcut(snippet_shortcut, move |_app, _shortcut, event| {
                    let state = app_handle_s.state::<AppState>();
                    match event.state {
                        ShortcutState::Pressed => {
                            if state.recording.load(Ordering::SeqCst) {
                                return;
                            }
                            *captured_target_s.lock().unwrap() = paste::capture_frontmost_app();
                            let started = engine::with_mut(|eng| eng.start_streaming());
                            match started {
                                Some(Ok(())) => {
                                    state.recording.store(true, Ordering::SeqCst);
                                    media::pause_media();
                                    sound::play_start();
                                    let _ = app_handle_s.emit("dictation-state", "snippet-recording");
                                    log::info!("Snippet shortcut: recording started");
                                }
                                Some(Err(e)) => {
                                    log::error!("Snippet shortcut: failed to start: {e}");
                                    let _ = app_handle_s.emit("dictation-error", e.as_str());
                                }
                                None => {
                                    log::warn!("Snippet shortcut: engine not ready");
                                    let _ = app_handle_s.emit("dictation-error", "Engine not ready. Wait for model to load.");
                                }
                            }
                        }
                        ShortcutState::Released => {
                            if !state.recording.swap(false, Ordering::SeqCst) {
                                return;
                            }
                            sound::play_stop();
                            let _ = app_handle_s.emit("dictation-state", "processing");
                            log::info!("Snippet shortcut: stopping recording");

                            let target = captured_target_s.lock().unwrap().take();
                            let deliver = delivery::DesktopDelivery { target };

                            let pending = match engine::with(|eng| eng.stop_recording()) {
                                Some(Ok(p)) => p,
                                Some(Err(e)) => {
                                    log::error!("Snippet shortcut: stop failed: {e}");
                                    media::resume_media();
                                    let _ = app_handle_s.emit("dictation-state", "idle");
                                    return;
                                }
                                None => return,
                            };

                            let app_for_snippet = app_handle_s.clone();
                            std::thread::Builder::new()
                                .name("snippet-transcribe".into())
                                .spawn(move || {
                                    match pending.finalize_raw() {
                                        Some(trigger_text) => {
                                            log::info!("Snippet shortcut: trigger text: \"{}\"", &trigger_text);
                                            let mgr = snippets::SnippetManager::global();
                                            if let Some(snippet) = mgr.find_match(&trigger_text) {
                                                log::info!("Snippet matched: {}", &snippet.id);
                                                use delivery::TextDelivery;
                                                match deliver.deliver(&snippet.body) {
                                                    Ok(_) => {}
                                                    Err(e) => {
                                                        log::error!("Snippet delivery failed: {e}");
                                                        sound::play_error();
                                                    }
                                                }
                                                let _ = app_for_snippet.emit("snippet-matched",
                                                    serde_json::json!({
                                                        "id": &snippet.id,
                                                        "body": &snippet.body,
                                                        "trigger_text": &trigger_text,
                                                    }));
                                            } else {
                                                log::info!("Snippet shortcut: no match for \"{}\"", &trigger_text);
                                                let _ = app_for_snippet.emit("snippet-no-match",
                                                    serde_json::json!({
                                                        "text": &trigger_text,
                                                        "snippets": &mgr.list(),
                                                    }));
                                            }
                                        }
                                        None => {
                                            log::warn!("Snippet shortcut: no text produced");
                                        }
                                    }
                                    media::resume_media();
                                    let _ = app_for_snippet.emit("dictation-state", "idle");
                                })
                                .ok();
                        }
                    }
                })?;

                // System tray
                let settings =
                    MenuItem::with_id(app, "settings", "Settings...", true, None::<&str>)?;
                let sep = PredefinedMenuItem::separator(app)?;
                let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

                let menu = Menu::with_items(app, &[&settings, &sep, &quit])?;

                let icon = app.default_window_icon().cloned().expect("no app icon");
                TrayIconBuilder::new()
                    .icon(icon)
                    .menu(&menu)
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "settings" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .build(app)?;

                // Close button hides the window instead of quitting — app keeps
                // running in the menu bar. Reopen via tray "Settings..." item.
                if let Some(window) = app.get_webview_window("main") {
                    let w = window.clone();
                    window.on_window_event(move |event| {
                        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                            api.prevent_close();
                            let _ = w.hide();
                        }
                    });
                }
            }

            // Initialize engine in background (shared path for all platforms)
            init_engine(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            is_engine_ready,
            list_models,
            list_audio_devices,
            get_config,
            save_config,
            download_model,
            delete_model,
            switch_model,
            list_history,
            clear_history,
            export_history,
            mispronunciations_list,
            export_mispronunciations,
            clear_mispronunciations,
            report_mispronunciation,
            copy_to_clipboard,
            get_vocab_entries,
            add_vocab_entry,
            remove_vocab_entry,
            list_snippets,
            save_snippet,
            update_snippet,
            delete_snippet,
            add_snippet_trigger,
            ui_start_recording,
            ui_stop_and_transcribe,
            ui_stop_and_transcribe_raw,
            tts_is_loaded,
            tts_is_speaking,
            tts_info,
            tts_load,
            tts_unload,
            tts_speak,
            tts_speak_cached,
            tts_sample,
            tts_stop,
            tts_pause,
            tts_resume,
            tts_seek,
            tts_list_custom_voices,
            tts_model_speakers,
            tts_cache_status,
            tts_cache_size_mb,
            tts_cache_clear,
            library_list,
            library_add,
            take_shared_text,
            fetch_article,
            library_get,
            library_delete,
            library_set_progress,
            library_set_duration,
            feeds_list,
            feed_add,
            feed_delete,
            feed_set_auto_add,
            feed_mark_seen,
            feed_checked,
            fetch_feed,
            webview_fetch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
