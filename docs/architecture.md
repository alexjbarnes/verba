# Verba: Architecture Notes

## Vision

Emulate the Wispr Flow experience: voice input that produces clean, context-appropriate
written text. Not a transcription tool. A dictation tool. The distinction matters.

Transcription captures exactly what was said.
Dictation produces what you meant to write.

## Wispr Flow: What We're Emulating

Flow's pipeline (roughly):

1. Microphone capture
2. Speech-to-text (ASR model)
3. LLM reformatting layer (the key differentiator)
4. Clean text inserted into the active text field

What Flow does beyond raw transcription:

- Removes filler words (um, uh, like, you know)
- Handles false starts and self-corrections (uses the corrected version)
- Restructures spoken language into grammatically correct written sentences
- Applies formatting appropriate to context (email vs chat vs document)
- Matches the user's personal writing style over time
- Supports voice commands for editing ("delete that last sentence", "make that a bullet point")
- System-wide integration: works in any text field on the OS

Flow uses cloud-based LLMs for the reformatting step. We want to do this locally.

## Our Stack

- Rust + Tauri (desktop app)
- Whisper or Parakeet models (local ASR)
- Harper (local grammar/spelling correction)
- GECToR or similar sequence-tagging model (fast grammar correction, single forward pass)
- Optional: small LLM fallback for complex restructuring (Qwen 2.5 0.5B / Gemma 3 1B)

## The Latency Problem

Target: sub-200ms for the post-processing step (to feel instant).

Autoregressive LLMs cannot hit this target on CPU. Even the smallest models:
- SmolLM2 135M: ~870ms for a paragraph (80 tokens in, 60 tokens out)
- Qwen 2.5 0.5B: ~1.6s for a paragraph
- Qwen 2.5 3B: ~4-5s for a paragraph

The bottleneck is token-by-token generation. Each output token requires a full forward
pass through the model. There is no way around this with current transformer architectures.

What CAN hit 200ms: non-generative models that classify/tag in a single forward pass.

## Text Processing Pipeline

```
Microphone -> ASR (Whisper/Parakeet) -> Fast Pipeline -> Output
                                            |
                                            v
                                   [1] Filler word removal (rule-based, ~1ms)
                                   [2] Punctuation restoration (sequence tagger, ~10-20ms)
                                   [3] Inverse text normalization (rules, ~5ms)
                                   [4] Grammar correction (GECToR via ONNX, ~20-40ms)
                                   [5] Harper (spelling/grammar polish, ~5-10ms)
                                            |
                                            v
                                   Total: ~40-80ms on CPU
```

For the 80% of cases where speech is fairly clean, this pipeline handles it.
For the remaining 20% (garbled speech, complex restructuring needed), a small LLM
fallback can be invoked at higher latency (~1-2s).

### Stage 1: Filler Word Removal (rule-based, ~1ms)
Strip verbal artifacts that don't belong in written text.
- "um", "uh", "er", "like" (when used as filler), "you know", "I mean", "sort of"
- False starts and repeated words
- Simple pattern matching, no model needed

### Stage 2: Punctuation Restoration (sequence tagger, ~10-20ms)
Models like fullstop-punctuation-multilang (based on XLM-RoBERTa).
- Single forward pass classifies each token: period, comma, question mark, or nothing
- Run via ONNX Runtime (`ort` crate)
- Handles the biggest gap in ASR output: missing punctuation

### Stage 3: Inverse Text Normalization (rules, ~5ms)
Convert spoken forms to written forms.
- "twenty three dollars" -> "$23"
- "january fifth twenty twenty six" -> "January 5, 2026"
- Mostly rule-based. NeMo has reference implementations.

### Stage 4: Grammar Correction -- GECToR (~20-40ms)
The key model in the pipeline. GECToR (Grammatical Error Correction via Token-level
Operations and Realization) is NOT a generative model. It uses a BERT-like encoder
with a tagging head.

How it works:
- Predicts edit operations per token: keep, delete, insert, replace
- Single forward pass, no autoregressive generation
- Base model: ~110M-350M params (BERT-base to DeBERTa-large)
- On CPU via ONNX: 5-30ms per sentence, 20-80ms per paragraph

