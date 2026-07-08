use std::time::Duration;

use arboard::{Clipboard, ImageData};

/// Result of a paste attempt.
pub enum PasteResult {
    /// Text was inserted into the focused text field.
    Pasted,
    /// Paste failed — text is left on the clipboard for manual Cmd+V.
    ClipboardOnly,
}

/// Target application captured at hotkey press time.
pub struct PasteTarget {
    /// PID from Accessibility API (used for direct AX text insertion).
    pid: Option<i32>,
    /// Bundle ID from osascript (used for clipboard+keystroke fallback).
    bundle_id: Option<String>,
}

/// Capture the frontmost app at hotkey press time.
///
/// Grabs both the PID (for AX insertion) and bundle ID (for fallback).
#[cfg(target_os = "macos")]
pub fn capture_frontmost_app() -> Option<PasteTarget> {
    let pid = ax::get_focused_pid();
    let bundle_id = osascript_bundle_id();

    if pid.is_none() && bundle_id.is_none() {
        log::warn!("Could not capture frontmost app");
        return None;
    }

    log::info!(
        "Captured target: pid={}, bundle={}",
        pid.map(|p| p.to_string()).unwrap_or("?".into()),
        bundle_id.as_deref().unwrap_or("?"),
    );
    Some(PasteTarget { pid, bundle_id })
}

#[cfg(not(target_os = "macos"))]
pub fn capture_frontmost_app() -> Option<PasteTarget> {
    None
}

/// Paste text into the previously focused application.
///
/// Strategy 1: macOS Accessibility API — write directly into the focused
///   text field via AXSelectedText (no clipboard involved, instant).
/// Strategy 2: Clipboard + activate app + Cmd+V via osascript.
/// Fallback: Leave text on clipboard, return ClipboardOnly.
pub fn paste(text: &str, target: Option<&PasteTarget>) -> Result<PasteResult, String> {
    // Read surrounding text context and adjust capitalization/punctuation
    #[cfg(target_os = "macos")]
    let adjusted: Option<String> = target
        .and_then(|t| t.pid)
        .and_then(|pid| ax::get_insertion_context(pid))
        .map(|(before, after)| adjust_for_context(text, &before, &after));
    #[cfg(target_os = "macos")]
    let text = adjusted.as_deref().unwrap_or(text);

    // Strategy 1: Direct AX text insertion (no clipboard, no keystroke)
    #[cfg(target_os = "macos")]
    if let Some(t) = target {
        if let Some(pid) = t.pid {
            match ax::insert_text(text, pid) {
                Ok(true) => {
                    log::info!("Pasted via Accessibility API: {text}");
                    return Ok(PasteResult::Pasted);
                }
                Ok(false) => {
                    log::info!("AX insert unverified, falling back to clipboard");
                }
                Err(e) => {
                    log::info!("AX insert failed ({e}), falling back to clipboard");
                }
            }
        }
    }

    // Strategy 2: Clipboard + keystroke
    clipboard_paste(text, target)
}

