# Verba External API: Design Proposal

Status: proposal, no code changes. Everything below is a recommendation open for review, not a committed plan. Open questions are marked explicitly.

Goal: let other applications use verba's on-device dictation (ASR) and TTS without going through the Tauri UI. Two surfaces are in scope: a local HTTP/WebSocket API (desktop, optionally Android) shaped to match the OpenAI audio API where that makes sense, and Android system integration points (intents, system-service interfaces) so verba can be invoked by, or stand in for, other apps and the OS.

Everything stays on-device. This adds no cloud calls and no new outbound network dependency.

## 0. Baseline: what exists today

Grounding for the rest of this document, from reading `lib.rs`, `engine.rs`, `transcribe.rs`, `vad.rs`, `tts.rs`, `android_ime.rs`, and the Android manifest.

| Area | Current state |
|---|---|
| Mic dictation | `ui_start_recording` / `ui_stop_and_transcribe(_raw)` commands, backed by `engine::Engine`. VAD-segments transcribe in the background during recording but are only joined and surfaced when recording stops. No partial-transcript event exists today. |
| File/batch transcription | Not implemented. No path decodes an arbitrary audio file today. |
| Transcriber decoupling | `Transcriber::transcribe(samples: Vec<f32>, sample_rate: i32)` already takes raw samples directly, independent of `cpal`/live mic capture. Only `AudioRecorder` is mic-coupled. |
| VAD decoupling | `Vad::accept(&mut self, samples: &[f32]) -> Option<Vec<f32>>` is a plain buffer-in/segment-out call, also independent of `cpal`. |
| TTS speak | `tts_speak` / `tts_speak_cached` commands, Piper ONNX (`piper.rs`), per-sentence chunk synthesis with disk cache (`tts_cache.rs`), playback via `player.rs`. |
| TTS voice addressing | Decomposed as model id string (e.g. `"tts-piper-alba"`) plus `sid: i32`, plus an optional custom-voice name. `config.rs` stores `tts_voice` as a bare sid string or `"custom:<name>"`. No single external voice-id string exists yet. |
| HTTP server | None. `Cargo.toml` has `reqwest` (client only) and `tokio` with a narrow feature set (no `net`). No `axum`/`warp`/`hyper`-server. `hound` (WAV) is dev-dependency only, no decode crate at all. |
| Android IME | Accessibility service + JNI (`android_ime.rs`, `VerbaAccessibilityService.kt`) sharing the `engine` singleton and `History` with the Tauri app. |
| ACTION_SEND | Implemented. `MainActivity` has an intent-filter for `text/plain`. `share.rs` / `take_shared_text` consume it. |
| ACTION_PROCESS_TEXT | Not implemented as a system entry point. A JNI export (`nativeReportMispronunciation`) exists for a bespoke accessibility-overlay text-selection menu, a different mechanism from the system-wide `ACTION_PROCESS_TEXT` intent filter its own doc comment alludes to. |
| RecognitionService, TextToSpeechService, AIDL, deep links | None implemented. |

Two findings reduce the effort estimate below: `Transcriber` and `Vad` are already decoupled from live mic capture, so file-based transcription and a network-fed streaming pipeline are buildable from existing pieces rather than new ASR plumbing.

## 1. Local HTTP API

### 1.1 Transport and process model

- Desktop: an HTTP/WebSocket listener started in-process alongside the Tauri app (new `axum` dependency or similar), bound to `127.0.0.1` only, opt-in via a Settings toggle, default off.
- Android: same server code, but only viable as a foreground service, needing a persistent notification while enabled. A bigger ask than desktop, recommend shipping after it (see phase 2).
- Port: fixed default, configurable, written with the bearer token to a small discovery file (`{"port": ..., "token": ...}`) in the app data directory, similar to Ollama (11434) or LM Studio (1234).

### 1.2 Endpoints

