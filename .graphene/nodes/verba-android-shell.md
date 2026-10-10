---
type: module
summary: Android host shell: MainActivity (share intent, back gesture), VerbaApp (JNI: clipboard, share sheet, hidden-WebView fetch), manifest, share.rs intake queue
entry_points:
  - src-tauri/gen/android/app/src/main/java/com/alexb151/verba/MainActivity.kt
  - src-tauri/gen/android/app/src/main/java/com/alexb151/verba/VerbaApp.kt
  - src-tauri/src/share.rs
  - src-tauri/src/android_bridge.rs
covers:
  - src-tauri/gen/android/app/src/main/AndroidManifest.xml
  - src-tauri/gen/android/app/src/main/java/com/alexb151/verba/
  - src-tauri/src/share.rs
  - src-tauri/src/android_bridge.rs
last_commit: 12ebb616a103c7f54cb562830c524f729e4d5099
---

- Shared-text intake (fixed 2026-10-10): MainActivity.handleSharedIntent must read EXTRA_TEXT with getCharSequenceExtra().toString(), not getStringExtra(): EXTRA_TEXT is a CharSequence by contract and senders that share a styled selection hand over a Spanned, for which Bundle.getString logs a typeWarning and returns null, so the share was silently dropped (app opened, nothing imported). Also: the frontend used to treat ANY shared text containing an http(s) URL as an article to fetch, so a paragraph mentioning a link was discarded when the fetch failed; importSharedText now falls back to saving the raw text with no url attached (helper fetchSharedArticle returns null on any failure). Manifest matches ACTION_SEND + text/plain only; MainActivity additionally checks type.startsWith("text/"). <!-- id:cfe3 -->
- Back handling: MainActivity registers the only OnBackPressedCallback (TauriActivity sets handleBackNavigation=false), evaluates window.verbaHandleBack() in the webview and backgrounds the app unless JS returns "true". VerbaApp keeps static JNI entry points (copyToClipboard, shareText, webViewFetch, nativeSharedText, nativeWebFetchDone); JNI_OnLoad caches the VerbaApp class, and clipboard/share-sheet work is posted to the main looper. Cold share: MainActivity stashes text in Rust (share.rs PENDING) before the webview exists; frontend pulls via take_shared_text at DOMContentLoaded. Warm share: share.rs emits shared-text and the frontend pulls the same way (delivered exactly once). gradlew needs JAVA_HOME (the justfile sets it); run `just apk`, not gradlew directly. <!-- id:ca07 -->
