//! Logger that mirrors Rust-side logs into the webview's Debug tab.
//! Everything is written to stderr; INFO and above is also emitted as a
//! `log-message` event the frontend renders.

use std::sync::Mutex;

use log::{Level, Log, Metadata, Record};
use serde_json::json;
use tauri::Emitter;

static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

pub fn set_app_handle(handle: tauri::AppHandle) {
    *APP_HANDLE.lock().unwrap() = Some(handle);
}

pub struct DebugLogger;

impl Log for DebugLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // Keep the pipe quiet-ish: info and up reaches the Debug tab.
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        let line = format!("{}", record.args());
        eprintln!("{} [{}] {}", record.level(), record.target(), line);
        if !self.enabled(record.metadata()) {
            return;
        }
        if let Some(h) = APP_HANDLE.lock().unwrap().as_ref() {
            let _ = h.emit(
                "log-message",
                json!({ "level": record.level().to_string(), "line": line }),
            );
        }
    }

    fn flush(&self) {}
}
