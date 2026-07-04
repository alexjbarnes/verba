# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
just apk            # Build debug Android APK (signed, ~153MB)
just apk-release    # Build release APK
just test           # Run Rust library tests (cargo test --lib in src-tauri/)
just check          # Fast compile check (no linking)
just eval           # Run pipeline eval harness against test cases
just clean          # Clean all build artifacts (cargo + gradle)
```

First-time setup requires building sherpa-onnx native libraries:
```bash
./scripts/android-build.sh --setup-only
```

The `just apk` pipeline: `npx tauri android build --target aarch64 --apk` then strip symbols, re-run Gradle to repackage, zipalign + apksigner. The Tauri CLI must be used (not raw `cargo ndk`) because it sets env vars that control how the Android WebView serves frontend assets at runtime.

Running a single test:
```bash
cd src-tauri && cargo test test_name --lib
```

## Architecture

Tauri v2 app with Rust backend, vanilla HTML/JS/CSS frontend, targeting desktop and Android.

### Three execution paths

1. **Desktop** - Tauri window with global hotkey (Alt+D), system tray, clipboard paste
2. **Android app** - Tauri WebView, dictation triggered from UI buttons
3. **Android IME** - Accessibility service with JNI bridge (`android_ime.rs`), no WebView, operates independently from the Tauri app

### Data flow

Audio (cpal) -> Resampler (16kHz) -> VAD (Silero) -> Transcriber (sherpa-onnx worker thread) -> Post-processing pipeline -> History + emit event to frontend

### Post-processing pipeline (`src-tauri/src/postprocess/`)

Five stages run sequentially. Pipeline returns `PipelineResult` with intermediate snapshots for stages that changed the text:
1. **Filler removal** (`filler.rs`) - rule-based: "um", "uh", word duplicates (~1ms)
2. **ITN** (`itn.rs`) - inverse text normalization: numbers, dates, ordinals (~5ms)
3. **Vocab** (`vocab.rs`) - user vocab substitution + built-in informal contractions (gonna->going to) (<1ms)
4. **Grammar** (`grammar_neural.rs`) - CoLA router + T5 corrector, see below (~4-65ms). Skipped for texts under 5 words
5. **Cleanup** (inline in `mod.rs`) - capitalize, spacing, trailing punctuation

### Grammar correction (`grammar_neural.rs`)

- CoLA router (ELECTRA-small, 14MB ONNX INT8) scores sentence acceptability P(acceptable)
- Below threshold (0.5, from `data/grammar/config.0.0.1.json`) routes to T5 corrector (T5-efficient-tiny, 30MB ONNX INT8)
- Per-sentence splitting and correction with negation guard (prevents meaning inversion) and case-insensitive task-prefix strip
- `build.rs` sets `grammar_neural_bundled` cfg flag only if ALL 7 model files exist; otherwise it prints a warning and the stage is a NO-OP in that build
- Model files are gitignored (except config + KV weights) — after a fresh checkout run `python3 scripts/download_t5_grammar_onnx.py --output-dir src-tauri/data/grammar/ --version 0.0.1 --verify` BEFORE building, or the APK ships without grammar correction
- Models embedded at compile time via `include_bytes!()`

### Snippets (`src-tauri/src/snippets.rs`)

Text expansion system for common phrases:
- Trigger phrases activate snippet body insertion
- Fuzzy matching via normalized Levenshtein distance (0.30 threshold)
- Self-healing: learns misheard triggers over time
- JSON persistence, dedicated UI tab with creation wizard

### Key Rust modules (`src-tauri/src/`)

- `dictation.rs` - `DictationManager` orchestrates record/transcribe/deliver cycle. Holds `Mutex<Option<AudioRecorder>>` and `Mutex<Option<Transcriber>>`, both lazily initialized on Android
- `transcribe.rs` - Dedicated worker thread owns the ONNX recognizer. Sends requests via mpsc channel. On Android, uses `fork()` for process isolation against native crashes
- `recorder.rs` - Audio capture with VAD-based segmentation. Resamples from device rate to 16kHz. Prefill buffer (300ms) prevents word clipping
- `models.rs` - `ModelManager` with built-in registry of Whisper/Parakeet models. Handles download, deletion, active model selection. `first_downloaded_model()` respects active selection
- `coordinator.rs` - State machine for shortcut debouncing (30ms window). Serializes press/release into Start/Stop/Cancel commands
- `android_ime.rs` - JNI exports for `VerbaAccessibilityService`. Has its own `OVERLAY_STATE` with separate recorder/transcriber instances. Writes to same history file but via separate `History` instance
- `history.rs` - JSON persistence. `list()` reloads from disk each call to pick up entries from IME path
- `snippets.rs` - Snippet management with exact/fuzzy matching and trigger learning
- `config.rs` - AppConfig persistence (language, threads, device index, haptic feedback, active model)
- `engine.rs` - Initialization orchestration and readiness checks

### Platform-specific code

Desktop-only deps (arboard, enigo, global-shortcut) gated with `cfg(not(target_os = "android"))`. Use `#[cfg(desktop)]` / `#[cfg(mobile)]` (Tauri aliases) or `#[cfg(target_os = "android")]` for platform splits.