/// Saved clipboard content — text or image.
enum SavedClipboard {
    Text(String),
    Image(ImageData<'static>),
}

/// Snapshot whatever is on the clipboard so we can restore it after pasting.
///
/// Check for images first: macOS screenshots on the clipboard often have
/// a text representation too (filename or empty string), so checking text
/// first would discard the image.
fn save_clipboard(clipboard: &mut Clipboard) -> Option<SavedClipboard> {
    if let Ok(image) = clipboard.get_image() {
        if image.width > 0 && image.height > 0 {
            log::info!(
                "Saved clipboard image: {}x{} ({} bytes)",
                image.width,
                image.height,
                image.bytes.len()
            );
            return Some(SavedClipboard::Image(image));
        }
    }
    if let Ok(text) = clipboard.get_text() {
        if !text.is_empty() {
            return Some(SavedClipboard::Text(text));
        }
    }
    None
}

/// Restore previously saved clipboard content.
fn restore_clipboard(clipboard: &mut Clipboard, saved: SavedClipboard) {
    match saved {
        SavedClipboard::Text(text) => {
            let _ = clipboard.set_text(text);
        }
        SavedClipboard::Image(image) => {
            let _ = clipboard.set_image(image);
        }
    }
}

/// Clipboard-based paste: set clipboard, activate target app, send Cmd+V.
fn clipboard_paste(text: &str, target: Option<&PasteTarget>) -> Result<PasteResult, String> {
    let mut clipboard = Clipboard::new().map_err(|e| format!("clipboard: {e}"))?;
    let saved = save_clipboard(&mut clipboard);

    clipboard
        .set_text(text)
        .map_err(|e| format!("set clipboard: {e}"))?;

    std::thread::sleep(Duration::from_millis(50));

    match simulate_paste_keystroke(target) {
        Ok(()) => {
            // Give the target app time to read the clipboard before restoring.
            // osascript activate + keystroke needs more than 100ms on some apps.
            std::thread::sleep(Duration::from_millis(250));
            if let Some(prev) = saved {
                restore_clipboard(&mut clipboard, prev);
            }
            log::info!("Pasted via clipboard: {text}");
            Ok(PasteResult::Pasted)
        }
        Err(e) => {
            // Leave text on clipboard for manual paste
            log::warn!("Paste keystroke failed ({e}), text left on clipboard");
            Ok(PasteResult::ClipboardOnly)
        }
    }
}

// ---------------------------------------------------------------------------
// macOS: osascript helpers (for fallback path)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn osascript_bundle_id() -> Option<String> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "System Events" to get bundle identifier of first application process whose frontmost is true"#)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(target_os = "macos")]
fn simulate_paste_keystroke(target: Option<&PasteTarget>) -> Result<(), String> {
    let bundle_id = target
        .and_then(|t| t.bundle_id.as_deref())
        .ok_or("no target app for keystroke")?;
    let script = format!(
        r#"tell application id "{bundle_id}" to activate
delay 0.05
tell application "System Events" to keystroke "v" using command down"#
    );
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("osascript: {stderr}"));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn simulate_paste_keystroke(_target: Option<&PasteTarget>) -> Result<(), String> {
    use enigo::{Enigo, Key, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| format!("enigo: {e}"))?;
    enigo.key(Key::Control, enigo::Direction::Press).map_err(|e| format!("{e}"))?;
    enigo.key(Key::Unicode('v'), enigo::Direction::Click).map_err(|e| format!("{e}"))?;
    enigo.key(Key::Control, enigo::Direction::Release).map_err(|e| format!("{e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// macOS Accessibility API — direct text insertion into focused text fields
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod ax {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;

    // Raw CF/AX types — these are all opaque pointers in practice.
    type CFTypeRef = *const std::ffi::c_void;
    type AXError = i32;

    const AX_ERROR_SUCCESS: AXError = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> CFTypeRef;
        fn AXUIElementCreateApplication(pid: libc::pid_t) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(
            element: CFTypeRef,
            attribute: CFTypeRef, // CFStringRef
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementSetAttributeValue(
            element: CFTypeRef,
            attribute: CFTypeRef, // CFStringRef
            value: CFTypeRef,
        ) -> AXError;
        fn AXUIElementIsAttributeSettable(
            element: CFTypeRef,
            attribute: CFTypeRef, // CFStringRef
            settable: *mut u8,   // Boolean (unsigned char)
        ) -> AXError;
        fn AXUIElementGetPid(element: CFTypeRef, pid: *mut libc::pid_t) -> AXError;
        fn AXIsProcessTrusted() -> u8;
        fn AXValueGetValue(value: CFTypeRef, value_type: u32, value_ptr: *mut std::ffi::c_void) -> u8;
        fn CFRelease(cf: CFTypeRef);
    }

    const AX_VALUE_TYPE_CF_RANGE: u32 = 4; // kAXValueTypeCFRange

    #[repr(C)]
    struct CFRange {
        location: isize, // CFIndex
        length: isize,   // CFIndex
    }

    /// RAII wrapper for CF types to prevent leaks.
    struct CfRef(CFTypeRef);

    impl CfRef {
        fn new(ptr: CFTypeRef) -> Option<Self> {
            if ptr.is_null() {
                None
            } else {
                Some(Self(ptr))
            }
        }
        fn ptr(&self) -> CFTypeRef {
            self.0
        }
    }

    impl Drop for CfRef {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) }
        }
    }

    /// Helper to convert a Rust &str to a CFTypeRef for use as an AX attribute name.
    fn cf_str(s: &str) -> CFString {
        CFString::new(s)
    }

    /// Helper to pass a CFString as a raw CFTypeRef.
    fn cf_ptr(s: &CFString) -> CFTypeRef {
        s.as_concrete_TypeRef() as CFTypeRef
    }

    /// Get the PID of the app that owns the currently focused UI element.
    pub fn get_focused_pid() -> Option<i32> {
        unsafe {
            let system = CfRef::new(AXUIElementCreateSystemWide())?;
            let attr = cf_str("AXFocusedUIElement");
            let mut value: CFTypeRef = std::ptr::null();
            let err =
                AXUIElementCopyAttributeValue(system.ptr(), cf_ptr(&attr), &mut value);
            if err != AX_ERROR_SUCCESS {
                return None;
            }
            let element = CfRef::new(value)?;
            let mut pid: libc::pid_t = 0;
            let err = AXUIElementGetPid(element.ptr(), &mut pid);
            if err != AX_ERROR_SUCCESS {
                return None;
            }
            Some(pid)
        }
    }

    /// Read the AXValue (full text content) of an element, if available.
    unsafe fn read_value(element: CFTypeRef) -> Option<String> {
        let attr = cf_str("AXValue");
        let mut value: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(element, cf_ptr(&attr), &mut value);
        if err != AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let cf_ref = CfRef::new(value)?;
        // Try to interpret as CFString
        let cf_string: CFString = CFString::wrap_under_get_rule(cf_ref.ptr() as *const _);
        Some(cf_string.to_string())
    }

    /// Insert text directly into the focused text field of the given app.
    ///
    /// Uses AXSelectedText which replaces the current selection (or inserts
    /// at the cursor if nothing is selected) — exactly right for dictation.
    ///
    /// Returns Ok(true) if write was verified, Ok(false) if AX reported
    /// success but verification was inconclusive (some apps update the AX
    /// value asynchronously). Only returns Err for genuine AX failures
    /// (permission denied, no focused element, write rejected).
    pub fn insert_text(text: &str, pid: i32) -> Result<bool, String> {
        unsafe {
            if AXIsProcessTrusted() == 0 {
                return Err("accessibility permission not granted".into());
            }

            // Get the focused element within the target app
            let app = CfRef::new(AXUIElementCreateApplication(pid))
                .ok_or("failed to create AX app element")?;

            let focused_attr = cf_str("AXFocusedUIElement");
            let mut value: CFTypeRef = std::ptr::null();
            let err =
                AXUIElementCopyAttributeValue(app.ptr(), cf_ptr(&focused_attr), &mut value);
            if err != AX_ERROR_SUCCESS {
                return Err(format!("no focused element in app pid {pid} (AX error {err})"));
            }
            let element =
                CfRef::new(value).ok_or("focused element is null")?;

            // Check if the element supports AXSelectedText writes
            let selected_text = cf_str("AXSelectedText");
            let mut settable: u8 = 0;
            let err = AXUIElementIsAttributeSettable(
                element.ptr(),
                cf_ptr(&selected_text),
                &mut settable,
            );

            if err != AX_ERROR_SUCCESS || settable == 0 {
                return Err("AXSelectedText not settable on focused element".into());
            }

            // Snapshot the field value before writing
            let value_before = read_value(element.ptr());
            let len_before = value_before.as_ref().map(|s| s.len()).unwrap_or(0);

            // Write text via AXSelectedText
            let cf_text = cf_str(text);
            let err = AXUIElementSetAttributeValue(
                element.ptr(),
                cf_ptr(&selected_text),
                cf_ptr(&cf_text),
            );
            if err != AX_ERROR_SUCCESS {
                return Err(format!("AXSelectedText write failed (AX error {err})"));
            }

            std::thread::sleep(std::time::Duration::from_millis(20));
            let len_after = read_value(element.ptr())
                .as_ref().map(|s| s.len()).unwrap_or(0);

            if len_after > len_before || text.is_empty() {
                return Ok(true);
            }

            // Native apps update within ~50ms. Electron never will.
            std::thread::sleep(std::time::Duration::from_millis(30));
            let len_retry = read_value(element.ptr())
                .as_ref().map(|s| s.len()).unwrap_or(0);

            if len_retry > len_before {
                return Ok(true);
            }

            Err("AX write silently discarded (Electron/Chromium app)".into())
        }
    }

    /// Read the cursor position (as character index) in the focused element.
    unsafe fn read_selected_range(element: CFTypeRef) -> Option<(usize, usize)> {
        let attr = cf_str("AXSelectedTextRange");
        let mut value: CFTypeRef = std::ptr::null();
        let err = AXUIElementCopyAttributeValue(element, cf_ptr(&attr), &mut value);
        if err != AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let cf_ref = CfRef::new(value)?;
        let mut range = CFRange { location: 0, length: 0 };
        if AXValueGetValue(
            cf_ref.ptr(),
            AX_VALUE_TYPE_CF_RANGE,
            &mut range as *mut CFRange as *mut std::ffi::c_void,
        ) == 0
        {
            return None;
        }
        Some((range.location as usize, range.length as usize))
    }

    /// Get the text surrounding the cursor in the focused text field.
    /// Returns (text_before_cursor, text_after_cursor).
    pub fn get_insertion_context(pid: i32) -> Option<(String, String)> {
        unsafe {
            if AXIsProcessTrusted() == 0 {
                return None;
            }
            let app = CfRef::new(AXUIElementCreateApplication(pid))?;
            let focused_attr = cf_str("AXFocusedUIElement");
            let mut value: CFTypeRef = std::ptr::null();
            let err =
                AXUIElementCopyAttributeValue(app.ptr(), cf_ptr(&focused_attr), &mut value);
            if err != AX_ERROR_SUCCESS {
                return None;
            }
            let element = CfRef::new(value)?;

            let full_text = read_value(element.ptr())?;
            let (cursor_pos, _sel_len) = read_selected_range(element.ptr())?;

            // CFRange uses character indices — split by chars, not bytes
            let chars: Vec<char> = full_text.chars().collect();
            let pos = cursor_pos.min(chars.len());
            let before: String = chars[..pos].iter().collect();
            let after: String = chars[pos..].iter().collect();

            Some((before, after))
        }
    }
}

/// Adjust transcribed text based on where it's being inserted.
///
/// - Mid-sentence: lowercase first letter, drop trailing period
/// - Start of field or after sentence-ending punctuation: keep as-is
/// - Add leading space if the text before doesn't end with whitespace
#[cfg(target_os = "macos")]
fn adjust_for_context(text: &str, before: &str, after: &str) -> String {
    if text.is_empty() {
        return text.to_string();
    }

    let mut result = text.to_string();

    // Should we capitalize the first letter?
    let at_sentence_start = if before.is_empty() {
        true
    } else {
        let trimmed = before.trim_end();
        trimmed.ends_with('.')
            || trimmed.ends_with('!')
            || trimmed.ends_with('?')
            || trimmed.ends_with('\n')
    };

    if !at_sentence_start {
        // Lowercase the first character
        let mut chars = result.chars();
        if let Some(first) = chars.next() {
            if first.is_uppercase() {
                let lower: String = first.to_lowercase().collect();
                result = format!("{lower}{}", chars.as_str());
            }
        }
    }

    // Remove trailing period if there's text after the cursor
    if !after.trim_start().is_empty() && result.ends_with('.') {
        result.pop();
    }

    // Add leading space if needed
    if !before.is_empty()
        && !before.ends_with(' ')
        && !before.ends_with('\n')
        && !before.ends_with('\t')
    {
        result = format!(" {result}");
    }

    log::info!(
        "Context adjustment: before={:?} after={:?} → {:?}",
        &before[before.len().saturating_sub(20)..],
        &after[..after.len().min(20)],
        &result,
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_roundtrip() {
        let Ok(mut clipboard) = Clipboard::new() else {
            eprintln!("No display server, skipping clipboard test");
            return;
        };

        let test_text = "verba_test_clipboard_roundtrip";
        let saved = save_clipboard(&mut clipboard);

        clipboard.set_text(test_text).unwrap();
        let got = clipboard.get_text().unwrap();
        assert_eq!(got, test_text);

        if let Some(prev) = saved {
            restore_clipboard(&mut clipboard, prev);
        }
    }

    #[cfg(target_os = "macos")]
    mod context_tests {
        use super::super::adjust_for_context;

        #[test]
        fn empty_field_keeps_capitalization() {
            assert_eq!(adjust_for_context("Hello world.", "", ""), "Hello world.");
        }

        #[test]
        fn mid_sentence_lowercases_and_drops_period() {
            assert_eq!(
                adjust_for_context("Hello world.", "I said ", "to everyone"),
                "hello world"
            );
        }

        #[test]
        fn after_period_keeps_capitalization() {
            assert_eq!(
                adjust_for_context("Hello world.", "First sentence. ", ""),
                "Hello world."
            );
        }

        #[test]
        fn adds_leading_space_when_missing() {
            assert_eq!(
                adjust_for_context("More text.", "some words", ""),
                " more text."
            );
        }

        #[test]
        fn no_double_space() {
            assert_eq!(
                adjust_for_context("More text.", "some words ", ""),
                "more text."
            );
        }

        #[test]
        fn after_exclamation() {
            assert_eq!(
                adjust_for_context("Next part.", "Wow! ", ""),
                "Next part."
            );
        }

        #[test]
        fn cursor_at_end_with_text_after() {
            // Inserting before existing text — no trailing period
            assert_eq!(
                adjust_for_context("Hello.", "The ", " is great"),
                "hello"
            );
        }
    }
}