This handles homophones, agreement errors, tense issues, missing articles, etc.
Pretrained checkpoints exist for English GEC.

### Stage 5: Harper (spelling/grammar polish, ~5-10ms)
Harper runs last as a final polish layer.
- Catches anything GECToR missed
- Pure Rust, no ONNX needed
- ~200 pattern-based rules
- Handles things like "all intensive purposes" -> "all intents and purposes"

### Fallback: Small LLM for Complex Restructuring
When the fast pipeline output still reads like spoken language (run-ons, fragments,
incoherent structure), route to a small LLM.

Best candidates (ranked by quality-per-latency):
1. Qwen 2.5 0.5B Q4_K_M (~350MB, ~1.6s/paragraph on CPU)
2. Gemma 3 1B Q4_K_M (~600MB, ~2s/paragraph on CPU, Google's on-device focus)
3. Qwen 2.5 1.5B Q4_K_M (~900MB, ~3s/paragraph, higher quality)

Alternatively, fine-tuned T5-small (60M params) or CoEdIT (Grammarly's text editing
model) via ONNX could handle restructuring at ~50-150ms per sentence.

A confidence-based router decides when to invoke the fallback:
- Simple heuristic: sentence length, disfluency count, incomplete thoughts
- Or a tiny classifier (~5ms) trained on "needs restructuring" labels

## Rust Inference Libraries

### The C++ dependency question

Cross-platform (especially Android) builds get painful with C/C++ deps. Three native
deps cause problems: whisper.cpp, llama.cpp, and ONNX Runtime. The question is whether
we can go pure Rust for all of them.

### Option A: Pure Rust (best for cross-platform)

**candle** (Hugging Face) for GECToR and LLM fallback:
- Pure Rust, no C/C++ deps. Compiles to Android with `cargo build --target aarch64-linux-android`.
- Has DeBERTa-v2 implementation (covers DeBERTa-v3 too, same architecture).
- Has `DebertaV2NERModel`: encoder + per-token classification head. This is exactly
  what GECToR needs. No custom architecture work required.
- Loads SafeTensors and PyTorch .bin weights via VarBuilder.
- Supports Whisper, Llama, Qwen, Phi, Gemma for the LLM fallback.
- ~20-40% slower than C++ equivalents. For GECToR (~40ms), this is irrelevant.
- candle-onnx exists but is immature (missing LayerNormalization op). Use native
  model implementations instead.

**tract** (Sonos) for Parakeet ASR:
- Pure Rust ONNX inference engine. Built by Sonos for production audio on embedded devices.
- ~85% ONNX operator coverage (opsets 9-18).
- Has `tract-pulse` for streaming/pulsing networks (designed for audio).
- **Tested against Parakeet TDT 0.6B V2 INT8** (see `src-tauri/tests/tract_parakeet.rs`).
- Result: tract can parse the encoder, decoder, and joiner ONNX files, but **cannot run
  inference** on the encoder. Two blocking issues:
  1. **Encoder**: symbolic dimension unification fails at Transpose nodes. tract cannot
     bind symbolic axis names (e.g. `audio_signal_dynamic_axes_1`) to concrete values
     during shape inference. Affects both `into_typed()` and `into_optimized()`.
  2. **Decoder**: ONNX uses dotted symbolic dim names (e.g. `states.1_dim_1`) that
     tract's TDim parser rejects.
- Joiner loads and parses without issues.
- Potential workarounds: preprocess ONNX to replace symbolic dims with static shapes
  (e.g. via onnx-simplifier), implement the LSTM decoder natively in Rust, or
  contribute a fix to tract's TDim unification.
- **Current verdict: tract cannot run Parakeet out of the box.** Workarounds exist
  but add complexity. For Android ASR, candle Whisper is the safer pure-Rust path.

**Summary of pure Rust stack:**
- Parakeet ASR: tract (ONNX) or candle Whisper as fallback
- GECToR: candle (native DeBERTa-v2 + NER head)
- Harper: pure Rust already
- LLM fallback: candle (Qwen, Gemma, etc.)
- Zero C/C++ dependencies. One `cargo build` per target.

### Option B: C++ deps (best for performance)

- `ort` crate (ONNX Runtime) for GECToR, Parakeet, and taggers. Full operator
  coverage, INT8 quantization, CoreML/NNAPI/DirectML acceleration.
- `llama-cpp-2` for LLM fallback. Fastest CPU inference available.
- `parakeet-rs` for Parakeet ASR (wraps ort). Works today, all variants supported.
- 20-40% faster than pure Rust, but requires NDK cross-compilation for Android,
  C++ STL coordination, and building ONNX Runtime from source for mobile targets.

### Option C: Hybrid (recommended)

- sherpa-onnx for Parakeet ASR on all platforms (proven, fast, ships pre-built
  Android libraries with RTF 0.07 on mobile)
- candle for GECToR and LLM fallback (pure Rust, easy cross-compile, performance
  penalty is negligible for these models)
- Harper stays as-is (pure Rust)

The C++ dependency (sherpa-onnx) stays for ASR because no pure Rust option can
match it on Android. sherpa-onnx ships pre-built Android libraries, eliminating
the NDK cross-compilation pain for ONNX Runtime.

**Pure Rust ASR is not viable on Android today** (benchmarked March 2026):
- candle Whisper on Pixel 7: RTF 3.0 (3x slower than real-time). Known SIMD
  issue with ARM NEON in NDK builds (GitHub #1048).
- tract cannot run Parakeet (symbolic dimension failures, tested in
  `src-tauri/tests/tract_parakeet.rs`).
- whisper.cpp on Android: RTF 3.52, 51x slower than sherpa-onnx on the same
  device (VoicePing benchmark Feb 2026).
- sherpa-onnx Whisper Tiny on Samsung Galaxy S10: RTF 0.07. The gap is ONNX
  Runtime's XNNPACK/NEON optimization vs unoptimized kernels in other runtimes.

Alternatives explored and rejected:
- ort (ONNX Runtime Rust bindings): same engine as sherpa-onnx but no pre-built
  Android binaries. You'd build ONNX Runtime from source for Android NDK.
- transcribe-rs: wraps ort, same Android problem. Parakeet pipeline in Rust is
  clean but gains nothing over sherpa-onnx which already works.

### Parakeet Model Details

Parakeet uses a FastConformer encoder with different decoder heads:

| Variant | Decoder | Autoregressive? | Pure Rust feasibility |
|---------|---------|-----------------|----------------------|
| parakeet-ctc | Conv projection + softmax | No | Best candidate. CTC decode is trivial (~20 lines). |
| parakeet-tdt | LSTM + duration prediction | Yes | Moderate. LSTM loop outside ONNX graph. |
| parakeet-rnnt | LSTM + joint network | Yes | Moderate. Similar to TDT. |

For pure Rust, target **parakeet-ctc**. The encoder runs as a single ONNX forward pass,
and CTC decoding is just argmax + collapse repeats + remove blanks.

ONNX exports exist on HuggingFace: `istupakov/parakeet-tdt-0.6b-v2-onnx` and others.
Sherpa-ONNX also provides pre-converted models with INT8 quantization.

### Existing Rust Parakeet Projects

- `parakeet-rs` (altunenes/parakeet-rs): Uses ort. Supports CTC, TDT, RNNT, streaming.
  Works today but brings in the C++ dep.
- `parakeet.cpp` (Frikallo/parakeet.cpp): Pure C++ reference. Could inform a Rust port
  if we ever want native candle-based Parakeet inference.

## Quantization Notes

For GECToR / tagger models (110M-350M params):
- If using ort: ONNX INT8 quantization. Negligible quality loss, good speed.
- If using candle: load SafeTensors in FP32/FP16. Models are small enough.
- Do NOT use INT4 for these small models. Quality degrades too much.

For LLM fallback (0.5B-1.5B params):
- If using candle: load GGUF Q4_K_M or Q5_K_M quantized weights.
- If using llama.cpp: GGUF Q4_K_M is the speed/quality sweet spot.
- At 0.5B params, Q4 starts to hurt quality. Consider Q5_K_M or Q8_0.

For Parakeet ASR:
- INT8 ONNX models available from sherpa-onnx. Recommended for mobile.
- FP16 for desktop where memory is not a constraint.

## Harper Integration Details

Crate: `harper-core` (NOT `harper` on crates.io, that's unrelated)

```toml
[dependencies]
harper-core = "1.12"
```

Key API:
- `Document::new_plain_english_curated(text)` -- parse text
- `LintGroup::new_curated(dict, Dialect::American)` -- create linter with all rules
- `linter.lint(&doc)` -- get list of issues
- `suggestion.apply(span, &mut chars)` -- apply fix in-place

Things to know:
- English only
- American or British dialect (binary choice)
- Dictionary loads once via LazyLock, cached for process lifetime
- First load has an init cost (FST construction)
- `Linter::lint` takes `&mut self` (internal caching), needs mutex for thread sharing
- Apply suggestions back-to-front to avoid offset invalidation
- ~200 grammar rules, some disabled by default (style-oriented ones)
- No network calls, fully offline
- Pure Rust, no C deps

## Streaming: Sentence-by-Sentence Processing

The pipeline should not wait for the user to stop speaking. Process sentence-by-sentence
as speech comes in. This changes the latency profile dramatically.

A single sentence is ~15-25 tokens. At that size:
- GECToR: 5-10ms
- Full pipeline (filler removal + punctuation + GECToR + Harper): under 20ms
- That is genuinely instant. Undetectable by the user.

The real latency becomes ASR, not post-processing:

```
User speaks sentence -> ASR processes (~200-500ms) -> pipeline corrects (~20ms) -> text appears
```

The correction pipeline is noise on top of the ASR latency. The user perceives a single
delay (the ASR step), and the text that appears is already clean.

### Sentence Boundary Detection

The question is: how do you know when a sentence ends during streaming ASR?

Three approaches, best used together:

1. Voice Activity Detection (VAD)
   - Silero VAD is tiny and fast. Likely already in use for start/stop detection.
   - A pause of 300-500ms in speech likely means end of sentence.
   - Flush the buffer and run the pipeline on what you have.

2. ASR punctuation output
   - Whisper sometimes produces periods, commas, question marks in its output.
   - When the ASR emits sentence-ending punctuation, treat it as a boundary.
   - Not always reliable, but free when it works.

3. Token count fallback
   - If the buffer exceeds ~30 tokens without a boundary, flush anyway.
   - Prevents unbounded buffering for fast continuous speech.
   - The pipeline handles fragments fine. Worse case: a sentence split awkwardly,
     which Harper and GECToR can still correct individually.

### Streaming Architecture

```
[Audio Stream]
     |
     v
[VAD] -- silence detected? --> [Flush buffer]
     |                              |
     v                              v
[ASR (Whisper/Parakeet)]     [Correction Pipeline]
     |                              |
     v                              v
[Token buffer] <-- accumulate  [Clean text -> UI]
```

Each flush is independent. The correction pipeline processes one sentence at a time.
No state carried between sentences (Harper and GECToR are stateless per invocation).

This means we can even double-buffer: display the previous corrected sentence while
the next one is still being transcribed. The user sees a smooth stream of clean text.

### Edge Cases

- Mid-sentence corrections: user says "I went to the... no, she went to the store."
  Filler removal (stage 1) should catch "I went to the... no," as a false start.
  This is the hardest part of rule-based filler removal. May need the LLM fallback
  for complex false starts.
- Very long sentences: some speakers don't pause. The token count fallback handles
  this, but the split point may be awkward. Punctuation restoration (stage 2) can
  help by inserting a period or comma at a natural break.
- Sentence-spanning context: "He went to the store. He bought milk." -- if processed
  independently, the second sentence can't resolve "He" to the correct referent.
  This is fine for grammar correction but matters if we ever add style rewriting.

## How Flow Probably Gets Away With It

Flow uses cloud LLMs, which changes the equation entirely:
- Server-side GPU inference is 10-50x faster than client-side CPU
- A 7B model on an A100 generates at 100+ tokens/sec
- Network round-trip adds latency, but the generation itself is fast
- They almost certainly do sentence-level streaming (process as you speak, not after)

To match that feel locally, we can't use the same approach. The pipeline of specialized
models is our best bet. With sentence-level streaming, the correction step is invisible
to the user -- it hides behind the ASR latency that exists regardless.

## Android Port

Currently Mac only. Tauri v2 ships with stable Android support (since October 2024).
The frontend (WebView) ports easily. The Rust backend and native ML dependencies are
where the work lives.

### What's Easy

- Tauri v2 Android is stable. `tauri android init` generates an Android Studio project.
  `tauri android build` cross-compiles Rust to `.so` files and packages an APK.
- Pure Rust crates (like harper-core) just work. If it compiles to `aarch64-linux-android`,
  it runs on Android.
- The WebView on Android is Chromium-based (System WebView, updated via Play Store).
  Less fragmented than you might expect.
- Platform-specific code via `#[cfg(target_os = "android")]` and conditional deps in
  Cargo.toml.

### What's Hard

**1. Native C/C++ dependencies (the big one)**

Every C/C++ dependency needs cross-compilation via the Android NDK. This affects:
- whisper.cpp (for ASR)
- llama.cpp (for LLM fallback)
- ONNX Runtime (for GECToR and tagger models)

Each has its own build story:
- whisper.cpp / llama.cpp: CMake with NDK toolchain. Documented and known to work.
  The `llama-cpp-sys-2` crate handles this, use the `static-stdcxx` feature to avoid
  C++ STL conflicts on Android.
- ONNX Runtime: The `ort` crate's pre-built binaries don't include Android. You must
  build ONNX Runtime from source with `--android` and use `ort`'s `system` strategy
  to point at your compiled `libonnxruntime.so`.

**2. C++ STL linking**

Android NDK uses `libc++`, not `libstdc++`. Many Rust `-sys` crates default to linking
`stdc++`. This causes either link failures or runtime crashes. You need to ensure every
native dependency links against `c++_static` or `c++_shared` consistently. The
`static-stdcxx` feature flags on llama-cpp-sys-2 exist for this reason.

**3. GPU acceleration is unreliable**

- Vulkan on Android: builds work but performance is poor to broken. Adreno GPUs often
  fail to load models entirely. Mali GPUs load models but sometimes run slower than CPU.
- OpenCL: Qualcomm contributed an optimized backend for Adreno GPUs. Best path for
  Snapdragon devices, but not universal.
- NNAPI: Unified interface to CPU/GPU/DSP/NPU. Available on Android 8.1+. Limited op
  support, so not all models work. The `ort` crate supports it via the `nnapi` feature.
- Recommendation: target CPU (ARM NEON) as the baseline, treat GPU/NPU as optional
  acceleration for specific chipsets.

**4. Thermal throttling**

Sustained inference on phones is fundamentally different from laptops. After 5-10
continuous inference rounds, CPU/GPU frequencies can be halved by the thermal governor.
Temperature rises from ~42C to ~67C. This means:
- First transcription is fast, 10th transcription in a row is noticeably slower
- Need to profile sustained workloads, not just cold-start benchmarks
- The lightweight pipeline approach (GECToR at 20ms, not LLM at 2s) matters even more
  on mobile. Less compute = less heat = more consistent performance.

**5. No system tray, global shortcuts, or accessibility hooks**

Desktop features like system-wide text insertion (accessibility APIs), global hotkeys,
and menu bar presence don't exist on mobile. The app model is different:
- Need a foreground service for background audio capture
- Share sheet or input method integration for system-wide use
- Android permissions: RECORD_AUDIO, FOREGROUND_SERVICE, POST_NOTIFICATIONS

### Android Performance Expectations

On a modern flagship (Snapdragon 8 Gen 3, Tensor G4):

| Component | Model | Expected Performance |
|-----------|-------|---------------------|
| ASR | Whisper Tiny (39M) | ~2s for 30s of audio |
| ASR | Whisper Base (74M) | 1.5-2x real-time |
| ASR | Whisper Small (244M) | Near real-time |
| GECToR | DeBERTa-base (110M) | ~10-30ms per sentence (ONNX + NNAPI) |
| Harper | Pure Rust | ~5-10ms (same as desktop) |
| LLM fallback | Qwen 2.5 0.5B Q4 | ~10-20 tok/s decode |

The correction pipeline (GECToR + Harper) is well within budget on mobile.
ASR is the bottleneck, same as desktop. Whisper Tiny or Base is probably the right
choice for mobile to keep latency and thermal impact low.

### Recommended Android Approach

1. Start with CPU-only inference (ARM NEON). It works everywhere.
2. Ship only `arm64-v8a` (64-bit ARM). Covers all phones from ~2017 onward.
   Avoid building for armv7, x86, x86_64 unless you have a reason.
3. Use `--split-per-abi` for APK size if needed.
4. Use sherpa-onnx for ASR. It ships pre-built Android libraries and has proven
   real-time performance (RTF 0.07 for Whisper Tiny, RTF 0.088 for Parakeet
   INT8 on ARM Cortex A76). No need to build ONNX Runtime from source.
5. Use Parakeet INT8 as the default model (~600MB). Falls back to Whisper Tiny
   or Base on devices with less memory.
6. Skip the LLM fallback on mobile initially. The fast pipeline (filler removal +
   GECToR + Harper) is the right scope for v1 Android.
7. Add NNAPI as an optional accelerator for devices that support it.

### Build Checklist

- Android SDK + NDK 28+ (required for Google Play's 16KB page alignment)
- JDK for Gradle
- Rust targets: `rustup target add aarch64-linux-android`
- `tauri android init` to generate the Android project
- Cross-compile ONNX Runtime for arm64-v8a with XNNPACK
- Ensure all C++ deps use `c++_static` (not `stdc++`)
- AndroidManifest.xml permissions: RECORD_AUDIO, INTERNET (if any cloud features),
  FOREGROUND_SERVICE
- Test on physical device early. Emulator performance is not representative for ML.

## Implementation Patterns (from Handy)

Handy (github.com/cjpais/Handy) is a Tauri v2 voice dictation app with a mature
implementation of several subsystems verba needs. MIT licensed. These patterns
are worth adopting regardless of inference engine choice.

### 1. Voice Activity Detection (VAD)

Verba currently has no VAD. All recorded audio (including silence) goes to the
transcription engine. This wastes inference time and can degrade accuracy.

**Architecture to adopt:**

Three layers, composed together:

```
Raw audio frames (30ms @ 16kHz)
    |
    v
[Silero VAD] -- neural network, 480 samples per frame, ONNX model (~2MB)
    |              returns speech probability 0.0-1.0
    v
[SmoothedVad] -- decorates any VAD with:
    |              - Onset: N consecutive speech frames to trigger (avoids false starts)
    |              - Hangover: M frames of continued output after silence (avoids cutting mid-word)
    |              - Prefill: ring buffer of last K frames, emitted on speech start
    v
[Speech / Noise decision]
```

Handy uses: onset=2 frames, hangover=15 frames (450ms), prefill=15 frames (450ms).

The prefill buffer is the key insight. Without it, the first 100-200ms of speech
gets clipped because the VAD takes a few frames to detect speech onset. The ring
buffer retroactively includes audio from before the detection trigger.

**Integration point:** VAD runs in the audio recording thread, between resampling
and sample accumulation. Only speech frames get appended to the output buffer.
The transcription engine never sees silence.

**Model:** Silero VAD v4 ONNX (~2MB). Runs via ort or sherpa-onnx (sherpa-onnx
already bundles Silero VAD support). Maintains LSTM hidden/cell state across
frames for temporal context.

### 2. Audio Recorder with Worker Thread

Verba's current recording is a sleep-polling loop in dictation.rs. cpal is
opened fresh each recording, samples accumulate behind a Mutex, and a 50ms
sleep loop keeps the stream alive. No resampling.

**Architecture to adopt:**

```
[cpal stream callback] -- runs on audio device thread
    |                      down-mixes multi-channel to mono
    |                      sends AudioChunk::Samples(Vec<f32>) via mpsc channel
    v
[Worker thread] -- dedicated thread, runs continuously
    |              receives chunks from channel
    |              resamples to 16kHz (rubato crate)
    |              runs VAD per 30ms frame
    |              accumulates speech samples only
    v
[start() / stop()] -- command channel (Cmd::Start, Cmd::Stop)
                       stop returns the accumulated Vec<f32>
```

Key design decisions:
- **Resampling** (rubato crate): device sample rates vary (44.1kHz, 48kHz, etc).
  Resampling to 16kHz in the worker thread means the rest of the pipeline can
  assume a fixed sample rate.
- **Two mic modes**: AlwaysOn (stream stays open, instant start) vs OnDemand
  (opened per recording, with lazy-close after 30s idle). AlwaysOn avoids the
  ~50-100ms mic open latency on each recording.
- **No Mutex on the sample buffer.** The mpsc channel decouples the cpal callback
  thread from the worker thread. The worker owns the buffer exclusively.

### 3. Shortcut State Machine

Verba's current shortcut handling is basic press/release on a single hardcoded
binding (alt+d). No debouncing, no cancel, no toggle mode.

**Architecture to adopt:**

A dedicated coordinator thread with an mpsc channel that serializes all events:

```
[Shortcut events] --+
[Signal events]   --+--> [mpsc channel] --> [Coordinator thread]
[Cancel events]   --+                           |
                                                v
                                        State machine:
                                        Idle -> Recording -> Processing -> Idle
```

The coordinator handles:
- **Push-to-talk**: start on press, stop on release
- **Toggle mode**: start on first press, stop on second press
- **Cancel binding**: abort recording, discard audio
- **Debouncing**: 30ms debounce window prevents double-triggers from key bounce
- **Serialization**: all state transitions happen on one thread, no race conditions

This matters more than it seems. Without debouncing, fast key taps can start
two recordings simultaneously. Without a cancel binding, the only way to abort
is to let it transcribe garbage.

### 4. Model Lifecycle Management

Verba loads models eagerly and keeps them in memory forever. For a dictation app
that might be used once an hour, this wastes hundreds of MB of RAM.

**Pattern to adopt:**
- **Idle unload**: configurable timeout (30s, 60s, 5min, never). A background
  thread watches for inactivity and drops the model.
- **Lazy load on first use**: model loads in a background thread when the shortcut
  is first pressed. A Condvar blocks the transcribe call until loading finishes.
- **Load/unload lifecycle**: `take()` the model out of an `Arc<Mutex<Option<Model>>>`
  before inference, drop the lock, run inference, then put it back. This avoids
  holding the mutex during the entire inference call.
- **Panic recovery**: wrap inference in `catch_unwind`. If the model panics, unload
  it rather than leaving the app in a broken state.

### 5. Cross-Platform Text Insertion

Verba currently uses osascript to simulate Cmd+V on macOS only.

**Pattern to adopt:**

The enigo crate handles keyboard simulation cross-platform. The full paste flow:

1. Save current clipboard contents
2. Write transcribed text to clipboard
3. Wait a configurable delay (paste_delay_ms)
4. Simulate paste keystroke
5. Restore original clipboard

Multiple paste methods, with platform-aware fallbacks:
- **Clipboard paste**: Ctrl+V (Windows/Linux), Cmd+V (macOS)
- **Direct typing**: enigo.text() for character-by-character injection
- **Linux fallback chain**: wtype (Wayland) -> xdotool (X11) -> ydotool (universal)

The configurable delay between clipboard write and paste simulation matters.
Some apps (Electron-based) need 50-100ms to register the clipboard change.

## Open Questions

- Can we get a GECToR checkpoint that handles spoken-to-written artifacts (not just
  grammar errors in written text)? May need fine-tuning on spoken/written parallel data.
  Datasets: Switchboard corpus, ATIS, or synthetic pairs generated by a large LLM.
- Should the LLM fallback be opt-in? Users with fast machines might want it always-on.
  Users on older hardware might prefer the fast pipeline only.
- How do we handle context-awareness (knowing if user is in Slack vs a document)?
  Tauri can potentially query the active window, but this is OS-specific.
- Voice command support: parse commands vs dictation text. This is a separate problem
  from text cleanup.
- Streaming strategy is defined above (sentence-by-sentence with VAD + punctuation
  boundaries). Needs prototyping to validate the boundary detection heuristics.
- Could we train a single small model (T5-small, 60M params) to do filler removal +
  punctuation + grammar in one pass? This would simplify the pipeline at the cost of
  a custom training effort.