### Frontend (`src/`)

Three files: `index.html`, `main.js`, `styles.css`. No build step, no framework. Uses Tailwind CDK + Material Symbols icons. Communicates with Rust via `window.__TAURI__.core.invoke()` and `window.__TAURI__.event.listen()`. Tauri embeds these at compile time via `generate_context!()`.

Navigation: collapsible sidebar (hamburger menu) with pages for History, Models, Audio, Snippets, Settings, Debug.

### Android overlay visibility (`VerbaAccessibilityService.kt`)

The dictation overlay (floating mic button) uses keyboard visibility as ground truth for show/hide decisions:
- `isKeyboardVisible()` checks `AccessibilityWindowInfo.TYPE_INPUT_METHOD` via the `windows` API (`flagRetrieveInteractiveWindows` enabled)
- **Show**: `VIEW_FOCUSED(editable=true)` shows overlay immediately. If keyboard hasn't appeared within 1.5s, treats it as phantom focus and hides (catches Maps search-to-navigation transitions)
- **Hide**: `scheduleHide()` checks keyboard visibility after 500ms debounce. Keyboard visible = keep. Keyboard gone = hide
- **Keyboard dismissal**: `TYPE_WINDOWS_CHANGED` detects keyboard disappearing in real time and triggers `scheduleHide`
- `findFocusedEditText()` is unreliable for WebView apps (Brave, Chrome) and is only used for text context retrieval during injection, not for show/hide decisions

### Android build details

- `build.rs` compiles `stubs.c` (NNAPI linker stub), forces `libc++_shared.so` linkage on Android, and sets `grammar_neural_bundled` cfg if model files present
- sherpa-onnx static libs live at `.android-deps/sherpa-onnx/install/lib/` (built once via `android-build.sh --setup-only`)
- `src-tauri/gen/android/` is the Gradle project. `RustWebViewClient.kt` handles WebView asset loading via JNI functions generated by Tauri
- Debug keystore at repo root (`debug.keystore`, gitignored). Must persist across builds for APK upgrade compatibility

### Eval harness (`src-tauri/src/bin/eval_pipeline.rs`)

Runs the postprocess pipeline against JSON test cases in `scripts/data/`. Outputs TSV metrics for comparing pipeline changes. Run with `just eval`.

### Tauri events

Backend emits: `dictation-state`, `dictation-error`, `transcription-result`, `download-progress`, `download-complete`, `log-message`. Frontend listens and updates UI reactively.

### Known constraints

- sherpa-onnx is the only viable ASR engine for real-time Android (candle too slow, tract can't run Parakeet)
- ONNX Runtime arm64 crashes during inference for some models (Parakeet TDT 0.6B). fork() isolates this
- Dual C++ runtime in final binary (c++_static from oboe/cpal + c++_shared from sherpa-onnx). Works but fragile if adding more C++ deps
- `findFocusedEditText()` (rootInActiveWindow.findFocus) cannot traverse WebView accessibility trees. Do not use it for show/hide logic