| Method + path | Purpose | OpenAI equivalent |
|---|---|---|
| `GET /v1/models` | List ASR/TTS models and TTS voices | `GET /v1/models` |
| `POST /v1/audio/transcriptions` | Transcribe an uploaded audio file | same |
| `POST /v1/audio/speech` | Synthesize speech from text | same |
| `WS /v1/dictation` | Live mic-streaming session, partial transcripts | none (closest: Realtime API) |
| `GET /v1/health` | Liveness probe, no auth required | none |

### 1.3 GET /v1/models

Merges the existing `list_models` (ASR) output with TTS model/voice listing (`ModelManager::tts_model_config`, `tts_model_speakers`):

```json
{ "object": "list", "data": [
  { "id": "whisper-base-en", "object": "model", "type": "asr",
    "downloaded": true, "active": true },
  { "id": "tts-piper-alba", "object": "model", "type": "tts",
    "voices": [ { "id": "tts-piper-alba#0", "sid": 0, "label": null } ] }
] }
```

Open question: most bundled multi-speaker Piper voices likely expose only numeric indices, not names, so `label` will often be null.

### 1.4 POST /v1/audio/transcriptions

`multipart/form-data`:

| Field | Notes |
|---|---|
| `file` | Phase 1: WAV only (decode via `hound`, resample to the model's rate). mp3/m4a/ogg are a non-goal for now, they need a `symphonia`-style dependency. |
| `model` | Verba ASR model id, e.g. `whisper-base-en`. Defaults to the active model. |
| `response_format` | `json` (default), `text`, `verbose_json`. No `srt`/`vtt` until word-level timing is confirmed available (open question). |
| `language` | Optional hint. Divergence: verba's language handling today is mostly a model-choice concern, not a per-request decode parameter, so this may be a no-op for some models pending a closer read of `transcribe.rs`. |

`json` response is `{"text": "..."}`, no token `usage` field. `verbose_json` sources segments from the engine's existing internal `ChunkResult` list (`text`, `audio_ms`, `transcribe_ms` per VAD segment), already computed and simply not surfaced today:

```json
{ "task": "transcribe", "duration_ms": 8470, "text": "...",
  "segments": [ { "id": 0, "start_ms": 0, "end_ms": 2100, "text": "..." } ] }
```

Implementation shape: decode file, resample, call `Transcriber::transcribe(samples, sample_rate)` directly (the whole file is one unit of work, no `AudioRecorder`/VAD/mic involved), run the existing postprocess pipeline. No `stream=true` in phase 1, batch files do not need it.

### 1.5 POST /v1/audio/speech

JSON body:

| Field | Notes |
|---|---|
| `model` | TTS model id, e.g. `tts-piper-alba` |
| `voice` | New convention: `<model-id>#<sid>` (e.g. `tts-piper-alba#0`), or `<model-id>+<custom-name>`, matching the existing internal `tts::set_voice_base` format. Omitted means sid 0. |
| `input` | Text to speak |
| `response_format` | `wav` (default), `pcm`. No mp3/opus/aac/flac: Piper only emits PCM, adding a lossy encoder is a real dependency, not a flag. |
| `speed` | Same semantics as the existing `speed` parameter |
| `stream` | True: chunked-transfer response, one HTTP chunk per synthesized sentence, matching the granularity `tts::speak` already produces for the player and cache. False: buffer and return one complete WAV. |

This mirrors OpenAI's own choice of chunked transfer over SSE for audio bytes, and fits well since verba's synthesis is already chunked internally. The HTTP handler becomes a second consumer of that stream.

### 1.6 WS /v1/dictation

`ws://127.0.0.1:<port>/v1/dictation`, bearer token sent as the first frame rather than a query parameter, to avoid leaking it into proxy or access logs.

Client to server frames:

- `{"type":"start","model":"whisper-base-en","sample_rate":16000,"encoding":"pcm_s16le"}`
- binary frames: raw PCM audio chunks
- `{"type":"stop"}`, or close the socket

Server to client frames:

- `{"type":"ready"}`
- `{"type":"partial","segment_id":3,"text":"...","audio_ms":2100}`
- `{"type":"final","text":"..."}`
- `{"type":"error","error":{"type":"...","message":"..."}}`

Divergence worth documenting prominently: "partial" here is segment-grained. It updates when the VAD detects a pause, not on every audio frame the way some cloud ASR APIs stream word by word. Client authors coming from cloud STT SDKs should not expect sub-second incremental text.

Implementation shape: since `Vad::accept` and `Transcriber::transcribe` are both plain sample-buffer interfaces, a session handler can feed client PCM into a fresh `Vad` and call `Transcriber` per completed segment, a small new pipeline built from existing pieces rather than a reuse of the mic-specific `Engine`/`AudioRecorder`. Open question: does a session share the existing single-session lock the UI and IME already use (simplest: yes, `409 engine_busy` if the UI is mid-recording) or get an independent lane? Recommend the shared-lock answer for phase 1.

### 1.7 Divergences from OpenAI semantics, summarised

| OpenAI concept | Verba equivalent |
|---|---|
| Cloud-hosted models | Local models only, whatever is downloaded on-device |
| Curated named voices (`alloy`, `coral`, ...) | Local `<model>#<sid>` addresses, mostly unnamed |
| API keys tied to a billing account | One bearer token per install, generated locally |
| `usage` token counts | Omitted, nothing is metered |
| Lossy audio codecs | wav/pcm only, no encoder dependency today |

## 2. Auth and security

- Bind `127.0.0.1` (and `::1`) only, never `0.0.0.0`. No LAN exposure, no mDNS advertisement, no port-forwarding help: explicit non-goals.
- Opt-in setting, default off. Enabling it generates a bearer token (shown once, with a regenerate button) and starts the listener.
- Every route except `GET /v1/health` requires `Authorization: Bearer <token>`. Matching the OpenAI/Anthropic header name and scheme means the official `openai` SDK can point `base_url` at the local port with the token as `api_key` and just work.
- CORS: no `Access-Control-Allow-Origin` header by default, blocking browser JS on arbitrary origins. Non-browser clients (curl, SDKs, other native processes) are unaffected, since CORS is a browser-side concept. An optional origin allowlist for local web-dev use can follow later, off by default, with a clear warning about token exposure.
- Token storage: plaintext in the app's local config directory is proportionate for a loopback-only secret. Whether to route it through the Android Keystore or a desktop OS keychain instead is an open question, not required for phase 1.
- Rate limiting and quota headers are not meaningful for a single-user loopback service. A `409 engine_busy` when a session is already active is more honest than fake `x-ratelimit-*` headers.

### 2.1 Error envelope

Adopt the Anthropic shape, a slightly better fit than OpenAI's flatter one because the outer `type` makes every error body self-describing:

```json
{ "type": "error", "error": { "type": "invalid_request_error", "message": "..." } }
```

| `error.type` | HTTP status | When |
|---|---|---|
| `invalid_request_error` | 400 | Bad parameters, malformed multipart, unsupported `response_format` |
| `unauthorized` | 401 | Missing or bad bearer token |
| `model_not_found` | 404 | Unknown model id |
| `model_not_downloaded` | 409 | Valid model id, not yet fetched |
| `engine_busy` | 409 | A recording/synthesis session is already active |
| `internal_error` | 500 | Anything else |

### 2.2 What to copy from OpenAI/Anthropic, and what to skip

| Convention | Copy | Reason |
|---|---|---|
| Versioned path (`/v1/...`) | Yes | Cheap, future-proofs breaking changes |
| `error: {type, message}` envelope | Yes | Consistent client error handling |
| `GET /v1/models` | Yes | Natural fit, backed by data verba already has |
| `Authorization: Bearer` header | Yes | SDK drop-in compatibility |
| Accounts, org/project headers | No | Single-user local install |
| `usage` token metering | No | Nothing is billed |
| Idempotency-Key header | No, for now | Matters for retried side-effecting POSTs. Verba's endpoints are read-only or session-scoped. Revisit if a batch-job endpoint appears |
| Rate-limit headers | No | Single device. `409 engine_busy` covers the real constraint |
| SSE for text deltas | Yes, for a future transcription stream | Matches a shape clients already parse |
| Chunked transfer for audio bytes | Yes | Matches OpenAI's TTS streaming and verba's existing chunking |

## 3. Android integration surfaces

| Surface | Mechanism | Effort | Fit / payoff | Verdict |
|---|---|---|---|---|
| `ACTION_SEND` | Activity intent-filter, `text/plain` | Done | High, already proven | Shipped |
| `ACTION_PROCESS_TEXT` | Small activity, intent-filter on `PROCESS_TEXT`, reads `EXTRA_PROCESS_TEXT`, calls existing `tts_speak` path | Low | High for "select text anywhere, tap Verba, hear it". Reuses all TTS plumbing | Phase 1 |
| Deep links (`verba://`) | Custom-scheme intent-filter plus a parsing activity | Low | Medium, automation glue (Tasker, notification actions), not suited to calls needing a structured response | Phase 2 |
| `RecognizerIntent` / `RecognitionService` | Extend `RecognitionService` (stateless), `BIND_RECOGNITION_SERVICE`, wire callbacks onto the shared `engine` | Medium-high | Low. Verified: Android has no standard user-facing picker for third-party recognizers comparable to the TTS engine picker. Reached only by apps that hardcode a package, or the "default assistant" slot, which demands far more than STT | Not prioritised |
| `TextToSpeechService` | Extend `TextToSpeechService`, `BIND_TEXT_TO_SPEECH_ENGINE`, voice-list meta-data, `onSynthesizeText` streams PCM via `SynthesisCallback` | Medium | High, verified. Settings has a dedicated "choose your TTS engine" screen. Every app calling Android's `TextToSpeech` API gets verba with zero work on their side | Phase 3, standout leverage |
| Bound service / AIDL | AIDL interface + `Binder`, exported service | Medium | Niche. No discovery mechanism, any consumer must hardcode verba's package and interface by hand | Deferred, speculative |

### 3.1 TextToSpeechService, evaluated honestly

This is the one surface where "implement the standard interface" and "reach many apps" coincide, because Android ships a dedicated Settings screen for exactly this (Accessibility, or System > Languages & input > Text-to-speech output, depending on OS version). `RecognitionService` has no equivalent, the main reason it ranks lower above despite being the conceptual mirror image.

Real costs, not just upside:

- `onSynthesizeText` runs synchronously on its own dedicated thread and must not hold the callback open past return. A good structural match for the existing per-chunk, cache-first synthesis in `piper.rs` / `tts_cache.rs`, but it still needs careful interaction with the `Mutex`-guarded TTS globals the Tauri command surface and the accessibility IME already touch.
- The service can be invoked while the main activity is not foregrounded, so model loading must follow the same lazy-init pattern the IME path already uses (`engine::try_claim_init`, `wait_until_ready`).
- The voice list this service advertises should come from the same source of truth as `GET /v1/models`, not be maintained twice.

## 4. Phased plan

Phase 1, smallest useful surface:

- Desktop-only HTTP API, opt-in, loopback bind, bearer token, no CORS by default.
- `GET /v1/models`, `POST /v1/audio/speech` (wav first, chunked streaming as a same-phase fast-follow), `POST /v1/audio/transcriptions` (WAV upload, batch only, `json`/`text`/`verbose_json`).
- Android `ACTION_PROCESS_TEXT` (text-out to TTS): cheap, no server needed, reuses the existing TTS path end to end.
- New dependencies: an HTTP router (`axum` or similar), tokio's `net` and `rt-multi-thread` features, `hound` promoted to a real dependency.

Phase 2:

- `WS /v1/dictation` on desktop, built from `Vad` + `Transcriber`, sharing the single-session lock as the starting concurrency policy.
- HTTP/WS server on Android as an opt-in foreground service, once the foreground-service-type open question is resolved.
- Android deep links for simple automation.

Phase 3:

- Android `TextToSpeechService`, the standout leverage item, once the HTTP API's voice-id convention has settled so both surfaces share one voice list.
- Re-evaluate `RecognitionService` only if a concrete consumer appears. Not worth building speculatively.
- AIDL bound service only if a specific first-party companion app needs it.

Explicit non-goals:

- No cloud relay or fallback, no change to the on-device-only network posture beyond the loopback surface described here.
- No multi-tenant auth, accounts, or usage billing.
- No lossy audio codecs (mp3/opus/aac) for speech output.
- No LAN or remote exposure, no discovery protocol, no port-forwarding help. Loopback only, by design.
- No replacement of the existing Tauri `invoke()`/event IPC. This is an additive surface for external processes, not a frontend rewrite.
- No word-level timestamps, SRT, or VTT until sherpa-onnx's timing support for the bundled models is confirmed.

## 5. Open questions

1. Android 14+ foreground-service type for a listener that does not itself capture the microphone (WS dictation needs `microphone`, plain file-upload/speech may not). Check against current Play Store policy before phase 2 Android work starts.
2. Does sherpa-onnx expose word-level timestamps for the bundled models? Determines whether `timestamp_granularities`, `srt`, `vtt` can honestly be offered.
3. Do bundled multi-speaker Piper voices carry per-speaker names, or only numeric `sid`s? Determines whether `GET /v1/models` can show human labels.
4. Concurrency model for a live WS session versus the existing single-session engine lock: shared with `409`, or an independent lane?
5. Token storage: plaintext config file versus Android Keystore or a desktop OS keychain, for a secret whose entire exposure is local.
6. Exact `TextToSpeechService` engine-descriptor meta-data name and XML schema should be re-checked against current documentation at implementation time, this proposal paraphrases the general shape.
7. Should `GET /v1/health` be the only unauthenticated route, for lightweight liveness probes by companion tools?

## 6. Sources

Verified this session (web): OpenAI `POST /v1/audio/speech` and `POST /v1/audio/transcriptions` parameters, `response_format` enums, and streaming behaviour (chunked transfer for speech, SSE `transcript.text.delta`/`transcript.text.done` for transcription), fetched from `developers.openai.com` (`platform.openai.com` API reference pages returned HTTP 403 to automated fetches this session). Android `TextToSpeechService` and `RecognitionService` abstract methods and manifest shapes, fetched from `developer.android.com` directly. OpenAI and Anthropic error envelope shapes, confirmed via search against community references and `docs.anthropic.com/en/api/errors`.

Recalled from training, not re-verified: that Android has no standard Settings picker for third-party `RecognitionService` implementations comparable to the TTS engine picker (the basis for ranking that surface lower in section 3, worth a direct check before a final decision), and the exact meta-data attribute name/XML schema for the TTS engine descriptor (open question 6).

Verified directly against this codebase, not web: the full `#[tauri::command]` list and `generate_handler!` registration in `lib.rs`. `Transcriber::transcribe(samples, sample_rate)` and `Vad::accept(samples)` signatures, both decoupled from `cpal`. No existing HTTP/TCP server code or dependency, `hound` as dev-only, no audio-decode crate, in `Cargo.toml`. No `ProcessTextActivity`, `RecognitionService`, `TextToSpeechService`, or AIDL service anywhere under `src-tauri/gen/android`, `ACTION_SEND` is the only implemented external Android entry point today. The internal TTS voice representation (model id string, numeric `sid`, optional `"custom:<name>"` form), the reason the proposed `<model>#<sid>` external voice id is a new but consistent convention rather than an existing one.
